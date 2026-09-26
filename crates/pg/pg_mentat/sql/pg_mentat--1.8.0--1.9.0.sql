-- pg_mentat 1.8.0 -> 1.9.0 upgrade.
--
-- 1.9.0 renames the four core SQL functions to the cross-backend edn_* names
-- shared with the SQLite and DuckDB extensions:
--
--   mentat_transact(text)        -> edn_t(text)
--   mentat_query(text, jsonb)    -> edn_q(text, jsonb)
--   mentat_pull(text, bigint)    -> edn_pull(text, bigint)
--   mentat_eval(text)            -> edn_eval(text)   (`script` builds only)
--
-- The old names are kept as deprecated LANGUAGE SQL wrappers over the new
-- ones, so existing SQL keeps working. mentat_query additionally gains
-- DEFAULT '{}' for `inputs` (matching mentat.q). The mentat.q / mentat.t /
-- mentat.pull aliases now call the edn_* functions directly. Every other
-- mentat_* function is unchanged.
--
-- ALTER EXTENSION UPDATE does not re-run the install script, so this creates
-- the new C entry points (same module symbols the old names used: the Rust
-- functions kept their names, only the SQL name changed) and then replaces the
-- old C functions in place with SQL wrappers. CREATE OR REPLACE keeps the
-- functions' OIDs, grants and extension membership.
--
-- Keep in sync with sql/26_deprecated_names.sql, sql/07_function_aliases.sql
-- and the deprecated_mentat_eval block in src/functions/script.rs.

-- 1. New C functions (same shape as the pgrx-generated 1.9.0 install SQL).
CREATE FUNCTION "edn_t"("edn_tx" TEXT) RETURNS TEXT
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_transact_wrapper';

CREATE FUNCTION "edn_q"("query" TEXT, "inputs" jsonb) RETURNS jsonb
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_query_wrapper';

CREATE FUNCTION "edn_pull"("pattern" TEXT, "entity_id" bigint) RETURNS jsonb
STRICT LANGUAGE c
AS 'MODULE_PATHNAME', 'mentat_pull_wrapper';

-- 2. Old names become deprecated SQL wrappers.
CREATE OR REPLACE FUNCTION public.mentat_transact(edn_tx TEXT)
RETURNS TEXT
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_t(edn_tx); $$;
COMMENT ON FUNCTION public.mentat_transact(TEXT) IS 'Deprecated since 1.9.0: use edn_t';

CREATE OR REPLACE FUNCTION public.mentat_query(query TEXT, inputs JSONB DEFAULT '{}'::JSONB)
RETURNS JSONB
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_q(query, inputs); $$;
COMMENT ON FUNCTION public.mentat_query(TEXT, JSONB) IS 'Deprecated since 1.9.0: use edn_q';

CREATE OR REPLACE FUNCTION public.mentat_pull(pattern TEXT, entity_id BIGINT)
RETURNS JSONB
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_pull(pattern, entity_id); $$;
COMMENT ON FUNCTION public.mentat_pull(TEXT, BIGINT) IS 'Deprecated since 1.9.0: use edn_pull';

-- 3. mentat_eval / edn_eval exist only when the module is built with the
--    `script` feature. Probe the module for the symbol: CREATE FUNCTION ...
--    LANGUAGE c fails with undefined_function if it is absent, in which case
--    this is a default build and there is nothing to do. (A 1.8.0 script
--    build upgraded to a non-script 1.9.0 module is left as-is, as before.)
DO $do$
BEGIN
    BEGIN
        CREATE FUNCTION "edn_eval"("script" TEXT) RETURNS TEXT
        STRICT LANGUAGE c
        AS 'MODULE_PATHNAME', 'mentat_eval_wrapper';
    EXCEPTION WHEN undefined_function THEN
        RETURN;
    END;

    CREATE OR REPLACE FUNCTION public.mentat_eval(script TEXT)
    RETURNS TEXT
    LANGUAGE SQL VOLATILE STRICT
    AS $$ SELECT public.edn_eval(script); $$;
    COMMENT ON FUNCTION public.mentat_eval(TEXT) IS 'Deprecated since 1.9.0: use edn_eval';
END
$do$;

-- 4. Short aliases call the new names directly.
CREATE OR REPLACE FUNCTION mentat.q(query TEXT, inputs JSONB DEFAULT '{}'::JSONB)
RETURNS JSONB
LANGUAGE SQL STABLE
AS $$ SELECT public.edn_q(query, inputs); $$;

CREATE OR REPLACE FUNCTION mentat.t(edn_tx TEXT)
RETURNS TEXT
LANGUAGE SQL VOLATILE
AS $$ SELECT public.edn_t(edn_tx); $$;

CREATE OR REPLACE FUNCTION mentat.pull(pattern TEXT, entity_id BIGINT)
RETURNS JSONB
LANGUAGE SQL STABLE
AS $$ SELECT public.edn_pull(pattern, entity_id); $$;
