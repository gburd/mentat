-- pg_mentat 1.16.1 -> 1.17.0 upgrade.
--
-- No table or data change.
--
-- 1. Trusted install (#331): the control file is now `superuser = true` +
--    `trusted = true`, so a role with CREATE on the database can install
--    pg_mentat. The installing role gets the same grants a fresh install gives
--    it (sql/27_dump_config.sql). @extowner@ is the role running ALTER
--    EXTENSION; for a superuser these are no-ops.
-- 2. Query changes are in the shared library and need only the new .so: get-else
--    and arithmetic results usable in predicates (#334), the EDN parser
--    accepting `+`, `-` and `/` as symbols, and create_store()'s fixed
--    lookup_entity. A store created by create_store() before 1.17.0 never got
--    one, because creation failed.
GRANT USAGE ON SCHEMA mentat TO @extowner@ WITH GRANT OPTION;
GRANT SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER
    ON ALL TABLES IN SCHEMA mentat TO @extowner@ WITH GRANT OPTION;
GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA mentat TO @extowner@ WITH GRANT OPTION;
