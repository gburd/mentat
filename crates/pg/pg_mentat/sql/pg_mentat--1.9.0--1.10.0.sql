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
