-- pg_mentat 1.9.0 -> 1.9.1 upgrade.
--
-- Registers mentat's tables and sequences with pg_extension_config_dump so
-- pg_dump includes their data. Before this, a logical backup of a database
-- using pg_mentat restored every store empty. No schema or data change.
-- Keep in sync with sql/27_dump_config.sql.

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
