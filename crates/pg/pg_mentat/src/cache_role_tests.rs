// The per-backend caches against roles (#402): a plan cached for one role
// was reused by another, and PostgreSQL checks schema USAGE only when a query
// is parsed; the schema cache answered ident lookups without the role's
// rights on the store's schema / idents tables. And against aborts (#404):
// the schema cache kept idents a rolled-back transaction installed.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    const SCHEMA_TX: &str = "[{:db/ident :cr/name :db/valueType :db.type/string \
         :db/cardinality :db.cardinality/one}]";
    const NAMES: &str = "[:find [?n ...] :where [_ :cr/name ?n]]";

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema");
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
    }

    /// SQLERRM of `sql`, or "" if it succeeded.
    fn error_of(sql: &str) -> String {
        Spi::get_one_with_args::<String>(
            "SELECT pg_temp.capture($1)",
            &[pgrx::datum::DatumWithOid::from(sql)],
        )
        .expect("capture")
        .unwrap_or_default()
    }

    fn q_sql(store: &str, query: &str) -> String {
        format!("SELECT q('{store}', '{query}', '{{}}')")
    }

    fn names(store: &str) -> serde_json::Value {
        Spi::get_one::<pgrx::JsonB>(&q_sql(store, NAMES))
            .unwrap_or_else(|e| panic!("q('{store}'): {e}"))
            .map(|j| j.0["result"].clone())
            .unwrap_or_default()
    }

    /// Store `cr_s` with one name, and roles:
    /// - cr_tbl: SELECT on every mentat_cr_s table, no USAGE on mentat_cr_s;
    /// - cr_none: nothing on mentat_cr_s;
    /// - cr_app: USAGE and SELECT (docs/src/multi-tenancy.md's recipe);
    /// - cr_tx: USAGE and SELECT on mentat_cr_s.transactions only.
    ///
    /// All can reach mentat.stores / cache_generation. The pg_test's
    /// transaction rolls the roles back.
    fn store_and_roles() {
        Spi::run("SELECT create_store('cr_s')").expect("create_store");
        Spi::run(&format!("SELECT t('cr_s', '{SCHEMA_TX}')")).expect("schema");
        Spi::run("SELECT t('cr_s', '[{:cr/name \"secret\"}]')").expect("data");
        Spi::run(
            "CREATE ROLE cr_tbl; CREATE ROLE cr_none; CREATE ROLE cr_app; CREATE ROLE cr_tx; \
             GRANT USAGE ON SCHEMA mentat TO cr_tbl, cr_none, cr_app, cr_tx; \
             GRANT SELECT ON mentat.stores, mentat.cache_generation TO cr_tbl, cr_none, cr_app, cr_tx; \
             GRANT EXECUTE ON FUNCTION pg_temp.capture(text) TO cr_tbl, cr_none, cr_app, cr_tx; \
             GRANT SELECT ON ALL TABLES IN SCHEMA mentat_cr_s TO cr_tbl, cr_app; \
             GRANT USAGE ON SCHEMA mentat_cr_s TO cr_app, cr_tx; \
             GRANT SELECT ON mentat_cr_s.transactions TO cr_tx",
        )
        .expect("roles");
    }

    /// After the superuser ran a query (its plan and the store's schema are
    /// cached), a role with table grants but no USAGE on the store's schema
    /// is refused, as it is in a fresh backend.
    #[pg_test]
    fn cached_plan_is_not_reused_by_a_role_without_usage() {
        setup();
        store_and_roles();
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        Spi::run("SET LOCAL ROLE cr_tbl").expect("set role");
        let err = error_of(&q_sql("cr_s", NAMES));
        Spi::run("RESET ROLE").expect("reset");
        assert!(
            err.contains("permission denied for schema mentat_cr_s"),
            "got: {err:?}"
        );
    }

    /// A role with no rights on the store gets "permission denied" for an
    /// attribute that exists and one that doesn't, not the cache's answer
    /// (which told them apart).
    #[pg_test]
    fn cached_schema_does_not_answer_a_role_without_rights() {
        setup();
        store_and_roles();
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        Spi::run("SET LOCAL ROLE cr_none").expect("set role");
        let real = error_of(&q_sql("cr_s", NAMES));
        let nope = error_of(&q_sql("cr_s", "[:find ?n :where [_ :cr/nope ?n]]"));
        Spi::run("RESET ROLE").expect("reset");
        for err in [&real, &nope] {
            assert!(
                err.contains("permission denied for schema mentat_cr_s"),
                "got: {err:?}"
            );
        }
    }

    /// A role with the documented rights still queries the store after the
    /// superuser warmed the caches; and plans are still reused within one
    /// role.
    #[pg_test]
    fn role_with_rights_works_and_plans_are_reused_per_role() {
        setup();
        store_and_roles();
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        Spi::run("SELECT mentat_stmt_cache_clear()").expect("clear");
        Spi::run("SET LOCAL ROLE cr_app").expect("set role");
        for _ in 0..3 {
            assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        }
        Spi::run("RESET ROLE").expect("reset");
        let stats = Spi::get_one::<pgrx::JsonB>("SELECT mentat_stmt_cache_stats()")
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(
            (stats["size"].as_i64(), stats["total_hits"].as_i64()),
            (Some(1), Some(2)),
            "{stats}"
        );
        // The superuser's own run is a new plan, then hits it.
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        let stats = Spi::get_one::<pgrx::JsonB>("SELECT mentat_stmt_cache_stats()")
            .unwrap()
            .unwrap()
            .0;
        assert_eq!(
            (stats["size"].as_i64(), stats["total_hits"].as_i64()),
            (Some(2), Some(3)),
            "{stats}"
        );
    }

    /// A REVOKE of USAGE takes effect for a role in a backend where it has
    /// already queried the store.
    #[pg_test]
    fn revoked_usage_takes_effect_in_the_same_backend() {
        setup();
        store_and_roles();
        Spi::run("SET LOCAL ROLE cr_app").expect("set role");
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        Spi::run("RESET ROLE").expect("reset");
        Spi::run("REVOKE USAGE ON SCHEMA mentat_cr_s FROM cr_app").expect("revoke");
        Spi::run("SET LOCAL ROLE cr_app").expect("set role");
        let real = error_of(&q_sql("cr_s", NAMES));
        let nope = error_of(&q_sql("cr_s", "[:find ?n :where [_ :cr/nope ?n]]"));
        Spi::run("RESET ROLE").expect("reset");
        for err in [&real, &nope] {
            assert!(
                err.contains("permission denied for schema mentat_cr_s"),
                "got: {err:?}"
            );
        }
    }

    /// A role with USAGE through a group, and table grants of its own, loses
    /// the store when it leaves the group (PostgreSQL does not re-parse a kept
    /// plan when a role membership changes). The query has no attribute
    /// ident, so the schema cache's own check does not run.
    #[pg_test]
    fn usage_through_a_group_ends_with_the_membership() {
        setup();
        store_and_roles();
        let any = "[:find ?v :where [_ ?a ?v] [(= ?v \"secret\")]]";
        let found = || {
            Spi::get_one::<pgrx::JsonB>(&q_sql("cr_s", any))
                .unwrap()
                .unwrap()
                .0["result"]
                .clone()
        };
        Spi::run(
            "CREATE ROLE cr_grp; GRANT USAGE ON SCHEMA mentat_cr_s TO cr_grp; \
             GRANT cr_grp TO cr_tbl",
        )
        .expect("group");
        Spi::run("SET LOCAL ROLE cr_tbl").expect("set role");
        assert_eq!(found(), serde_json::json!([["secret"]]));
        assert_eq!(found(), serde_json::json!([["secret"]]));
        Spi::run("RESET ROLE").expect("reset");
        Spi::run("REVOKE cr_grp FROM cr_tbl").expect("revoke");
        Spi::run("SET LOCAL ROLE cr_tbl").expect("set role");
        let err = error_of(&q_sql("cr_s", any));
        Spi::run("RESET ROLE").expect("reset");
        assert!(
            err.contains("permission denied for schema mentat_cr_s"),
            "got: {err:?}"
        );
    }

    /// The script path resolves keyword entity ids through the same cache: a
    /// role that can open the store (USAGE + SELECT on its transactions
    /// table) but not read its schema gets the same error for an ident that
    /// exists and one that doesn't.
    #[cfg(feature = "script")]
    #[pg_test]
    fn script_entity_ident_needs_the_schema_tables() {
        setup();
        store_and_roles();
        assert_eq!(names("cr_s"), serde_json::json!(["secret"]));
        let entity = |kw: &str| {
            error_of(&format!(
                "SELECT edn_eval($s$(mentat.store/entity \
                 (mentat.store/db (mentat.store/open \"cr_s\")) {kw})$s$)"
            ))
        };
        // The superuser sees the difference: it may.
        assert_eq!(entity(":cr/name"), "");
        assert!(entity(":cr/nope").contains("unknown ident"));
        Spi::run("SET LOCAL ROLE cr_tx").expect("set role");
        let real = entity(":cr/name");
        let nope = entity(":cr/nope");
        Spi::run("RESET ROLE").expect("reset");
        for err in [&real, &nope] {
            assert!(
                err.contains("permission denied for table schema"),
                "got: {err:?}"
            );
        }
    }

    fn unknown_sql(store: &str, attr: &str) -> String {
        q_sql(store, &format!("[:find ?v :where [_ {attr} ?v]]"))
    }

    fn unknown(store: &str, attr: &str) -> String {
        error_of(&unknown_sql(store, attr))
    }

    fn define(store: &str, attr: &str) {
        Spi::run(&format!(
            "SELECT t('{store}', '[{{:db/ident {attr} :db/valueType :db.type/string \
             :db/cardinality :db.cardinality/one}}]')"
        ))
        .expect("define");
    }

    /// #404: an ident installed in a rolled-back subtransaction (a plpgsql
    /// EXCEPTION block; SPI can't run SAVEPOINT) is gone from the cache, also
    /// when a read after the schema write loaded the cache again.
    #[pg_test]
    fn ident_of_a_rolled_back_subtransaction_leaves_the_cache() {
        setup();
        Spi::run("SELECT create_store('cr_rb')").expect("create_store");
        for store in ["default", "cr_rb"] {
            let err = error_of(&format!(
                "DO $d$ BEGIN \
                   PERFORM t('{store}', '[{{:db/ident :cr/ghost :db/valueType :db.type/string \
                                            :db/cardinality :db.cardinality/one}}]'); \
                   PERFORM q('{store}', '[:find ?v :where [_ :cr/ghost ?v]]', '{{}}'); \
                   RAISE EXCEPTION 'undo'; \
                 END $d$"
            ));
            assert_eq!(
                err, "undo",
                "{store}: defined and read in the subtransaction"
            );
            let err = unknown(store, ":cr/ghost");
            assert!(
                err.contains(":db.error/unknown-attribute"),
                "{store}: got {err:?}"
            );
        }
    }

    /// #404 in a second session (dblink: a pg_test is one transaction, and
    /// SPI can't run SAVEPOINT): define and read, then ROLLBACK or ROLLBACK
    /// TO SAVEPOINT; the ident is unknown in that session after. Skipped
    /// (passes) without dblink.
    #[pg_test]
    fn idents_of_rollback_and_rollback_to_savepoint_leave_the_cache() {
        setup();
        let available: i64 =
            Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'dblink'")
                .unwrap()
                .unwrap();
        if available == 0 {
            return;
        }
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS dblink; \
             SELECT dblink_connect('cr_c', pg_catalog.format('dbname=%s port=%s host=%s', \
               current_database(), current_setting('port'), \
               split_part(current_setting('unix_socket_directories'), ',', 1)))",
        )
        .expect("connect");
        let on = |sql: &str| {
            Spi::run_with_args(
                "SELECT dblink_exec('cr_c', $1)",
                &[pgrx::datum::DatumWithOid::from(sql)],
            )
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
        };
        // SQLERRM of `sql` in the session, or "".
        let error_there = |sql: &str| {
            Spi::get_one_with_args::<String>(
                "SELECT r FROM dblink('cr_c', \
                   'SELECT pg_temp.capture(' || pg_catalog.quote_literal($1) || ')') AS t(r text)",
                &[pgrx::datum::DatumWithOid::from(sql)],
            )
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_default()
        };
        on("SET search_path TO mentat, public");
        on("CREATE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
            LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
            EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$");
        let define_there = |attr: &str| {
            on(&format!(
                "DO $d$ BEGIN PERFORM t('default', '[{{:db/ident {attr} \
                 :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one}}]'); END $d$"
            ))
        };
        let read = |attr: &str| error_there(&unknown_sql("default", attr));
        // ROLLBACK.
        on("BEGIN");
        define_there(":cr/top");
        let in_tx = read(":cr/top");
        on("ROLLBACK");
        let after_rollback = read(":cr/top");
        // ROLLBACK TO SAVEPOINT; the transaction goes on and commits.
        on("BEGIN");
        on("SAVEPOINT sp");
        define_there(":cr/sp");
        let in_sp = read(":cr/sp");
        on("ROLLBACK TO SAVEPOINT sp");
        let after_sp = read(":cr/sp");
        on("COMMIT");
        Spi::run("SELECT dblink_disconnect('cr_c')").expect("disconnect");
        assert_eq!(
            (in_tx.as_str(), in_sp.as_str()),
            ("", ""),
            "defined, then read"
        );
        for err in [&after_rollback, &after_sp] {
            assert!(err.contains(":db.error/unknown-attribute"), "got: {err:?}");
        }
    }

    /// #404: an error inside t() after its schema part (here a trigger on
    /// the narrow table raises) rolls the idents back; the cache drops them.
    #[pg_test]
    fn error_after_the_schema_part_leaves_the_cache_clean() {
        setup();
        Spi::run(
            "CREATE FUNCTION pg_temp.cr_boom() RETURNS trigger LANGUAGE plpgsql AS \
             $$ BEGIN IF NEW.v = 'boom' THEN RAISE EXCEPTION 'boom'; END IF; RETURN NEW; END $$; \
             CREATE TRIGGER cr_boom BEFORE INSERT ON mentat.datoms_text_new \
             FOR EACH ROW EXECUTE FUNCTION pg_temp.cr_boom()",
        )
        .expect("trigger");
        let err = error_of(
            "SELECT edn_t('[{:db/ident :cr/boom :db/valueType :db.type/string \
             :db/cardinality :db.cardinality/one} {:cr/boom \"boom\"}]')",
        );
        assert!(err.contains("boom"), "got: {err:?}");
        let err = unknown("default", ":cr/boom");
        assert!(err.contains(":db.error/unknown-attribute"), "got: {err:?}");
        Spi::run("DROP TRIGGER cr_boom ON mentat.datoms_text_new").expect("drop trigger");
    }
}
