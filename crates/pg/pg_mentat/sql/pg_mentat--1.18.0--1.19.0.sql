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
