-- Speculative transactions (with / mentat_with, #403).
--
-- with() runs the real transaction in this function's EXCEPTION block (a
-- subtransaction), then raises a marker exception carrying the report, which
-- rolls the writes back. Created once here, owned by the extension: before
-- 1.19.0 with() ran CREATE OR REPLACE FUNCTION on every call, which only the
-- owner of the mentat schema may, so it failed for every other role.
--
-- Runs as the caller (not SECURITY DEFINER): a speculative transaction needs
-- the privileges a real one needs. EXECUTE is PUBLIC's by default, as with()'s
-- is. Called only from Rust with bound parameters. Keep in sync with the
-- pg-cache section of sql/pg_mentat--1.18.0--1.19.0.sql.
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
