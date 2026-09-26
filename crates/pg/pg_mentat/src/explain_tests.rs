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
            "SELECT mentat_transact('[
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
}
