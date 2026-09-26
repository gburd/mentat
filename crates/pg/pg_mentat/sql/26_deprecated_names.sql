-- Deprecated pre-1.9.0 names for the core functions.
--
-- 1.9.0 renamed the four core SQL functions to the cross-backend edn_* names
-- shared with the SQLite and DuckDB extensions:
--
--   mentat_transact(text)        -> edn_t(text)
--   mentat_query(text, jsonb)    -> edn_q(text, jsonb)
--   mentat_pull(text, bigint)    -> edn_pull(text, bigint)
--   mentat_eval(text)            -> edn_eval(text)   (`script` builds only;
--                                   its wrapper is in src/functions/script.rs)
--
-- The old names stay as thin SQL wrappers so existing SQL keeps working.
-- Volatility and strictness match the pre-1.9.0 C functions (VOLATILE,
-- STRICT). Keep in sync with sql/pg_mentat--1.8.0--1.9.0.sql.

CREATE FUNCTION public.mentat_transact(edn_tx TEXT)
RETURNS TEXT
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_t(edn_tx); $$;
COMMENT ON FUNCTION public.mentat_transact(TEXT) IS 'Deprecated since 1.9.0: use edn_t';

CREATE FUNCTION public.mentat_query(query TEXT, inputs JSONB DEFAULT '{}'::JSONB)
RETURNS JSONB
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_q(query, inputs); $$;
COMMENT ON FUNCTION public.mentat_query(TEXT, JSONB) IS 'Deprecated since 1.9.0: use edn_q';

CREATE FUNCTION public.mentat_pull(pattern TEXT, entity_id BIGINT)
RETURNS JSONB
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_pull(pattern, entity_id); $$;
COMMENT ON FUNCTION public.mentat_pull(TEXT, BIGINT) IS 'Deprecated since 1.9.0: use edn_pull';
