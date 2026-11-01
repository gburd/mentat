// Exhaustive lookup ref tests: resolution via unique attributes,
// error cases, usage in transactions and queries.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._test_raises_error(stmt TEXT) RETURNS BOOLEAN
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN false;
             EXCEPTION WHEN OTHERS THEN
                 RETURN true;
             END;
             $$",
        )
        .expect("create helper");
    }

    fn raises_error(sql: &str) -> bool {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<bool>(&format!("SELECT mentat._test_raises_error('{}')", escaped))
            .expect("raises_error call")
            .unwrap_or(false)
    }

    fn setup_lr_schema() {
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"e\" :db/ident :lr/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                {:db/id \"c\" :db/ident :lr/code :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/value}
                {:db/id \"n\" :db/ident :lr/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"v\" :db/ident :lr/val :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/id \"r\" :db/ident :lr/ref :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
                {:db/id \"t\" :db/ident :lr/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
            ]'::TEXT)",
        ).expect("lr schema");
    }

    // ========================================================================
    // Basic lookup ref in transactions
    // ========================================================================

    #[pg_test]
    fn test_lr_add_via_identity_lookup() {
        setup();
        setup_lr_schema();
        Spi::run("SELECT edn_t('[[:db/add \"e\" :lr/email \"alice@test.com\"] [:db/add \"e\" :lr/name \"Alice\"]]'::TEXT)").expect("create");
        Spi::run("SELECT edn_t('[[:db/add [:lr/email \"alice@test.com\"] :lr/val 42]]'::TEXT)")
            .expect("lookup ref add");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :lr/email \"alice@test.com\"] [?e :lr/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_i64().expect("v"), 42);
    }

    #[pg_test]
    fn test_lr_add_via_value_lookup() {
        setup();
        setup_lr_schema();
        Spi::run("SELECT edn_t('[[:db/add \"e\" :lr/code \"C001\"] [:db/add \"e\" :lr/name \"CodeEntity\"]]'::TEXT)").expect("create");
        Spi::run("SELECT edn_t('[[:db/add [:lr/code \"C001\"] :lr/val 99]]'::TEXT)")
            .expect("lookup ref add");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :lr/code \"C001\"] [?e :lr/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_i64().expect("v"), 99);
    }

    // ========================================================================
    // Lookup ref for retraction
    // ========================================================================

    #[pg_test]
    fn test_lr_retract_via_lookup() {
        setup();
        setup_lr_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e\" :lr/email \"retract@test.com\" :lr/name \"Gone\" :lr/val 10}]'::TEXT)").expect("create");
        Spi::run(
            "SELECT edn_t('[[:db/retract [:lr/email \"retract@test.com\"] :lr/val 10]]'::TEXT)",
        )
        .expect("retract via lookup");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :lr/email \"retract@test.com\"] [?e :lr/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(j["result"].is_null());
    }

    // ========================================================================
    // Error cases
    // ========================================================================

    #[pg_test]
    fn test_lr_nonexistent_entity_fails() {
        setup();
        setup_lr_schema();
        assert!(
            raises_error(
                "SELECT edn_t('[[:db/add [:lr/email \"nobody@test.com\"] :lr/val 1]]'::TEXT)"
            ),
            "Lookup ref for nonexistent entity should fail"
        );
    }

    #[pg_test]
    fn test_lr_non_unique_attr_fails() {
        setup();
        setup_lr_schema();
        Spi::run("SELECT edn_t('[[:db/add \"e\" :lr/name \"Test\"]]'::TEXT)").expect("create");
        assert!(
            raises_error("SELECT edn_t('[[:db/add [:lr/name \"Test\"] :lr/val 1]]'::TEXT)"),
            "Lookup ref on non-unique attr should fail"
        );
    }

    // ========================================================================
    // Multiple lookups in one tx
    // ========================================================================

    #[pg_test]
    fn test_lr_multiple_lookups_same_tx() {
        setup();
        setup_lr_schema();
        Spi::run(
            "SELECT edn_t('[
            {:db/id \"e1\" :lr/email \"a@test.com\" :lr/name \"A\"}
            {:db/id \"e2\" :lr/email \"b@test.com\" :lr/name \"B\"}
        ]'::TEXT)",
        )
        .expect("create");

        Spi::run(
            "SELECT edn_t('[
            [:db/add [:lr/email \"a@test.com\"] :lr/val 100]
            [:db/add [:lr/email \"b@test.com\"] :lr/val 200]
        ]'::TEXT)",
        )
        .expect("multi lookup");

        let qa = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :lr/email \"a@test.com\"] [?e :lr/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let ja: serde_json::Value = serde_json::from_str(&qa).expect("parse");
        assert_eq!(ja["result"].as_i64().expect("v"), 100);

        let qb = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :lr/email \"b@test.com\"] [?e :lr/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let jb: serde_json::Value = serde_json::from_str(&qb).expect("parse");
        assert_eq!(jb["result"].as_i64().expect("v"), 200);
    }

    // ========================================================================
    // Lookup ref with update
    // ========================================================================

    #[pg_test]
    fn test_lr_update_via_lookup() {
        setup();
        setup_lr_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e\" :lr/email \"update@test.com\" :lr/val 10}]'::TEXT)")
            .expect("create");
        Spi::run("SELECT edn_t('[[:db/add [:lr/email \"update@test.com\"] :lr/val 20]]'::TEXT)")
            .expect("update");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :lr/email \"update@test.com\"] [?e :lr/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_i64().expect("v"), 20);
    }

    // ========================================================================
    // Lookup ref with cardinality-many
    // ========================================================================

    #[pg_test]
    fn test_lr_add_many_via_lookup() {
        setup();
        setup_lr_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e\" :lr/email \"tags@test.com\" :lr/name \"Tagged\"}]'::TEXT)").expect("create");
        Spi::run(
            "SELECT edn_t('[
            [:db/add [:lr/email \"tags@test.com\"] :lr/tags \"t1\"]
            [:db/add [:lr/email \"tags@test.com\"] :lr/tags \"t2\"]
        ]'::TEXT)",
        )
        .expect("add tags via lookup");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?t ...] :where [?e :lr/email \"tags@test.com\"] [?e :lr/tags ?t]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_array().expect("arr").len(), 2);
    }

    // ========================================================================
    // :in inputs that identify no entity (Refs #374)
    // ========================================================================

    /// The sorted :u/n values `query` finds on `store` for `inputs`.
    fn in_ns(store: &str, query: &str, inputs: &str) -> Vec<i64> {
        let r = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT q('{store}', '{}', '{}'::jsonb)",
            query.replace('\'', "''"),
            inputs.replace('\'', "''")
        ))
        .unwrap_or_else(|e| panic!("{query} {inputs}: {e}"))
        .expect("NULL")
        .0;
        let mut out: Vec<i64> = r["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{r}"))
            .iter()
            .map(|row| row[0].as_i64().unwrap_or_else(|| panic!("{r}")))
            .collect();
        out.sort();
        out
    }

    /// An :in value for an entity that identifies none (a lookup ref nobody
    /// holds, an unknown ident, a string) matched EVERY entity: the
    /// constraint was dropped. It matches nothing, on the default store and
    /// on a named store.
    #[pg_test]
    fn test_lr_in_unresolved_matches_nothing() {
        setup();
        Spi::run("SELECT create_store('lr_in_s')").expect("create_store");
        for store in ["default", "lr_in_s"] {
            let t = |edn: &str| {
                Spi::run(&format!(
                    "SELECT t('{store}', '{}')",
                    edn.replace('\'', "''")
                ))
                .unwrap_or_else(|e| panic!("{store} {edn}: {e}"))
            };
            t("[{:db/ident :u/email :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/identity} \
                {:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one} \
                {:db/ident :u/boss :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]");
            t("[{:db/id \"a\" :u/email \"a@x\" :u/n 1} \
                {:db/id \"b\" :u/email \"b@x\" :u/n 2 :u/boss \"a\"}]");
            let one = "[:find ?n :in $ ?e :where [?e :u/n ?n]]";
            let coll = "[:find ?n :in $ [?e ...] :where [?e :u/n ?n]]";
            let rel = "[:find ?n :in $ [[?e]] :where [?e :u/n ?n]]";
            let boss = "[:find ?n :in $ ?b :where [?e :u/boss ?b] [?e :u/n ?n]]";
            let attr = "[:find ?v :in $ ?a :where [?e :u/email \"a@x\"] [?e ?a ?v]]";
            let none: Vec<i64> = vec![];
            let cases: Vec<(&str, &str, Vec<i64>)> = vec![
                (one, r#"{"inputs": [[":u/email", "a@x"]]}"#, vec![1]),
                (
                    one,
                    r#"{"inputs": [[":u/email", "nobody@x"]]}"#,
                    none.clone(),
                ),
                (one, r#"{"inputs": [[":u/nonesuch", "a@x"]]}"#, none.clone()),
                (one, r#"{"inputs": ["a@x"]}"#, none.clone()),
                (one, r#"{"inputs": [":u/nonesuch"]}"#, none.clone()),
                (
                    coll,
                    r#"{"inputs": [[[":u/email", "nobody@x"]]]}"#,
                    none.clone(),
                ),
                (
                    coll,
                    r#"{"inputs": [[[":u/email", "nobody@x"], [":u/email", "b@x"]]]}"#,
                    vec![2],
                ),
                (rel, r#"{"inputs": [[[[":u/email", "b@x"]]]]}"#, vec![2]),
                (
                    rel,
                    r#"{"inputs": [[[[":u/email", "nobody@x"]]]]}"#,
                    none.clone(),
                ),
                (boss, r#"{"inputs": [[":u/email", "a@x"]]}"#, vec![2]),
                (
                    boss,
                    r#"{"inputs": [[":u/email", "nobody@x"]]}"#,
                    none.clone(),
                ),
                // An attribute by its ident; an unknown one names nothing.
                (attr, r#"{"inputs": [":u/n"]}"#, vec![1]),
                (attr, r#"{"inputs": [":u/nonesuch"]}"#, none.clone()),
            ];
            for (query, inputs, want) in cases {
                assert_eq!(
                    in_ns(store, query, inputs),
                    want,
                    "{store}: {query} {inputs}"
                );
            }
        }
        Spi::run("SELECT drop_store('lr_in_s')").expect("drop_store");
    }
}
