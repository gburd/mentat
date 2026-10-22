// Rule head variables bound to values of every type, and rule invocations
// joined to the rest of the query (1.18.0).
//
// Before 1.18.0 a rule CTE carried one BIGINT column per head argument, so a
// head variable bound to a non-ref value came back NULL.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
    }

    fn tx(edn: &str) -> serde_json::Value {
        let r = Spi::get_one_with_args::<String>(
            "SELECT edn_t($1)::TEXT",
            &[pgrx::datum::DatumWithOid::from(edn)],
        )
        .expect(edn)
        .expect("NULL tx report");
        serde_json::from_str(&r).expect("parse tx report")
    }

    /// The rows of a FindRel result, sorted, as compact JSON strings.
    fn rows(query: &str) -> Vec<String> {
        let mut v: Vec<String> =
            crate::functions::query::mentat_query(query, pgrx::JsonB(serde_json::json!({})))
                .expect(query)
                .0["results"]
                .as_array()
                .expect("rel")
                .iter()
                .map(|r| r.to_string())
                .collect();
        v.sort();
        v
    }

    fn sorted(mut v: Vec<String>) -> Vec<String> {
        v.sort();
        v
    }

    /// One entity per value type; returns (entity, other entity it refs).
    fn setup_types() -> (i64, i64) {
        setup();
        tx("[{:db/ident :rv/str :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
             {:db/ident :rv/lng :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
             {:db/ident :rv/dbl :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
             {:db/ident :rv/bool :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
             {:db/ident :rv/kw :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
             {:db/ident :rv/inst :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}
             {:db/ident :rv/uuid :db/valueType :db.type/uuid :db/cardinality :db.cardinality/one}
             {:db/ident :rv/ref :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
             {:db/ident :rv/bytes :db/valueType :db.type/bytes :db/cardinality :db.cardinality/one}]");
        let j = tx("[{:db/id \"o\" :rv/str \"other\"}
              {:db/id \"e\" :rv/str \"hi\" :rv/lng 42 :rv/dbl 2.5 :rv/bool false :rv/kw :rv/k
               :rv/inst #inst \"2024-01-15T10:30:00Z\"
               :rv/uuid #uuid \"550e8400-e29b-41d4-a716-446655440000\"
               :rv/ref \"o\" :rv/bytes #bytes 0a0b}]");
        (
            j["tempids"]["e"].as_i64().expect("e"),
            j["tempids"]["o"].as_i64().expect("o"),
        )
    }

    /// A rule head variable bound to a value of each type returns that
    /// value, decoded as a plain pattern decodes it.
    #[pg_test]
    fn test_rule_head_value_of_every_type() {
        let (e, o) = setup_types();
        for (attr, want) in [
            ("str", "\"hi\"".to_string()),
            ("lng", "42".to_string()),
            ("dbl", "2.5".to_string()),
            ("bool", "false".to_string()),
            ("kw", "\":rv/k\"".to_string()),
            ("inst", "\"2024-01-15T10:30:00.000000Z\"".to_string()),
            (
                "uuid",
                "\"550e8400-e29b-41d4-a716-446655440000\"".to_string(),
            ),
            ("ref", o.to_string()),
            ("bytes", "\"0a0b\"".to_string()),
        ] {
            let plain = format!("[:find ?e ?v :where [?e :rv/{attr} ?v] [(= ?e {e})]]");
            let ruled = format!(
                "[:find ?e ?v :rules [[(has ?e ?v) [?e :rv/{attr} ?v]]] \
                 :where (has ?e ?v) [(= ?e {e})]]"
            );
            assert_eq!(rows(&plain), [format!("[{e},{want}]")], "{plain}");
            assert_eq!(rows(&ruled), rows(&plain), "{ruled}");
        }
    }

    /// The reported query: a rule of an entity and its string name.
    #[pg_test]
    fn test_rule_head_string_value() {
        setup();
        tx("[{:db/ident :rv/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]");
        let j = tx("[{:db/id \"a\" :rv/name \"ann\"} {:db/id \"b\" :rv/name \"bob\"}]");
        let (a, b) = (
            j["tempids"]["a"].as_i64().unwrap(),
            j["tempids"]["b"].as_i64().unwrap(),
        );
        let q = "[:find ?e ?n :rules [[(named ?e ?n) [?e :rv/name ?n]]] :where (named ?e ?n)]";
        assert_eq!(
            rows(q),
            sorted(vec![format!("[{a},\"ann\"]"), format!("[{b},\"bob\"]")])
        );
        // A rule value joins a later pattern's value, and a predicate reads
        // it in its own type.
        let q = "[:find ?x :rules [[(named ?e ?n) [?e :rv/name ?n]]] \
                 :where (named ?e ?n) [?x :rv/name ?n] [(> ?n \"b\")]]";
        assert_eq!(rows(q), [format!("[{b}]")]);
    }

    /// A head variable bound to a different type in each rule body keeps
    /// each value's type, as a variable-attribute pattern `[?e _ ?v]` does.
    #[pg_test]
    fn test_rule_head_mixed_types_across_bodies() {
        let (e, o) = setup_types();
        let q = format!(
            "[:find ?v :rules [[(val ?e ?v) [?e :rv/str ?v]] [(val ?e ?v) [?e :rv/lng ?v]] \
                               [(val ?e ?v) [?e :rv/dbl ?v]] [(val ?e ?v) [?e :rv/bool ?v]] \
                               [(val ?e ?v) [?e :rv/ref ?v]]] \
             :where (val ?e ?v) [(= ?e {e})]]"
        );
        assert_eq!(
            rows(&q),
            sorted(vec![
                "[\"hi\"]".into(),
                "[42]".into(),
                "[2.5]".into(),
                "[false]".into(),
                format!("[{o}]"),
            ])
        );
    }

    /// Head variables bound to an entity, an `added` flag and a function
    /// result.
    #[pg_test]
    fn test_rule_head_entity_flag_and_function() {
        let (e, _o) = setup_types();
        let q = format!(
            "[:find ?e ?d :rules [[(dbl ?e ?d) [?e :rv/lng ?n] [(* ?n 2) ?d]]] \
             :where (dbl ?e ?d)]"
        );
        assert_eq!(rows(&q), [format!("[{e},84]")]);
        let q = "[:find ?e ?op :rules [[(seen ?e ?op) [?e :rv/lng _ _ ?op]]] :where (seen ?e ?op)]";
        assert_eq!(rows(q), [format!("[{e},true]")]);
        // An entity id head variable joins a later pattern's entity.
        let q = "[:find ?s :rules [[(longy ?e) [?e :rv/lng _]]] :where (longy ?e) [?e :rv/str ?s]]";
        assert_eq!(rows(q), ["[\"hi\"]"]);
    }

    /// Recursive rules (transitive closure) still work, right or left
    /// recursive, and carry a value through the recursion.
    #[pg_test]
    fn test_rule_recursive_carries_values() {
        setup();
        tx("[{:db/ident :rv/boss :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
             {:db/ident :rv/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]");
        // d -> c -> b -> a
        let j = tx("[{:db/id \"a\" :rv/name \"a\"} {:db/id \"b\" :rv/name \"b\" :rv/boss \"a\"}
                     {:db/id \"c\" :rv/name \"c\" :rv/boss \"b\"} {:db/id \"d\" :rv/name \"d\" :rv/boss \"c\"}]");
        let id = |k: &str| j["tempids"][k].as_i64().unwrap();
        let (a, b, c, d) = (id("a"), id("b"), id("c"), id("d"));
        let closure = sorted(vec![
            format!("[{b},{a}]"),
            format!("[{c},{a}]"),
            format!("[{c},{b}]"),
            format!("[{d},{a}]"),
            format!("[{d},{b}]"),
            format!("[{d},{c}]"),
        ]);
        for q in [
            "[:find ?e ?b :rules [[(up ?e ?b) [?e :rv/boss ?b]] \
                                 [(up ?e ?b) [?e :rv/boss ?x] (up ?x ?b)]] :where (up ?e ?b)]",
            // Recursive call before the pattern (left recursion).
            "[:find ?e ?b :rules [[(up ?e ?b) [?e :rv/boss ?b]] \
                                 [(up ?e ?b) (up ?e ?x) [?x :rv/boss ?b]]] :where (up ?e ?b)]",
        ] {
            assert_eq!(rows(q), closure, "{q}");
        }
        // A string value carried through the recursion: every boss's name.
        let q = "[:find ?e ?n :rules [[(boss-name ?e ?n) [?e :rv/boss ?b] [?b :rv/name ?n]] \
                                     [(boss-name ?e ?n) [?e :rv/boss ?x] (boss-name ?x ?n)]] \
                 :where (boss-name ?e ?n) [?e :rv/name \"d\"]]";
        assert_eq!(
            rows(q),
            sorted(vec![
                format!("[{d},\"a\"]"),
                format!("[{d},\"b\"]"),
                format!("[{d},\"c\"]"),
            ])
        );
    }
}
