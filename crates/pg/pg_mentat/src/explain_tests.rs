// Regression tests for mentat_explain / mentat_explain_store.
//
// mentat_explain runs `EXPLAIN` on the generated SQL through SPI. Before the
// fix it used a read-only SPI connection, so PostgreSQL rejected the EXPLAIN
// with "EXPLAIN is not allowed in a non-volatile function" even though the
// function is declared VOLATILE. No test exercised it end to end, so the
// regression shipped and was only caught by the Phase 2 benchmark. These tests
// call the function for real and assert it returns a plan.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;
    use pgrx::JsonB;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"n\" :db/ident :ex/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
             ]'::TEXT)",
        )
        .expect("explain schema");
    }

    #[pg_test]
    fn test_explain_returns_a_plan_not_an_error() {
        setup();
        // The bug made this raise; assert it comes back with a plan instead.
        let plan: Option<JsonB> = Spi::get_one(
            "SELECT mentat_explain('[:find ?e :where [?e :ex/name ?n]]', '{}'::jsonb)",
        )
        .expect("mentat_explain must not error");
        let plan = plan.expect("mentat_explain returned NULL");
        let obj = plan.0.as_object().expect("explain result is a JSON object");
        // The EXPLAIN output must be present and non-empty.
        let explain_plan = obj
            .get("explain_plan")
            .and_then(|v| v.as_str())
            .expect("explain_plan key");
        assert!(
            explain_plan.contains("Scan") || explain_plan.contains("cost="),
            "explain_plan should carry a real plan, got: {explain_plan}"
        );
        // And the generated SQL round-trips into the result.
        assert!(obj.get("generated_sql").is_some(), "generated_sql key");
    }

    // Issue #330: mentat_explain and mentat_query_sql must honour collection
    // `[?x ...]` :in inputs. Before the fix they compiled the query with the
    // collection silently dropped, so the SQL (and its plan) had no filter on
    // ?n and misrepresented what edn_q runs. Now the inputs flow through the
    // same enriched-binding path as edn_q: the collection reaches the SQL as a
    // bound array, and omitting it is the same clear error edn_q gives.
    #[pg_test]
    fn test_explain_and_query_sql_honor_collection_in() {
        setup();
        let q = "[:find ?e :in [?n ...] :where [?e :ex/name ?n]]";
        let with = r#"{"inputs": [["alpha", "beta"]]}"#;

        let call = |f: &str, key: &str| -> String {
            let j: Option<JsonB> =
                Spi::get_one(&format!("SELECT {f}('{q}', '{with}'::jsonb)")).expect(f);
            j.expect("NULL").0[key].as_str().expect(key).to_string()
        };
        // The collection is applied: the SQL filters ?n against the bound
        // array (= ANY), which the dropped-input SQL never contained.
        let sql = call("mentat_query_sql", "sql");
        assert!(
            sql.contains("ANY"),
            "query_sql must apply the collection: {sql}"
        );
        let gen = call("mentat_explain", "generated_sql");
        assert!(
            gen.contains("ANY"),
            "explain must plan the collection: {gen}"
        );

        // Omitting the declared input is an error, exactly as in edn_q (it used
        // to silently produce unfiltered SQL).
        // (An ERROR inside a pgrx test aborts it, so trap it in plpgsql.)
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._ex_raises(stmt TEXT) RETURNS BOOLEAN
             LANGUAGE plpgsql AS $$
             BEGIN EXECUTE stmt; RETURN false;
             EXCEPTION WHEN OTHERS THEN RETURN true; END; $$",
        )
        .expect("helper");
        let raises = |f: &str| -> bool {
            let stmt = format!("SELECT {f}('{q}', '{{}}'::jsonb)").replace('\'', "''");
            Spi::get_one::<bool>(&format!("SELECT mentat._ex_raises('{stmt}')"))
                .expect("raises")
                .unwrap_or(false)
        };
        assert!(
            raises("mentat_query_sql"),
            "query_sql must refuse a missing :in"
        );
        assert!(
            raises("mentat_explain"),
            "explain must refuse a missing :in"
        );

        // And it agrees with what edn_q actually returns: no ?n match -> 0 rows.
        let rows = Spi::get_one::<JsonB>(&format!("SELECT edn_q('{q}', '{with}'::jsonb)"))
            .expect("edn_q")
            .expect("NULL");
        assert_eq!(rows.0["result"].as_array().map(|a| a.len()), Some(0));
    }
}
