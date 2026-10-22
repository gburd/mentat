-- pg_mentat 1.17.2 -> 1.18.0 upgrade (1.17.1 reaches it through 1.17.1 -> 1.17.2).
--
-- Each section below belongs to one change; add yours as a new section.

-- === pull (as-of/since) ===
-- ---------------------------------------------------------------------------
-- Pull on a historical basis: edn_pull(pattern, entity_id, options).
--
-- A new 3-argument overload; the 2-argument edn_pull is unchanged. `options`
-- takes edn_q's temporal keys ({"asOf": T} / {"since": T}). edn_q's find-pull
-- calls it under an asOf/since query, so a pull in a historical query reads
-- that basis (before 1.18.0 it returned current values). Same shape as the
-- pgrx-generated install SQL.
-- ---------------------------------------------------------------------------
CREATE FUNCTION "edn_pull"("pattern" TEXT, "entity_id" bigint, "options" jsonb) RETURNS jsonb
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_pull_with_options_wrapper';

-- ---------------------------------------------------------------------------
-- since reads the current-state projection (issue #364): a TX index per
-- current_<type> table, as in sql/24_current_projection.sql, so a since
-- query that matches few rows doesn't scan every live datom of the
-- attribute. Plain CREATE INDEX: ALTER EXTENSION runs in a transaction, so
-- CONCURRENTLY is not available; the build takes a SHARE lock (reads
-- continue, writes wait). On a large store, pre-build them CONCURRENTLY with
-- these exact names first; IF NOT EXISTS then makes this a no-op.
-- ---------------------------------------------------------------------------
CREATE INDEX IF NOT EXISTS idx_current_ref_tx     ON mentat.current_ref     (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_long_tx    ON mentat.current_long    (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_text_tx    ON mentat.current_text    (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_double_tx  ON mentat.current_double  (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_instant_tx ON mentat.current_instant (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_keyword_tx ON mentat.current_keyword (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_uuid_tx    ON mentat.current_uuid    (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_bytes_tx   ON mentat.current_bytes   (store_id, tx);
CREATE INDEX IF NOT EXISTS idx_current_boolean_tx ON mentat.current_boolean (store_id, tx);
