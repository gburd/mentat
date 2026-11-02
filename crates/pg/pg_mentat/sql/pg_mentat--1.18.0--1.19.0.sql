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
-- holding its live [O :db/ident ident] datom: the shape the bug left. For a
-- bootstrap ident (:db/doc), O is its fixed entid (its schema row, below
-- 100): a store has no :db/ident datoms for those, so re-asserting one
-- ({:db/ident :db/doc :db/doc "..."}) moved the row to a new entity that also
-- got the :db/ident datom.
--   ident     the ident, ':color/red'
--   moved_to  N, the entid the idents row names
--   owner     O, the entity with the live :db/ident datom (or the bootstrap
--             entid)
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
    -- 10 is :db/ident; its values are stored without the leading ':'. A
    -- bootstrap ident's entity is its schema row's (entid < 100).
    FOR rec IN EXECUTE format(
        'SELECT i.ident, i.entid AS moved_to, coalesce(b.entid, k.e) AS owner
           FROM %1$I.idents i
           LEFT JOIN %1$I.schema b ON b.ident = i.ident AND b.entid < 100
           LEFT JOIN %1$I.current_keyword k
             ON k.store_id = $1 AND k.a = 10 AND k.v = substr(i.ident, 2)
          WHERE coalesce(b.entid, k.e) <> i.entid
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
-- A real run blocks every edn_t call on the store until the transaction it
-- runs in ends: it takes the store's write key exclusive, which each call
-- takes shared before it resolves an ident, so a writer waits, then reloads
-- the idents and writes to O (under READ COMMITTED; a REPEATABLE READ or
-- SERIALIZABLE writer still sees the idents of its first snapshot). It
-- waits for calls in flight to commit first. Back up first; run it in your
-- own transaction and check moved_idents(store) before COMMIT.
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
    h BIGINT;
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
        -- A snapshot taken before the wait below would miss what the writers it
        -- waited for wrote, and leave their refs on the empty entity.
        IF pg_catalog.current_setting('transaction_isolation') <> 'read committed' THEN
            RAISE EXCEPTION 'pg_mentat: repair_moved_idents(%, dry_run => false) must run under READ COMMITTED, not %',
                store, pg_catalog.current_setting('transaction_isolation');
        END IF;
        -- Wait for, and hold off, every edn_t call on the store: each takes the
        -- store's advisory key shared before it resolves an ident (write_locks.rs:
        -- namespace "Mnt0", then FNV-1a-32 of the store_id's 8 little-endian
        -- bytes). Taken before the generation row, which a schema-changing call
        -- locks after the key, so the two can't wait for each other.
        h := 2166136261;
        FOR i IN 0..7 LOOP
            h := ((h # ((sid >> (8 * i)) & 255)) * 16777619) & 4294967295;
        END LOOP;
        PERFORM pg_advisory_xact_lock(1299084336,
            (CASE WHEN h >= 2147483648 THEN h - 4294967296 ELSE h END)::int4);
        -- Then make every backend reload its idents (and wait for a
        -- schema-changing transaction in flight, which holds this row).
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
                    ), kr AS (
                        -- Only these can overlap: O's own cardinality-one
                        -- attributes, and :db.unique refs to O.
                        SELECT * FROM (
                            SELECT k.*, sc.cardinality = 'one' AND k.me = $3 AS c1,
                                   sc.unique_constraint IS NOT NULL AND %12$s AS u1
                              FROM k JOIN %1$I.schema sc ON sc.entid = k.a) x
                         WHERE c1 OR u1
                    ), ch AS (
                        SELECT * FROM (
                            SELECT me, a, mv, tx, st, c1, u1,
                                   lag(st) OVER (PARTITION BY me, a, mv ORDER BY tx) AS prev,
                                   bool_or(hit) OVER (PARTITION BY me, a, mv) AS h,
                                   bool_and(allm) OVER (PARTITION BY me, a, mv) AS am
                              FROM kr) x
                         WHERE st IS DISTINCT FROM coalesce(prev, false)
                    ), live AS (
                        SELECT * FROM (
                            SELECT ch.*, lead(tx) OVER (PARTITION BY me, a, mv ORDER BY tx) AS t1
                              FROM ch) x
                         WHERE st
                    ), one AS (
                        SELECT * FROM live WHERE c1
                    ), uniq AS (
                        SELECT * FROM live WHERE u1
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
                    CASE WHEN ty = 'ref' THEN 'k.mv = $3' ELSE 'false' END,
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
    -- 10 is :db/ident; its values are stored without the leading ':'. A
    -- bootstrap ident's entity is its schema row's (entid < 100).
    SELECT pg_catalog.count(*) INTO n
      FROM mentat.idents i
      LEFT JOIN mentat.schema b ON b.ident = i.ident AND b.entid < 100
      LEFT JOIN mentat.current_keyword k
        ON k.store_id = 0 AND k.a = 10 AND k.v = pg_catalog.substr(i.ident, 2)
     WHERE coalesce(b.entid, k.e) <> i.entid;
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

-- === pg-views ===
-- ---------------------------------------------------------------------------
-- Entities excised before 1.19.0 (#396). From 1.5.0 to 1.18.0 mentat_excise
-- deleted only the entities' log rows. Their current_<type> rows, which
-- every current-time read serves from, stayed, and so did their text in the
-- store's fulltext table, an enum entity's idents row and the default
-- store's attr_<n>_{vector,geom,tvec} rows. The 1.18 store views read the
-- log and hid them; the views below read the projection, so this runs
-- first. For each store mentat.excision_log names, it deletes what
-- mentat_excise deletes now:
--   * the excised entities' current_<type> rows that no log row backs (one
--     with the same e, a and tx): a value asserted for the same entity id
--     after the excision has its log row and stays;
--   * for an excised entity with no log row left, its idents rows and, in
--     the default store, its attr_<n>_* rows;
--   * text in the store's fulltext table that no assertion in the store's
--     log holds (the table has no entity column, and the excised entities'
--     log rows are gone).
-- An installed attribute is left alone (excising one is refused since
-- 1.19.0). This runs as superuser over tables a store's owner owns, so a
-- store is skipped, with a WARNING, unless every table it touches is a
-- plain table with built-in column types, and the ones it deletes from have
-- no DELETE trigger or rule and no foreign key pointing at them; every
-- statement names its table with ONLY. Nothing here runs code a store's
-- owner (or, on the default store, the installing role) wrote. A store_name that
-- names no store now (dropped or renamed) is skipped too; one WARNING lists
-- them.
-- ---------------------------------------------------------------------------
DO $excised$
DECLARE
    st RECORD;
    ty TEXT;
    tabs TEXT[];
    bad TEXT;
    ids BIGINT[];
    gone BIGINT[];
    done TEXT[] := '{}';
    n BIGINT;
    types TEXT[] := ARRAY['ref', 'boolean', 'long', 'double', 'instant', 'text', 'keyword', 'uuid', 'bytes'];
    srr TEXT := pg_catalog.current_setting('session_replication_role');
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    FOR st IN
        SELECT s.store_id, s.store_name, n.oid AS ns, n.nspname,
               pg_catalog.array_agg(DISTINCT xe) AS ids
          FROM mentat.excision_log l
          CROSS JOIN LATERAL pg_catalog.unnest(l.entity_ids) AS xe
          JOIN mentat.stores s ON s.store_name = l.store_name
          JOIN pg_namespace n ON n.nspname = s.schema_name
         WHERE xe >= 10000
           AND ((s.store_name = 'default' AND s.schema_name = 'mentat')
                OR (s.schema_name = 'mentat_' || s.store_name
                    AND s.store_name ~ '^[A-Za-z_][A-Za-z0-9_]*$'
                    AND pg_catalog.length(s.store_name) <= 56
                    AND pg_catalog.lower(s.store_name) NOT IN ('default', 'information_schema')
                    AND pg_catalog.lower(s.store_name) NOT LIKE 'pg\_%'))
         GROUP BY s.store_id, s.store_name, n.oid, n.nspname
         ORDER BY s.store_id
    LOOP
        done := done || st.store_name;
        tabs := ARRAY['schema', 'idents', 'fulltext']
                || ARRAY(SELECT 'current_' || t FROM pg_catalog.unnest(types) AS t)
                || ARRAY(SELECT 'datoms_' || t || '_new' FROM pg_catalog.unnest(types) AS t);
        IF st.nspname = 'mentat' THEN
            tabs := tabs || ARRAY(SELECT c.relname::TEXT FROM pg_class c
                                   WHERE c.relnamespace = st.ns AND c.relkind = 'r'
                                     AND c.relname ~ '^attr_[0-9]+_(vector|geom|tvec)$');
        END IF;
        SELECT pg_catalog.string_agg(tname, ', ' ORDER BY tname) INTO bad
          FROM pg_catalog.unnest(tabs) AS tname
          LEFT JOIN pg_class c ON c.relnamespace = st.ns AND c.relname = tname
         WHERE c.oid IS NULL OR c.relkind <> 'r'
            -- On the tables it deletes from: no DELETE trigger or rule, and no
            -- foreign key whose ON DELETE action would reach another table.
            OR (tname NOT LIKE 'datoms\_%' AND tname <> 'schema'
                AND (EXISTS (SELECT 1 FROM pg_trigger g
                              WHERE g.tgrelid = c.oid AND NOT g.tgisinternal AND (g.tgtype & 8) <> 0)
                     OR EXISTS (SELECT 1 FROM pg_rewrite r WHERE r.ev_class = c.oid AND r.ev_type = '4')
                     OR EXISTS (SELECT 1 FROM pg_constraint k WHERE k.confrelid = c.oid)))
            OR EXISTS (SELECT 1 FROM pg_attribute a JOIN pg_type y ON y.oid = a.atttypid
                        WHERE a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped
                          AND a.attname IN ('store_id', 'e', 'a', 'v', 'tx', 'added', 'entid', 'text_value')
                          AND NOT (a.attname = 'v' AND tname LIKE 'attr\_%')
                          AND y.typnamespace <> 'pg_catalog'::regnamespace);
        IF bad IS NOT NULL THEN
            RAISE WARNING 'pg_mentat upgrade: entities excised from store "%" before 1.19.0 were not looked '
                          'at: % in schema % (missing, not a plain table, a DELETE trigger or rule, or a '
                          'column type of its own). Their current values may still show. After the '
                          'upgrade, run mentat_excise again on the ids mentat.excision_log lists.',
                          st.store_name, bad, st.nspname;
            CONTINUE;
        END IF;

        -- Installed attributes stay.
        EXECUTE pg_catalog.format('SELECT pg_catalog.array_agg(x) FROM pg_catalog.unnest($1) AS x '
                                  'WHERE NOT EXISTS (SELECT 1 FROM ONLY %I.schema s WHERE s.entid = x)', st.nspname)
           INTO ids USING st.ids;
        CONTINUE WHEN ids IS NULL;
        gone := ids;
        FOREACH ty IN ARRAY types LOOP
            EXECUTE pg_catalog.format(
                'DELETE FROM ONLY %1$I.%2$I c WHERE c.store_id = $1 AND c.e = ANY($2) '
                'AND NOT EXISTS (SELECT 1 FROM ONLY %1$I.%3$I d WHERE d.store_id = c.store_id '
                'AND d.e = c.e AND d.a = c.a AND d.v = c.v AND d.tx = c.tx)',
                st.nspname, 'current_' || ty, 'datoms_' || ty || '_new') USING st.store_id, ids;
            EXECUTE pg_catalog.format('SELECT pg_catalog.array_agg(x) FROM pg_catalog.unnest($1) AS x '
                                      'WHERE NOT EXISTS (SELECT 1 FROM ONLY %I.%I d WHERE d.store_id = $2 AND d.e = x)',
                                      st.nspname, 'datoms_' || ty || '_new')
               INTO gone USING gone, st.store_id;
        END LOOP;
        IF gone IS NOT NULL THEN
            FOREACH ty IN ARRAY tabs LOOP
                IF ty LIKE 'attr\_%' THEN
                    EXECUTE pg_catalog.format('DELETE FROM ONLY %I.%I WHERE e = ANY($1)', st.nspname, ty) USING gone;
                END IF;
            END LOOP;
            EXECUTE pg_catalog.format('DELETE FROM ONLY %I.idents WHERE entid = ANY($1)', st.nspname) USING gone;
            GET DIAGNOSTICS n = ROW_COUNT;
            IF n > 0 THEN
                -- Other sessions reload the store's idents. In replica mode: the
                -- installing role holds TRIGGER on this table.
                PERFORM pg_catalog.set_config('session_replication_role', 'replica', true);
                UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = st.store_name;
                PERFORM pg_catalog.set_config('session_replication_role', srr, true);
            END IF;
        END IF;
        EXECUTE pg_catalog.format('DELETE FROM ONLY %1$I.fulltext f WHERE NOT EXISTS (SELECT 1 FROM ONLY %1$I.datoms_text_new d '
                                  'WHERE d.store_id = $1 AND d.v = f.text_value AND d.added)', st.nspname)
           USING st.store_id;
    END LOOP;
    SELECT pg_catalog.string_agg(DISTINCT pg_catalog.quote_literal(l.store_name), ', ') INTO bad
      FROM mentat.excision_log l
     WHERE l.store_name <> ALL (done);
    IF bad IS NOT NULL THEN
        RAISE WARNING 'pg_mentat upgrade: mentat.excision_log lists entities excised from %, which name no '
                      'store now (dropped, renamed, or a mentat.stores row edited by hand). For a renamed '
                      'store, run mentat_excise on those ids again under its new name after the upgrade.',
                      bad;
    END IF;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$excised$;

-- ---------------------------------------------------------------------------
-- SQL objects that answer for the current state read the current_<type>
-- projections (#395). Since 1.5.0 the datoms_<type>_new log is append-only:
-- a replacement or retraction appends a retraction row and leaves the old
-- row's added = true, so a read of the log with only `added = true` returned
-- values an entity no longer has. Same bodies as the install source:
-- src/lib.rs (datom_helpers), sql/23_operational_accessors.sql and
-- sql/monitoring_views.sql. Signatures and view columns are unchanged.
-- ---------------------------------------------------------------------------
    -- datom_text_like: Find entities whose text attribute matches a LIKE pattern.
    CREATE OR REPLACE FUNCTION mentat.datom_text_like(
        attr_ident TEXT,
        pattern TEXT
    )
    RETURNS TABLE(entity_id BIGINT, value TEXT, tx BIGINT)
    AS $$
    DECLARE
        attr_entid BIGINT;
    BEGIN
        SELECT mentat.resolve_ident(attr_ident) INTO attr_entid;
        IF attr_entid IS NULL THEN
            RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
        END IF;

        RETURN QUERY
        SELECT d.e, d.v, d.tx
        FROM mentat.current_text d
        WHERE d.store_id = 0
          AND d.a = attr_entid
          AND d.v LIKE pattern;
    END;
    $$ LANGUAGE plpgsql STABLE;

    -- datom_long_between: Find entities whose long attribute falls within a range.
    CREATE OR REPLACE FUNCTION mentat.datom_long_between(
        attr_ident TEXT,
        low_val BIGINT,
        high_val BIGINT
    )
    RETURNS TABLE(entity_id BIGINT, value BIGINT, tx BIGINT)
    AS $$
    DECLARE
        attr_entid BIGINT;
    BEGIN
        SELECT mentat.resolve_ident(attr_ident) INTO attr_entid;
        IF attr_entid IS NULL THEN
            RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
        END IF;

        RETURN QUERY
        SELECT d.e, d.v, d.tx
        FROM mentat.current_long d
        WHERE d.store_id = 0
          AND d.a = attr_entid
          AND d.v BETWEEN low_val AND high_val;
    END;
    $$ LANGUAGE plpgsql STABLE;

    -- datom_ref_in: Find entities whose ref attribute points to one of a set of targets.
    CREATE OR REPLACE FUNCTION mentat.datom_ref_in(
        attr_ident TEXT,
        ref_ids BIGINT[]
    )
    RETURNS TABLE(entity_id BIGINT, ref_value BIGINT, tx BIGINT)
    AS $$
    DECLARE
        attr_entid BIGINT;
    BEGIN
        SELECT mentat.resolve_ident(attr_ident) INTO attr_entid;
        IF attr_entid IS NULL THEN
            RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
        END IF;

        RETURN QUERY
        SELECT d.e, d.v, d.tx
        FROM mentat.current_ref d
        WHERE d.store_id = 0
          AND d.a = attr_entid
          AND d.v = ANY(ref_ids);
    END;
    $$ LANGUAGE plpgsql STABLE;

    -- datom_text_values: Get all current text values for a cardinality-many attribute on an entity.
    CREATE OR REPLACE FUNCTION mentat.datom_text_values(
        eid BIGINT,
        attr_ident TEXT
    )
    RETURNS TABLE(value TEXT, tx BIGINT)
    AS $$
    DECLARE
        attr_entid BIGINT;
    BEGIN
        SELECT mentat.resolve_ident(attr_ident) INTO attr_entid;
        IF attr_entid IS NULL THEN
            RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
        END IF;

        RETURN QUERY
        SELECT d.v, d.tx
        FROM mentat.current_text d
        WHERE d.store_id = 0
          AND d.e = eid
          AND d.a = attr_entid
        ORDER BY d.tx;
    END;
    $$ LANGUAGE plpgsql STABLE;

    -- datom_ref_values: Get all current ref values for a cardinality-many attribute on an entity.
    CREATE OR REPLACE FUNCTION mentat.datom_ref_values(
        eid BIGINT,
        attr_ident TEXT
    )
    RETURNS TABLE(ref_value BIGINT, tx BIGINT)
    AS $$
    DECLARE
        attr_entid BIGINT;
    BEGIN
        SELECT mentat.resolve_ident(attr_ident) INTO attr_entid;
        IF attr_entid IS NULL THEN
            RAISE EXCEPTION 'Unknown attribute ident: %', attr_ident;
        END IF;

        RETURN QUERY
        SELECT d.v, d.tx
        FROM mentat.current_ref d
        WHERE d.store_id = 0
          AND d.e = eid
          AND d.a = attr_entid
        ORDER BY d.tx;
    END;
    $$ LANGUAGE plpgsql STABLE;

CREATE OR REPLACE FUNCTION mentat.current(p_e BIGINT, p_a BIGINT, p_store BIGINT DEFAULT 0)
RETURNS TEXT
LANGUAGE plpgsql STABLE
AS $$
DECLARE
    v_type TEXT;
    v_out  TEXT;
BEGIN
    SELECT value_type::text INTO v_type FROM mentat.schema WHERE entid = p_a;

    -- Dispatch on the declared type: a single indexed lookup.
    IF v_type = 'string' THEN
        SELECT v INTO v_out FROM mentat.current_text
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'keyword' THEN
        SELECT v INTO v_out FROM mentat.current_keyword
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'long' THEN
        SELECT v::text INTO v_out FROM mentat.current_long
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'ref' THEN
        SELECT v::text INTO v_out FROM mentat.current_ref
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'double' THEN
        SELECT v::text INTO v_out FROM mentat.current_double
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'boolean' THEN
        SELECT v::text INTO v_out FROM mentat.current_boolean
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'instant' THEN
        SELECT to_char(v, 'YYYY-MM-DD"T"HH24:MI:SS.US"Z"') INTO v_out
            FROM mentat.current_instant
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'uuid' THEN
        SELECT v::text INTO v_out FROM mentat.current_uuid
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSIF v_type = 'bytes' THEN
        SELECT encode(v, 'hex') INTO v_out FROM mentat.current_bytes
            WHERE store_id = p_store AND e = p_e AND a = p_a
            ORDER BY tx DESC LIMIT 1;
    ELSE
        -- Type unknown: probe all tables, newest tx wins.
        SELECT value INTO v_out FROM (
            SELECT v AS value, tx FROM mentat.current_text    WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT v, tx FROM mentat.current_keyword WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT v::text, tx FROM mentat.current_long    WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT v::text, tx FROM mentat.current_ref     WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT v::text, tx FROM mentat.current_double  WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT v::text, tx FROM mentat.current_boolean WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT to_char(v,'YYYY-MM-DD"T"HH24:MI:SS.US"Z"'), tx FROM mentat.current_instant WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT v::text, tx FROM mentat.current_uuid    WHERE store_id=p_store AND e=p_e AND a=p_a
            UNION ALL SELECT encode(v,'hex'), tx FROM mentat.current_bytes WHERE store_id=p_store AND e=p_e AND a=p_a
            ORDER BY tx DESC LIMIT 1
        ) probe;
    END IF;

    RETURN v_out;
END;
$$;

CREATE OR REPLACE FUNCTION mentat.attribute_health()
RETURNS TABLE (
    attr_ident   TEXT,
    value_type   TEXT,
    backing_table TEXT,
    live_datoms  BIGINT,
    dead_pct     NUMERIC
)
LANGUAGE sql STABLE
AS $$
    WITH tbl AS (
        SELECT
            s.ident                                   AS attr_ident,
            s.value_type::text                        AS value_type,
            'mentat.datoms_' || CASE s.value_type::text
                WHEN 'string'  THEN 'text'
                WHEN 'boolean' THEN 'boolean'
                ELSE s.value_type::text
            END || '_new'                             AS backing_table,
            s.entid                                   AS a
        FROM mentat.schema s
    ),
    deadstats AS (
        SELECT
            ('mentat.' || c.relname)                  AS backing_table,
            n_live_tup, n_dead_tup,
            CASE WHEN (n_live_tup + n_dead_tup) > 0
                 THEN round(100.0 * n_dead_tup / (n_live_tup + n_dead_tup), 1)
                 ELSE 0 END                           AS dead_pct
        FROM pg_stat_user_tables c
        WHERE c.schemaname = 'mentat' AND c.relname LIKE 'datoms_%_new'
    )
    SELECT
        t.attr_ident,
        t.value_type,
        t.backing_table,
        (SELECT count(*) FROM mentat.current_datoms WHERE a = t.a)::BIGINT AS live_datoms,
        COALESCE(d.dead_pct, 0) AS dead_pct
    FROM tbl t
    LEFT JOIN deadstats d ON d.backing_table = t.backing_table
    ORDER BY live_datoms DESC;
$$;

CREATE OR REPLACE VIEW mentat.store_overview AS
WITH counts AS (
    SELECT value_type_tag, count(*) AS cnt
    FROM mentat.current_datoms
    GROUP BY value_type_tag
)
SELECT
    s.store_name,
    s.store_id,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 0), 0)::BIGINT  AS ref_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 1), 0)::BIGINT  AS boolean_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 2), 0)::BIGINT  AS long_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 3), 0)::BIGINT  AS double_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 4), 0)::BIGINT  AS instant_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 7), 0)::BIGINT  AS text_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 8), 0)::BIGINT  AS keyword_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 10), 0)::BIGINT AS uuid_datoms,
    COALESCE((SELECT cnt FROM counts WHERE value_type_tag = 11), 0)::BIGINT AS bytes_datoms,
    COALESCE((SELECT SUM(cnt) FROM counts), 0)::BIGINT                      AS total_datoms
FROM mentat.stores s
ORDER BY s.store_name;

-- ---------------------------------------------------------------------------
-- create_vertex_view / create_edge_view (sql/19_pg19_graph_helpers.sql)
-- make views over the current_<type> projection, like the views above: a
-- vertex or edge is what an entity has now. Same bodies as the install
-- source. The views they made before (mentat.v_<attr> / mentat.e_<attr>, over
-- the log with `AND added`) are made again with them, keeping their owner,
-- grants and options: only a view whose definition is exactly what 1.18 made
-- for the attribute its name is for, so a view of that name the installing
-- role wrote itself is left alone. Columns are unchanged.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.create_vertex_view(attr_ident TEXT)
RETURNS TEXT
LANGUAGE plpgsql
AS $$
DECLARE
    v_entid BIGINT;
    v_view TEXT;
    v_type TEXT;
    v_table TEXT;
BEGIN
    SELECT entid, value_type::text INTO v_entid, v_type
        FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION ':db.error/unknown-attribute Attribute % is not registered.', attr_ident;
    END IF;
    v_view := mentat._pg19_vertex_view_name(attr_ident);

    -- Map value_type to the current-state projection table.
    v_table := CASE v_type
        WHEN 'string'  THEN 'current_text'
        WHEN 'keyword' THEN 'current_keyword'
        WHEN 'long'    THEN 'current_long'
        WHEN 'ref'     THEN 'current_ref'
        WHEN 'double'  THEN 'current_double'
        WHEN 'boolean' THEN 'current_boolean'
        WHEN 'instant' THEN 'current_instant'
        WHEN 'uuid'    THEN 'current_uuid'
        WHEN 'bytes'   THEN 'current_bytes'
        ELSE NULL
    END;
    IF v_table IS NULL THEN
        RAISE EXCEPTION ':db.error/fn-arg attribute % has unsupported value_type %.',
            attr_ident, v_type;
    END IF;

    EXECUTE format(
        'CREATE OR REPLACE VIEW mentat.%I AS ' ||
        'SELECT e, v::TEXT AS label FROM mentat.%I WHERE store_id = 0 AND a = %s',
        v_view, v_table, v_entid
    );
    RETURN 'mentat.' || v_view;
END;
$$;

CREATE OR REPLACE FUNCTION mentat.create_edge_view(attr_ident TEXT)
RETURNS TEXT
LANGUAGE plpgsql
AS $$
DECLARE
    v_entid BIGINT;
    v_view TEXT;
    v_type TEXT;
BEGIN
    SELECT entid, value_type::text INTO v_entid, v_type
        FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION ':db.error/unknown-attribute Attribute % is not registered.', attr_ident;
    END IF;
    IF v_type <> 'ref' THEN
        RAISE EXCEPTION ':db.error/fn-arg attribute % must be :db.type/ref to be used as an edge (got type %).',
            attr_ident, v_type;
    END IF;
    v_view := mentat._pg19_edge_view_name(attr_ident);

    EXECUTE format(
        'CREATE OR REPLACE VIEW mentat.%I AS ' ||
        'SELECT (e * 100000 + v) AS id, e AS src, v AS dst, %L AS label ' ||
        'FROM mentat.current_ref WHERE store_id = 0 AND a = %s',
        v_view, attr_ident, v_entid
    );
    RETURN 'mentat.' || v_view;
END;
$$;

DO $graphviews$
DECLARE
    v RECORD;
    old_sql TEXT;
    same BOOLEAN;
    member BOOLEAN;
    ext OID;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    ext := (SELECT e.oid FROM pg_extension e WHERE e.extname = 'pg_mentat');
    FOR v IN
        SELECT c.oid, c.relname, c.reloptions, s.entid, s.ident, s.value_type::TEXT AS vt,
               c.relname = mentat._pg19_edge_view_name(s.ident) AS edge
          FROM pg_class c
          JOIN mentat.schema s ON c.relname IN (mentat._pg19_vertex_view_name(s.ident),
                                                mentat._pg19_edge_view_name(s.ident))
         WHERE c.relnamespace = 'mentat'::regnamespace AND c.relkind = 'v'
         ORDER BY c.relname
    LOOP
        CONTINUE WHEN v.edge AND v.vt <> 'ref';
        CONTINUE WHEN v.vt NOT IN ('string', 'keyword', 'long', 'ref', 'double', 'boolean', 'instant',
                                   'uuid', 'bytes');
        -- The statement 1.18 ran, made again as a temporary view: the two
        -- deparse alike only if the view is what 1.18 made.
        old_sql := CASE WHEN v.edge THEN
                pg_catalog.format('SELECT (e * 100000 + v) AS id, e AS src, v AS dst, %L AS label '
                                  'FROM mentat.datoms_ref_new WHERE a = %s AND added', v.ident, v.entid)
            ELSE
                pg_catalog.format('SELECT e, v::TEXT AS label FROM mentat.%I WHERE a = %s AND added',
                                  'datoms_' || CASE v.vt WHEN 'string' THEN 'text' ELSE v.vt END || '_new',
                                  v.entid)
            END;
        EXECUTE 'CREATE TEMPORARY VIEW pg_mentat_upgrade_graph_view AS ' || old_sql;
        same := pg_catalog.pg_get_viewdef(v.oid)
                = pg_catalog.pg_get_viewdef('pg_temp.pg_mentat_upgrade_graph_view'::regclass);
        DROP VIEW pg_temp.pg_mentat_upgrade_graph_view;
        CONTINUE WHEN NOT same;
        member := EXISTS (SELECT 1 FROM pg_depend d
                           WHERE d.classid = 'pg_class'::regclass AND d.objid = v.oid
                             AND d.refclassid = 'pg_extension'::regclass AND d.refobjid = ext
                             AND d.deptype = 'e');
        IF NOT member THEN
            EXECUTE pg_catalog.format('ALTER EXTENSION pg_mentat ADD VIEW mentat.%I', v.relname);
        END IF;
        IF v.edge THEN
            PERFORM mentat.create_edge_view(v.ident);
        ELSE
            PERFORM mentat.create_vertex_view(v.ident);
        END IF;
        -- CREATE OR REPLACE VIEW resets the view's options (security_invoker, ...).
        IF v.reloptions IS NOT NULL THEN
            EXECUTE pg_catalog.format('ALTER VIEW mentat.%I SET (%s)', v.relname,
                (SELECT pg_catalog.string_agg(pg_catalog.format('%I = %L', pg_catalog.split_part(o, '=', 1),
                                                                pg_catalog.substr(o, pg_catalog.strpos(o, '=') + 1)), ', ')
                   FROM pg_catalog.unnest(v.reloptions) AS o));
        END IF;
        IF NOT member THEN
            EXECUTE pg_catalog.format('ALTER EXTENSION pg_mentat DROP VIEW mentat.%I', v.relname);
        END IF;
    END LOOP;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$graphviews$;

-- ---------------------------------------------------------------------------
-- Multi-tenant Row-Level Security on the current_<type> projections. Since
-- 1.18.0 edn_q's current-time reads, and now current() and the datom_*
-- helpers too, read these tables, which had no policy: with
-- enable_multi_tenant_rls(true) armed, a tenant read every store's current
-- values. They get the policy the log tables have (sql/24_current_projection.sql),
-- enable_multi_tenant_rls toggles all eighteen tables (sql/10_narrow_storage.sql),
-- and where the log has RLS enabled now, so does the projection. Enabling
-- it takes an ACCESS EXCLUSIVE lock on each table, briefly.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.enable_multi_tenant_rls(enable BOOLEAN)
RETURNS INT AS $$
DECLARE
    t TEXT;
    n INT := 0;
    narrow_tables TEXT[] := ARRAY[
        'datoms_ref_new', 'datoms_long_new', 'datoms_text_new',
        'datoms_double_new', 'datoms_instant_new', 'datoms_keyword_new',
        'datoms_uuid_new', 'datoms_bytes_new', 'datoms_boolean_new',
        'current_ref', 'current_long', 'current_text',
        'current_double', 'current_instant', 'current_keyword',
        'current_uuid', 'current_bytes', 'current_boolean'
    ];
BEGIN
    FOREACH t IN ARRAY narrow_tables LOOP
        IF enable THEN
            EXECUTE format('ALTER TABLE mentat.%I ENABLE ROW LEVEL SECURITY', t);
        ELSE
            EXECUTE format('ALTER TABLE mentat.%I DISABLE ROW LEVEL SECURITY', t);
        END IF;
        n := n + 1;
    END LOOP;
    RETURN n;
END;
$$ LANGUAGE plpgsql;

COMMENT ON FUNCTION mentat.enable_multi_tenant_rls(boolean) IS
  'Toggle Row-Level Security on the nine narrow datom tables and the nine '
  'current_<type> projection tables. '
  'Pass true to enforce per-store isolation (queries are filtered by '
  'store_id = mentat.current_store_id()); pass false to disable enforcement '
  'and revert to single-store behaviour. Returns the number of tables '
  'affected (always 18 on success; 9 before 1.19.0). Pair with the mentat.enable_multi_tenant_rls '
  'GUC, which signals operator intent at the session/role level.';

DO $rls$
DECLARE
    ty TEXT;
    armed BOOLEAN;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    FOREACH ty IN ARRAY ARRAY['ref', 'long', 'text', 'double', 'instant', 'keyword', 'uuid', 'bytes', 'boolean'] LOOP
        EXECUTE pg_catalog.format('DROP POLICY IF EXISTS %I ON mentat.%I', 'store_isolation_current_' || ty,
                                  'current_' || ty);
        EXECUTE pg_catalog.format(
            'CREATE POLICY %I ON mentat.%I '
            'USING (store_id = mentat.current_store_id()) '
            'WITH CHECK (store_id = mentat.current_store_id())',
            'store_isolation_current_' || ty, 'current_' || ty);
        armed := (SELECT c.relrowsecurity FROM pg_class c
                   WHERE c.oid = pg_catalog.format('mentat.%I', 'datoms_' || ty || '_new')::regclass);
        EXECUTE pg_catalog.format('ALTER TABLE mentat.%I %s ROW LEVEL SECURITY', 'current_' || ty,
                                  CASE WHEN armed THEN 'ENABLE' ELSE 'DISABLE' END);
    END LOOP;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$rls$;

-- ---------------------------------------------------------------------------
-- bm25-match reads the default store's current_text (#395), so
-- create_fts_index builds its pg_fts index there, and the indexes it built
-- on datoms_text_new (named mentat_fts_<entid>, partial on a = <entid> AND
-- store_id = 0 AND added) are rebuilt on current_text with the same text
-- search configuration. Same body as sql/28_pg_fts_helpers.sql. Rebuilding
-- an index reads the attribute's live values: on a big attribute it takes a
-- while.
-- ---------------------------------------------------------------------------
CREATE OR REPLACE FUNCTION mentat.create_fts_index(
    attr_ident TEXT,
    regconfig  TEXT DEFAULT 'english'
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE
    attr_entid BIGINT;
    attr_type  TEXT;
    idx_name   TEXT;
BEGIN
    IF NOT mentat.has_pg_fts() THEN
        RAISE EXCEPTION USING
            ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_fts is not installed in this database. ' ||
                      'pg_fts is an OPTIONAL BM25 full-text index for pg_mentat (PostgreSQL 17+). ' ||
                      'To enable: install pg_fts per https://codeberg.org/gregburd/pg_fts, then ' ||
                      'run CREATE EXTENSION pg_fts; before retrying. Without it, :db/fulltext ' ||
                      'still works via the built-in tsvector/GIN path.';
    END IF;

    SELECT s.entid, s.value_type::text
      INTO attr_entid, attr_type
      FROM mentat.schema s
     WHERE s.ident = attr_ident;

    IF attr_entid IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) ||
                      ' is not defined in mentat.schema. Define it first with ' ||
                      ':db/valueType :db.type/string.';
    END IF;

    IF attr_type <> 'string' THEN
        RAISE EXCEPTION USING
            ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/wrong-type-for-fts-index Attribute ' || quote_literal(attr_ident) ||
                      ' has value type ''' || attr_type || ''' but pg_fts indexes only ' ||
                      ':db.type/string.';
    END IF;

    idx_name := 'mentat_fts_' || attr_entid;
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON mentat.current_text ' ||
        'USING fts (to_ftsdoc(%L, v)) WHERE a = %s AND store_id = 0',
        idx_name, regconfig, attr_entid
    );
    RETURN idx_name;
END;
$$;

DO $ftsidx$
DECLARE
    idx RECORD;
    fts_schema TEXT;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    -- pg_fts's functions, through its schema (the path is pinned).
    fts_schema := (SELECT n.nspname FROM pg_extension e JOIN pg_namespace n ON n.oid = e.extnamespace
                    WHERE e.extname = 'pg_fts');
    FOR idx IN
        SELECT c.relname, s.entid, e.cfg
          FROM pg_class c
          JOIN pg_index i ON i.indexrelid = c.oid
          JOIN pg_am am ON am.oid = c.relam
          JOIN mentat.schema s ON c.relname = 'mentat_fts_' || s.entid
          -- to_ftsdoc('<config>'::regconfig, v): the configuration it was built
          -- with, schema-qualified by pg_get_expr unless it is in pg_catalog.
          CROSS JOIN LATERAL (SELECT pg_catalog.substring(pg_catalog.pg_get_expr(i.indexprs, i.indrelid),
                                                          '^[^(]*\(''([^'']+)''::regconfig, v\)$') AS cfg) e
         WHERE c.relnamespace = 'mentat'::regnamespace
           AND i.indrelid = 'mentat.datoms_text_new'::regclass
           AND am.amname = 'fts'
           AND fts_schema IS NOT NULL
    LOOP
        IF idx.cfg IS NULL THEN
            RAISE WARNING 'pg_mentat upgrade: pg_fts index mentat.% not moved to mentat.current_text: it is '
                          'not on to_ftsdoc(<configuration>, v). bm25-match reads current_text now, so it '
                          'no longer uses it: drop it and run mentat.create_fts_index again.', idx.relname;
            CONTINUE;
        END IF;
        EXECUTE pg_catalog.format('DROP INDEX %I.%I', 'mentat', idx.relname);
        EXECUTE pg_catalog.format(
            'CREATE INDEX %I ON mentat.current_text USING fts (%I.to_ftsdoc(%L::pg_catalog.regconfig, v)) '
            'WHERE a = %s AND store_id = 0',
            idx.relname, fts_schema, idx.cfg, idx.entid);
    END LOOP;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$ftsidx$;

-- ---------------------------------------------------------------------------
-- A full-text GIN index on current_text (sql/24_current_projection.sql),
-- here and below on each named store's. A store's find_text() and
-- searchable_text, and q()'s (fulltext ...) in English, filter on
-- to_tsvector('english', v): on the log that was idx_datoms_text_new_fts,
-- which the projection had no counterpart for. The log's index stays (a
-- query of your own over the log may use it). Building it reads every live
-- string value.
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_current_text_fts
    ON mentat.current_text USING gin (pg_catalog.to_tsvector('english'::pg_catalog.regconfig, v));

-- ---------------------------------------------------------------------------
-- A named store's views and helper functions (facts, text_values, ...,
-- lookup_entity, entity_value, find_text) read the store's current_<type>
-- projections too; tx_log, entity_history and recent_changes are history
-- and still read the log. mentat._store_virtual_tables(store) returns the
-- CREATE OR REPLACE statements create_store uses now. Same shape as the
-- pgrx-generated install SQL.
-- ---------------------------------------------------------------------------
CREATE FUNCTION mentat."_store_virtual_tables"("store_name" TEXT)
RETURNS TABLE ("kind" TEXT, "name" TEXT, "args" TEXT, "sql" TEXT)
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'store_virtual_tables_wrapper';

-- Replace each of these objects a named store has, and only those: a store
-- may lack one (dropped by its owner), and nothing is created here. CREATE
-- OR REPLACE keeps the object's owner, grants and the user views built on
-- it (a view is never dropped), and the options and function attributes
-- its owner set. An object that can't be replaced (its owner changed a
-- view's columns or a function's return type, or another extension owns it)
-- keeps its old definition, with a WARNING; any other error aborts the
-- upgrade.
--
-- In an extension script CREATE OR REPLACE refuses an object the extension
-- doesn't own (CVE-2022-2625), and a store's objects are not members (the
-- 1.18.0 upgrade and create_store leave them out). So each is added to the
-- extension, replaced and dropped from it again, inside a subtransaction:
-- its membership ends as it began.
--
-- The trigram indexes create_store made with pg_trgm,
-- idx_datoms_{text,keyword}_new_trgm on the log, move to the projections
-- the views read (idx_current_{text,keyword}_trgm), unless a trigram index
-- is already there. This builds the index: on a big store it takes a while.
--
-- Stores are the ones the 1.18.0 upgrade would rebuild: mentat.stores is
-- writable by the installing role, so a row that create_store() would not
-- have made is skipped. So is a store without the 1.18 layout (one the
-- 1.18.0 upgrade skipped), with a WARNING. Nothing here reads a store's tables or calls a
-- function in a store's schema.
-- ---------------------------------------------------------------------------
DO $views$
DECLARE
    st RECORD;
    obj RECORD;
    idx RECORD;
    oid_ OID;
    ident TEXT;
    member BOOLEAN;
    ext OID;
    missing TEXT;
    types TEXT[] := ARRAY['ref', 'boolean', 'long', 'double', 'instant', 'text', 'keyword', 'uuid', 'bytes'];
    opts TEXT[];
    old_fn RECORD;
    new_fn RECORD;
    alter_ TEXT;
    cfg TEXT;
    cfg_name TEXT;
    cfg_val TEXT;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    ext := (SELECT e.oid FROM pg_extension e WHERE e.extname = 'pg_mentat');
    -- Rows left alone, as the 1.18.0 upgrade leaves them: one whose schema
    -- is gone, or that names a schema create_store() would not have made,
    -- gets a WARNING.
    FOR st IN
        SELECT s.store_name, s.schema_name, n.oid AS ns
          FROM mentat.stores s
          LEFT JOIN pg_namespace n ON n.nspname = s.schema_name
         WHERE s.schema_name IS DISTINCT FROM 'mentat'
           AND ((n.oid IS NULL AND s.schema_name = 'mentat_' || s.store_name)
                OR (n.oid IS NOT NULL AND s.schema_name IS DISTINCT FROM 'mentat_' || s.store_name))
         ORDER BY s.store_id
    LOOP
        IF st.ns IS NULL THEN
            RAISE WARNING 'pg_mentat upgrade: views of named store "%" not replaced: its schema % does not '
                          'exist (dropped by hand?). drop_store(%) removes the store.',
                          st.store_name, st.schema_name, pg_catalog.quote_literal(st.store_name);
        ELSE
            RAISE WARNING 'pg_mentat upgrade: views in schema % not replaced: its mentat.stores row "%" '
                          'would not have been made by create_store() (renamed by hand?). Rename it back, '
                          'or use rename_store, then run create_virtual_tables(%).',
                          st.schema_name, st.store_name, pg_catalog.quote_literal(st.store_name);
        END IF;
    END LOOP;
    FOR st IN
        SELECT s.store_name, n.oid AS ns, n.nspname
          FROM mentat.stores s
          JOIN pg_namespace n ON n.nspname = s.schema_name
         WHERE s.schema_name = 'mentat_' || s.store_name
           AND s.store_name ~ '^[A-Za-z_][A-Za-z0-9_]*$'
           AND pg_catalog.length(s.store_name) <= 56
           AND pg_catalog.lower(s.store_name) NOT IN ('default', 'information_schema')
           AND pg_catalog.lower(s.store_name) NOT LIKE 'pg\_%'
         ORDER BY s.store_id
    LOOP
        -- The tables the new definitions name. A 1.17 store the 1.18.0
        -- upgrade skipped (mentat_<name>_pre118 already existed, ...) still
        -- has the old wide layout.
        SELECT pg_catalog.string_agg(t, ', ' ORDER BY t) INTO missing
          FROM pg_catalog.unnest(ARRAY['schema', 'idents', 'transactions']
                 || ARRAY(SELECT 'current_' || ty FROM pg_catalog.unnest(types) AS ty)
                 || ARRAY(SELECT 'datoms_' || ty || '_new' FROM pg_catalog.unnest(types) AS ty)) AS t
         WHERE NOT EXISTS (SELECT 1 FROM pg_class c WHERE c.relnamespace = st.ns AND c.relname = t);
        IF missing IS NOT NULL THEN
            RAISE WARNING 'pg_mentat upgrade: views of named store "%" not replaced: schema % does not have the '
                          '1.18 layout (no %), so the 1.18.0 upgrade did not rebuild it. See "A store the upgrade '
                          'skipped" under "Upgrading from 1.17" in docs/src/multi-tenancy.md.',
                          st.store_name, st.nspname, missing;
            CONTINUE;
        END IF;
        -- The full-text index find_text and searchable_text use (see above),
        -- if v is still text.
        IF EXISTS (SELECT 1 FROM pg_class t JOIN pg_attribute a ON a.attrelid = t.oid
                    WHERE t.relnamespace = st.ns AND t.relname = 'current_text' AND t.relkind = 'r'
                      AND a.attname = 'v' AND a.atttypid = 'pg_catalog.text'::regtype
                      AND NOT a.attisdropped) THEN
            EXECUTE pg_catalog.format('CREATE INDEX IF NOT EXISTS idx_current_text_fts ON %I.current_text '
                                      'USING gin (pg_catalog.to_tsvector(''english''::pg_catalog.regconfig, v))',
                                      st.nspname);
        END IF;
        FOR obj IN SELECT * FROM mentat._store_virtual_tables(st.store_name) LOOP
            IF obj.kind = 'VIEW' THEN
                oid_ := (SELECT c.oid FROM pg_class c
                          WHERE c.relnamespace = st.ns AND c.relname = obj.name AND c.relkind = 'v');
                ident := pg_catalog.format('%I.%I', st.nspname, obj.name);
                member := EXISTS (SELECT 1 FROM pg_depend d
                                   WHERE d.classid = 'pg_class'::regclass AND d.objid = oid_
                                     AND d.refclassid = 'pg_extension'::regclass AND d.refobjid = ext
                                     AND d.deptype = 'e');
            ELSE
                oid_ := pg_catalog.to_regprocedure(pg_catalog.format('%I.%I(%s)', st.nspname, obj.name, obj.args));
                -- to_regprocedure finds a procedure or an aggregate too.
                IF (SELECT p.prokind FROM pg_proc p WHERE p.oid = oid_) <> 'f' THEN
                    RAISE WARNING 'pg_mentat upgrade: % of store "%" not replaced: it is not a function.',
                                  oid_::regprocedure, st.store_name;
                    CONTINUE;
                END IF;
                ident := oid_::regprocedure::TEXT;
                member := EXISTS (SELECT 1 FROM pg_depend d
                                   WHERE d.classid = 'pg_proc'::regclass AND d.objid = oid_
                                     AND d.refclassid = 'pg_extension'::regclass AND d.refobjid = ext
                                     AND d.deptype = 'e');
            END IF;
            CONTINUE WHEN oid_ IS NULL;
            -- CREATE OR REPLACE resets what its statement doesn't say: a
            -- view's options, a function's SECURITY DEFINER, SET, COST, ROWS,
            -- LEAKPROOF, PARALLEL, STRICT and volatility. Both 1.18.0 and
            -- this release make them with the defaults (and STABLE), so a
            -- setting that differs is the owner's: it is put back as it was.
            IF obj.kind = 'VIEW' THEN
                opts := (SELECT c.reloptions FROM pg_class c WHERE c.oid = oid_);
            ELSE
                SELECT p.* INTO old_fn FROM pg_proc p WHERE p.oid = oid_;
            END IF;
            BEGIN
                IF NOT member THEN
                    EXECUTE pg_catalog.format('ALTER EXTENSION pg_mentat ADD %s %s', obj.kind, ident);
                END IF;
                EXECUTE obj.sql;
                IF obj.kind = 'VIEW' AND opts IS NOT NULL THEN
                    EXECUTE pg_catalog.format('ALTER VIEW %s SET (%s)', ident,
                        (SELECT pg_catalog.string_agg(pg_catalog.format('%I = %L', pg_catalog.split_part(o, '=', 1),
                                                                        pg_catalog.substr(o, pg_catalog.strpos(o, '=') + 1)), ', ')
                           FROM pg_catalog.unnest(opts) AS o));
                ELSIF obj.kind = 'FUNCTION' THEN
                    SELECT p.* INTO new_fn FROM pg_proc p WHERE p.oid = oid_;
                    alter_ := '';
                    IF old_fn.prosecdef AND NOT new_fn.prosecdef THEN
                        alter_ := alter_ || ' SECURITY DEFINER';
                    END IF;
                    IF old_fn.proleakproof AND NOT new_fn.proleakproof THEN
                        alter_ := alter_ || ' LEAKPROOF';
                    END IF;
                    IF old_fn.proisstrict <> new_fn.proisstrict THEN
                        alter_ := alter_ || CASE WHEN old_fn.proisstrict THEN ' STRICT' ELSE ' CALLED ON NULL INPUT' END;
                    END IF;
                    IF old_fn.provolatile <> new_fn.provolatile THEN
                        alter_ := alter_ || CASE old_fn.provolatile WHEN 'i' THEN ' IMMUTABLE' WHEN 's' THEN ' STABLE'
                                                 ELSE ' VOLATILE' END;
                    END IF;
                    IF old_fn.proparallel <> new_fn.proparallel THEN
                        alter_ := alter_ || CASE old_fn.proparallel WHEN 's' THEN ' PARALLEL SAFE'
                                                 WHEN 'r' THEN ' PARALLEL RESTRICTED' ELSE ' PARALLEL UNSAFE' END;
                    END IF;
                    IF old_fn.procost <> new_fn.procost THEN
                        alter_ := alter_ || ' COST ' || old_fn.procost;
                    END IF;
                    IF old_fn.prorows <> new_fn.prorows THEN
                        alter_ := alter_ || ' ROWS ' || old_fn.prorows;
                    END IF;
                    IF alter_ <> '' THEN
                        EXECUTE 'ALTER FUNCTION ' || ident || alter_;
                    END IF;
                    -- As pg_dump writes them: a list that quotes its elements
                    -- (search_path, ...) element by element, any other value
                    -- as one literal.
                    FOREACH cfg IN ARRAY COALESCE(old_fn.proconfig, '{}') LOOP
                        cfg_name := pg_catalog.split_part(cfg, '=', 1);
                        cfg_val := pg_catalog.substr(cfg, pg_catalog.strpos(cfg, '=') + 1);
                        IF pg_catalog.lower(cfg_name) IN ('local_preload_libraries', 'oauth_validator_libraries',
                                                          'search_path', 'session_preload_libraries',
                                                          'shared_preload_libraries', 'temp_tablespaces',
                                                          'unix_socket_directories') THEN
                            cfg_val := (SELECT pg_catalog.string_agg(pg_catalog.format('%L',
                                                 CASE WHEN m[1] LIKE '"%' THEN pg_catalog.replace(
                                                          pg_catalog.substr(m[1], 2, pg_catalog.length(m[1]) - 2), '""', '"')
                                                      ELSE m[1] END), ', ')
                                          FROM pg_catalog.regexp_matches(cfg_val, '("(?:[^"]|"")*"|[^,\s]+)', 'g') AS m);
                            EXECUTE pg_catalog.format('ALTER FUNCTION %s SET %I = %s', ident, cfg_name,
                                                      COALESCE(cfg_val, ''''''));
                        ELSE
                            EXECUTE pg_catalog.format('ALTER FUNCTION %s SET %I = %L', ident, cfg_name, cfg_val);
                        END IF;
                    END LOOP;
                END IF;
                IF NOT member THEN
                    EXECUTE pg_catalog.format('ALTER EXTENSION pg_mentat DROP %s %s', obj.kind, ident);
                END IF;
            EXCEPTION WHEN invalid_table_definition OR invalid_function_definition
                           OR object_not_in_prerequisite_state THEN
                -- object_not_in_prerequisite_state is the class code 55000, so
                -- it also catches the rest of class 55: 55P03 (lock_timeout)
                -- and 55006 must abort the upgrade, not leave a view behind.
                -- 55000 itself: "already a member of extension".
                IF SQLSTATE NOT IN ('42P16', '42P13', '55000') THEN
                    RAISE;
                END IF;
                RAISE WARNING 'pg_mentat upgrade: % % of store "%" not replaced (%: %). It still reads the '
                              'log and returns replaced and retracted values.',
                              pg_catalog.lower(obj.kind), ident, st.store_name, SQLSTATE, SQLERRM;
            END;
        END LOOP;

        FOR idx IN
            SELECT c.relname AS old_name,
                   pg_catalog.regexp_replace(t.relname, '^datoms_(.*)_new$', 'current_\1') AS cur_table,
                   n.nspname AS trgm_schema
              FROM pg_class c
              JOIN pg_index i ON i.indexrelid = c.oid
              JOIN pg_class t ON t.oid = i.indrelid
              JOIN pg_opclass o ON o.oid = i.indclass[0]
              JOIN pg_namespace n ON n.oid = o.opcnamespace
             WHERE c.relnamespace = st.ns
               AND t.relname IN ('datoms_text_new', 'datoms_keyword_new')
               AND c.relname = 'idx_' || t.relname || '_trgm'
               AND o.opcname = 'gin_trgm_ops'
        LOOP
            -- A plain table with a text column v (its owner could have
            -- altered it), with no trigram index yet.
            IF EXISTS (SELECT 1 FROM pg_class t JOIN pg_attribute a ON a.attrelid = t.oid
                        WHERE t.relnamespace = st.ns AND t.relname = idx.cur_table AND t.relkind = 'r'
                          AND a.attname = 'v' AND a.atttypid = 'pg_catalog.text'::regtype
                          AND NOT a.attisdropped)
               AND NOT EXISTS (SELECT 1 FROM pg_index i JOIN pg_opclass o ON o.oid = i.indclass[0]
                                JOIN pg_class t ON t.oid = i.indrelid
                               WHERE t.relnamespace = st.ns AND t.relname = idx.cur_table
                                 AND o.opcname = 'gin_trgm_ops') THEN
                EXECUTE pg_catalog.format('CREATE INDEX IF NOT EXISTS %I ON %I.%I USING GIN (v %I.gin_trgm_ops)',
                                          'idx_' || idx.cur_table || '_trgm', st.nspname, idx.cur_table,
                                          idx.trgm_schema);
            END IF;
            EXECUTE pg_catalog.format('DROP INDEX %I.%I', st.nspname, idx.old_name);
        END LOOP;
    END LOOP;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$views$;
