-- pg_mentat 1.17.2 -> 1.18.0 upgrade (1.17.1 reaches it through 1.17.1 -> 1.17.2).
--
-- Each section below belongs to one change.

-- ===========================================================================
-- Named stores (#337).
--
-- create_store(name) used to build the pre-1.5 wide layout in mentat_<name>
-- (a wide `datoms` table plus schema / idents / partitions / transactions /
-- fulltext), which the transactor had not written to since 1.5.0: the first
-- t() into such a store failed with `relation "mentat_<name>.current_instant"
-- does not exist`. Before 1.17.0 create_store() itself failed on every call.
-- So an old named store can hold only rows inserted by hand.
--
-- 1.18.0 gives a named store the default store's full layout in its schema
-- (sql/30_store_layout.sql, the same functions a fresh install has), and
-- create_store() / t() / q() / pull() / entity() / log() all use it. This
-- section installs those functions and rebuilds every old named store:
--
--   * the old schema is renamed to mentat_<name>_pre118 (cut to 63
--     characters) and kept, never dropped and never read: rows inserted
--     into it by hand stay there for the owner to copy
--     (docs/src/multi-tenancy.md has the queries);
--   * a fresh, empty mentat_<name> gets the new layout, owned by the old
--     schema's owner, with the default store's grants;
--   * the store's views and helper functions are recreated and its objects
--     are taken out of the extension (an upgrade script's objects would
--     otherwise become extension members, which drop_store could not drop);
--   * a WARNING names each store rebuilt, and each store skipped and why.
--
-- A store already on the new layout (it has current_instant) is left alone.
-- ===========================================================================

-- ---------------------------------------------------------------------------
-- The layout functions (identical to sql/30_store_layout.sql).
-- ---------------------------------------------------------------------------
-- Per-schema layout for named stores (issue #337).
--
-- create_store(name) gives the store its own schema `mentat_<name>` holding
-- the same tables as the default store's `mentat` schema: the nine narrow
-- datom logs, the nine current-state projections, schema / idents /
-- partitions / transactions / fulltext, the partition sequences and the
-- datoms / current_datoms views (datoms with INSTEAD OF INSERT / DELETE
-- triggers, like the default store's). The transactor and the query engine take
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

-- INSTEAD OF triggers for a named store's `datoms` view, as
-- mentat.datoms_view_insert / _delete are for the default store's: route the
-- row to the narrow table in the view's own schema. store_id takes the
-- column default, which _create_store_layout sets to the store's id. Value
-- type tags as in sql/10_narrow_storage.sql.
CREATE OR REPLACE FUNCTION mentat._store_datoms_view_insert()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $fn$
DECLARE
    ty TEXT;
    col TEXT;
BEGIN
    IF NEW.tx IS NULL OR NEW.added IS NULL THEN
        RAISE EXCEPTION '%.datoms INSERT requires tx and added to be non-NULL', TG_TABLE_SCHEMA;
    END IF;
    SELECT m.ty, m.col INTO ty, col
      FROM (VALUES (0, 'ref', 'v_ref'), (1, 'boolean', 'v_bool'), (2, 'long', 'v_long'),
                   (3, 'double', 'v_double'), (4, 'instant', 'v_instant'), (7, 'text', 'v_text'),
                   (8, 'keyword', 'v_keyword'), (10, 'uuid', 'v_uuid'), (11, 'bytes', 'v_bytes'))
           AS m(tag, ty, col)
     WHERE m.tag = NEW.value_type_tag;
    IF ty IS NULL THEN
        RAISE EXCEPTION '%.datoms INSERT: unknown value_type_tag %', TG_TABLE_SCHEMA, NEW.value_type_tag;
    END IF;
    EXECUTE format('INSERT INTO %I.datoms_%s_new (e, a, v, tx, added) VALUES ($1, $2, ($3).%I, $4, $5) '
                   'ON CONFLICT DO NOTHING', TG_TABLE_SCHEMA, ty, col)
        USING NEW.e, NEW.a, NEW, NEW.tx, NEW.added;
    RETURN NEW;
END;
$fn$;

CREATE OR REPLACE FUNCTION mentat._store_datoms_view_delete()
RETURNS TRIGGER
LANGUAGE plpgsql
AS $fn$
DECLARE
    ty TEXT;
BEGIN
    SELECT m.ty INTO ty
      FROM (VALUES (0, 'ref'), (1, 'boolean'), (2, 'long'), (3, 'double'), (4, 'instant'),
                   (7, 'text'), (8, 'keyword'), (10, 'uuid'), (11, 'bytes')) AS m(tag, ty)
     WHERE m.tag = OLD.value_type_tag;
    IF ty IS NOT NULL THEN
        EXECUTE format('DELETE FROM %I.datoms_%s_new WHERE e = $1 AND a = $2 AND tx = $3',
                       TG_TABLE_SCHEMA, ty)
            USING OLD.e, OLD.a, OLD.tx;
    END IF;
    RETURN OLD;
END;
$fn$;

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
    -- By name, never a text -> regnamespace cast: that folds an unquoted
    -- name to lower case and would find mentat_mystore for mentat_MyStore.
    IF NOT EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = p_schema) THEN
        EXECUTE format('CREATE SCHEMA %I', p_schema);
    END IF;

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
    IF NOT EXISTS (SELECT 1 FROM pg_trigger WHERE tgrelid = format('%I.datoms', p_schema)::regclass
                                             AND tgname = 'datoms_view_insert') THEN
        EXECUTE format('CREATE TRIGGER datoms_view_insert INSTEAD OF INSERT ON %I.datoms '
                       'FOR EACH ROW EXECUTE FUNCTION mentat._store_datoms_view_insert()', p_schema);
        EXECUTE format('CREATE TRIGGER datoms_view_delete INSTEAD OF DELETE ON %I.datoms '
                       'FOR EACH ROW EXECUTE FUNCTION mentat._store_datoms_view_delete()', p_schema);
    END IF;

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
    ns OID;
    owner_oid OID;
    r RECORD;
    a RECORD;
    kind TEXT;
BEGIN
    -- The schema's oid by exact name (see _create_store_layout). Role names
    -- come from regrole output, which is already quoted: %s, never %I.
    SELECT oid, nspowner INTO ns, owner_oid FROM pg_namespace WHERE nspname = p_schema;
    IF owner_oid IS NULL THEN
        RAISE EXCEPTION 'schema % does not exist', p_schema;
    END IF;

    FOR r IN
        SELECT c.oid, c.relname, c.relkind, c.relowner
          FROM pg_class c
         WHERE c.relnamespace = ns AND c.relkind IN ('r', 'v', 'm', 'S')
    LOOP
        kind := CASE r.relkind WHEN 'S' THEN 'SEQUENCE' WHEN 'v' THEN 'VIEW'
                               WHEN 'm' THEN 'MATERIALIZED VIEW' ELSE 'TABLE' END;
        IF r.relowner <> owner_oid THEN
            EXECUTE format('ALTER %s %I.%I OWNER TO %s', kind, p_schema, r.relname, owner_oid::regrole);
        END IF;
    END LOOP;
    FOR r IN
        SELECT p.oid FROM pg_proc p WHERE p.pronamespace = ns AND p.proowner <> owner_oid
    LOOP
        EXECUTE format('ALTER ROUTINE %s OWNER TO %s', r.oid::regprocedure, owner_oid::regrole);
    END LOOP;
    -- Extended statistics the tables' LIKE ... INCLUDING ALL copied.
    FOR r IN
        SELECT s.stxname FROM pg_statistic_ext s WHERE s.stxnamespace = ns AND s.stxowner <> owner_oid
    LOOP
        EXECUTE format('ALTER STATISTICS %I.%I OWNER TO %s', p_schema, r.stxname, owner_oid::regrole);
    END LOOP;

    FOR a IN
        SELECT (aclexplode(nspacl)).* FROM pg_namespace WHERE nspname = 'mentat'
    LOOP
        CONTINUE WHEN a.grantee = owner_oid
                   OR a.grantee = (SELECT nspowner FROM pg_namespace WHERE nspname = 'mentat');
        EXECUTE format('GRANT %s ON SCHEMA %I TO %s%s', a.privilege_type, p_schema,
                       CASE a.grantee WHEN 0 THEN 'PUBLIC' ELSE a.grantee::regrole::text END,
                       CASE WHEN a.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
    END LOOP;

    FOR r IN
        SELECT d.relname, d.relkind, s.relacl, s.relowner
          FROM pg_class d
          JOIN pg_class s ON s.relname = d.relname AND s.relnamespace = 'mentat'::regnamespace
         WHERE d.relnamespace = ns AND d.relkind IN ('r', 'v', 'S')
           AND s.relacl IS NOT NULL
    LOOP
        kind := CASE r.relkind WHEN 'S' THEN 'SEQUENCE' ELSE 'TABLE' END;
        FOR a IN SELECT * FROM aclexplode(r.relacl) LOOP
            CONTINUE WHEN a.grantee = owner_oid OR a.grantee = r.relowner;
            EXECUTE format('GRANT %s ON %s %I.%I TO %s%s', a.privilege_type, kind, p_schema, r.relname,
                           CASE a.grantee WHEN 0 THEN 'PUBLIC' ELSE a.grantee::regrole::text END,
                           CASE WHEN a.is_grantable THEN ' WITH GRANT OPTION' ELSE '' END);
        END LOOP;
    END LOOP;
END;
$fn$;

-- ---------------------------------------------------------------------------
-- Rebuild pre-1.18 named stores.
--
-- This runs as the bootstrap superuser, so it trusts nothing that the
-- installing role or a store's owner controls:
--   * mentat.stores is writable by the installing role (27_dump_config.sql).
--     Only a row shaped like one create_store() made is rebuilt: schema
--     mentat_<name>, a valid store name, an ordinary `datoms` table with a
--     value_type_tag column (the old wide layout).
--   * The old schema belongs to the store's owner, who can replace any table
--     in it with a view or a function. So this block never reads a relation
--     in it: it renames the schema and copies nothing. Rows inserted there by
--     hand stay for the owner to copy (docs/src/multi-tenancy.md).
-- A store it cannot rebuild is skipped with a WARNING; the upgrade goes on.
-- ---------------------------------------------------------------------------
DO $migrate$
DECLARE
    st RECORD;
    rec RECORD;
    old_schema TEXT;
    new_ns OID;
    srr TEXT;
    ext OID;
    old_path TEXT := pg_catalog.current_setting('search_path');
BEGIN
    -- Names resolve in pg_catalog only while this block runs; the caller's
    -- path is restored at the end (not SET LOCAL: that lasts to the end of
    -- the script's transaction).
    PERFORM pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true);
    ext := (SELECT oid FROM pg_extension WHERE extname = 'pg_mentat');
    FOR st IN
        SELECT s.store_id, s.store_name, s.schema_name, n.oid AS ns, n.nspowner
          FROM mentat.stores s
          LEFT JOIN pg_namespace n ON n.nspname = s.schema_name
         WHERE s.schema_name IS DISTINCT FROM 'mentat'
         ORDER BY s.store_id
    LOOP
        IF st.schema_name IS DISTINCT FROM 'mentat_' || st.store_name
           OR st.store_name !~ '^[A-Za-z_][A-Za-z0-9_]*$' THEN
            -- Not a create_store() row: a store_id row for Row-Level Security,
            -- or edited by hand. Say so only if it names an existing schema.
            IF st.ns IS NOT NULL THEN
                RAISE WARNING 'pg_mentat upgrade: mentat.stores row "%" names schema %, which create_store() '
                              'would not have made; left alone.', st.store_name, st.schema_name;
            END IF;
            CONTINUE;
        END IF;
        -- validate_store_name's reserved names: create_store() never made
        -- these, and create_virtual_tables (below) would refuse them.
        IF pg_catalog.lower(st.store_name) IN ('default', 'information_schema')
           OR pg_catalog.lower(st.store_name) LIKE 'pg\_%' THEN
            RAISE WARNING 'pg_mentat upgrade: named store "%" not rebuilt: "%" is a reserved store name. '
                          'See "Upgrading from 1.17" in docs/src/multi-tenancy.md.', st.store_name, st.store_name;
            CONTINUE;
        END IF;
        -- 'mentat_' + 56 characters is the 63-character identifier limit.
        -- 1.17 took names up to 63 and PostgreSQL cut the schema name short,
        -- so the row no longer names its schema.
        IF pg_catalog.length(st.store_name) > 56 THEN
            RAISE WARNING 'pg_mentat upgrade: named store "%" not rebuilt: store names are limited to 56 '
                          'characters. See "Upgrading from 1.17" in docs/src/multi-tenancy.md.', st.store_name;
            CONTINUE;
        END IF;
        IF st.ns IS NULL THEN
            RAISE WARNING 'pg_mentat upgrade: named store "%" not rebuilt: its schema % does not exist '
                          '(dropped by hand?). drop_store(''%'') removes the store.',
                          st.store_name, st.schema_name, st.store_name;
            CONTINUE;
        END IF;
        IF EXISTS (SELECT 1 FROM pg_class
                    WHERE relnamespace = st.ns AND relname = 'current_instant' AND relkind = 'r') THEN
            RAISE WARNING 'pg_mentat upgrade: named store "%" not rebuilt: schema % already has the 1.18 layout.',
                          st.store_name, st.schema_name;
            CONTINUE;
        END IF;
        IF NOT EXISTS (SELECT 1 FROM pg_class c JOIN pg_attribute a ON a.attrelid = c.oid
                        WHERE c.relnamespace = st.ns AND c.relname = 'datoms' AND c.relkind = 'r'
                          AND a.attname = 'value_type_tag' AND NOT a.attisdropped) THEN
            RAISE WARNING 'pg_mentat upgrade: named store "%" not rebuilt: schema % does not hold the '
                          'pre-1.18 layout. See "Upgrading from 1.17" in docs/src/multi-tenancy.md.',
                          st.store_name, st.schema_name;
            CONTINUE;
        END IF;
        -- Cut to 63 characters, so it can never truncate to the store's own name.
        old_schema := pg_catalog.left(st.schema_name, 63 - pg_catalog.length('_pre118')) || '_pre118';
        IF EXISTS (SELECT 1 FROM pg_namespace WHERE nspname = old_schema) THEN
            RAISE WARNING 'pg_mentat upgrade: named store "%" not rebuilt: schema % already exists. '
                          'See "Upgrading from 1.17" in docs/src/multi-tenancy.md.', st.store_name, old_schema;
            CONTINUE;
        END IF;

        EXECUTE pg_catalog.format('ALTER SCHEMA %I RENAME TO %I', st.schema_name, old_schema);
        EXECUTE pg_catalog.format('CREATE SCHEMA %I AUTHORIZATION %s', st.schema_name, st.nspowner::regrole);  -- regrole: already quoted
        new_ns := (SELECT oid FROM pg_namespace WHERE nspname = st.schema_name);
        PERFORM mentat._create_store_layout(st.schema_name, st.store_id);

        -- 1.17 stores have no cache_generation row, so schema changes would
        -- not reach other backends. The installing role holds TRIGGER on this
        -- table: in replica mode a trigger it made cannot fire as superuser
        -- (only the table's owner can mark a trigger ALWAYS).
        srr := pg_catalog.current_setting('session_replication_role');
        PERFORM pg_catalog.set_config('session_replication_role', 'replica', true);
        INSERT INTO mentat.cache_generation (store_name, gen) VALUES (st.store_name, 1)
        ON CONFLICT (store_name) DO UPDATE SET gen = mentat.cache_generation.gen + 1;
        PERFORM pg_catalog.set_config('session_replication_role', srr, true);

        -- The store's views and helper functions, as create_store makes them.
        -- In the extension's own schema, wherever it was installed.
        PERFORM @extschema@.create_virtual_tables(st.store_name);

        -- Objects an extension script creates become members of the
        -- extension. A store's schema is user data, as it is when
        -- create_store() makes it: drop_store must be able to drop it, and
        -- pg_dump must dump it.
        FOR rec IN
            SELECT o.type, o.identity
              FROM pg_depend d, LATERAL pg_catalog.pg_identify_object(d.classid, d.objid, 0) o
             WHERE d.refclassid = 'pg_extension'::regclass AND d.refobjid = ext AND d.deptype = 'e'
               AND ((d.classid = 'pg_class'::regclass
                     AND (SELECT relnamespace FROM pg_class WHERE oid = d.objid) = new_ns)
                 OR (d.classid = 'pg_proc'::regclass
                     AND (SELECT pronamespace FROM pg_proc WHERE oid = d.objid) = new_ns)
                 OR (d.classid = 'pg_namespace'::regclass AND d.objid = new_ns))
             ORDER BY d.classid = 'pg_namespace'::regclass
        LOOP
            EXECUTE pg_catalog.format('ALTER EXTENSION pg_mentat DROP %s %s', rec.type, rec.identity);
        END LOOP;

        PERFORM mentat._store_grants(st.schema_name);

        -- WARNING, not NOTICE: PostgreSQL runs an extension script with
        -- client_min_messages at WARNING or above, so a NOTICE is never shown.
        RAISE WARNING 'pg_mentat upgrade: rebuilt named store "%" in a new, empty schema %. Its old tables '
                      'are in schema % and were not copied: see "Upgrading from 1.17" in '
                      'docs/src/multi-tenancy.md. DROP SCHEMA % CASCADE once you no longer need them.',
                      st.store_name, st.schema_name, old_schema, pg_catalog.quote_ident(old_schema);
    END LOOP;
    PERFORM pg_catalog.set_config('search_path', old_path, true);
END
$migrate$;

-- ===========================================================================
-- End of the named stores (#337) section.
-- ===========================================================================
