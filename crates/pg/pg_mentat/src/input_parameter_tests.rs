// Input parameter tests for queries: binding variables from external inputs.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
    }

    fn setup_ip_schema_and_data() {
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"n\" :db/ident :ip/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"v\" :db/ident :ip/val :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/id \"d\" :db/ident :ip/dept :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"f\" :db/ident :ip/flag :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        ).expect("ip schema");

        Spi::run(
            "SELECT edn_t('[
                {:db/id \"e1\" :ip/name \"Alice\" :ip/val 100 :ip/dept \"Eng\" :ip/flag true}
                {:db/id \"e2\" :ip/name \"Bob\" :ip/val 200 :ip/dept \"Eng\" :ip/flag false}
                {:db/id \"e3\" :ip/name \"Carol\" :ip/val 150 :ip/dept \"Design\" :ip/flag true}
                {:db/id \"e4\" :ip/name \"Dave\" :ip/val 300 :ip/dept \"Product\" :ip/flag true}
            ]'::TEXT)",
        )
        .expect("ip data");
    }

    // ========================================================================
    // JSON input parameters
    // ========================================================================

    #[pg_test]
    fn test_ip_empty_params() {
        setup();
        setup_ip_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?n ...] :where [?e :ip/name ?n]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_array().expect("arr").len(), 4);
    }

    // ========================================================================
    // Queries with constant bindings (as params)
    // ========================================================================

    #[pg_test]
    fn test_ip_constant_string_in_where() {
        setup();
        setup_ip_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :ip/name \"Alice\"] [?e :ip/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_i64().expect("v"), 100);
    }

    #[pg_test]
    fn test_ip_constant_long_in_where() {
        setup();
        setup_ip_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?n . :where [?e :ip/name ?n] [?e :ip/val 200]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_str().expect("n"), "Bob");
    }

    #[pg_test]
    fn test_ip_constant_boolean_in_where() {
        setup();
        setup_ip_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?n ...] :where [?e :ip/name ?n] [?e :ip/flag false]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let names = j["result"].as_array().expect("arr");
        assert_eq!(names.len(), 1);
        assert_eq!(names[0].as_str().expect("n"), "Bob");
    }

    // ========================================================================
    // Parameterized queries via dynamic SQL
    // ========================================================================

    #[pg_test]
    fn test_ip_dynamic_name_filter() {
        setup();
        setup_ip_schema_and_data();
        for name in &["Alice", "Bob", "Carol", "Dave"] {
            let q = Spi::get_one::<String>(&format!(
                "SELECT edn_q('[:find ?v . :where [?e :ip/name \"{}\"] [?e :ip/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
                name
            )).expect("q").expect("NULL");
            let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
            assert!(
                j["result"].as_i64().is_some(),
                "Should find val for {}",
                name
            );
        }
    }

    #[pg_test]
    fn test_ip_dynamic_dept_filter() {
        setup();
        setup_ip_schema_and_data();
        let dept_counts = vec![("Eng", 2), ("Design", 1), ("Product", 1)];
        for (dept, expected) in dept_counts {
            let q = Spi::get_one::<String>(&format!(
                "SELECT edn_q('[:find [?n ...] :where [?e :ip/name ?n] [?e :ip/dept \"{}\"]]'::TEXT, '{{}}'::jsonb)::TEXT",
                dept
            )).expect("q").expect("NULL");
            let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
            assert_eq!(
                j["result"].as_array().expect("arr").len(),
                expected,
                "Dept {} should have {} people",
                dept,
                expected
            );
        }
    }

    #[pg_test]
    fn test_ip_dynamic_val_threshold() {
        setup();
        setup_ip_schema_and_data();
        let thresholds = vec![(0, 4), (100, 4), (150, 3), (200, 2), (300, 1), (301, 0)];
        for (threshold, expected) in thresholds {
            let q = Spi::get_one::<String>(&format!(
                "SELECT edn_q('[:find [?n ...] :where [?e :ip/name ?n] [?e :ip/val ?v] [(>= ?v {})]]'::TEXT, '{{}}'::jsonb)::TEXT",
                threshold
            )).expect("q").expect("NULL");
            let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
            assert_eq!(
                j["result"].as_array().expect("arr").len(),
                expected,
                "Threshold {} should match {} entities",
                threshold,
                expected
            );
        }
    }

    // ========================================================================
    // Entity ID as parameter
    // ========================================================================

    #[pg_test]
    fn test_ip_entity_id_parameter() {
        setup();
        setup_ip_schema_and_data();
        // Get Alice's entity ID
        let q1 = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?e . :where [?e :ip/name \"Alice\"]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j1: serde_json::Value = serde_json::from_str(&q1).expect("parse");
        let eid = j1["result"].as_i64().expect("eid");

        // Use entity ID to query
        let q2 = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?n ?v :where [{} :ip/name ?n] [{} :ip/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid, eid
        )).expect("q").expect("NULL");
        let j2: serde_json::Value = serde_json::from_str(&q2).expect("parse");
        let results = j2["results"].as_array().expect("arr");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0][0].as_str().expect("n"), "Alice");
        assert_eq!(results[0][1].as_i64().expect("v"), 100);
    }

    // ========================================================================
    // :in bindings must all be supplied (Datomic / embedded semantics)
    // ========================================================================

    fn q_err(query: &str, inputs: serde_json::Value) -> String {
        crate::functions::query::mentat_query(query, pgrx::JsonB(inputs))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_else(|| panic!("{query} should fail"))
    }

    #[pg_test]
    fn test_ip_unbound_in_var_is_an_error() {
        setup();
        setup_ip_schema_and_data();
        let q = "[:find ?n :in $ ?n :where [?e :ip/name ?n]]";
        for opts in [serde_json::json!({}), serde_json::json!({"inputs": []})] {
            let e = q_err(q, opts);
            assert!(
                e.contains(
                    ":in ?n declared but no input value was given (expected 1 inputs, got 0)"
                ),
                "{e}"
            );
        }
        // The first missing form is named, whatever its shape.
        let e = q_err(
            "[:find ?n :in $ ?d [?n ...] :where [?e :ip/dept ?d] [?e :ip/name ?n]]",
            serde_json::json!({"inputs": ["Eng"]}),
        );
        assert!(
            e.contains(":in [?n ...] declared") && e.contains("expected 2 inputs, got 1"),
            "{e}"
        );
        // Too many is as wrong as too few.
        let e = q_err(q, serde_json::json!({"inputs": ["Bob", "Carol"]}));
        assert!(e.contains("1 :in binding(s) but \"inputs\" has 2"), "{e}");
        // Supplied, it filters.
        let r = crate::functions::query::mentat_query(
            q,
            pgrx::JsonB(serde_json::json!({"inputs": ["Bob"]})),
        )
        .expect("bound query");
        assert_eq!(r.0["results"], serde_json::json!([["Bob"]]));
    }

    // ========================================================================
    // An integer input in a ref attribute's value place is an entid
    // ========================================================================

    #[pg_test]
    fn test_ip_ref_value_input_is_an_entid() {
        setup();
        setup_ip_schema_and_data();
        Spi::run(
            "SELECT edn_t('[{:db/ident :ip/friend :db/valueType :db.type/ref \
                               :db/cardinality :db.cardinality/many}]'::TEXT)",
        )
        .expect("ref schema");
        let q = |query: &str, inputs: serde_json::Value| {
            crate::functions::query::mentat_query(query, pgrx::JsonB(inputs))
                .expect(query)
                .0
        };
        let eid = |name: &str| {
            q(
                &format!("[:find ?e . :where [?e :ip/name \"{name}\"]]"),
                serde_json::json!({}),
            )["result"]
                .as_i64()
                .expect("eid")
        };
        let (alice, bob, carol, dave) = (eid("Alice"), eid("Bob"), eid("Carol"), eid("Dave"));
        // Cardinality-many: Alice has two friends, Dave befriends Bob too.
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {alice} :ip/friend {bob}] [:db/add {alice} :ip/friend {carol}] \
                            [:db/add {dave} :ip/friend {bob}]]'::TEXT)"
        ))
        .expect("friends");

        let who = |query: &str, inputs: serde_json::Value| {
            let mut v: Vec<String> = q(query, inputs)["results"]
                .as_array()
                .expect("rel")
                .iter()
                .map(|r| r[0].as_str().expect("name").to_string())
                .collect();
            v.sort();
            v
        };
        // Scalar.
        let scalar = "[:find ?n :in $ ?f :where [?e :ip/friend ?f] [?e :ip/name ?n]]";
        assert_eq!(
            who(scalar, serde_json::json!({"inputs": [bob]})),
            ["Alice", "Dave"]
        );
        assert_eq!(
            who(scalar, serde_json::json!({"inputs": [carol]})),
            ["Alice"]
        );
        // An entid that is nobody's friend is no match, not an error.
        assert!(who(scalar, serde_json::json!({"inputs": [alice]})).is_empty());
        // Scalar ref mixed with a collection of longs (the model test's shape).
        assert_eq!(
            q(
                "[:find ?n . :in $ ?f [?v ...] :where [?e :ip/friend ?f] [?e :ip/val ?v] [?e :ip/name ?n]]",
                serde_json::json!({"inputs": [carol, [100, 101]]}),
            )["result"],
            "Alice"
        );
        // Collection, tuple and relation forms of a ref value.
        assert_eq!(
            who(
                "[:find ?n :in $ [?f ...] :where [?e :ip/friend ?f] [?e :ip/name ?n]]",
                serde_json::json!({"inputs": [[carol, 999_999_999]]}),
            ),
            ["Alice"]
        );
        assert_eq!(
            who(
                "[:find ?n :in $ [?f ?v] :where [?e :ip/friend ?f] [?e :ip/val ?v] [?e :ip/name ?n]]",
                serde_json::json!({"inputs": [[bob, 300]]}),
            ),
            ["Dave"]
        );
        assert_eq!(
            who(
                "[:find ?n :in $ [[?f ?v]] :where [?e :ip/friend ?f] [?e :ip/val ?v] [?e :ip/name ?n]]",
                serde_json::json!({"inputs": [[[bob, 300], [carol, 100], [carol, 300]]]}),
            ),
            ["Alice", "Dave"]
        );
    }
}
