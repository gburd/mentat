-- pg_mentat 1.18.0 -> 1.19.0 upgrade.
--
-- Each section below belongs to one change; add yours as a new section under a
-- line `-- === <name> ===`. Script-time rules: pg_catalog-qualify every call,
-- pin search_path per DO block with pg_catalog.set_config and restore it, and
-- never SET LOCAL search_path (scripts/pg/check-script-search-path.py).

-- === pg-subs ===
-- ---------------------------------------------------------------------------
-- Subscription and materialized-view rows left by 1.17's drop_store (#380).
-- 1.17's drop_store dropped the store's schema but kept its rows in
-- mentat.subscriptions and mentat.materialized_views. A leftover row blocks
-- a later store of that name: rename_store to it failed on the unique key,
-- and subscribe() said "already exists" (a subscription name is one per
-- database since 1.18.0, so it blocked that name for every store). Rows whose
-- store is neither 'default' nor in mentat.stores are deleted; their triggers
-- and views went with the dropped schema.
--
-- In replica mode: the installing role holds TRIGGER on these tables, and a
-- trigger it made would otherwise fire here as superuser (see the 1.18.0
-- upgrade's cache_generation insert).
--
-- Subscription and on_write view triggers made before 1.19.0 keep their old
-- bodies (no pinned search_path, #383). They are not regenerated here: the
-- query text in mentat.subscriptions is writable by the installing role.
-- To pick up the fix, unsubscribe and subscribe again (drop_matview and
-- materialize for an on_write view).
-- ---------------------------------------------------------------------------
DO $orphans$
DECLARE
    gone TEXT;
    srr TEXT := pg_catalog.current_setting('session_replication_role');
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    PERFORM pg_catalog.set_config('session_replication_role', 'replica', true);
    WITH d AS (
        DELETE FROM mentat.subscriptions s
         WHERE s.store_name <> 'default'
           AND NOT EXISTS (SELECT 1 FROM mentat.stores st WHERE st.store_name = s.store_name)
        RETURNING s.store_name, s.name
    )
    SELECT pg_catalog.string_agg(pg_catalog.format('%L on store %L', d.name, d.store_name), ', '
                                 ORDER BY d.store_name, d.name)
      INTO gone FROM d;
    IF gone IS NOT NULL THEN
        RAISE WARNING 'pg_mentat upgrade: deleted subscriptions % (their stores were dropped)', gone;
    END IF;
    WITH d AS (
        DELETE FROM mentat.materialized_views v
         WHERE v.store_name <> 'default'
           AND NOT EXISTS (SELECT 1 FROM mentat.stores st WHERE st.store_name = v.store_name)
        RETURNING v.store_name, v.view_name
    )
    SELECT pg_catalog.string_agg(pg_catalog.format('%L on store %L', d.view_name, d.store_name), ', '
                                 ORDER BY d.store_name, d.view_name)
      INTO gone FROM d;
    IF gone IS NOT NULL THEN
        RAISE WARNING 'pg_mentat upgrade: deleted materialized views % (their stores were dropped)', gone;
    END IF;
    PERFORM pg_catalog.set_config('session_replication_role', srr, true);
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$orphans$;

-- ---------------------------------------------------------------------------
-- Name the subscription and on_write view trigger functions made before
-- 1.19.0 (#383): they have no pinned search_path (see above). A WARNING only;
-- they work as before.
-- ---------------------------------------------------------------------------
DO $unpinned$
DECLARE
    fns TEXT;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    SELECT pg_catalog.string_agg(pg_catalog.format('%I.%I()', n.nspname, p.proname), ', '
                                 ORDER BY n.nspname, p.proname)
      INTO fns
      FROM pg_proc p
      JOIN pg_namespace n ON n.oid = p.pronamespace
      JOIN mentat.stores s ON s.schema_name = n.nspname
     WHERE p.prorettype = 'pg_catalog.trigger'::pg_catalog.regtype
       AND (p.proname LIKE 'mentat\_sub\_%' OR p.proname LIKE 'mentat\_matview\_refresh\_%')
       AND NOT EXISTS (SELECT 1 FROM pg_catalog.unnest(p.proconfig) AS setting
                        WHERE setting LIKE 'search\_path=%');
    IF fns IS NOT NULL THEN
        RAISE WARNING 'pg_mentat upgrade: trigger functions % were made before 1.19.0 and run with the '
                      'writer''s search_path. To pin it, unsubscribe and subscribe again '
                      '(mentat_sub_<name>), or drop_matview and materialize (mentat_matview_refresh_<name>).',
                      fns;
    END IF;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$unpinned$;

-- ---------------------------------------------------------------------------
-- Indexes 1.17 named stores made on the default store's tables (#381).
-- 1.17's create_store('x') made idx_x_fts_text on mentat.datoms_text_new and,
-- with pg_trgm, idx_x_trgm_text / idx_x_trgm_keyword on
-- mentat.datoms_text_new / mentat.datoms_keyword_new (names unquoted, so
-- folded to lower case, and cut to 63). They duplicate the default store's
-- own indexes, and every default-store write paid for them; neither the
-- 1.18.0 upgrade nor drop_store removed them. Only a superuser could make
-- them (CREATE INDEX needs the table's owner).
--
-- Dropped by exact name, for every store in mentat.stores and every
-- mentat_<name>_pre118 schema the 1.18.0 upgrade left, and only on those two
-- tables. The default store's own indexes there are *_pkey and
-- idx_datoms_<type>_new_{eavt,aevt,vaet,tx,fts}: none ends in _fts_text,
-- _trgm_text or _trgm_keyword, and all are under 63 characters, so no name
-- built here is one of them. An index that only looks like one (its store
-- was renamed or dropped under 1.17) gets a WARNING instead.
-- ---------------------------------------------------------------------------
DO $oldidx$
DECLARE
    idx RECORD;
    others TEXT;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    FOR idx IN
        WITH names AS (
            SELECT s.store_name AS n FROM mentat.stores s WHERE s.store_name <> 'default'
            UNION
            SELECT pg_catalog.substr(ns.nspname, 8, pg_catalog.length(ns.nspname) - 14)
              FROM pg_namespace ns WHERE ns.nspname LIKE 'mentat\_%\_pre118'
        )
        SELECT DISTINCT c.relname
          FROM names
         CROSS JOIN pg_catalog.unnest(ARRAY['_fts_text', '_trgm_text', '_trgm_keyword']) AS sfx
          JOIN pg_class c
            ON c.relname = pg_catalog.left('idx_' || pg_catalog.lower(names.n) || sfx, 63)
          JOIN pg_index i ON i.indexrelid = c.oid
         WHERE c.relnamespace = 'mentat'::regnamespace
           AND i.indrelid IN ('mentat.datoms_text_new'::regclass, 'mentat.datoms_keyword_new'::regclass)
         ORDER BY c.relname
    LOOP
        EXECUTE pg_catalog.format('DROP INDEX IF EXISTS %I.%I', 'mentat', idx.relname);
        RAISE WARNING 'pg_mentat upgrade: dropped index mentat.%, a duplicate a 1.17 named store made '
                     'on the default store''s tables', idx.relname;
    END LOOP;

    SELECT pg_catalog.string_agg(pg_catalog.format('%I.%I', 'mentat', c.relname), ', ' ORDER BY c.relname)
      INTO others
      FROM pg_class c JOIN pg_index i ON i.indexrelid = c.oid
     WHERE c.relnamespace = 'mentat'::regnamespace
       AND i.indrelid IN ('mentat.datoms_text_new'::regclass, 'mentat.datoms_keyword_new'::regclass)
       AND c.relname ~ '^idx_.+_(fts_text|trgm_text|trgm_keyword)$';
    IF others IS NOT NULL THEN
        RAISE WARNING 'pg_mentat upgrade: indexes % on mentat.datoms_text_new / mentat.datoms_keyword_new '
                      'look like ones a 1.17 named store made, for a store renamed or dropped since. '
                      'They duplicate the default store''s own indexes and slow its writes. If you did '
                      'not make them, DROP INDEX each.', others;
    END IF;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$oldidx$;

-- ---------------------------------------------------------------------------
-- subscribe(store, name, query, token_only) (#379): with token_only the
-- NOTIFY payload is {"tx": T, "hash": H} instead of the query result, so a
-- listener (any role that can connect can LISTEN) learns only that the
-- result changed and re-runs the query with its own privileges. A new
-- overload; the three-argument subscribe is unchanged. Same shape as the
-- pgrx-generated install SQL.
-- ---------------------------------------------------------------------------
CREATE FUNCTION "subscribe"("store_name" TEXT, "subscription_name" TEXT, "query" TEXT, "token_only" bool)
RETURNS TEXT
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'subscribe_token_only_wrapper';

-- ---------------------------------------------------------------------------
-- mentat.refresh_datalog_view(view_name, concurrently) could not be called
-- on any PostgreSQL version (#398): `concurrently` is a keyword in a PL/pgSQL
-- IF, so its body failed to compile ("syntax error at end of input"). The
-- parameter is now p_concurrently. CREATE OR REPLACE can't rename a
-- parameter, so the function is dropped and made again (nothing can depend
-- on a function that never compiled). Same body as src/lib.rs.
-- ---------------------------------------------------------------------------
DROP FUNCTION IF EXISTS mentat.refresh_datalog_view(TEXT, BOOLEAN);
CREATE FUNCTION mentat.refresh_datalog_view(
    view_name TEXT,
    p_concurrently BOOLEAN DEFAULT FALSE
)
RETURNS TEXT AS $$
DECLARE
    refresh_sql TEXT;
    safe_view_name TEXT;
BEGIN
    IF view_name !~ '^[a-zA-Z_][a-zA-Z0-9_]*(\.[a-zA-Z_][a-zA-Z0-9_]*)?$' THEN
        RAISE EXCEPTION 'Invalid view name: %. Use only alphanumeric characters and underscores.', view_name;
    END IF;
    safe_view_name := view_name;

    IF p_concurrently THEN
        refresh_sql := format('REFRESH MATERIALIZED VIEW CONCURRENTLY %s', safe_view_name);
    ELSE
        refresh_sql := format('REFRESH MATERIALIZED VIEW %s', safe_view_name);
    END IF;

    EXECUTE refresh_sql;

    RETURN format('MATERIALIZED VIEW %s refreshed', safe_view_name);
END;
$$ LANGUAGE plpgsql;

-- === pg-cache ===
-- ---------------------------------------------------------------------------
-- with() / mentat_with()'s helper mentat._speculative_exec (#403). Before
-- 1.19.0 with() ran CREATE OR REPLACE FUNCTION on every call, so it failed
-- ("permission denied for schema mentat") for every role that doesn't own
-- the mentat schema, and the function, where one exists, belongs to whoever
-- called with() first and not to the extension. It is dropped and made again
-- as an extension member (CREATE OR REPLACE refuses a function the extension
-- doesn't own). Runs as the caller; EXECUTE is PUBLIC's, as with()'s is. Same
-- body as sql/31_speculative_exec.sql.
-- ---------------------------------------------------------------------------
DROP FUNCTION IF EXISTS mentat._speculative_exec(TEXT, TEXT);
CREATE FUNCTION mentat._speculative_exec(p_schema TEXT, p_edn TEXT)
RETURNS TEXT
LANGUAGE plpgsql
AS $fn$
DECLARE
    _result TEXT;
BEGIN
    IF p_schema = 'mentat' THEN
        _result := @extschema@.edn_t(p_edn);
    ELSE
        -- p_schema is mentat_<store>
        _result := @extschema@.t(pg_catalog.substr(p_schema, 8), p_edn);
    END IF;
    -- Roll the subtransaction back, carrying the report out.
    RAISE EXCEPTION 'SPECULATIVE_OK:%', _result;
EXCEPTION
    WHEN OTHERS THEN
        IF SQLERRM LIKE 'SPECULATIVE_OK:%' THEN
            RETURN pg_catalog.substr(SQLERRM, 16);
        END IF;
        RAISE;
END;
$fn$;

-- === txtempid ===
-- ---------------------------------------------------------------------------
-- mentat.entid_collision_report (#414): a transaction's own annotations, made
-- with the reserved tempid "datomic.tx", are datoms on its entid, and the
-- report flagged every such entid as a collision (so repair_entid_collisions
-- would have moved the annotations to a new entity). A transaction entid is
-- now a collision only if a non-tx sequence could have issued it: at most
-- partition_user_seq's or partition_db_seq's last_value. Same body as
-- sql/25_entid_collision_repair.sql; entid_collision_count and
-- repair_entid_collisions read this function and pick up the change.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.entid_collision_report(store BIGINT DEFAULT 0)
RETURNS TABLE (
    entid          BIGINT,
    non_tx_datoms  BIGINT,
    is_attribute   BOOLEAN,
    ref_incoming   BIGINT
)
LANGUAGE sql STABLE
AS $$
    WITH tx_entids AS (
        SELECT tx AS e FROM mentat.transactions
         WHERE tx <= GREATEST((SELECT last_value FROM mentat.partition_user_seq),
                              (SELECT last_value FROM mentat.partition_db_seq))
    ),
    non_tx AS (
        -- entids that carry a real (non-txInstant) datom as an entity
        SELECT d.e, count(*) AS n
        FROM mentat.datoms d
        WHERE d.added AND d.a <> 50   -- 50 = :db/txInstant
        GROUP BY d.e
    )
    SELECT
        t.e AS entid,
        nt.n AS non_tx_datoms,
        EXISTS (SELECT 1 FROM mentat.datoms a WHERE a.a = t.e) AS is_attribute,
        (SELECT count(*) FROM mentat.datoms r
          WHERE r.value_type_tag = 0 AND r.v_ref = t.e AND r.added) AS ref_incoming
    FROM tx_entids t
    JOIN non_tx nt ON nt.e = t.e
    ORDER BY t.e
$$;

-- === pg-idents ===
-- ---------------------------------------------------------------------------
-- Idents moved by 1.18.0 and earlier (#415). Re-asserting an existing ident
-- without :db/id ([{:db/ident :color/red :db/doc "red"}]) pointed the store's
-- idents row at a new, empty entity, so values written afterwards went to it
-- and the ident's data split across two entities. The transactor no longer
-- does that. The upgrade does not repoint existing ones: values written after
-- a move refer to the new entity, merging them needs a check (two values of
-- one attribute), and a second move left no trace of the first. It adds
-- mentat.moved_idents(store), a read-only list, and
-- mentat.repair_moved_idents(store, dry_run DEFAULT true) (same bodies as
-- sql/32_moved_idents.sql), and WARNs if the default store has a moved ident.
-- ---------------------------------------------------------------------------
-- mentat.moved_idents(store TEXT DEFAULT 'default')
--
-- One row per ident whose idents row names a different entity than the one
-- holding its live [O :db/ident ident] datom: the shape the bug left.
--   ident     the ident, ':color/red'
--   moved_to  N, the entid the idents row names
--   owner     O, the entity with the live :db/ident datom
--   n_datoms  live datoms with N as their entity
--   n_refs    live ref values that point at N
--   blocker   NULL if repair_moved_idents can repair it, else why it can't
--
-- A second re-assert moved the ident again, from N to a new N2, and left no
-- trace of N: refs written to N between the two moves are not found here.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.moved_idents(store TEXT DEFAULT 'default')
RETURNS TABLE (
    ident    TEXT,
    moved_to BIGINT,
    owner    BIGINT,
    n_datoms BIGINT,
    n_refs   BIGINT,
    blocker  TEXT
)
LANGUAGE plpgsql STABLE
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    s TEXT;
    sid BIGINT;
    rec RECORD;
    ty TEXT;
    n BIGINT;
    hit BIGINT;
    types TEXT[] := ARRAY['ref', 'boolean', 'long', 'double', 'instant',
                          'text', 'keyword', 'uuid', 'bytes'];
BEGIN
    SELECT st.schema_name, st.store_id INTO s, sid
      FROM mentat.stores st WHERE st.store_name = store;
    IF s IS NULL THEN
        RAISE EXCEPTION 'pg_mentat: no store named %', store;
    END IF;
    -- 10 is :db/ident; its values are stored without the leading ':'.
    FOR rec IN EXECUTE format(
        'SELECT i.ident, i.entid AS moved_to, k.e AS owner
           FROM %1$I.idents i
           JOIN %1$I.current_keyword k
             ON k.store_id = $1 AND k.a = 10 AND k.v = substr(i.ident, 2)
          WHERE k.e <> i.entid
          ORDER BY i.ident', s) USING sid
    LOOP
        ident := rec.ident;
        moved_to := rec.moved_to;
        owner := rec.owner;
        n_datoms := 0;
        blocker := NULL;
        EXECUTE format('SELECT count(*) FROM %I.current_ref WHERE store_id = $1 AND v = $2', s)
            INTO n_refs USING sid, rec.moved_to;
        FOREACH ty IN ARRAY types LOOP
            EXECUTE format('SELECT count(*) FROM %I.%I WHERE store_id = $1 AND e = $2',
                           s, 'current_' || ty)
                INTO n USING sid, rec.moved_to;
            n_datoms := n_datoms + n;
            -- A cardinality-one attribute with a different value on each:
            -- the merged entity would hold two. (Two sets of a
            -- cardinality-many attribute merge into one.)
            EXECUTE format(
                'SELECT min(x.a) FROM %1$I.%2$I x
                   JOIN %1$I.schema sc ON sc.entid = x.a AND sc.cardinality = ''one''
                  WHERE x.store_id = $1 AND x.e = $2
                    AND EXISTS (SELECT 1 FROM %1$I.%2$I y
                                 WHERE y.store_id = $1 AND y.e = $3 AND y.a = x.a
                                   AND %3$s IS DISTINCT FROM %4$s)',
                s, 'current_' || ty,
                CASE WHEN ty = 'ref' THEN 'CASE WHEN y.v = $2 THEN $3 ELSE y.v END' ELSE 'y.v' END,
                CASE WHEN ty = 'ref' THEN 'CASE WHEN x.v = $2 THEN $3 ELSE x.v END' ELSE 'x.v' END)
                INTO hit USING sid, rec.moved_to, rec.owner;
            IF hit IS NOT NULL AND blocker IS NULL THEN
                blocker := format('both entities have a different value of attribute %s', hit);
            END IF;
            -- N as an attribute: its datoms would need N's schema row.
            EXECUTE format('SELECT count(*) FROM %I.%I WHERE store_id = $1 AND a = $2',
                           s, 'datoms_' || ty || '_new')
                INTO hit USING sid, rec.moved_to;
            IF hit > 0 AND blocker IS NULL THEN
                blocker := format('entity %s is used as an attribute', rec.moved_to);
            END IF;
        END LOOP;
        -- A :db.unique ref attribute that one entity has with value O and
        -- another with value N: both would hold one unique value.
        IF blocker IS NULL THEN
            EXECUTE format(
                'SELECT format(''entities %%s and %%s would both hold %%s %%s'', y.e, x.e, sc.ident, $4)
                   FROM %1$I.current_ref x
                   JOIN %1$I.schema sc ON sc.entid = x.a AND sc.unique_constraint IS NOT NULL
                   JOIN %1$I.current_ref y ON y.store_id = $1 AND y.a = x.a AND y.v = $3
                  WHERE x.store_id = $1 AND x.v = $2
                    AND CASE WHEN x.e = $2 THEN $3 ELSE x.e END
                        <> CASE WHEN y.e = $2 THEN $3 ELSE y.e END
                  ORDER BY sc.ident, y.e, x.e LIMIT 1', s)
                INTO blocker USING sid, rec.moved_to, rec.owner, rec.ident;
        END IF;
        EXECUTE format('SELECT count(*) FROM %I.idents WHERE entid = $1', s)
            INTO hit USING rec.owner;
        IF hit > 0 AND blocker IS NULL THEN
            blocker := format('entity %s has another ident in %I.idents', rec.owner, s);
        END IF;
        RETURN NEXT;
    END LOOP;
END;
$fn$;

-- ---------------------------------------------------------------------------
-- mentat.repair_moved_idents(store TEXT DEFAULT 'default',
--                            dry_run BOOLEAN DEFAULT true)
--
-- For each row of moved_idents(store) with no blocker: point the idents row
-- back at O, and move what was written to N onto O in the log: datoms with
-- N as entity get O, ref values that point at N point at O. Transaction ids
-- stay as they are, so the history keeps when each value was written; as-of
-- a time after the move now shows the value on O.
--
-- A datom written through both entities becomes one datom with one merged
-- history. A row that only repeats the state the datom already had is
-- dropped (no as-of answer changes): re-loading {:c/name "car" :c/color
-- :color/red} after the move, in one transaction, retracted [car :c/color O]
-- and asserted [car :c/color N]; the repair keeps neither row, and car's
-- color stays O, asserted when it was first written. The latest row wins,
-- as for any datom: a value asserted through O and retracted through N is
-- retracted. The current state of the merged datoms is then recomputed from
-- the log, so it equals as-of the latest transaction, and checked: a
-- mismatch, two values of a cardinality-one attribute, or one :db.unique
-- value on two entities fails the ident.
--
-- A row with a blocker is skipped and left as it is; an ident whose repair
-- fails is rolled back on its own. Returns one row per ident: action is what
-- was done ('repaired', 'would repair', 'skipped: why', or 'failed: error'),
-- and notes (NULL if none, one per line) what the merge changes besides
-- moving N's datoms onto O, read from the log before the rewrite, so a dry
-- run shows them too:
--   past overlap: from tx T1 until tx T2, O has two values of a
--     cardinality-one attribute (one written through each entity), or two
--     entities both hold O as one :db.unique value. As of those
--     transactions, queries show both.
--   [e a v] is live now and retracted after the repair: a later retraction
--     through the other entity wins (or the current row has no datom in the
--     log at all). The reverse, not live now and live after, only shows on
--     a store whose current state already disagreed with its log.
-- At most 50 lines per ident, then a count of the rest.
--
-- Run it with nothing writing to the store: a writer that resolved the ident
-- before the repair committed can still write a ref to N. Back up first; run
-- it in your own transaction and check moved_idents(store) is empty before
-- COMMIT.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.repair_moved_idents(
    store   TEXT    DEFAULT 'default',
    dry_run BOOLEAN DEFAULT true
)
RETURNS TABLE (ident TEXT, moved_to BIGINT, owner BIGINT, action TEXT, notes TEXT)
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    s TEXT;
    sid BIGINT;
    rec RECORD;
    nrow RECORD;
    ty TEXT;
    vmap TEXT;
    moved TEXT;
    scope TEXT;
    bad TEXT;
    ent TEXT;
    attr TEXT;
    shown TEXT;
    nk INT[];
    nt BIGINT[];
    nl TEXT[];
    repaired BOOLEAN := false;
    types TEXT[] := ARRAY['ref', 'boolean', 'long', 'double', 'instant',
                          'text', 'keyword', 'uuid', 'bytes'];
BEGIN
    SELECT st.schema_name, st.store_id INTO s, sid
      FROM mentat.stores st WHERE st.store_name = store;
    IF s IS NULL THEN
        RAISE EXCEPTION 'pg_mentat: no store named %', store;
    END IF;
    IF NOT dry_run THEN
        -- Wait for, and hold off, schema-changing transactions of the store
        -- (they lock this row first), and make every backend reload its idents.
        UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = store;
    END IF;
    -- How a note shows entity @ ($3 is O, $4 its ident) and attribute @.
    ent := format('coalesce(CASE WHEN @ = $3 THEN $4 END, '
                  '(SELECT ii.ident FROM %I.idents ii WHERE ii.entid = @), (@)::text)', s);
    attr := format('coalesce((SELECT sa.ident FROM %I.schema sa WHERE sa.entid = @), (@)::text)', s);
    FOR rec IN SELECT * FROM mentat.moved_idents(store) LOOP
        ident := rec.ident;
        moved_to := rec.moved_to;
        owner := rec.owner;
        notes := NULL;
        nk := '{}';
        nt := '{}';
        nl := '{}';
        IF rec.blocker IS NOT NULL THEN
            action := 'skipped: ' || rec.blocker;
        ELSE
          BEGIN
            IF NOT dry_run THEN
                EXECUTE format('UPDATE %I.idents SET entid = $1 WHERE ident = $2', s)
                    USING rec.owner, rec.ident;
            END IF;
            FOREACH ty IN ARRAY types LOOP
                -- $2 is N, $3 is O.
                IF ty = 'ref' THEN
                    vmap := 'CASE WHEN v = $2 THEN $3 ELSE v END';
                    moved := 'e = $2 OR v = $2';
                    scope := 'e IN ($2, $3) OR v IN ($2, $3)';
                    shown := ent;
                ELSE
                    vmap := 'v';
                    moved := 'e = $2';
                    scope := 'e IN ($2, $3)';
                    shown := CASE ty WHEN 'keyword' THEN $s$(':' || @)$s$
                                     WHEN 'text' THEN 'to_json(@)::text'
                                     ELSE '(@)::text' END;
                END IF;
                -- The notes. Each merged datom's state per transaction is
                -- asserted if any of its rows there is (the rule below); from
                -- its changes of state come the intervals it is live in. Only
                -- datoms with a row through N change: a pair of datoms both
                -- written only through O, or both only through N, overlapped
                -- before the repair too.
                FOR nrow IN EXECUTE format($q$
                    WITH k AS (
                        SELECT CASE WHEN e = $2 THEN $3 ELSE e END AS me, a, %3$s AS mv, tx,
                               bool_or(added) AS st, bool_or(%5$s) AS hit, bool_and(%5$s) AS allm
                          FROM %1$I.%2$I
                         WHERE store_id = $1 AND (%4$s)
                         GROUP BY 1, 2, 3, 4
                    ), ch AS (
                        SELECT * FROM (
                            SELECT me, a, mv, tx, st,
                                   lag(st) OVER (PARTITION BY me, a, mv ORDER BY tx) AS prev,
                                   bool_or(hit) OVER (PARTITION BY me, a, mv) AS h,
                                   bool_and(allm) OVER (PARTITION BY me, a, mv) AS am
                              FROM k) x
                         WHERE st IS DISTINCT FROM coalesce(prev, false)
                    ), live AS (
                        SELECT * FROM (
                            SELECT ch.*, lead(tx) OVER (PARTITION BY me, a, mv ORDER BY tx) AS t1
                              FROM ch) x
                         WHERE st
                    ), one AS (
                        SELECT live.* FROM live
                          JOIN %1$I.schema sc ON sc.entid = live.a AND sc.cardinality = 'one'
                    ), uniq AS (
                        SELECT live.* FROM live
                          JOIN %1$I.schema sc ON sc.entid = live.a AND sc.unique_constraint IS NOT NULL
                         WHERE %12$s
                    ), fin AS (
                        SELECT DISTINCT ON (me, a, mv) me, a, mv, st
                          FROM k ORDER BY me, a, mv, tx DESC
                    ), cur AS (
                        SELECT DISTINCT CASE WHEN e = $2 THEN $3 ELSE e END AS me, a, %3$s AS mv
                          FROM %1$I.%6$I
                         WHERE store_id = $1 AND (%4$s)
                    )
                    SELECT 1 AS k, greatest(i.tx, j.tx) AS t,
                           format('past overlap: from tx %%s until %%s, %%s has two values of %%s: %%s and %%s',
                                  greatest(i.tx, j.tx), coalesce('tx ' || least(i.t1, j.t1), 'now'),
                                  %7$s, %8$s, %9$s, %10$s) AS line
                      FROM one i
                      JOIN one j ON j.me = i.me AND j.a = i.a AND j.mv > i.mv
                     WHERE greatest(i.tx, j.tx) < coalesce(least(i.t1, j.t1), 9223372036854775807)
                       AND NOT ((NOT i.h AND NOT j.h) OR (i.am AND j.am))
                    UNION ALL
                    SELECT 1, greatest(i.tx, j.tx),
                           format('past overlap: from tx %%s until %%s, entities %%s and %%s both hold %%s %%s',
                                  greatest(i.tx, j.tx), coalesce('tx ' || least(i.t1, j.t1), 'now'),
                                  %7$s, %11$s, %8$s, $4)
                      FROM uniq i
                      JOIN uniq j ON j.a = i.a AND j.mv = i.mv AND j.me > i.me
                     WHERE greatest(i.tx, j.tx) < coalesce(least(i.t1, j.t1), 9223372036854775807)
                       AND NOT ((NOT i.h AND NOT j.h) OR (i.am AND j.am))
                    UNION ALL
                    SELECT 2, 0, format('[%%s %%s %%s] is live now and retracted after the repair (%%s)',
                                        %13$s, %14$s, %15$s,
                                        CASE WHEN fin.st IS NULL THEN 'it has no datom in the log'
                                             ELSE 'a later retraction wins' END)
                      FROM cur LEFT JOIN fin USING (me, a, mv)
                     WHERE NOT coalesce(fin.st, false)
                    UNION ALL
                    SELECT 2, 0, format('[%%s %%s %%s] is not live now and live after the repair',
                                        %16$s, %17$s, %18$s)
                      FROM fin
                     WHERE fin.st AND NOT EXISTS (SELECT 1 FROM cur
                                                   WHERE (cur.me, cur.a, cur.mv) = (fin.me, fin.a, fin.mv))$q$,
                    s, 'datoms_' || ty || '_new', vmap, scope, moved, 'current_' || ty,
                    replace(ent, '@', 'i.me'), replace(attr, '@', 'i.a'),
                    replace(shown, '@', 'i.mv'), replace(shown, '@', 'j.mv'),
                    replace(ent, '@', 'j.me'),
                    CASE WHEN ty = 'ref' THEN 'live.mv = $3' ELSE 'false' END,
                    replace(ent, '@', 'cur.me'), replace(attr, '@', 'cur.a'),
                    replace(shown, '@', 'cur.mv'),
                    replace(ent, '@', 'fin.me'), replace(attr, '@', 'fin.a'),
                    replace(shown, '@', 'fin.mv'))
                    USING sid, rec.moved_to, rec.owner, rec.ident
                LOOP
                    nk := nk || nrow.k;
                    nt := nt || nrow.t;
                    nl := nl || nrow.line;
                END LOOP;
                CONTINUE WHEN dry_run;
                -- In each datom that has a row through N, drop the rows that
                -- repeat its state, and keep one row per transaction (its net
                -- effect: asserted if any row of it is).
                EXECUTE format($q$
                    WITH m AS (
                        SELECT e, a, v, tx, added, (%4$s) AS moved,
                               CASE WHEN e = $2 THEN $3 ELSE e END AS me, %3$s AS mv
                          FROM %1$I.%2$I
                         WHERE store_id = $1 AND (%5$s)
                    ), k AS (
                        SELECT me, a, mv, tx, bool_or(added) AS st, bool_or(moved) AS hit
                          FROM m GROUP BY me, a, mv, tx
                    ), p AS (
                        SELECT k.*, lag(st) OVER w AS prev, bool_or(hit) OVER (PARTITION BY me, a, mv) AS mixed
                          FROM k WINDOW w AS (PARTITION BY me, a, mv ORDER BY tx)
                    ), r AS (
                        SELECT m.e, m.a, m.v, m.tx, p.st = coalesce(p.prev, false) AS noop,
                               row_number() OVER (PARTITION BY m.me, m.a, m.mv, m.tx
                                                  ORDER BY m.added = p.st DESC) AS rn
                          FROM m JOIN p USING (me, a, mv, tx)
                         WHERE p.mixed
                    )
                    DELETE FROM %1$I.%2$I d USING r
                     WHERE d.store_id = $1 AND d.e = r.e AND d.a = r.a AND d.v = r.v
                       AND d.tx = r.tx AND (r.noop OR r.rn > 1)$q$,
                    s, 'datoms_' || ty || '_new', vmap, moved, scope)
                    USING sid, rec.moved_to, rec.owner;
                EXECUTE format('UPDATE %1$I.%2$I SET e = CASE WHEN e = $2 THEN $3 ELSE e END, v = %3$s
                                 WHERE store_id = $1 AND (%4$s)',
                               s, 'datoms_' || ty || '_new', vmap, moved)
                    USING sid, rec.moved_to, rec.owner;
                -- The current state of those datoms, from the log: the latest
                -- row of each wins (mentat.rebuild_current_projection's rule).
                EXECUTE format('DELETE FROM %1$I.%2$I WHERE store_id = $1 AND (%3$s)',
                               s, 'current_' || ty, scope)
                    USING sid, rec.moved_to, rec.owner;
                EXECUTE format($q$
                    INSERT INTO %1$I.%2$I (store_id, e, a, v, tx)
                    SELECT store_id, e, a, v, tx
                      FROM (SELECT DISTINCT ON (e, a, v) store_id, e, a, v, tx, added
                              FROM %1$I.%3$I
                             WHERE store_id = $1 AND (%4$s)
                             ORDER BY e, a, v, tx DESC) l
                     WHERE added$q$,
                    s, 'current_' || ty, 'datoms_' || ty || '_new', scope)
                    USING sid, rec.moved_to, rec.owner;
                -- Check what the repair wrote: current == log, one value per
                -- cardinality-one attribute, one entity per :db.unique value.
                EXECUTE format($q$
                    WITH l AS (
                        SELECT e, a, v FROM (SELECT DISTINCT ON (e, a, v) e, a, v, added
                                               FROM %1$I.%3$I
                                              WHERE store_id = $1 AND (%4$s)
                                              ORDER BY e, a, v, tx DESC) x
                         WHERE added
                    ), c AS (
                        SELECT e, a, v FROM %1$I.%2$I WHERE store_id = $1 AND (%4$s)
                    )
                    SELECT coalesce(
                        (SELECT format('the current state of entity %%s differs from its log', e)
                           FROM ((SELECT * FROM l EXCEPT SELECT * FROM c)
                                 UNION ALL (SELECT * FROM c EXCEPT SELECT * FROM l)) d LIMIT 1),
                        (SELECT format('entity %%s has two values of attribute %%s', c.e, c.a)
                           FROM c JOIN %1$I.schema sc ON sc.entid = c.a AND sc.cardinality = 'one'
                          GROUP BY c.e, c.a HAVING count(*) > 1 LIMIT 1),
                        (SELECT format('two entities hold one value of unique attribute %%s', c.a)
                           FROM c JOIN %1$I.schema sc ON sc.entid = c.a
                                 AND sc.unique_constraint IS NOT NULL
                          GROUP BY c.a, c.v HAVING count(*) > 1 LIMIT 1))$q$,
                    s, 'current_' || ty, 'datoms_' || ty || '_new', scope)
                    INTO bad USING sid, rec.moved_to, rec.owner;
                IF bad IS NOT NULL THEN
                    RAISE EXCEPTION '%', bad;
                END IF;
            END LOOP;
            IF dry_run THEN
                action := 'would repair';
            ELSE
                repaired := true;
                action := 'repaired';
            END IF;
          EXCEPTION WHEN OTHERS THEN
            action := 'failed: ' || SQLERRM;
          END;
        END IF;
        SELECT string_agg(u.l, E'\n' ORDER BY u.k, u.t, u.l) INTO notes
          FROM (SELECT * FROM unnest(nk, nt, nl) AS x(k, t, l) ORDER BY 1, 2, 3 LIMIT 50) u;
        IF cardinality(nl) > 50 THEN
            notes := notes || format(E'\n... and %s more', cardinality(nl) - 50);
        END IF;
        RETURN NEXT;
    END LOOP;
    IF repaired THEN
        UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = store;
    END IF;
END;
$fn$;

-- The check runs as the bootstrap superuser, so it reads only the default
-- store's own tables (extension members), never a named store's schema (its
-- owner controls what a relation there runs) or mentat.stores' schema_name
-- (writable by the installing role). Named stores are listed for the
-- operator to check.
DO $movedidents$
DECLARE
    n BIGINT;
    others TEXT;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    -- 10 is :db/ident; its values are stored without the leading ':'.
    SELECT pg_catalog.count(*) INTO n
      FROM mentat.idents i
      JOIN mentat.current_keyword k
        ON k.store_id = 0 AND k.a = 10 AND k.v = pg_catalog.substr(i.ident, 2)
     WHERE k.e <> i.entid;
    IF n > 0 THEN
        RAISE WARNING 'pg_mentat upgrade: the default store has % ident(s) that 1.18.0 or '
                      'earlier moved to an empty entity (issue #415). See SELECT * FROM '
                      'mentat.moved_idents() and mentat.repair_moved_idents().', n;
    END IF;
    SELECT pg_catalog.string_agg(pg_catalog.quote_literal(s.store_name), ', ' ORDER BY s.store_name)
      INTO others
      FROM mentat.stores s WHERE s.store_name <> 'default';
    IF others IS NOT NULL THEN
        RAISE WARNING 'pg_mentat upgrade: stores % were not checked for idents 1.18.0 or '
                      'earlier moved (issue #415). As each store''s owner, run SELECT * FROM '
                      'mentat.moved_idents(store).', others;
    END IF;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$movedidents$;
