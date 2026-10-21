-- Per-schema layout for named stores (issue #337).
--
-- create_store(name) gives the store its own schema `mentat_<name>` holding
-- the same tables as the default store's `mentat` schema: the nine narrow
-- datom logs, the nine current-state projections, schema / idents /
-- partitions / transactions / fulltext, the partition sequences and the
-- datoms / current_datoms views. The transactor and the query engine take
-- the store's schema as a prefix, so a named store reads and writes only its
-- own tables.
--
-- There is one DDL source: the default store. Every table is copied with
-- CREATE TABLE ... (LIKE mentat.<t>), its PRIMARY KEY / UNIQUE constraints,
-- storage options and shipped indexes are copied from the catalog, and the
-- views are copied from pg_get_viewdef. A column or index added to the
-- default store by a release reaches stores created afterwards without
-- touching this file. (Stores created earlier need the release's upgrade
-- script to add it, as the default store does.)
--
-- Rows keep the store_id column and carry the store's id from mentat.stores;
-- the column default is set to it, so a raw INSERT lands in the right store.

-- Store tables, in creation order. Keep in sync with what the transactor and
-- the query engine read under a store's schema prefix.
CREATE OR REPLACE FUNCTION mentat._store_tables()
RETURNS TEXT[]
LANGUAGE sql IMMUTABLE
AS $$
    SELECT ARRAY[
        'schema', 'idents', 'partitions', 'transactions', 'fulltext',
        'datoms_ref_new', 'datoms_boolean_new', 'datoms_long_new',
        'datoms_double_new', 'datoms_instant_new', 'datoms_text_new',
        'datoms_keyword_new', 'datoms_uuid_new', 'datoms_bytes_new',
        'current_ref', 'current_boolean', 'current_long', 'current_double',
        'current_instant', 'current_text', 'current_keyword', 'current_uuid',
        'current_bytes'
    ]
$$;

-- Create (or complete) the layout of store `p_store_id` in schema `p_schema`.
-- Idempotent: an object that already exists is left alone, so the upgrade
-- migration can use it to complete a pre-1.18 store.
CREATE OR REPLACE FUNCTION mentat._create_store_layout(p_schema TEXT, p_store_id BIGINT)
RETURNS VOID
LANGUAGE plpgsql
-- pg_get_viewdef / pg_get_indexdef schema-qualify every name only when the
-- schema is not on the search path; pinning it makes the copies point at the
-- store's tables, never back at mentat's.
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    t TEXT;
    src OID;
    dst TEXT;
    r RECORD;
    def TEXT;
BEGIN
    IF p_schema = 'mentat' THEN
        RAISE EXCEPTION 'mentat._create_store_layout: the default store''s schema is created by CREATE EXTENSION';
    END IF;
    EXECUTE format('CREATE SCHEMA IF NOT EXISTS %I', p_schema);

    FOREACH t IN ARRAY mentat._store_tables() LOOP
        IF to_regclass(format('%I.%I', p_schema, t)) IS NOT NULL THEN
            CONTINUE;
        END IF;
        src := format('mentat.%I', t)::regclass;
        dst := format('%I.%I', p_schema, t);
        -- Columns, defaults, CHECK constraints, storage, statistics, comments.
        -- Indexes are copied below, minus the ones a user or the
        -- per-attribute helpers added to the default store.
        EXECUTE format('CREATE TABLE %s (LIKE mentat.%I INCLUDING ALL EXCLUDING INDEXES)', dst, t);

        SELECT reloptions INTO r FROM pg_class WHERE oid = src;
        IF r.reloptions IS NOT NULL THEN
            EXECUTE format('ALTER TABLE %s SET (%s)', dst, array_to_string(r.reloptions, ', '));
        END IF;

        FOR r IN
            SELECT conname, pg_get_constraintdef(oid) AS def
              FROM pg_constraint
             WHERE conrelid = src AND contype IN ('p', 'u', 'x')
             ORDER BY contype, conname
        LOOP
            EXECUTE format('ALTER TABLE %s ADD CONSTRAINT %I %s', dst, r.conname, r.def);
        END LOOP;

        -- The shipped EAVT / AEVT / VAET / AVET / TX / FTS indexes. Extend
        -- the pattern when a release ships an index named otherwise.
        FOR r IN
            SELECT i.indexrelid
              FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid
             WHERE i.indrelid = src
               AND NOT EXISTS (SELECT 1 FROM pg_constraint k WHERE k.conindid = i.indexrelid)
               AND (c.relname ~ '^idx_(datoms_[a-z]+_new|current_[a-z]+)_(eavt|aevt|vaet|avet|tx|fts|aev|vae)$'
                    OR c.relname = 'idx_fulltext_search')
        LOOP
            def := pg_get_indexdef(r.indexrelid);
            def := replace(def, format(' ON mentat.%s ', quote_ident(t)), format(' ON %s ', dst));
            EXECUTE def;
        END LOOP;

        IF EXISTS (SELECT 1 FROM pg_attribute WHERE attrelid = src AND attname = 'store_id' AND NOT attisdropped) THEN
            EXECUTE format('ALTER TABLE %s ALTER COLUMN store_id SET DEFAULT %s', dst, p_store_id);
        END IF;
    END LOOP;

    IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid = format('%I.fulltext', p_schema)::regclass
                                             AND tgname = 'fulltext_update') THEN
        EXECUTE format('CREATE TRIGGER fulltext_update BEFORE INSERT OR UPDATE ON %I.fulltext '
                       'FOR EACH ROW EXECUTE FUNCTION mentat.fulltext_update_trigger()', p_schema);
    END IF;

    -- Partition sequences: the default store's bands, starting at the floor.
    FOR r IN
        SELECT c.relname, s.*
          FROM pg_sequence s JOIN pg_class c ON c.oid = s.seqrelid
         WHERE c.relnamespace = 'mentat'::regnamespace
           AND c.relname IN ('partition_db_seq', 'partition_user_seq', 'partition_tx_seq')
    LOOP
        EXECUTE format('CREATE SEQUENCE IF NOT EXISTS %I.%I INCREMENT %s MINVALUE %s MAXVALUE %s START %s CACHE %s',
                       p_schema, r.relname, r.seqincrement, r.seqmin, r.seqmax, r.seqstart, r.seqcache);
    END LOOP;

    -- Views over this store's tables, filtered to its store_id.
    FOREACH t IN ARRAY ARRAY['datoms', 'current_datoms'] LOOP
        IF to_regclass(format('%I.%I', p_schema, t)) IS NULL THEN
            def := pg_get_viewdef(format('mentat.%I', t)::regclass);
            def := regexp_replace(def, '\mmentat\.', quote_ident(p_schema) || '.', 'g');
            def := regexp_replace(def, '(store_id = )0\M', '\1' || p_store_id, 'g');
            EXECUTE format('CREATE VIEW %I.%I AS %s', p_schema, t, def);
        END IF;
    END LOOP;

    -- Seed rows: the ones CREATE EXTENSION puts in the default store (the
    -- same set sql/27_dump_config.sql filters out of a dump).
    EXECUTE format('INSERT INTO %I.schema SELECT * FROM mentat.schema WHERE entid < 100 ON CONFLICT DO NOTHING', p_schema);
    EXECUTE format('INSERT INTO %I.idents SELECT * FROM mentat.idents WHERE entid < 100 ON CONFLICT DO NOTHING', p_schema);
    EXECUTE format($q$INSERT INTO %I.partitions SELECT * FROM mentat.partitions
                      WHERE name IN ('db.part/db', 'db.part/user', 'db.part/tx') ON CONFLICT DO NOTHING$q$, p_schema);
    EXECUTE format('UPDATE %I.partitions SET allow_excision = FALSE', p_schema);
    EXECUTE format('INSERT INTO %I.transactions SELECT * FROM mentat.transactions WHERE tx = 1000000 ON CONFLICT DO NOTHING', p_schema);
END;
$fn$;

-- Ownership and privileges for a store's schema, run after every object in
-- it exists. Objects belong to the schema's owner (the role that called
-- create_store; an upgrade runs as the bootstrap superuser, so it hands them
-- back). Every role that has a privilege on the default store's schema,
-- tables, views or sequences gets the same one on the store's, with the same
-- grant option: the installing role of a trusted install (sql/27_dump_config.sql)
-- and whoever it granted to. Nothing new goes to PUBLIC.
CREATE OR REPLACE FUNCTION mentat._store_grants(p_schema TEXT)
RETURNS VOID
LANGUAGE plpgsql
SET search_path = pg_catalog, pg_temp
AS $fn$
DECLARE
    owner_oid OID;
    r RECORD;
    a RECORD;
    kind TEXT;
BEGIN
    SELECT nspowner INTO owner_oid FROM pg_namespace WHERE nspname = p_schema;
    IF owner_oid IS NULL THEN
        RAISE EXCEPTION 'schema % does not exist', p_schema;
    END IF;

    FOR r IN
        SELECT c.oid, c.relname, c.relkind, c.relowner
          FROM pg_class c
         WHERE c.relnamespace = p_schema::regnamespace AND c.relkind IN ('r', 'v', 'm', 'S')
    LOOP
        kind := CASE r.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'v' THEN 'VIEW'
                               WHEN 'm' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END;
        IF r.relowner <> owner_oid THEN
            EXECUTE format('ALTER %s %I.%I OWNER TO %I', kind, p_schema, r.relname, owner_oid::regrole);
        END IF;
    END LOOP;
    FOR r IN
        SELECT p.oid FROM pg_proc p WHERE p.pronamespace = p_schema::regnamespace AND p.proowner <> owner_oid
    LOOP
        EXECUTE format('ALTER ROUTINE %s OWNER TO %I', r.oid::regprocedure, owner_oid::regrole);
    END LOOP;

    FOR a IN
        SELECT (aclexplode(nspacl)).* FROM pg_namespace WHERE nspname = 'mentat'
    LOOP
        CONTINUE WHEN a.grantee = owner_oid
                   OR a.grantee = (SELECT nspowner FROM pg_namespace WHERE nspname = 'mentat');
        EXECUTE format('GRANT %s ON SCHEMA %I TO %s%s', a.privilege_type, p_schema,
                       CASE a.grantee WHEN 0 THEN 'PUBLIC' ELSE quote_ident(a.grantee::regrole::text) END,
                       CASE WHEN a.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
    END LOOP;

    FOR r IN
        SELECT d.relname, d.relkind, s.relacl, s.relowner
          FROM pg_class d
          JOIN pg_class s ON s.relname = d.relname AND s.relnamespace = 'mentat'::regnamespace
         WHERE d.relnamespace = p_schema::regnamespace AND d.relkind IN ('r', 'v', 'S')
           AND s.relacl IS NOT NULL
    LOOP
        kind := CASE r.relkind WHEN 'S' THEN 'SEQUENCE' ELSE 'TABLE' END;
        FOR a IN SELECT * FROM aclexplode(r.relacl) LOOP
            CONTINUE WHEN a.grantee = owner_oid OR a.grantee = r.relowner;
            EXECUTE format('GRANT %s ON %s %I.%I TO %s%s', a.privilege_type, kind, p_schema, r.relname,
                           CASE a.grantee WHEN 0 THEN 'PUBLIC' ELSE quote_ident(a.grantee::regrole::text) END,
                           CASE WHEN a.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
        END LOOP;
    END LOOP;
END;
$fn$;
