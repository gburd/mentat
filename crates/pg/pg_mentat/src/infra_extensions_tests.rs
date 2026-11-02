// Regression tests for the PG19 graph + TimescaleDB + pg_partman +
// pg_cron integration helpers. All four are SOFT dependencies;
// detection-helper tests run unconditionally, happy-path tests
// skip when the underlying extension isn't installed.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._inf_capture(stmt TEXT) RETURNS TEXT
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN '';
             EXCEPTION WHEN OTHERS THEN
                 RETURN SQLERRM;
             END;
             $$",
        )
        .expect("error-capture helper");
    }

    fn capture_error(sql: &str) -> String {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<String>(&format!("SELECT mentat._inf_capture('{}')", escaped))
            .expect("capture")
            .unwrap_or_default()
    }

    fn install_basic_schema() {
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :person/employer :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
                {:db/ident :company/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        )
        .expect("schema tx");
    }

    // ====================================================================
    // PG19 graph helpers
    // ====================================================================

    #[pg_test]
    fn pg_test_inf_has_pg19_graph_returns_bool() {
        setup();
        let _ = Spi::get_one::<bool>("SELECT mentat.has_pg19_graph()")
            .expect("call")
            .unwrap_or(false);
    }

    #[pg_test]
    fn pg_test_inf_create_vertex_view_string_attr() {
        setup();
        install_basic_schema();
        let v = Spi::get_one::<String>("SELECT mentat.create_vertex_view(':person/name')")
            .expect("create")
            .expect("NULL");
        assert!(v.starts_with("mentat.v_"), "got: {}", v);

        // Verify the view is queryable.
        Spi::run(&format!("SELECT count(*) FROM {}", v)).expect("query view");
    }

    #[pg_test]
    fn pg_test_inf_create_edge_view_ref_attr() {
        setup();
        install_basic_schema();
        let v = Spi::get_one::<String>("SELECT mentat.create_edge_view(':person/employer')")
            .expect("create")
            .expect("NULL");
        assert!(v.starts_with("mentat.e_"));
    }

    #[pg_test]
    fn pg_test_inf_create_edge_view_rejects_non_ref() {
        setup();
        install_basic_schema();
        let err = capture_error("SELECT mentat.create_edge_view(':person/name')");
        assert!(
            err.contains(":db.error/fn-arg") && err.contains(":db.type/ref"),
            "expected ref-required error, got: {}",
            err,
        );
    }

    #[pg_test]
    fn pg_test_inf_property_graph_ddl_generation() {
        setup();
        install_basic_schema();
        Spi::run("SELECT mentat.create_vertex_view(':person/name')").expect("v1");
        Spi::run("SELECT mentat.create_vertex_view(':company/name')").expect("v2");
        Spi::run("SELECT mentat.create_edge_view(':person/employer')").expect("e1");

        let ddl = Spi::get_one::<String>(
            "SELECT mentat.create_property_graph_ddl(
                'social',
                ARRAY[':person/name', ':company/name'],
                ARRAY[':person/employer']
            )",
        )
        .expect("ddl")
        .expect("NULL");
        assert!(ddl.starts_with("CREATE PROPERTY GRAPH social"));
        assert!(ddl.contains("VERTEX TABLES"));
        assert!(ddl.contains("EDGE TABLES"));
        assert!(ddl.contains("mentat.v_") && ddl.contains("LABEL"));
    }

    fn rows(sql: &str) -> Vec<String> {
        let mut v: Vec<String> = Spi::connect(|client| {
            client
                .select(sql, None, &[])
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                .map(|r| r.get::<String>(1).unwrap().unwrap_or_default())
                .collect()
        });
        v.sort();
        v
    }

    // Vertex and edge views read the log with only `added`, so a replaced
    // name or employer was still a vertex or an edge (#395). They read the
    // projection now, and the 1.19.0 upgrade makes the 1.18 ones again,
    // keeping their owner, grants and options. A view of the same name with
    // another body is left alone.
    #[pg_test]
    fn pg_test_inf_vertex_and_edge_views_read_current_state() {
        setup();
        install_basic_schema();
        Spi::run(
            "SELECT edn_t('[{:db/id \"c1\" :company/name \"Acme\"} {:db/id \"c2\" :company/name \"Bolt\"} \
                            {:db/id \"p\" :person/name \"Pat\" :person/employer \"c1\"}]'::TEXT)",
        )
        .expect("data");
        let eid = |n: &str| -> i64 {
            Spi::get_one::<i64>(&format!(
                "SELECT e FROM mentat.current_text WHERE store_id = 0 AND v = '{n}'"
            ))
            .unwrap()
            .unwrap()
        };
        let (c2, p) = (eid("Bolt"), eid("Pat"));
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {p} :person/name \"Patricia\"] [:db/add {p} :person/employer {c2}]]'::TEXT)"
        ))
        .expect("replace");
        let check = |round: &str| {
            assert_eq!(
                rows("SELECT label FROM mentat.v__person_name"),
                vec!["Patricia"],
                "{round}: vertices"
            );
            assert_eq!(
                rows("SELECT dst::TEXT FROM mentat.e__person_employer"),
                vec![c2.to_string()],
                "{round}: edges"
            );
        };
        Spi::run("SELECT mentat.create_vertex_view(':person/name'), mentat.create_edge_view(':person/employer')")
            .expect("views");
        check("1.19");

        // What 1.18's create_vertex_view / create_edge_view made, with an
        // owner, a grant and an option; and a view of its own named like one.
        let attr = |i: &str| -> i64 {
            Spi::get_one::<i64>(&format!("SELECT mentat.attr_id('{i}')"))
                .unwrap()
                .unwrap()
        };
        Spi::run(&format!(
            "CREATE OR REPLACE VIEW mentat.v__person_name AS \
               SELECT e, v::TEXT AS label FROM mentat.datoms_text_new WHERE a = {} AND added; \
             CREATE OR REPLACE VIEW mentat.e__person_employer AS \
               SELECT (e * 100000 + v) AS id, e AS src, v AS dst, ':person/employer' AS label \
               FROM mentat.datoms_ref_new WHERE a = {} AND added; \
             CREATE VIEW mentat.v__company_name AS SELECT 1::BIGINT AS e, 'mine'::TEXT AS label; \
             CREATE ROLE inf_gv_reader NOLOGIN; \
             GRANT SELECT ON mentat.v__person_name TO inf_gv_reader; \
             ALTER VIEW mentat.e__person_employer SET (security_invoker = true)",
            attr(":person/name"),
            attr(":person/employer")
        ))
        .expect("1.18 views");
        assert_eq!(
            rows("SELECT label FROM mentat.v__person_name"),
            vec!["Pat", "Patricia"],
            "1.18 reads the log"
        );
        let upgrade = include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql");
        let block = &upgrade[upgrade.find("DO $graphviews$").unwrap()..];
        let block = &block[..block.find("$graphviews$;").unwrap() + "$graphviews$;".len()];
        Spi::run(block).expect("upgrade");
        check("upgraded");
        assert_eq!(
            rows("SELECT label FROM mentat.v__company_name"),
            vec!["mine"],
            "another view of that name"
        );
        assert_eq!(
            rows(
                "SELECT relname || ' ' || coalesce(relacl::TEXT, '-') || ' ' || coalesce(reloptions::TEXT, '-') \
                   FROM pg_class WHERE relname IN ('v__person_name', 'e__person_employer')"
            )
            .iter()
            .map(|r| r.contains("inf_gv_reader=r") || r.contains("security_invoker=true"))
            .collect::<Vec<_>>(),
            vec![true, true]
        );
    }

    #[pg_test]
    fn pg_test_inf_drop_vertex_view_idempotent() {
        setup();
        install_basic_schema();
        Spi::run("SELECT mentat.create_vertex_view(':person/name')").expect("v");
        let d1 = Spi::get_one::<bool>("SELECT mentat.drop_vertex_view(':person/name')")
            .expect("drop1")
            .expect("NULL");
        let d2 = Spi::get_one::<bool>("SELECT mentat.drop_vertex_view(':person/name')")
            .expect("drop2")
            .expect("NULL");
        assert!(d1, "first drop true");
        assert!(!d2, "second drop false");
    }

    // ====================================================================
    // TimescaleDB helpers
    // ====================================================================

    #[pg_test]
    fn pg_test_inf_has_timescaledb_returns_bool() {
        setup();
        let _ = Spi::get_one::<bool>("SELECT mentat.has_timescaledb()")
            .expect("call")
            .unwrap_or(false);
    }

    #[pg_test]
    fn pg_test_inf_timescale_attach_without_extension() {
        setup();
        let has = Spi::get_one::<bool>("SELECT mentat.has_timescaledb()")
            .ok()
            .flatten()
            .unwrap_or(false);
        if has {
            return;
        }
        let err = capture_error("SELECT mentat.timescale_attach_transactions()");
        assert!(
            err.contains(":db.error/missing-extension") && err.contains("TimescaleDB"),
            "expected missing-extension TimescaleDB error, got: {}",
            err,
        );
    }

    // ====================================================================
    // pg_partman helpers
    // ====================================================================

    #[pg_test]
    fn pg_test_inf_has_pg_partman_returns_bool() {
        setup();
        let _ = Spi::get_one::<bool>("SELECT mentat.has_pg_partman()")
            .expect("call")
            .unwrap_or(false);
    }

    /// pg_partman attach refuses to convert a plain mentat.transactions
    /// table into a partitioned one — that requires a manual rewrite.
    /// We assert the helpful error message guides the user.
    #[pg_test]
    fn pg_test_inf_partman_attach_rejects_plain_table() {
        setup();
        let has = Spi::get_one::<bool>("SELECT mentat.has_pg_partman()")
            .ok()
            .flatten()
            .unwrap_or(false);
        if !has {
            return;
        }
        let err = capture_error("SELECT mentat.partman_attach_transactions()");
        assert!(
            err.contains(":db.error/manual-step") && err.contains("not a partitioned table"),
            "expected manual-step error, got: {}",
            err,
        );
    }

    #[pg_test]
    fn pg_test_inf_partman_set_retention_without_attach() {
        setup();
        let has = Spi::get_one::<bool>("SELECT mentat.has_pg_partman()")
            .ok()
            .flatten()
            .unwrap_or(false);
        if !has {
            return;
        }
        let err = capture_error("SELECT mentat.partman_set_transaction_retention('30 days')");
        // Either missing-config (transactions not registered) or another
        // pg_partman-side error; we just verify it surfaces something.
        assert!(
            !err.is_empty(),
            "expected an error when retention is set without attach"
        );
    }

    // ====================================================================
    // pg_cron helpers
    // ====================================================================

    #[pg_test]
    fn pg_test_inf_has_pg_cron_returns_bool() {
        setup();
        let _ = Spi::get_one::<bool>("SELECT mentat.has_pg_cron()")
            .expect("call")
            .unwrap_or(false);
    }

    #[pg_test]
    fn pg_test_inf_cron_schedule_without_extension() {
        setup();
        let has = Spi::get_one::<bool>("SELECT mentat.has_pg_cron()")
            .ok()
            .flatten()
            .unwrap_or(false);
        if has {
            return;
        }
        let err = capture_error("SELECT mentat.cron_schedule('test', '* * * * *', 'SELECT 1')");
        assert!(
            err.contains(":db.error/missing-extension") && err.contains("pg_cron"),
            "expected missing-extension pg_cron error, got: {}",
            err,
        );
    }
}
