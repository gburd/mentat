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
-- The old names stay as thin wrappers so existing SQL keeps working, but each
-- raises a one-time-per-session WARNING pointing at its edn_* replacement, so
-- a migrating codebase gets a loud, actionable nudge without flooding the log
-- on every call. The guard uses a session-local GUC (mentat.warned_<name>);
-- set_config(..., false) scopes it to the session. edn_* remains canonical.
--
-- NOTE: only these four names are deprecated. The other mentat_* functions
-- (mentat_explain, mentat_with, mentat_schema, ...) have no edn_* equivalent
-- and are NOT deprecated.
--
-- Volatility and strictness match the pre-1.9.0 C functions (VOLATILE,
-- STRICT). Keep in sync with sql/pg_mentat--1.8.0--1.9.0.sql.

CREATE FUNCTION public.mentat_transact(edn_tx TEXT)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE STRICT
AS $$
BEGIN
    IF current_setting('mentat.warned_transact', true) IS DISTINCT FROM 'on' THEN
        RAISE WARNING 'mentat_transact is deprecated since 1.9.0; use edn_t instead';
        PERFORM set_config('mentat.warned_transact', 'on', false);
    END IF;
    RETURN public.edn_t(edn_tx);
END;
$$;
COMMENT ON FUNCTION public.mentat_transact(TEXT) IS 'Deprecated since 1.9.0: use edn_t';

CREATE FUNCTION public.mentat_query(query TEXT, inputs JSONB DEFAULT '{}'::JSONB)
RETURNS JSONB
LANGUAGE plpgsql VOLATILE STRICT
AS $$
BEGIN
    IF current_setting('mentat.warned_query', true) IS DISTINCT FROM 'on' THEN
        RAISE WARNING 'mentat_query is deprecated since 1.9.0; use edn_q instead';
        PERFORM set_config('mentat.warned_query', 'on', false);
    END IF;
    RETURN public.edn_q(query, inputs);
END;
$$;
COMMENT ON FUNCTION public.mentat_query(TEXT, JSONB) IS 'Deprecated since 1.9.0: use edn_q';

CREATE FUNCTION public.mentat_pull(pattern TEXT, entity_id BIGINT)
RETURNS JSONB
LANGUAGE plpgsql VOLATILE STRICT
AS $$
BEGIN
    IF current_setting('mentat.warned_pull', true) IS DISTINCT FROM 'on' THEN
        RAISE WARNING 'mentat_pull is deprecated since 1.9.0; use edn_pull instead';
        PERFORM set_config('mentat.warned_pull', 'on', false);
    END IF;
    RETURN public.edn_pull(pattern, entity_id);
END;
$$;
COMMENT ON FUNCTION public.mentat_pull(TEXT, BIGINT) IS 'Deprecated since 1.9.0: use edn_pull';
