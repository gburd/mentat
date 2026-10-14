-- pg_mentat 1.12.0 -> 1.13.0 upgrade.
--
-- 1.13.0 adds optional pg_fts (BM25) and pg_turbovec (vector) integration
-- helpers, makes the three deprecated SQL wrappers raise a one-time-per-session
-- WARNING, and ships a TRUSTED packaged control file. The engine, storage, and
-- query semantics are unchanged.

-- (1) Deprecation WARNING on the pre-1.9.0 wrappers (issue #329). The old
--     bodies were plain LANGUAGE SQL; replace with plpgsql that warns once per
--     session, then delegates to the edn_* function. Only these three are
--     deprecated; the mentat_eval wrapper (script builds) is replaced by the
--     extension's own CREATE OR REPLACE in that build.
CREATE OR REPLACE FUNCTION public.mentat_transact(edn_tx TEXT)
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

CREATE OR REPLACE FUNCTION public.mentat_query(query TEXT, inputs JSONB DEFAULT '{}'::JSONB)
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

CREATE OR REPLACE FUNCTION public.mentat_pull(pattern TEXT, entity_id BIGINT)
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

-- (2) Optional pg_fts (BM25) integration helpers (issue #332).
CREATE OR REPLACE FUNCTION mentat.has_pg_fts()
RETURNS boolean
LANGUAGE sql STABLE
AS $$
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_fts');
$$;

CREATE OR REPLACE FUNCTION mentat.create_fts_index(
    attr_ident TEXT,
    regconfig  TEXT DEFAULT 'english'
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE
    attr_entid BIGINT;
    attr_type  TEXT;
    idx_name   TEXT;
BEGIN
    IF NOT mentat.has_pg_fts() THEN
        RAISE EXCEPTION USING
            ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_fts is not installed in this database. ' ||
                      'Install it per https://codeberg.org/gregburd/pg_fts (PostgreSQL 17+), then ' ||
                      'run CREATE EXTENSION pg_fts; (:db/fulltext still works via tsvector/GIN).';
    END IF;
    SELECT s.entid, s.value_type::text INTO attr_entid, attr_type
      FROM mentat.schema s WHERE s.ident = attr_ident;
    IF attr_entid IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) || ' is not defined.';
    END IF;
    IF attr_type <> 'string' THEN
        RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/wrong-type-for-fts-index ' || quote_literal(attr_ident) || ' is not :db.type/string.';
    END IF;
    idx_name := 'mentat_fts_' || attr_entid;
    EXECUTE format(
        'CREATE INDEX IF NOT EXISTS %I ON mentat.datoms_text_new ' ||
        'USING fts (to_ftsdoc(%L, v)) WHERE a = %s AND store_id = 0 AND added',
        idx_name, regconfig, attr_entid);
    RETURN idx_name;
END;
$$;

-- (3) Optional pg_turbovec (vector) integration helpers (issue #332).
CREATE OR REPLACE FUNCTION mentat.has_pg_turbovec()
RETURNS boolean
LANGUAGE sql STABLE
AS $$
    SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = 'pg_turbovec');
$$;

CREATE OR REPLACE FUNCTION mentat.attach_turbovec_attribute(attr_ident TEXT, dim INT)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE v_entid BIGINT; v_table TEXT;
BEGIN
    IF NOT mentat.has_pg_turbovec() THEN
        RAISE EXCEPTION USING ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_turbovec is not installed. See https://codeberg.org/gregburd/pg_turbovec.';
    END IF;
    SELECT entid INTO v_entid FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) || ' is not registered.';
    END IF;
    IF dim <= 0 OR dim > 16000 THEN
        RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/fn-arg dim must be in (0, 16000], got ' || dim || '.';
    END IF;
    v_table := 'attr_' || v_entid || '_tvec';
    EXECUTE format('CREATE TABLE IF NOT EXISTS mentat.%I (e BIGINT PRIMARY KEY, v turbovec.vector(%s))', v_table, dim);
    RETURN 'mentat.' || v_table;
END;
$$;

CREATE OR REPLACE FUNCTION mentat.create_turbovec_index(attr_ident TEXT, ops TEXT DEFAULT 'turbovec.vec_cosine_ops')
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE v_entid BIGINT; v_table TEXT; idx_name TEXT;
BEGIN
    IF NOT mentat.has_pg_turbovec() THEN
        RAISE EXCEPTION USING ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_turbovec is not installed.';
    END IF;
    SELECT entid INTO v_entid FROM mentat.schema WHERE ident = attr_ident;
    IF v_entid IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(attr_ident) || ' is not registered.';
    END IF;
    v_table := 'attr_' || v_entid || '_tvec';
    idx_name := 'mentat_tvec_' || v_entid;
    EXECUTE format('CREATE INDEX IF NOT EXISTS %I ON mentat.%I USING turbovec (v %s)', idx_name, v_table, ops);
    RETURN idx_name;
END;
$$;
