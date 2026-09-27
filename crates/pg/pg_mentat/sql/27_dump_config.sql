-- Include pg_mentat's data in pg_dump.
--
-- Every mentat table is an extension member, and pg_dump dumps the data of an
-- extension member only when the table is registered with
-- pg_extension_config_dump(). None were, so a logical backup (pg_dump) of a
-- database using pg_mentat restored the schema with EMPTY stores: every datom,
-- transaction, attribute and ident that users had transacted was silently
-- missing. Found on a production database whose nightly dump carried data for
-- 0 of 28 mentat tables (2026-09-27).
--
-- Tables CREATE EXTENSION seeds (stores, idents, schema, partitions,
-- transactions, cache_generation) get a filter that excludes exactly the seed
-- rows, so pg_restore does not collide with the rows CREATE EXTENSION already
-- inserted into the restored database. Everything else is dumped whole.
--
-- The partition/tx sequences are dumped too: without them a restored store
-- would re-issue entids that its datoms already use.
--
-- pg_extension_config_dump() may only be called from an extension script, so
-- this lives here and in the upgrade edge pg_mentat--1.9.0--1.9.1.sql. The
-- DO block registers every member table the running install actually has, so
-- tables added by later releases are covered by adding them here, and a table
-- that an older store lacks is skipped rather than erroring.

DO $dump$
DECLARE
    seed_filter CONSTANT jsonb := jsonb_build_object(
        'stores',           $f$WHERE store_name <> 'default'$f$,
        'idents',           'WHERE entid >= 100',
        'schema',           'WHERE entid >= 100',
        'partitions',       $f$WHERE name NOT IN ('db.part/db', 'db.part/user', 'db.part/tx')$f$,
        'transactions',     'WHERE tx <> 1000000',
        'cache_generation', $f$WHERE store_name <> 'default'$f$
    );
    r record;
BEGIN
    FOR r IN
        SELECT c.oid, c.relname, c.relkind
          FROM pg_depend d
          JOIN pg_class c ON c.oid = d.objid
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE d.classid = 'pg_class'::regclass
           AND d.refclassid = 'pg_extension'::regclass
           AND d.refobjid = (SELECT oid FROM pg_extension WHERE extname = 'pg_mentat')
           AND d.deptype = 'e'
           AND n.nspname = 'mentat'
           AND c.relkind IN ('r', 'S')
           AND NOT c.relispartition
    LOOP
        PERFORM pg_catalog.pg_extension_config_dump(
            r.oid::regclass,
            COALESCE(seed_filter ->> r.relname, ''));
    END LOOP;
END
$dump$;
