-- pg_mentat <-> pg_turbovec integration helpers.
--
-- pg_turbovec (https://codeberg.org/gregburd/pg_turbovec, Apache-2.0) is a
-- vector-similarity index AM backed by Google Research's TurboQuant quantizer:
-- 1-bit sign-BQ with full-precision reranking, or 2/3/4-bit TurboQuant, with a
-- pgvector-compatible `vector` type and distance operators (<->, <#>, <=>,
-- <+>). It is a SOFT dependency, in the same spirit as the pgvector helpers
-- (sql/15); a project may use either. pg_turbovec installs into the `turbovec`
-- schema and supports PostgreSQL 13-19.
--
-- Design mirrors the pgvector integration: vectors live in a per-attribute aux
-- table keyed by entid, so no :db.type/vector schema change is needed:
--
--   mentat.attr_<entid>_tvec(e BIGINT PRIMARY KEY, v turbovec.vector(N))
--
-- A KNN query uses the (turbovec-near $ :attr "[...]" k) Datalog where-fn,
-- which orders by an operator (e.g. turbovec.<=>) against the aux table and
-- joins the rest of the clause graph back through the entid column.
--
-- Reference: https://codeberg.org/gregburd/pg_turbovec

-- mentat.has_pg_turbovec(): true if the pg_turbovec extension is installed.
CREATE OR REPLACE FUNCTION mentat.has_pg_turbovec()
RETURNS boolean
LANGUAGE sql STABLE
AS $$
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_turbovec');
$$;

-- mentat.attach_turbovec_attribute(attr_ident text, dim int)
--
-- Create the per-attribute aux table for storing TurboQuant vectors. `dim` is
-- the fixed embedding dimensionality. The attribute must already be registered
-- in mentat.schema (as :db.type/string or :db.type/long; the aux table is
-- keyed by entid only, like the pgvector helper). Idempotent; returns the
-- table name.
CREATE OR REPLACE FUNCTION mentat.attach_turbovec_attribute(
    attr_ident TEXT,
    dim        INT
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE
    v_entid BIGINT;
    v_table TEXT;
BEGIN
    IF NOT mentat.has_pg_turbovec() THEN
        RAISE EXCEPTION USING
            ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_turbovec is not installed in this database. ' ||
                      'pg_turbovec is an OPTIONAL vector-similarity index for pg_mentat. To enable: ' ||
                      'install it per https://codeberg.org/gregburd/pg_turbovec, then run ' ||
                      'CREATE EXTENSION pg_turbovec; before retrying. (pgvector is an alternative; ' ||
                      'see mentat.attach_vector_attribute.)';
    END IF;

    SELECT entid INTO v_entid FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) ||
                      ' is not registered in mentat.schema.';
    END IF;

    IF dim <= 0 OR dim > 16000 THEN
        RAISE EXCEPTION USING
            ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/fn-arg vector dimensionality must be in (0, 16000], got ' || dim || '.';
    END IF;

    v_table := 'attr_' || v_entid || '_tvec';
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS mentat.%I (e BIGINT PRIMARY KEY, v turbovec.vector(%s))',
        v_table, dim
    );
    RETURN 'mentat.' || v_table;
END;
$$;

-- mentat.create_turbovec_index(attr_ident text, ops text DEFAULT 'turbovec.vec_cosine_ops')
--
-- Build the pg_turbovec index on an aux table created by
-- attach_turbovec_attribute. `ops` selects the distance (vec_cosine_ops,
-- vec_l2_ops, vec_ip_ops, ...). Idempotent; returns the index name.
CREATE OR REPLACE FUNCTION mentat.create_turbovec_index(
    attr_ident TEXT,
    ops        TEXT DEFAULT 'turbovec.vec_cosine_ops'
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE
    v_entid  BIGINT;
    v_table  TEXT;
    idx_name TEXT;
BEGIN
    IF NOT mentat.has_pg_turbovec() THEN
        RAISE EXCEPTION USING
            ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_turbovec is not installed. ' ||
                      'See https://codeberg.org/gregburd/pg_turbovec.';
    END IF;

    SELECT entid INTO v_entid FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION USING
            ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) ||
                      ' is not registered in mentat.schema.';
    END IF;

    v_table  := 'attr_' || v_entid || '_tvec';
    idx_name := 'mentat_tvec_' || v_entid;
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON mentat.%I USING turbovec (v %s)',
        idx_name, v_table, ops
    );
    RETURN idx_name;
END;
$$;

COMMENT ON FUNCTION mentat.has_pg_turbovec() IS
    'True if the optional pg_turbovec vector-similarity extension is installed.';
COMMENT ON FUNCTION mentat.attach_turbovec_attribute(TEXT, INT) IS
    'Create a per-attribute turbovec.vector aux table (optional; needs pg_turbovec).';
COMMENT ON FUNCTION mentat.create_turbovec_index(TEXT, TEXT) IS
    'Build a pg_turbovec index on an attached turbovec aux table.';
