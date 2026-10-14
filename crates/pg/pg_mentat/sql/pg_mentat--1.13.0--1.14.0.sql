-- pg_mentat 1.13.0 -> 1.14.0 upgrade.
--
-- Datomic-review follow-ups (Codeberg #325, #326, #327, #328, #330, #332).
-- Most of the change is in the shared library (transactor, query compiler);
-- this script only brings the catalog in line:
--
--   (1) bootstrap the :db/fn attribute (entid 20) that user-defined
--       transaction functions live in (#325);
--   (2) fix mentat.attach_turbovec_attribute from 1.13.0, which declared
--       `turbovec.vector(N)` -- pg_turbovec's type takes no typmod, so the
--       helper always failed. The dimension is now a CHECK constraint (#332).

-- (1) :db/fn  ---------------------------------------------------------------
-- Same rows a fresh 1.14.0 install has (06_bootstrap_data.sql): the attribute
-- lives in mentat.schema / mentat.idents. CREATE EXTENSION writes no bootstrap
-- datoms for any attribute, so neither does the upgrade.
INSERT INTO mentat.schema (entid, ident, value_type, cardinality, unique_constraint, indexed, fulltext, component, no_history)
VALUES (20, ':db/fn', 'string', 'one', NULL, FALSE, FALSE, FALSE, FALSE)
ON CONFLICT (entid) DO NOTHING;

INSERT INTO mentat.idents (ident, entid) VALUES (':db/fn', 20)
ON CONFLICT (ident) DO NOTHING;

-- (2) attach_turbovec_attribute without the invalid typmod  -----------------
CREATE OR REPLACE FUNCTION mentat.attach_turbovec_attribute(attr_ident TEXT, dim INT)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE v_entid BIGINT; v_table TEXT;
BEGIN
    IF NOT mentat.has_pg_turbovec() THEN
        RAISE EXCEPTION USING ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_turbovec is not installed in this database. ' ||
                      'pg_turbovec is an OPTIONAL vector-similarity index for pg_mentat. To enable: ' ||
                      'install it per https://codeberg.org/gregburd/pg_turbovec, then run ' ||
                      'CREATE EXTENSION pg_turbovec; before retrying. (pgvector is an alternative; ' ||
                      'see mentat.attach_vector_attribute.)';
    END IF;
    SELECT entid INTO v_entid FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) ||
                      ' is not registered in mentat.schema.';
    END IF;
    IF dim <= 0 OR dim > 16000 THEN
        RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/fn-arg vector dimensionality must be in (0, 16000], got ' || dim || '.';
    END IF;
    v_table := 'attr_' || v_entid || '_tvec';
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS mentat.%I (e BIGINT PRIMARY KEY, ' ||
        'v turbovec.vector NOT NULL CHECK (turbovec.vector_dims(v) = %s))',
        v_table, dim);
    RETURN 'mentat.' || v_table;
END;
$$;
