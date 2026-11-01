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
        RAISE NOTICE 'pg_mentat upgrade: deleted subscriptions % (their stores were dropped)', gone;
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
        RAISE NOTICE 'pg_mentat upgrade: deleted materialized views % (their stores were dropped)', gone;
    END IF;
    PERFORM pg_catalog.set_config('session_replication_role', srr, true);
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$orphans$;
