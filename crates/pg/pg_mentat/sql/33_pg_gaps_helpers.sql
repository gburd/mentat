-- pg_mentat <-> pg_gaps integration helpers.
--
-- pg_gaps (https://codeberg.org/gregburd/pg_gaps, PostgreSQL license) brings
-- BayesStore-style probabilistic graphical models to ordinary tables: some
-- columns may be missing values, a first-order Bayesian network stored in the
-- `gaps` schema says how those values are distributed and correlated, and
-- gaps.marginals / gaps.impute return the distribution of each missing value,
-- or its most likely completion, conditioned on the data present.
--
-- It is a SOFT dependency (pg_gaps needs PostgreSQL 17+); nothing here forces
-- it to be installed. mentat.create_gaps_view works without it (on PostgreSQL
-- 15+), and the helpers that need it raise :db.error/missing-extension with
-- install instructions.
--
-- Mentat's data is entity-attribute-value: an entity whose cardinality-one
-- attribute has no current value is exactly a "missing value" to pg_gaps.
-- The bridge is a view with one row per entity and one column per attribute:
--
--   mentat.create_gaps_view('patients', ARRAY[':patient/smoker', ':patient/cancer'])
--     -> VIEW patients (e bigint, patient_smoker text, patient_cancer text)
--
-- NULL where the entity has no current value. Values are text in the
-- spelling pg_gaps domains use: a keyword as `:ns/name`, a ref as the
-- `:db/ident` of the entity it points to (an enum value such as
-- `:smoker/yes`) or else the entid, a boolean as `true` / `false`, a long or
-- a string as itself. The view reads the current-state projection
-- (mentat.current_<type>), so it is always current; it is security_invoker, so
-- the caller's privileges and row-level security (multi-tenant stores) apply.
--
--   SELECT gaps.create_model('cancer');
--   SELECT mentat.gaps_declare('cancer', 'patients', ':patient/smoker');
--   SELECT mentat.gaps_declare('cancer', 'patients', ':patient/cancer');
--   SELECT gaps.add_factor('cancer', 'f_smoker', 'patient_smoker');
--   SELECT gaps.add_factor('cancer', 'f_cancer', 'patient_cancer', ARRAY['patient_smoker']);
--   SELECT * FROM gaps.learn_cpts('cancer');
--
-- and then, from Datalog, the Datalog where-fns bind the posterior per entity:
--
--   [:find ?name ?v ?p
--    :where [(gaps-marginals "cancer" :patient/cancer) [[?e ?v ?p]]]
--           [?e :patient/name ?name]]
--
-- (gaps-impute "model" :attr) binds the most likely completion instead. See
-- docs/src/pg_gaps.md, and pg_gaps' own documentation for what may be combined.
--
-- Each function pins search_path to pg_catalog, pg_temp and names mentat's and
-- pg_gaps' objects with their schemas; this file runs nothing at script time.

-- mentat.has_pg_gaps(): true if the pg_gaps extension is installed here.
CREATE OR REPLACE FUNCTION mentat.has_pg_gaps()
RETURNS boolean
LANGUAGE sql STABLE
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_extension WHERE extname = 'pg_gaps');
$$;

-- mentat.gaps_column(attr_ident text): the view column name for an attribute
-- ident, `:patient/smoker` -> `patient_smoker`. ASCII letters are lowered;
-- every other character that is not an ASCII letter, digit or underscore
-- becomes an underscore. The gaps-marginals and gaps-impute where-fns find
-- the column by its comment instead, so they need no copy of this rule.
CREATE OR REPLACE FUNCTION mentat.gaps_column(attr_ident TEXT)
RETURNS TEXT
LANGUAGE sql IMMUTABLE STRICT
SET search_path = pg_catalog, pg_temp
AS $$
    SELECT regexp_replace(
        translate(ltrim(attr_ident, ':'), 'ABCDEFGHIJKLMNOPQRSTUVWXYZ',
                  'abcdefghijklmnopqrstuvwxyz'),
        '[^a-z0-9_]', '_', 'g');
$$;

-- mentat.create_gaps_view(view_name text, attrs text[],
--                         store text DEFAULT 'default', entities text DEFAULT NULL)
--
-- Create (or replace) a view with one row per entity: column `e` (the entid)
-- and one text column per attribute in `attrs`, named by mentat.gaps_column,
-- NULL where the entity has no current value. The entities are those with a
-- current value for `entities`, an attribute ident naming the population (for
-- example `:patient/id`), so an entity missing every one of `attrs` still
-- appears; with `entities` NULL, those with a current value for any of
-- `attrs`.
--
-- Each attribute must be cardinality one (one value per entity) and of a
-- discrete type: ref, keyword, boolean, long or string. The view is
-- security_invoker (PostgreSQL 15+) and each column carries its ident as a
-- COMMENT, which mentat.gaps_declare and the gaps-marginals / gaps-impute
-- where-fns read back. `store` names a mentat store. Returns the
-- view's name, schema-qualified. pg_gaps need not be installed.
--
-- An unqualified view_name goes where an unqualified CREATE VIEW by the
-- caller would: the first schema of the caller's search_path (this function
-- pins its own path for every other name it uses).
CREATE OR REPLACE FUNCTION mentat.create_gaps_view(
    view_name TEXT,
    attrs     TEXT[],
    store     TEXT DEFAULT 'default',
    entities  TEXT DEFAULT NULL
)
RETURNS TEXT
LANGUAGE plpgsql VOLATILE
AS $$
DECLARE
    -- Where an unqualified view_name goes: the caller's first schema, read
    -- before this function pins search_path below.
    v_target  TEXT := current_schema();
    v_path    TEXT := current_setting('search_path');
    v_view    TEXT;
    v_schema  TEXT;
    v_store   BIGINT;
    v_attr    TEXT;
    v_entid   BIGINT;
    v_type    TEXT;
    v_card    TEXT;
    v_col     TEXT;
    v_cols    TEXT[] := '{}';
    v_select  TEXT := '';
    v_pop     TEXT := '';
    v_expr    TEXT;
    v_sql     TEXT;
    v_i       INT;
BEGIN
    -- From here on names resolve in pg_catalog only, as in the other helpers;
    -- the caller's path is restored before returning, and on error the
    -- transaction (or subtransaction) that set it is rolled back. (A
    -- function-level SET would hide the caller's path, which v_target needs.)
    PERFORM set_config('search_path', 'pg_catalog, pg_temp', true);
    -- security_invoker views are PostgreSQL 15+. Without it the view would
    -- read the store's tables as its owner, bypassing the caller's privileges
    -- and row-level security, so refuse rather than degrade.
    IF current_setting('server_version_num')::int < 150000 THEN
        RAISE EXCEPTION USING ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/unsupported mentat.create_gaps_view needs PostgreSQL 15 or ' ||
                      'later (security_invoker views); this server is ' ||
                      current_setting('server_version') || '. pg_gaps itself needs 17+.';
    END IF;
    IF view_name IS NULL
       OR view_name !~ '^[a-zA-Z_][a-zA-Z0-9_]*(\.[a-zA-Z_][a-zA-Z0-9_]*)?$' THEN
        RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/fn-arg Invalid view name: ' || coalesce(view_name, 'NULL') ||
                      '. Use letters, digits and underscores, optionally schema-qualified.';
    END IF;
    IF view_name LIKE '%.%' THEN
        v_view := format('%I.%I', split_part(view_name, '.', 1), split_part(view_name, '.', 2));
    ELSIF v_target IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'invalid_schema_name',
            MESSAGE = ':db.error/fn-arg No schema on the search_path to create view ' ||
                      view_name || ' in. Schema-qualify the name.';
    ELSE
        v_view := format('%I.%I', v_target, view_name);
    END IF;
    IF coalesce(array_length(attrs, 1), 0) = 0 THEN
        RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
            MESSAGE = ':db.error/fn-arg mentat.create_gaps_view needs at least one attribute.';
    END IF;

    SELECT s.schema_name, s.store_id INTO v_schema, v_store
      FROM mentat.stores s WHERE s.store_name = store;
    IF v_schema IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'undefined_object',
            MESSAGE = ':db.error/store-not-found Store ' || quote_literal(store) ||
                      ' does not exist. Create it with mentat.create_store first.';
    END IF;

    FOR v_i IN 1 .. array_length(attrs, 1) LOOP
        v_attr := attrs[v_i];
        EXECUTE format(
            'SELECT s.entid, s.value_type::text, s.cardinality::text FROM %I.schema s WHERE s.ident = $1',
            v_schema)
          INTO v_entid, v_type, v_card USING v_attr;
        IF v_entid IS NULL THEN
            RAISE EXCEPTION USING ERRCODE = 'undefined_object',
                MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(v_attr) ||
                          ' is not defined in store ' || quote_literal(store) || '.';
        END IF;
        IF v_card <> 'one' THEN
            RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
                MESSAGE = ':db.error/fn-arg ' || v_attr || ' is cardinality many. A pg_gaps ' ||
                          'attribute has one value per entity, so it must be cardinality one.';
        END IF;
        IF v_type NOT IN ('ref', 'keyword', 'boolean', 'long', 'string') THEN
            RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
                MESSAGE = ':db.error/fn-arg ' || v_attr || ' is :db.type/' || v_type ||
                          '. pg_gaps models discrete values: use a ref (enum), keyword, ' ||
                          'boolean, long or string attribute.';
        END IF;
        v_col := mentat.gaps_column(v_attr);
        IF octet_length(v_col) > 63 THEN
            RAISE EXCEPTION USING ERRCODE = 'name_too_long',
                MESSAGE = ':db.error/fn-arg ' || v_attr || ' gives column name ' || v_col ||
                          ', longer than PostgreSQL''s 63-byte limit. Use a shorter ident.';
        END IF;
        IF v_col = 'e' OR v_col = ANY (v_cols) THEN
            RAISE EXCEPTION USING ERRCODE = 'duplicate_column',
                MESSAGE = ':db.error/fn-arg ' || v_attr || ' maps to column ' || v_col ||
                          ', which another attribute of this view (or the entity column e) ' ||
                          'already uses.';
        END IF;
        v_cols := v_cols || v_col;

        -- The current value of this attribute for entity p.e, as text in the
        -- spelling pg_gaps domains use.
        v_expr := CASE v_type
            WHEN 'ref' THEN format(
                '(SELECT coalesce((SELECT i.ident FROM %1$I.idents i WHERE i.entid = c.v '
                'ORDER BY i.ident LIMIT 1), c.v::text) FROM %1$I.current_ref c '
                'WHERE c.store_id = %2$s AND c.e = p.e AND c.a = %3$s)', v_schema, v_store, v_entid)
            WHEN 'keyword' THEN format(
                '(SELECT '':'' || c.v FROM %1$I.current_keyword c '
                'WHERE c.store_id = %2$s AND c.e = p.e AND c.a = %3$s)', v_schema, v_store, v_entid)
            WHEN 'boolean' THEN format(
                '(SELECT c.v::text FROM %1$I.current_boolean c '
                'WHERE c.store_id = %2$s AND c.e = p.e AND c.a = %3$s)', v_schema, v_store, v_entid)
            WHEN 'long' THEN format(
                '(SELECT c.v::text FROM %1$I.current_long c '
                'WHERE c.store_id = %2$s AND c.e = p.e AND c.a = %3$s)', v_schema, v_store, v_entid)
            ELSE format(
                '(SELECT c.v FROM %1$I.current_text c '
                'WHERE c.store_id = %2$s AND c.e = p.e AND c.a = %3$s)', v_schema, v_store, v_entid)
        END;
        v_select := v_select || format(', %s AS %I', v_expr, v_col);

        IF entities IS NULL THEN
            IF v_pop <> '' THEN
                v_pop := v_pop || ' UNION ';
            END IF;
            v_pop := v_pop || format(
                'SELECT c.e FROM %I.%I c WHERE c.store_id = %s AND c.a = %s',
                v_schema,
                CASE v_type WHEN 'string' THEN 'current_text' ELSE 'current_' || v_type END,
                v_store, v_entid);
        END IF;
    END LOOP;

    IF entities IS NOT NULL THEN
        EXECUTE format(
            'SELECT s.entid, s.value_type::text FROM %I.schema s WHERE s.ident = $1', v_schema)
          INTO v_entid, v_type USING entities;
        IF v_entid IS NULL THEN
            RAISE EXCEPTION USING ERRCODE = 'undefined_object',
                MESSAGE = ':db.error/attribute-not-found Attribute ' || quote_literal(entities) ||
                          ' (the entities argument) is not defined in store ' ||
                          quote_literal(store) || '.';
        END IF;
        v_pop := format(
            'SELECT DISTINCT c.e FROM %I.%I c WHERE c.store_id = %s AND c.a = %s',
            v_schema,
            CASE v_type WHEN 'string' THEN 'current_text' ELSE 'current_' || v_type END,
            v_store, v_entid);
    END IF;

    v_sql := format(
        'CREATE OR REPLACE VIEW %s WITH (security_invoker = true) AS SELECT p.e%s FROM (%s) p',
        v_view, v_select, v_pop);
    EXECUTE v_sql;

    EXECUTE format('COMMENT ON VIEW %s IS %L', v_view,
        'pg_mentat gaps view of store ' || store || ': one row per entity (column e), ' ||
        'one column per attribute; NULL is a missing value. See mentat.create_gaps_view.');
    FOR v_i IN 1 .. array_length(attrs, 1) LOOP
        EXECUTE format('COMMENT ON COLUMN %s.%I IS %L', v_view, v_cols[v_i], attrs[v_i]);
    END LOOP;
    PERFORM set_config('search_path', v_path, true);
    RETURN v_view;
END;
$$;

-- mentat.gaps_declare(model text, view regclass, attr_ident text,
--                     domain text[] DEFAULT NULL, stripe text DEFAULT NULL)
--
-- Declare the column of `view` (made by mentat.create_gaps_view) that holds
-- `attr_ident` as a probabilistic attribute of pg_gaps model `model`, keyed by
-- the entity column e, and add a stripe over all its entities, named `stripe`
-- (by default the column name), ready for gaps.add_factor. `domain` defaults to
-- the column's distinct current values, sorted; pass it when some value of
-- the domain may not occur yet (pg_gaps refuses a value outside the domain).
-- Returns the domain declared. Requires pg_gaps; runs as the caller.
CREATE OR REPLACE FUNCTION mentat.gaps_declare(
    model      TEXT,
    view       REGCLASS,
    attr_ident TEXT,
    domain     TEXT[] DEFAULT NULL,
    stripe     TEXT DEFAULT NULL
)
RETURNS TEXT[]
LANGUAGE plpgsql VOLATILE
SET search_path = pg_catalog, pg_temp
AS $$
DECLARE
    v_col    NAME;
    v_domain TEXT[] := domain;
BEGIN
    IF NOT mentat.has_pg_gaps() THEN
        RAISE EXCEPTION USING ERRCODE = 'feature_not_supported',
            MESSAGE = ':db.error/missing-extension pg_gaps is not installed in this database. ' ||
                      'pg_gaps is an OPTIONAL probabilistic-inference extension for pg_mentat ' ||
                      '(PostgreSQL 17+). To enable: install it per ' ||
                      'https://codeberg.org/gregburd/pg_gaps, then run CREATE EXTENSION pg_gaps; ' ||
                      'before retrying.';
    END IF;

    -- The column whose comment is the ident (create_gaps_view writes it).
    SELECT a.attname INTO v_col
      FROM pg_catalog.pg_attribute a
     WHERE a.attrelid = view AND a.attnum > 0 AND NOT a.attisdropped
       AND col_description(a.attrelid, a.attnum) = attr_ident;
    IF v_col IS NULL THEN
        RAISE EXCEPTION USING ERRCODE = 'undefined_column',
            MESSAGE = ':db.error/attribute-not-found ' || view::text || ' has no column for ' ||
                      attr_ident || '. Create it with mentat.create_gaps_view(' ||
                      quote_literal(view::text) || ', ARRAY[..., ' ||
                      quote_literal(attr_ident) || ']).';
    END IF;

    IF v_domain IS NULL THEN
        EXECUTE format(
            'SELECT array_agg(DISTINCT x.%1$I ORDER BY x.%1$I) FROM %2$s x '
            'WHERE x.%1$I IS NOT NULL', v_col, view)
          INTO v_domain;
        IF v_domain IS NULL THEN
            RAISE EXCEPTION USING ERRCODE = 'invalid_parameter_value',
                MESSAGE = ':db.error/fn-arg ' || attr_ident || ' has no current values in ' ||
                          view::text || ', so its domain cannot be read from the data. ' ||
                          'Pass domain => ARRAY[...].';
        END IF;
    END IF;

    PERFORM gaps.declare_attribute(model, view, v_col, v_domain, ARRAY['e']::name[]);
    PERFORM gaps.add_stripe(model, coalesce(stripe, v_col::text), view, v_col);
    RETURN v_domain;
END;
$$;
