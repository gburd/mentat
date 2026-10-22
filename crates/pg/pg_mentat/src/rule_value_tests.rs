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

    /// Recursive rules (transitive closure) work right or left recursive,
    /// with the recursive clause first or last, and carry a value through
    /// the recursion.
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
            // The recursive clause listed first (a SQL error before 1.18.0).
            "[:find ?e ?b :rules [[(up ?e ?b) [?e :rv/boss ?x] (up ?x ?b)] \
                                 [(up ?e ?b) [?e :rv/boss ?b]]] :where (up ?e ?b)]",
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

    /// ann (30) <- bob (12) <- cy (5) <- dee (40), by :rv/boss.
    fn setup_chain() -> [i64; 4] {
        setup();
        tx("[{:db/ident :rv/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
             {:db/ident :rv/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
             {:db/ident :rv/role :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
             {:db/ident :rv/boss :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]");
        let j = tx(
            "[{:db/id \"a\" :db/ident :rv/ann :rv/name \"ann\" :rv/age 30 :rv/role :rv/lead}
                     {:db/id \"b\" :rv/name \"bob\" :rv/age 12 :rv/boss \"a\" :rv/role :rv/dev}
                     {:db/id \"c\" :rv/name \"cy\" :rv/age 5 :rv/boss \"b\" :rv/role :rv/dev}
                     {:db/id \"d\" :rv/name \"dee\" :rv/age 40 :rv/boss \"c\"}]",
        );
        let id = |k: &str| j["tempids"][k].as_i64().unwrap();
        [id("a"), id("b"), id("c"), id("d")]
    }

    const UP: &str = "[[(up ?e ?b) [?e :rv/boss ?b]] [(up ?e ?b) [?e :rv/boss ?x] (up ?x ?b)]]";

    /// Two invocations sharing a variable join on it (they were a cross
    /// product: `(adult ?p) (kid ?p)` returned the kids).
    #[pg_test]
    fn test_rule_invocations_join_on_shared_variable() {
        let [a, _b, _c, d] = setup_chain();
        let rules = "[[(adult ?p) [?p :rv/age ?n] [(>= ?n 18)]] \
                      [(kid ?p) [?p :rv/age ?n] [(< ?n 18)]] \
                      [(lead ?p) [?p :rv/role :rv/lead]]]";
        assert!(rows(&format!(
            "[:find ?p :rules {rules} :where (adult ?p) (kid ?p)]"
        ))
        .is_empty());
        assert_eq!(
            rows(&format!(
                "[:find ?p :rules {rules} :where (adult ?p) (lead ?p)]"
            )),
            [format!("[{a}]")]
        );
        // A shared variable in an or arm, next to a top-level invocation.
        assert_eq!(
            rows(&format!(
                "[:find ?p :rules {rules} :where (adult ?p) (or (kid ?p) (lead ?p))]"
            )),
            [format!("[{a}]")]
        );
        // Recursive: everyone above dee who is an adult.
        let q = "[:find ?b :rules [[(up ?e ?b) [?e :rv/boss ?b]] \
                                   [(up ?e ?b) [?e :rv/boss ?x] (up ?x ?b)] \
                                   [(adult ?p) [?p :rv/age ?n] [(>= ?n 18)]]] \
                 :where [?e :rv/name \"dee\"] (up ?e ?b) (adult ?b)]";
        assert_eq!(rows(q), [format!("[{a}]")]);
        let _ = d;
    }

    /// A constant rule argument filters the rule (it was ignored).
    #[pg_test]
    fn test_rule_constant_arguments() {
        let [a, b, c, d] = setup_chain();
        let named = "[[(named ?e ?n) [?e :rv/name ?n]]]";
        assert_eq!(
            rows(&format!(
                "[:find ?e :rules {named} :where (named ?e \"bob\")]"
            )),
            [format!("[{b}]")]
        );
        let aged = "[[(aged ?e ?n) [?e :rv/age ?n]]]";
        assert_eq!(
            rows(&format!("[:find ?e :rules {aged} :where (aged ?e 5)]")),
            [format!("[{c}]")]
        );
        let role = "[[(role ?e ?r) [?e :rv/role ?r]]]";
        assert_eq!(
            rows(&format!(
                "[:find ?e :rules {role} :where (role ?e :rv/lead)]"
            )),
            [format!("[{a}]")]
        );
        // An entity argument: an entid, or an ident, of a recursive rule.
        let under_ann = sorted(vec![format!("[{b}]"), format!("[{c}]"), format!("[{d}]")]);
        assert_eq!(
            rows(&format!("[:find ?e :rules {UP} :where (up ?e {a})]")),
            under_ann
        );
        assert_eq!(
            rows(&format!("[:find ?e :rules {UP} :where (up ?e :rv/ann)]")),
            under_ann
        );
        assert_eq!(
            rows(&format!("[:find ?b :rules {UP} :where (up {d} ?b)]")),
            sorted(vec![format!("[{a}]"), format!("[{b}]"), format!("[{c}]")])
        );
        // A constant argument of the recursive call in a rule body filters
        // the recursion: (r ?t ann) is only bob, so dee gets bob, not cy too.
        let q = format!(
            "[:find ?t :rules [[(r ?e ?t) [?e :rv/boss ?t]] \
                              [(r ?e ?t) [?e :rv/age 40] (r ?t {a})]] :where (r {d} ?t)]"
        );
        assert_eq!(rows(&q), sorted(vec![format!("[{b}]"), format!("[{c}]")]));
    }

    /// The same rule invoked twice is two rows of its CTE (it was one, so
    /// `(boss ?x ?y) (boss ?y ?z)` returned one-hop pairs).
    #[pg_test]
    fn test_rule_invoked_twice() {
        let [a, b, c, d] = setup_chain();
        let boss = "[[(boss ?e ?b) [?e :rv/boss ?b]]]";
        assert_eq!(
            rows(&format!(
                "[:find ?x ?z :rules {boss} :where (boss ?x ?y) (boss ?y ?z)]"
            )),
            sorted(vec![format!("[{c},{a}]"), format!("[{d},{b}]")])
        );
        // A recursive rule invoked twice, joined on ?y: x is above y is above z.
        assert_eq!(
            rows(&format!(
                "[:find ?x ?z :rules {UP} :where (up ?x ?y) (up ?y ?z)]"
            )),
            sorted(vec![
                format!("[{c},{a}]"),
                format!("[{d},{a}]"),
                format!("[{d},{b}]"),
            ])
        );
        // Twice with no shared variable is a cross product of the two.
        assert_eq!(
            rows(&format!(
                "[:find ?x ?z :rules {boss} :where (boss ?x {a}) (boss ?z {b})]"
            )),
            [format!("[{b},{c}]")]
        );
    }

    /// A rule with several bodies (an or), joined to patterns.
    #[pg_test]
    fn test_rule_multiple_bodies() {
        let [_a, _b, c, d] = setup_chain();
        // dev-or-old: bob, cy (devs), dee (40). Whose boss has a boss: not
        // bob (ann has none).
        let q = "[:find ?e :rules [[(dev-or-old ?e) [?e :rv/role :rv/dev]] \
                                   [(dev-or-old ?e) [?e :rv/age ?n] [(> ?n 35)]]] \
                 :where (dev-or-old ?e) [?e :rv/boss ?x] [?x :rv/boss _]]";
        assert_eq!(rows(q), sorted(vec![format!("[{c}]"), format!("[{d}]")]));
    }

    /// Invoking a rule the query doesn't define is an error, also when it
    /// defines no rules (that invocation was ignored, matching every row).
    #[pg_test]
    fn test_rule_undefined_is_an_error() {
        setup_chain();
        for q in [
            "[:find ?e :where [?e :rv/name _] (nope ?e)]",
            "[:find ?e :rules [[(boss ?e ?b) [?e :rv/boss ?b]]] :where [?e :rv/name _] (nope ?e)]",
        ] {
            let r = crate::functions::query::mentat_query(q, pgrx::JsonB(serde_json::json!({})));
            let err = r.err().map(|e| e.to_string()).unwrap_or_default();
            assert!(err.contains("rule-not-found"), "{q}: {err}");
        }
    }

    fn query_with(query: &str, inputs: serde_json::Value) -> serde_json::Value {
        crate::functions::query::mentat_query(query, pgrx::JsonB(inputs))
            .expect(query)
            .0
    }

    /// A non-recursive rule invoked twice is inlined, not materialized, so
    /// the query's own filters reach its datoms: a plan with no CTE Scan.
    /// (Materialized, a 200k-entity rule invoked twice ran 33x slower.) A
    /// recursive rule stays a CTE.
    #[pg_test]
    fn test_rule_non_recursive_is_inlined() {
        let [_a, b, _c, _d] = setup_chain();
        let plan = |q: &str| -> String {
            Spi::get_one::<String>(&format!(
                "SELECT mentat_explain($q${q}$q$, '{{}}'::jsonb)->>'explain_plan'"
            ))
            .expect("explain")
            .expect("NULL plan")
        };
        let q = "[:find ?e :rules [[(aged ?e ?n) [?e :rv/age ?n]]] \
                 :where [?e :rv/name \"bob\"] (aged ?e ?n) (aged ?e ?m)]";
        let p = plan(q);
        assert!(!p.contains("CTE Scan"), "{q}:\n{p}");
        assert_eq!(rows(q), [format!("[{b}]")]);
        let q = format!("[:find ?e :rules {UP} :where (up ?e ?b) (up ?b ?c)]");
        assert!(plan(&q).contains("CTE Scan"), "{q}");
    }

    /// A rule column of one known type is stored as that one typed column,
    /// so a recursive rule's rows are as narrow as a plain CTE's (they were
    /// 10 columns per argument, doubling the temp files of a transitive
    /// closure); a column of mixed types keeps the type tag and every value
    /// column. Both still answer.
    #[pg_test]
    fn test_rule_typed_columns_are_narrow() {
        let [a, b, c, d] = setup_chain();
        let sql = |q: &str| -> String {
            Spi::get_one::<String>(&format!(
                "SELECT mentat_explain($q${q}$q$, '{{}}'::jsonb)->>'generated_sql'"
            ))
            .expect("explain")
            .expect("NULL sql")
        };
        let q = format!("[:find ?e ?b :rules {UP} :where (up ?e ?b)]");
        assert!(
            sql(&q).contains("up(col0_v_ref, col1_v_ref) AS ("),
            "{}",
            sql(&q)
        );
        assert_eq!(rows(&q).len(), 6);
        let q =
            "[:find ?e ?v :rules [[(val ?e ?v) [?e :rv/age ?v]] [(val ?e ?v) [?e :rv/name ?v]]] \
                 :where (val ?e ?v) [?e :rv/name \"bob\"]]";
        let s = sql(q);
        assert!(
            s.contains("(col0_v_ref, col1_value_type_tag, col1_v_ref, col1_v_bool, col1_v_long"),
            "{s}"
        );
        assert_eq!(
            rows(q),
            sorted(vec![format!("[{b},12]"), format!("[{b},\"bob\"]")])
        );
        // A typed string column carried through a recursion, joined to a pattern.
        let q = "[:find ?n :rules [[(boss-name ?e ?n) [?e :rv/boss ?b] [?b :rv/name ?n]] \
                                   [(boss-name ?e ?n) [?e :rv/boss ?x] (boss-name ?x ?n)]] \
                 :where [?e :rv/name \"dee\"] (boss-name ?e ?n) [?o :rv/name ?n] [?o :rv/age 30]]";
        assert_eq!(rows(q), ["[\"ann\"]"]);
        let _ = (a, c, d);
    }

    /// A rule column of longs and doubles compares, orders and aggregates
    /// numerically: `[(> ?v 6)]` and `[(= ?v 5)]` were SQL errors ("text >
    /// bigint"), min/max/:order were text order (max 7.5 of 40) and sum an
    /// error. A column of other mixed types compares without a SQL error.
    #[pg_test]
    fn test_rule_long_and_double_column_is_numeric() {
        let [a, b, c, d] = setup_chain();
        tx("[{:db/ident :rv/score :db/valueType :db.type/double :db/cardinality :db.cardinality/one}]");
        tx(&format!(
            "[{{:db/id {a} :rv/score 2.5}} {{:db/id {b} :rv/score 7.5}} {{:db/id {c} :rv/score 5.0}}]"
        ));
        // ages ann 30, bob 12, cy 5, dee 40; scores ann 2.5, bob 7.5, cy 5.0.
        let num = "[[(num ?e ?v) [?e :rv/age ?v]] [(num ?e ?v) [?e :rv/score ?v]]]";
        let q = |body: &str| format!("[:find ?e ?v :rules {num} :where (num ?e ?v) {body}]");
        assert_eq!(
            rows(&q("[(> ?v 6)]")),
            sorted(vec![
                format!("[{a},30]"),
                format!("[{b},12]"),
                format!("[{b},7.5]"),
                format!("[{d},40]"),
            ])
        );
        // 5 (cy's age) and 5.0 (cy's score) are both 5, as on the embedded engine.
        assert_eq!(
            rows(&q("[(= ?v 5)]")),
            sorted(vec![format!("[{c},5]"), format!("[{c},5.0]")])
        );
        assert_eq!(rows(&q("[(<= ?v 2.5)]")), [format!("[{a},2.5]")]);
        assert_eq!(
            rows(&q("[(!= ?v 5)] [?e :rv/name \"cy\"]")),
            Vec::<String>::new()
        );
        let k = format!("[:find ?e :in ?k :rules {num} :where (num ?e ?v) [(> ?v ?k)]]");
        assert_eq!(
            query_with(&k, serde_json::json!({"inputs": [35]}))["results"],
            serde_json::json!([[d]])
        );
        let agg = |f: &str| {
            query_with(
                &format!("[:find ({f} ?v) . :rules {num} :where (num ?e ?v)]"),
                serde_json::json!({}),
            )["result"]
                .as_f64()
                .unwrap_or(f64::NAN)
        };
        assert_eq!(agg("max"), 40.0);
        assert_eq!(agg("min"), 2.5);
        assert_eq!(agg("sum"), 30.0 + 12.0 + 5.0 + 40.0 + 2.5 + 7.5 + 5.0);
        assert!((agg("avg") - 102.0 / 7.0).abs() < 1e-9);
        let ordered = query_with(
            &format!(
                "[:find ?v :order (desc ?v) :rules {num} :where (num ?e ?v) [?e :rv/name \"bob\"]]"
            ),
            serde_json::json!({}),
        );
        assert_eq!(ordered["results"], serde_json::json!([[12], [7.5]]));
        // In a rule body: a value of unknown type compared with a number.
        let big = "[:find ?e ?v :rules [[(big ?e ?v) [?e :rv/name _] [?e ?a ?v] [(> ?v 35)]]] \
                   :where (big ?e ?v)]";
        assert_eq!(rows(big), [format!("[{d},40]")]);
        // A ref value in a mixed column equals an entity.
        let boss = "[[(val ?e ?v) [?e :rv/age ?v]] [(val ?e ?v) [?e :rv/boss ?v]]]";
        assert_eq!(
            rows(&format!(
                "[:find ?e :rules {boss} :where (val ?e ?v) [?b :rv/name \"ann\"] [(= ?v ?b)]]"
            )),
            [format!("[{b}]")]
        );
        // Against a string a number matches nothing; no SQL error.
        assert!(rows(&q("[(= ?v \"x\")]")).is_empty());
        // A column of longs and strings: a number compares the longs, a
        // string the strings, neither raises a SQL error.
        let val = "[[(val ?e ?v) [?e :rv/age ?v]] [(val ?e ?v) [?e :rv/name ?v]]]";
        let qv = |body: &str| format!("[:find ?e :rules {val} :where (val ?e ?v) {body}]");
        assert_eq!(
            rows(&qv("[(> ?v 20)]")),
            sorted(vec![format!("[{a}]"), format!("[{d}]")])
        );
        assert_eq!(rows(&qv("[(= ?v \"cy\")]")), [format!("[{c}]")]);
        assert_eq!(rows(&qv("[(= ?v 12)]")), [format!("[{b}]")]);
        assert_eq!(query_error(&qv("[(> ?v ?e)]")), "");
    }

    /// A rule invoked with the wrong number of arguments is
    /// :db.error/rule-arity (more was a SQL error on a missing column, fewer
    /// ignored the rule's other columns). A lookup-ref argument is
    /// unsupported, and the message says what is.
    #[pg_test]
    fn test_rule_arity_and_argument_errors() {
        setup_chain();
        let boss = "[[(boss ?e ?b) [?e :rv/boss ?b]]]";
        for call in ["(boss ?e ?b ?c)", "(boss ?e)", "(boss ?x ?y) (boss ?e)"] {
            let q = format!("[:find ?e :rules {boss} :where {call}]");
            let err = query_error(&q);
            assert!(
                err.contains(":db.error/rule-arity") && err.contains("takes 2 arguments"),
                "{q}: {err}"
            );
        }
        let q = format!("[:find ?e :rules {boss} :where (boss ?e [:rv/name \"ann\"])]");
        let err = query_error(&q);
        assert!(
            err.contains(":db.error/unsupported-rule-arg")
                && err.contains("lookup ref")
                && err.contains("number, string, keyword"),
            "{q}: {err}"
        );
    }

    /// A binding function's result for a variable a pattern (or rule) also
    /// binds constrains it: `[(+ 12 0) ?k] [?x :rv/age ?k]` is bob, aged 12.
    /// The function was ignored, so every ?x matched with ?k = 12.
    #[pg_test]
    fn test_fn_result_constrains_a_bound_variable() {
        let [a, b, c, _d] = setup_chain();
        for q in [
            "[:find ?x ?k :where [(+ 12 0) ?k] [?x :rv/age ?k]]",
            "[:find ?x ?k :where [?x :rv/age ?k] [(+ 12 0) ?k]]",
            "[:find ?x ?k :rules [[(aged ?e ?n) [?e :rv/age ?n]]] :where [(+ 12 0) ?k] (aged ?x ?k)]",
            // In an or arm.
            "[:find ?x ?k :where [?x :rv/name _] \
             (or (and [?x :rv/age ?k] [(* 6 2) ?k]) (and [?x :rv/age ?k] [(+ 99 0) ?k]))]",
            // In a rule body.
            "[:find ?x ?k :rules [[(twelve ?e ?n) [?e :rv/age ?n] [(+ 12 0) ?n]]] \
             :where (twelve ?x ?k)]",
        ] {
            assert_eq!(rows(q), [format!("[{b},12]")], "{q}");
        }
        // An entity: ann is cy's boss's boss; (+ ?y 0) is ann again.
        let q = format!("[:find ?x :where [?y :rv/name \"ann\"] [(+ ?y 0) ?x] [?x :rv/age 30]]");
        assert_eq!(rows(&q), [format!("[{a}]")]);
        // A result no pattern binds is unchanged: a new value.
        let q = "[:find ?x ?d :where [?x :rv/age 5] [(* 5 2) ?d]]";
        assert_eq!(rows(q), [format!("[{c},10]")]);
    }

    /// The error a query raises, or "" if it runs.
    fn query_error(query: &str) -> String {
        crate::functions::query::mentat_query(query, pgrx::JsonB(serde_json::json!({})))
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default()
    }

    /// A rule head takes variables only (as in Datomic): a constant there is
    /// an error. It matched nothing, so `(isa ?e :rv/dev)` returned [].
    #[pg_test]
    fn test_rule_head_constant_is_an_error() {
        let [_a, b, c, _d] = setup_chain();
        for (rules, call) in [
            (
                "[[(isa ?e :rv/dev) [?e :rv/role :rv/dev]]]",
                "(isa ?e :rv/dev)",
            ),
            ("[[(isa ?e \"bob\") [?e :rv/name \"bob\"]]]", "(isa ?e ?n)"),
            (
                "[[(aged ?e ?n) [?e :rv/age ?n]] [(aged ?e 5) [?e :rv/age 5]]]",
                "(aged ?e 5)",
            ),
        ] {
            let q = format!("[:find ?e :rules {rules} :where {call}]");
            let err = query_error(&q);
            assert!(
                err.contains(":db.error/invalid-rule") && err.contains("variables only"),
                "{q}: {err}"
            );
        }
        // The supported form: a variable in the head, the constant at the call.
        assert_eq!(
            rows("[:find ?e :rules [[(isa ?e ?r) [?e :rv/role ?r]]] :where (isa ?e :rv/dev)]"),
            sorted(vec![format!("[{b}]"), format!("[{c}]")])
        );
    }

    /// A recursive rule doing arithmetic on a value its recursive call
    /// binds keeps it a long: the hop count was stored as a double from the
    /// second hop on, so (depth ?e 2) matched nothing and (max ?n) was 3.0.
    #[pg_test]
    fn test_rule_recursive_arithmetic_is_long() {
        let [a, b, c, d] = setup_chain();
        let depth = "[[(depth ?e ?n) [?e :rv/name \"ann\"] [(+ 0 0) ?n]] \
                      [(depth ?e ?n) [?e :rv/boss ?b] (depth ?b ?m) [(+ ?m 1) ?n]]]";
        assert_eq!(
            rows(&format!(
                "[:find ?e ?n :rules {depth} :where (depth ?e ?n)]"
            )),
            sorted(vec![
                format!("[{a},0]"),
                format!("[{b},1]"),
                format!("[{c},2]"),
                format!("[{d},3]"),
            ])
        );
        assert_eq!(
            rows(&format!("[:find ?e :rules {depth} :where (depth ?e 2)]")),
            [format!("[{c}]")]
        );
        let q = format!("[:find ?e :in ?k :rules {depth} :where (depth ?e ?k)]");
        assert_eq!(
            query_with(&q, serde_json::json!({"inputs": [2]}))["results"],
            serde_json::json!([[c]])
        );
        let q = format!("[:find (max ?n) . :rules {depth} :where (depth ?e ?n)]");
        assert_eq!(
            query_with(&q, serde_json::json!({}))["result"],
            serde_json::json!(3)
        );
        assert_eq!(
            rows(&format!(
                "[:find ?e :rules {depth} :where (depth ?e ?n) [(> ?n 1)]]"
            )),
            sorted(vec![format!("[{c}]"), format!("[{d}]")])
        );
        // The recursive clause listed first: the same types.
        let depth_rev = "[[(depth ?e ?n) [?e :rv/boss ?b] (depth ?b ?m) [(+ ?m 1) ?n]] \
                          [(depth ?e ?n) [?e :rv/name \"ann\"] [(+ 0 0) ?n]]]";
        assert_eq!(
            rows(&format!(
                "[:find ?e :rules {depth_rev} :where (depth ?e 3)]"
            )),
            [format!("[{d}]")]
        );
    }
}
