// edn_q_rows (1.10.0): streamed results, and the result-size limit errors.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "SELECT edn_t('[{:db/ident :qr/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                            {:db/ident :qr/s :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]'::TEXT)",
        )
        .expect("schema");
        Spi::run(
            "SELECT edn_t('[' || string_agg(format('{:qr/n %s :qr/s \"s%s\"}', g, g), ' ') || ']') \
             FROM generate_series(1, 2500) g",
        )
        .expect("data");
    }

    fn error_of(sql: &str) -> String {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<String>(&format!("SELECT mentat._qr_error('{escaped}')"))
            .expect("error probe")
            .unwrap_or_else(|| panic!("expected an error from {sql}"))
    }

    fn install_error_probe() {
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._qr_error(stmt TEXT) RETURNS TEXT LANGUAGE plpgsql AS $$
             DECLARE m TEXT; h TEXT;
             BEGIN
               EXECUTE stmt;
               RETURN NULL;
             EXCEPTION WHEN OTHERS THEN
               GET STACKED DIAGNOSTICS m = MESSAGE_TEXT, h = PG_EXCEPTION_HINT;
               RETURN m || ' HINT: ' || coalesce(h, '');
             END $$",
        )
        .expect("probe");
    }

    #[pg_test]
    fn test_edn_q_rows_streams_one_array_per_row() {
        setup();
        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM edn_q_rows('[:find ?e ?n :where [?e :qr/n ?n]]')",
        )
        .expect("count");
        assert_eq!(n, Some(2500));
        // One JSON array per row in :find order, typed like edn_q's values.
        let (sum, first) = Spi::get_two::<i64, String>(
            "SELECT sum((r->>1)::bigint)::bigint, min(r->>2) FROM \
             edn_q_rows('[:find ?e ?n ?s :where [?e :qr/n ?n] [?e :qr/s ?s]]') r",
        )
        .expect("row");
        assert_eq!(sum, Some(2500 * 2501 / 2));
        assert_eq!(first.as_deref(), Some("s1"));
        let ty = Spi::get_one::<String>(
            "SELECT jsonb_typeof(r->0) FROM edn_q_rows('[:find ?n :where [_ :qr/n ?n]]') r LIMIT 1",
        )
        .expect("type");
        assert_eq!(ty.as_deref(), Some("number"));
        // Same inputs as edn_q: :in bindings and pagination.
        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM edn_q_rows('[:find ?e :in $ ?lo :where [?e :qr/n ?n] [(>= ?n ?lo)]]', \
             '{\"inputs\": [2401]}')",
        )
        .expect("count");
        assert_eq!(n, Some(100));
        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM edn_q_rows('[:find ?e :where [?e :qr/n _]]', '{\"limit\": 7}')",
        )
        .expect("count");
        assert_eq!(n, Some(7));
        // Agrees with edn_q.
        let same = Spi::get_one::<bool>(
            "SELECT (SELECT jsonb_agg(r ORDER BY r->>0) FROM edn_q_rows('[:find ?s :where [_ :qr/s ?s]]') r) \
               = (SELECT jsonb_agg(x ORDER BY x->>0) FROM \
                  jsonb_array_elements(edn_q('[:find ?s :where [_ :qr/s ?s]]', '{}')->'results') x)",
        )
        .expect("cmp");
        assert_eq!(same, Some(true));
    }

    #[pg_test]
    fn test_result_limit_errors_name_the_guc() {
        setup();
        install_error_probe();
        Spi::run("SET LOCAL mentat.max_result_rows = 1000").expect("guc");
        for f in [
            "edn_q('[:find ?e :where [?e :qr/n _]]', '{}')",
            "count(*) FROM edn_q_rows('[:find ?e :where [?e :qr/n _]]')",
        ] {
            let err = error_of(&format!("SELECT {f}"));
            assert!(err.contains(":db.error/result-limit-exceeded"), "{err}");
            assert!(err.contains("mentat.max_result_rows = 1000"), "{err}");
            assert!(
                err.contains("SET LOCAL mentat.max_result_rows = 10000"),
                "{err}"
            );
        }
        // An explicit limit, or max_result_rows = 0, is not an error.
        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM edn_q_rows('[:find ?e :where [?e :qr/n _]]', '{\"limit\": 2000}')",
        )
        .expect("count");
        assert_eq!(n, Some(2000));
        Spi::run("SET LOCAL mentat.max_result_rows = 0").expect("guc");
        let n = Spi::get_one::<i64>(
            "SELECT count(*) FROM edn_q_rows('[:find ?e :where [?e :qr/n _]]')",
        )
        .expect("count");
        assert_eq!(n, Some(2500));
    }

    #[pg_test]
    fn test_temp_file_limit_error_names_the_guc() {
        setup();
        install_error_probe();
        // A tiny work_mem forces the DISTINCT to spill; a 64kB temp limit
        // then aborts it.
        Spi::run("SET LOCAL mentat.temp_file_limit = '64kB'").expect("guc");
        Spi::run("SET LOCAL work_mem = '64kB'").expect("guc");
        Spi::run("SET LOCAL mentat.enable_optimizer_hints = off").expect("guc");
        Spi::run("SET LOCAL max_parallel_workers_per_gather = 0").expect("guc");
        Spi::run("SET LOCAL enable_hashagg = off").expect("guc");
        let err = error_of(
            "SELECT edn_q('[:find ?a ?b :where [?a :qr/s ?x] [?b :qr/s ?y]]', '{\"limit\": 1}')",
        );
        assert!(err.contains("mentat.temp_file_limit = 64kB"), "{err}");
        assert!(
            err.contains("SET LOCAL mentat.temp_file_limit = '256kB'"),
            "{err}"
        );
    }

    /// Collection :in bindings bind one array parameter (`= ANY($n)`), so
    /// the statement is the same for any collection size; every value type
    /// still matches.
    #[pg_test]
    fn test_collection_input_binds_one_array() {
        setup();
        let q = |query: &str, inputs: &str| -> i64 {
            Spi::get_one::<i64>(&format!(
                "SELECT count(*) FROM edn_q_rows('{query}', '{inputs}')"
            ))
            .expect("q")
            .unwrap_or(-1)
        };
        let long = "[:find ?e :in $ [?n ...] :where [?e :qr/n ?n]]";
        assert_eq!(q(long, r#"{"inputs": [[1, 2, 3, 99999]]}"#), 3);
        assert_eq!(q(long, r#"{"inputs": [[]]}"#), 0);
        let text = "[:find ?e :in $ [?s ...] :where [?e :qr/s ?s]]";
        assert_eq!(q(text, r#"{"inputs": [["s1", "s2", "nope"]]}"#), 2);
        let e = Spi::get_one::<i64>(
            "SELECT e FROM mentat.current_long WHERE v = 7 AND a = \
             (SELECT entid FROM mentat.idents WHERE ident = ':qr/n')",
        )
        .expect("e")
        .expect("NULL");
        let ent = "[:find ?n :in $ [?e ...] :where [?e :qr/n ?n]]";
        assert_eq!(q(ent, &format!(r#"{{"inputs": [[{e}, 1]]}}"#)), 1);
        // Keywords (stored without the ':').
        Spi::run(
            "SELECT edn_t('[{:db/ident :qr/k :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
                            {:qr/k :k/a} {:qr/k :k/b} {:qr/k :k/c}]'::TEXT)",
        )
        .expect("kw");
        let kw = "[:find ?e :in $ [?k ...] :where [?e :qr/k ?k]]";
        assert_eq!(q(kw, r#"{"inputs": [[":k/a", ":k/c"]]}"#), 2);
    }
}
