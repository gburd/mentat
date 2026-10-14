// Tests for the optional pg_fts / pg_turbovec integration (issue #332):
//   helpers:   mentat.has_pg_fts(), mentat.create_fts_index(attr, regconfig)
//              mentat.has_pg_turbovec(), mentat.attach_turbovec_attribute(attr, dim),
//              mentat.create_turbovec_index(attr, ops)
//   where-fns: [(bm25-match $ :attr "query" k) [[?e ?score]]]
//              [(turbovec-near $ :attr "[v1,...]" k [op]) [[?e ?dist]]]
//
// Both extensions are OPTIONAL. The "absent" tests run everywhere and pin the
// clear :db.error/missing-extension message; the happy-path tests run only when
// the extension is installed in the test cluster and skip otherwise.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;
    use pgrx::JsonB;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._ext_capture(stmt TEXT) RETURNS TEXT
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
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :xd/body :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :xd/vec  :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :xd/n    :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
             ]'::TEXT)",
        )
        .expect("schema");
    }

    fn capture_error(sql: &str) -> String {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<String>(&format!("SELECT mentat._ext_capture('{}')", escaped))
            .expect("capture")
            .unwrap_or_default()
    }

    fn installed(ext: &str) -> bool {
        // Try to install it in this database; tests skip if it isn't built.
        let _ = capture_error(&format!("CREATE EXTENSION IF NOT EXISTS {ext}"));
        Spi::get_one::<bool>(&format!(
            "SELECT EXISTS (SELECT 1 FROM pg_extension WHERE extname = '{ext}')"
        ))
        .ok()
        .flatten()
        .unwrap_or(false)
    }

    fn q(query: &str) -> serde_json::Value {
        let escaped = query.replace('\'', "''");
        Spi::get_one::<JsonB>(&format!("SELECT edn_q('{escaped}', '{{}}'::jsonb)"))
            .expect("edn_q")
            .expect("NULL")
            .0
    }

    // ---- helpers & error messages (run everywhere) -------------------------

    #[pg_test]
    fn has_fns_return_bool() {
        setup();
        let fts = Spi::get_one::<bool>("SELECT mentat.has_pg_fts()").expect("has_pg_fts");
        let tv = Spi::get_one::<bool>("SELECT mentat.has_pg_turbovec()").expect("has_pg_turbovec");
        assert!(fts.is_some() && tv.is_some());
    }

    #[pg_test]
    fn create_fts_index_errors_clearly_when_absent_or_wrong_type() {
        setup();
        // Probe/install FIRST, so the helper sees the real presence state.
        let present = installed("pg_fts");
        let err = capture_error("SELECT mentat.create_fts_index(':xd/n')");
        if present {
            assert!(
                err.contains("wrong-type-for-fts-index"),
                "a :db.type/long attr must be refused, got: {err}"
            );
        } else {
            assert!(err.contains("missing-extension"), "got: {err}");
            assert!(err.contains("pg_fts"), "got: {err}");
        }
        let err = capture_error("SELECT mentat.create_fts_index(':xd/nope')");
        assert!(
            err.contains("missing-extension") || err.contains("attribute-not-found"),
            "got: {err}"
        );
    }

    #[pg_test]
    fn turbovec_helpers_error_clearly_when_absent() {
        setup();
        if installed("pg_turbovec") {
            return; // covered by the happy path below
        }
        let err = capture_error("SELECT mentat.attach_turbovec_attribute(':xd/vec', 3)");
        assert!(err.contains("missing-extension"), "got: {err}");
        assert!(err.contains("pg_turbovec"), "got: {err}");
    }

    #[pg_test]
    fn where_fns_reject_bad_arity_and_args() {
        setup();
        let err = capture_error(
            "SELECT edn_q('[:find ?e :where [(bm25-match $ :xd/body \"x\") [[?e ?s]]]]', '{}'::jsonb)",
        );
        assert!(
            err.contains("fn-arity") && err.contains("bm25-match"),
            "got: {err}"
        );
        let err = capture_error(
            "SELECT edn_q('[:find ?e :where [(turbovec-near $ :xd/vec \"[1,2,3]\" 0) [[?e ?d]]]]', '{}'::jsonb)",
        );
        assert!(err.contains("k must be > 0"), "got: {err}");
        let err = capture_error(
            "SELECT edn_q('[:find ?e :where [(turbovec-near $ :xd/vec \"[1,2,3]\" 2 :bogus) [[?e ?d]]]]', '{}'::jsonb)",
        );
        assert!(err.contains("distance op"), "got: {err}");
        let err = capture_error(
            "SELECT edn_q('[:find ?e :where [(bm25-match $ :xd/unknown \"x\" 3) [[?e ?s]]]]', '{}'::jsonb)",
        );
        assert!(err.contains("unknown-attribute"), "got: {err}");
    }

    // ---- happy paths (need the extensions) ---------------------------------

    #[pg_test]
    fn bm25_match_ranks_and_joins_back() {
        setup();
        if !installed("pg_fts") {
            return;
        }
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"a\" :xd/body \"the quick brown fox jumps\" :xd/n 1}
                {:db/id \"b\" :xd/body \"a quick fox\"               :xd/n 2}
                {:db/id \"c\" :xd/body \"lazy dogs sleep all day\"   :xd/n 3}
             ]'::TEXT)",
        )
        .expect("data");
        let idx = Spi::get_one::<String>("SELECT mentat.create_fts_index(':xd/body')")
            .expect("create_fts_index")
            .expect("NULL");
        assert!(idx.starts_with("mentat_fts_"), "index name: {idx}");
        // Idempotent.
        Spi::run("SELECT mentat.create_fts_index(':xd/body')").expect("idempotent");

        // Matches only the two fox docs, joined back to :xd/n.
        let v = q(
            "[:find ?n :where [(bm25-match $ :xd/body \"quick fox\" 10) [[?e ?s]]] [?e :xd/n ?n]]",
        );
        let mut ns: Vec<i64> = v["result"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r[0].as_i64().expect("n"))
            .collect();
        ns.sort();
        assert_eq!(ns, vec![1, 2], "got {v}");

        // k limits the result.
        let v = q("[:find ?e :where [(bm25-match $ :xd/body \"quick fox\" 1) [[?e ?s]]]]");
        assert_eq!(v["result"].as_array().map(|a| a.len()), Some(1), "got {v}");
    }

    #[pg_test]
    fn turbovec_near_finds_nearest() {
        setup();
        if !installed("pg_turbovec") {
            return;
        }
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"a\" :xd/vec \"x\" :xd/n 1}
                {:db/id \"b\" :xd/vec \"x\" :xd/n 2}
                {:db/id \"c\" :xd/vec \"x\" :xd/n 3}
             ]'::TEXT)",
        )
        .expect("data");
        let table = Spi::get_one::<String>("SELECT mentat.attach_turbovec_attribute(':xd/vec', 8)")
            .expect("attach")
            .expect("NULL");
        // Vectors keyed by entid (looked up through :xd/n).
        // pg_turbovec requires dim % 8 == 0.
        for (n, v) in [
            (1, "[1,0,0,0,0,0,0,0]"),
            (2, "[0.9,0.1,0,0,0,0,0,0]"),
            (3, "[0,0,0,0,0,0,0,1]"),
        ] {
            Spi::run(&format!(
                "INSERT INTO {table} (e, v) SELECT d.e, '{v}'::turbovec.vector \
                 FROM mentat.datoms_long_new d \
                 WHERE d.a = (SELECT entid FROM mentat.schema WHERE ident = ':xd/n') \
                   AND d.v = {n} AND d.added"
            ))
            .expect("insert vector");
        }
        Spi::run("SELECT mentat.create_turbovec_index(':xd/vec')").expect("index");

        // Nearest two to e1 by cosine are n=1 then n=2.
        let v = q(
            "[:find ?n ?d :where [(turbovec-near $ :xd/vec \"[1,0,0,0,0,0,0,0]\" 2) [[?e ?d]]] [?e :xd/n ?n]]",
        );
        let ns: Vec<i64> = v["result"]
            .as_array()
            .expect("rows")
            .iter()
            .map(|r| r[0].as_i64().expect("n"))
            .collect();
        assert_eq!(ns.len(), 2, "got {v}");
        assert!(
            ns.contains(&1) && ns.contains(&2) && !ns.contains(&3),
            "got {v}"
        );
    }
}
