-- pg_mentat 1.9.0 -> 1.10.0 upgrade.
--
-- 1. AVET indexes on the non-ref current_<type> projection tables (see
--    sql/24_current_projection.sql). Plain CREATE INDEX: ALTER EXTENSION
--    runs in a transaction, so CONCURRENTLY is not available; the build
--    takes a SHARE lock (reads continue, writes wait) for the duration.
--    On a large store, pre-build them CONCURRENTLY with these exact names
--    before running ALTER EXTENSION and the IF NOT EXISTS makes this a no-op.
CREATE INDEX IF NOT EXISTS idx_current_long_avet    ON mentat.current_long    (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_text_avet    ON mentat.current_text    (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_double_avet  ON mentat.current_double  (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_instant_avet ON mentat.current_instant (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_keyword_avet ON mentat.current_keyword (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_uuid_avet    ON mentat.current_uuid    (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_bytes_avet   ON mentat.current_bytes   (store_id, a, v, e);
CREATE INDEX IF NOT EXISTS idx_current_boolean_avet ON mentat.current_boolean (store_id, a, v, e);

-- 2. Automatic index management: registry, evidence and rules behind
--    mentat_tune_indexes (the mentat.auto_index* GUCs live in the module).
--    Verbatim copy of sql/27_auto_index.sql -- keep in sync.
-- Automatic index management (1.10.0). See docs/src/operations.md,
-- "Automatic index management", and src/auto_index.rs.
--
-- Everything the manager creates is named mentat_auto_* and recorded in
-- mentat.managed_indexes; the drop rule only ever touches rows of that table
-- (the CHECK pins the name), so it can never drop a primary key, the shipped
-- EAVT/AEVT/VAET/AVET indexes, or a user's index.

CREATE TABLE IF NOT EXISTS mentat.managed_indexes (
    store_id          BIGINT      NOT NULL,
    index_name        TEXT        PRIMARY KEY CHECK (index_name LIKE 'mentat\_auto\_%'),
    table_name        TEXT        NOT NULL,
    attr              BIGINT      NOT NULL,
    kind              TEXT        NOT NULL,
    reason            TEXT        NOT NULL,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    scans_at_creation BIGINT      NOT NULL DEFAULT 0,
    last_checked      TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Vestigial-rule state: the index's idx_scan and its table's write
    -- counter as of the last time the index was seen in use (or created).
    last_used         TIMESTAMPTZ NOT NULL DEFAULT now(),
    last_scans        BIGINT      NOT NULL DEFAULT 0,
    writes_at_use     BIGINT      NOT NULL DEFAULT 0
);

-- Range-predicate evidence, flushed from each backend's counters. The CHECK
-- keeps table_name to the typed history tables (it is spliced into DDL).
CREATE TABLE IF NOT EXISTS mentat.index_evidence (
    store_id   BIGINT NOT NULL,
    attr       BIGINT NOT NULL,
    table_name TEXT   NOT NULL CHECK (table_name ~ '^mentat\.datoms_[a-z]+_new$'),
    hits       BIGINT NOT NULL DEFAULT 0,
    last_seen  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (store_id, attr, table_name)
);

-- Cumulative scans of an index / writes to a table: pg_stat plus the
-- current transaction's not-yet-flushed counts.
CREATE OR REPLACE FUNCTION mentat._auto_index_scans(idx REGCLASS) RETURNS BIGINT
LANGUAGE sql STABLE AS $$
    SELECT pg_stat_get_numscans(idx) + pg_stat_get_xact_numscans(idx)
$$;

CREATE OR REPLACE FUNCTION mentat._auto_index_writes(tbl REGCLASS) RETURNS BIGINT
LANGUAGE sql STABLE AS $$
    SELECT pg_stat_get_tuples_inserted(tbl) + pg_stat_get_tuples_updated(tbl)
         + pg_stat_get_tuples_deleted(tbl) + pg_stat_get_xact_tuples_inserted(tbl)
         + pg_stat_get_xact_tuples_updated(tbl) + pg_stat_get_xact_tuples_deleted(tbl)
$$;

-- The rules. Returns what was (or, with dry_run, would be) done.
CREATE OR REPLACE FUNCTION mentat._tune_indexes(
    dry_run BOOLEAN, min_queries BIGINT, min_rows BIGINT, idle_window_s BIGINT)
RETURNS TABLE(action TEXT, index_name TEXT, table_name TEXT, reason TEXT)
LANGUAGE plpgsql AS $fn$
#variable_conflict use_column
DECLARE
    ev RECORD;
    m RECORD;
    idx TEXT;
    n_rows BIGINT;
    n_scans BIGINT;
    n_writes BIGINT;
    why TEXT;
    idle INTERVAL := make_interval(secs => idle_window_s);
BEGIN
    -- Create: an attribute seen in >= min_queries range predicates of
    -- temporal (as-of / since / history) queries, with >= min_rows rows in
    -- its history table, gets (store_id, v, e, tx) WHERE a = <entid> AND
    -- added there. The shipped VAET index leads with v, not a, so without it
    -- such a range scans every value of the attribute (AEVT + Filter).
    -- Current-state reads are not candidates: the shipped AVET index
    -- (store_id, a, v, e) already serves their ranges.
    FOR ev IN
        SELECT e.* FROM mentat.index_evidence e
        WHERE e.hits >= min_queries
        ORDER BY e.hits DESC, e.store_id, e.attr, e.table_name
    LOOP
        idx := format('mentat_auto_%s_a%s', replace(ev.table_name, 'mentat.', ''), ev.attr);
        CONTINUE WHEN to_regclass('mentat.' || idx) IS NOT NULL;
        -- Bounded count: reads at most min_rows index entries.
        EXECUTE format('SELECT count(*) FROM (SELECT 1 FROM %s WHERE store_id = $1 AND a = $2 '
                       'LIMIT $3) s', ev.table_name)
           INTO n_rows USING ev.store_id, ev.attr, min_rows;
        CONTINUE WHEN n_rows < min_rows;
        why := format('%s temporal queries with a range predicate on attribute %s (store %s), '
                      '>= mentat.auto_index_min_queries = %s; >= %s rows in %s',
                      ev.hits, ev.attr, ev.store_id, min_queries, n_rows, ev.table_name);
        action := 'create'; index_name := idx; table_name := ev.table_name; reason := why;
        IF NOT dry_run THEN
            -- store_id is a bound parameter in the generated SQL, so it is a
            -- key column: only literals in the predicate, which a generic
            -- cached plan can still prove.
            EXECUTE format('CREATE INDEX %I ON %s (store_id, v, e, tx) WHERE a = %s AND added',
                           idx, ev.table_name, ev.attr);
            INSERT INTO mentat.managed_indexes
                (store_id, index_name, table_name, attr, kind, reason, writes_at_use)
            VALUES (ev.store_id, idx, ev.table_name, ev.attr, 'range', why,
                    mentat._auto_index_writes(ev.table_name::regclass));
            DELETE FROM mentat.index_evidence e
             WHERE e.store_id = ev.store_id AND e.attr = ev.attr AND e.table_name = ev.table_name;
            RAISE LOG 'mentat auto_index: created %: %', idx, why;
        END IF;
        RETURN NEXT;
    END LOOP;

    -- Drop: a managed index whose idx_scan has not grown for the idle window
    -- while its table took writes. The window also bounds the index's
    -- minimum age (hysteresis: nothing is dropped within it of creation).
    FOR m IN SELECT * FROM mentat.managed_indexes mi ORDER BY mi.index_name LOOP
        IF to_regclass('mentat.' || m.index_name) IS NULL THEN
            action := 'forget'; index_name := m.index_name; table_name := m.table_name;
            reason := 'index no longer exists';
            IF NOT dry_run THEN
                DELETE FROM mentat.managed_indexes mi WHERE mi.index_name = m.index_name;
            END IF;
            RETURN NEXT;
            CONTINUE;
        END IF;
        n_scans := mentat._auto_index_scans(('mentat.' || m.index_name)::regclass);
        n_writes := mentat._auto_index_writes(m.table_name::regclass);
        IF n_scans > m.last_scans THEN
            IF NOT dry_run THEN
                UPDATE mentat.managed_indexes mi
                   SET last_scans = n_scans, last_used = now(), writes_at_use = n_writes,
                       last_checked = now()
                 WHERE mi.index_name = m.index_name;
            END IF;
        ELSIF now() - m.last_used >= idle AND now() - m.created_at >= idle
              AND n_writes > m.writes_at_use THEN
            why := format('idx_scan unchanged at %s since %s (mentat.auto_index_idle_window = %s) '
                          'while %s took %s writes',
                          n_scans, m.last_used, idle, m.table_name, n_writes - m.writes_at_use);
            action := 'drop'; index_name := m.index_name; table_name := m.table_name; reason := why;
            IF NOT dry_run THEN
                EXECUTE format('DROP INDEX IF EXISTS mentat.%I', m.index_name);
                DELETE FROM mentat.managed_indexes mi WHERE mi.index_name = m.index_name;
                RAISE LOG 'mentat auto_index: dropped %: %', m.index_name, why;
            END IF;
            RETURN NEXT;
        ELSIF NOT dry_run THEN
            UPDATE mentat.managed_indexes mi SET last_checked = now()
             WHERE mi.index_name = m.index_name;
        END IF;
    END LOOP;
END
$fn$;

-- Amortized entry point (called from edn_t): flush evidence, optionally
-- tune, and never fail the caller. The evidence is committed with the
-- caller's transaction even when tuning is skipped. lock_ms
-- (mentat.auto_index_lock_timeout) bounds the wait for CREATE INDEX's SHARE
-- lock; on any error the tuning subtransaction rolls back, the skip is
-- LOGged, and the next tick retries. The caller's lock_timeout is restored.
CREATE OR REPLACE FUNCTION mentat._auto_index_tick(
    stores BIGINT[], attrs BIGINT[], tables TEXT[], hits BIGINT[],
    tune BOOLEAN, min_queries BIGINT, min_rows BIGINT, idle_window_s BIGINT,
    lock_ms BIGINT)
RETURNS BOOLEAN
LANGUAGE plpgsql
AS $fn$
DECLARE
    r RECORD;
    old_lock_timeout TEXT := current_setting('lock_timeout');
BEGIN
    PERFORM set_config('lock_timeout', lock_ms || 'ms', true);
    BEGIN
        INSERT INTO mentat.index_evidence AS e (store_id, attr, table_name, hits)
        SELECT * FROM unnest(stores, attrs, tables, hits)
        ON CONFLICT (store_id, attr, table_name)
        DO UPDATE SET hits = e.hits + EXCLUDED.hits, last_seen = now();
    EXCEPTION WHEN OTHERS THEN
        PERFORM set_config('lock_timeout', old_lock_timeout, true);
        RAISE LOG 'mentat auto_index: evidence flush skipped (%: %)', SQLSTATE, SQLERRM;
        RETURN false;
    END;
    IF tune THEN
        BEGIN
            FOR r IN SELECT * FROM mentat._tune_indexes(false, min_queries, min_rows, idle_window_s)
            LOOP END LOOP;
        EXCEPTION WHEN OTHERS THEN
            RAISE LOG 'mentat auto_index: tuning skipped (%: %)', SQLSTATE, SQLERRM;
        END;
    END IF;
    PERFORM set_config('lock_timeout', old_lock_timeout, true);
    RETURN true;
END
$fn$;

CREATE FUNCTION "mentat_tune_indexes"("dry_run" bool DEFAULT true)
RETURNS TABLE ("action" TEXT, "index_name" TEXT, "table_name" TEXT, "reason" TEXT)
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_tune_indexes_wrapper';

-- 3. edn_q_rows: stream a query's result rows (one JSON array per row).
CREATE FUNCTION "edn_q_rows"("query" TEXT, "inputs" jsonb DEFAULT '{}')
RETURNS SETOF jsonb
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_query_rows_wrapper';
