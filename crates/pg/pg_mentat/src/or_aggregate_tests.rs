// An `or` whose arms bind one variable to a long in one arm and a double in
// another (#390). Each arm ran the whole aggregate on its own rows and the
// arms' results were UNIONed, so `(sum ?v) .` returned one arm's sum (87 for
// the reviewer's data, want 102.0) and count, min and avg were one arm's too.
// The variable now has the rule form's merged type ("number" for longs and
// doubles, unknown for longs and strings), so the `or` form and the rule
// form of a query give the same answers.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;
    use serde_json::{json, Value};

    /// ages a 30, b 12, c 5, d 40; scores a 2.5, b 7.5, c 5.0; ranks a 1,
    /// d 5; roles b and c :oa/dev. Returns the entities a, b, c, d.
    fn setup() -> [i64; 4] {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        let tx = |edn: &str| {
            let r = Spi::get_one_with_args::<String>(
                "SELECT edn_t($1)::TEXT",
                &[pgrx::datum::DatumWithOid::from(edn)],
            )
            .expect(edn)
            .expect("NULL tx report");
            serde_json::from_str::<Value>(&r).expect("tx report")
        };
        tx("[{:db/ident :oa/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity}
             {:db/ident :oa/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
             {:db/ident :oa/score :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
             {:db/ident :oa/rank :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
             {:db/ident :oa/role :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}]");
        let j = tx(
            "[{:db/id \"a\" :oa/name \"a\" :oa/age 30 :oa/score 2.5 :oa/rank 1}
                     {:db/id \"b\" :oa/name \"b\" :oa/age 12 :oa/score 7.5 :oa/role :oa/dev}
                     {:db/id \"c\" :oa/name \"c\" :oa/age 5 :oa/score 5.0 :oa/role :oa/dev}
                     {:db/id \"d\" :oa/name \"d\" :oa/age 40 :oa/rank 5}]",
        );
        ["a", "b", "c", "d"].map(|t| j["tempids"][t].as_i64().expect(t))
    }

    /// The query's "result", sorted when it is a collection or a relation
    /// with no :order, so two forms of a query compare as sets.
    fn result(query: &str) -> Value {
        let r = crate::functions::query::mentat_query(query, pgrx::JsonB(json!({})))
            .unwrap_or_else(|e| panic!("{query}: {e}"))
            .0["result"]
            .clone();
        match r {
            Value::Array(mut v) if !query.contains(":order") => {
                v.sort_by_key(|x| x.to_string());
                Value::Array(v)
            }
            r => r,
        }
    }

    fn sorted(v: Value) -> Value {
        let Value::Array(mut v) = v else { return v };
        v.sort_by_key(|x| x.to_string());
        Value::Array(v)
    }

    const AGE_SCORE: &str = "(or [?e :oa/age ?v] [?e :oa/score ?v])";
    const AGE_SCORE_RULE: &str = "[[(num ?e ?v) [?e :oa/age ?v]] [(num ?e ?v) [?e :oa/score ?v]]]";

    /// The `or` form and the rule form of `[:find <find> :where [?e :oa/name _]
    /// <or> <body>]`; asserts they agree and returns the answer.
    fn or_and_rule(or: &str, rules: &str, find: &str, body: &str) -> Value {
        let by_or = result(&format!(
            "[:find {find} :where [?e :oa/name _] {or} {body}]"
        ));
        let by_rule = result(&format!(
            "[:find {find} :rules {rules} :where [?e :oa/name _] (num ?e ?v) {body}]"
        ));
        assert_eq!(
            by_or, by_rule,
            "{find} {or} {body}: the or and the rule differ"
        );
        by_or
    }

    /// cases14's or-* queries: the aggregates were one arm's (sum 87, min 5,
    /// count 3, avg 21.75). The set of ?v keeps 5 and 5.0, two values in
    /// Datomic (Clojure's (= 5 5.0) is false), so the sum is 102.0.
    #[pg_test]
    fn test_or_long_and_double_aggregates() {
        let [a, b, c, d] = setup();
        let q = |find: &str, body: &str| {
            result(&format!(
                "[:find {find} :where [?e :oa/name _] {AGE_SCORE} {body}]"
            ))
        };
        assert_eq!(q("(sum ?v) .", ""), json!(102.0));
        assert_eq!(q("(max ?v) .", ""), json!(40.0));
        assert_eq!(q("(min ?v) .", ""), json!(2.5));
        assert!((q("(avg ?v) .", "").as_f64().unwrap() - 102.0 / 7.0).abs() < 1e-9);
        assert_eq!(q("(count-distinct ?v) .", ""), json!(7));
        assert_eq!(q("(count ?v) .", ""), json!(7));
        assert_eq!(q("(sum ?v) . :with ?e", ""), json!(102.0));
        assert_eq!(
            q("[?v ...]", ""),
            sorted(json!([30, 12, 5, 40, 2.5, 7.5, 5.0]))
        );
        assert_eq!(
            q("[?v ...]", "[(> ?v 6)]"),
            sorted(json!([30, 12, 40, 7.5]))
        );
        assert_eq!(
            q("[?v ...]", "[(!= ?v 5)]"),
            sorted(json!([30, 12, 40, 2.5, 7.5]))
        );
        assert_eq!(
            q("?e (sum ?v)", ""),
            sorted(json!([[a, 32.5], [b, 19.5], [c, 10.0], [d, 40.0]]))
        );
        assert_eq!(
            q("(min ?v) (max ?v) (count ?e)", ""),
            json!([[2.5, 40.0, 7]])
        );
        // :order reads ?v as a number: it was text order (12 30 40 5 2.5 ...).
        assert_eq!(
            q("?v :order ?v", "[(!= ?v 5)]"),
            json!([[2.5], [7.5], [12], [30], [40]])
        );
        assert_eq!(
            q("?v :order (desc ?v)", "[(> ?v 6)]"),
            json!([[40], [30], [12], [7.5]])
        );
        // Two arms of one type each count their rows: it was 1 (each arm
        // counted 1 and the UNION kept one "1").
        assert_eq!(
            result("[:find (count ?e) . :where (or [?e :oa/age 30] [?e :oa/age 12])]"),
            json!(2)
        );
        // No rows: nothing for sum, 0 for count (#237).
        assert_eq!(q("(sum ?v) .", "[(> ?v 100)]"), Value::Null);
        assert_eq!(q("(count ?v) .", "[(> ?v 100)]"), json!(0));
    }

    /// The `or` form and the rule form of the same query agree, on
    /// aggregates, comparisons and :order.
    #[pg_test]
    fn test_or_matches_rule_form() {
        setup();
        for (find, body) in [
            ("(max ?v) .", ""),
            ("(min ?v) .", ""),
            ("(sum ?v) .", ""),
            ("(avg ?v) .", ""),
            ("(count ?v) .", ""),
            ("(count-distinct ?v) .", ""),
            ("(sum ?v) . :with ?e", ""),
            ("?e (max ?v)", ""),
            ("[?v ...]", ""),
            ("[?v ...]", "[(> ?v 6)]"),
            ("[?v ...]", "[(!= ?v 5)]"),
            ("[?v ...]", "[(= ?v 5)]"),
            ("[?v ...]", "[(<= ?v 2.5)]"),
            ("[?v ...]", "[(= ?v \"x\")]"),
            ("?v :order (desc ?v)", "[(!= ?v 5)]"),
            ("(max ?v) .", "[(< ?v 20)]"),
        ] {
            or_and_rule(AGE_SCORE, AGE_SCORE_RULE, find, body);
        }
        // A value an :in input bounds.
        let k = |q: String| {
            crate::functions::query::mentat_query(&q, pgrx::JsonB(json!({"inputs": [6]})))
                .expect(&q)
                .0["result"]
                .clone()
        };
        assert_eq!(
            k(format!(
                "[:find (sum ?v) . :in ?k :where [?e :oa/name _] {AGE_SCORE} [(> ?v ?k)]]"
            )),
            k(format!(
                "[:find (sum ?v) . :in ?k :rules {AGE_SCORE_RULE} \
                 :where [?e :oa/name _] (num ?e ?v) [(> ?v ?k)]]"
            ))
        );
        // A long function result in each arm aggregates as a long, as in a rule.
        let inc = "[[(num ?e ?v) [?e :oa/age ?x] [(+ ?x 1) ?v]] \
                    [(num ?e ?v) [?e :oa/age ?x] [(+ ?x 2) ?v]]]";
        let or = "(or (and [?e :oa/age ?x] [(+ ?x 1) ?v]) (and [?e :oa/age ?x] [(+ ?x 2) ?v]))";
        assert_eq!(or_and_rule(or, inc, "(sum ?v) .", ""), json!(186));
        assert_eq!(or_and_rule(or, inc, "(min ?v) .", ""), json!(6));
    }

    /// or-join, `and` arms and a third arm (long, double, long).
    #[pg_test]
    fn test_or_join_and_arms_three_arms() {
        let [a, b, _c, d] = setup();
        let join = "(or-join [?e ?v] [?e :oa/age ?v] [?e :oa/score ?v])";
        assert_eq!(
            or_and_rule(join, AGE_SCORE_RULE, "(sum ?v) .", ""),
            json!(102.0)
        );
        assert_eq!(
            or_and_rule(join, AGE_SCORE_RULE, "(min ?v) .", ""),
            json!(2.5)
        );
        // An or-join's own variable of a different type in each arm.
        assert_eq!(
            result(
                "[:find [?e ...] :where [?e :oa/name _] (or-join [?e] \
                 (and [?e :oa/age ?x] [(> ?x 20)]) (and [?e :oa/score ?x] [(> ?x 6)]))]"
            ),
            sorted(json!([a, b, d]))
        );
        assert_eq!(
            result(
                "[:find (count ?e) . :where [?e :oa/name _] \
                 (or-join [?e] [?e :oa/age ?x] [?e :oa/score ?x])]"
            ),
            json!(4)
        );

        let and = "(or (and [?e :oa/age ?v] [(> ?v 10)]) [?e :oa/score ?v])";
        let and_rule =
            "[[(num ?e ?v) [?e :oa/age ?v] [(> ?v 10)]] [(num ?e ?v) [?e :oa/score ?v]]]";
        assert_eq!(or_and_rule(and, and_rule, "(sum ?v) .", ""), json!(97.0));
        assert_eq!(or_and_rule(and, and_rule, "(count ?v) .", ""), json!(6));
        // (A rule body has no `not`, so this one has no rule form.)
        assert_eq!(
            result(
                "[:find (sum ?v) . :where [?e :oa/name _] \
                 (or (and [?e :oa/age ?v] (not [?e :oa/role :oa/dev])) [?e :oa/score ?v])]"
            ),
            json!(85.0)
        );

        // d's rank 5 is c's age 5: one value of the set of ?v, two of ?v ?e.
        let three = "(or [?e :oa/age ?v] [?e :oa/score ?v] [?e :oa/rank ?v])";
        let three_rule = "[[(num ?e ?v) [?e :oa/age ?v]] [(num ?e ?v) [?e :oa/score ?v]] \
                          [(num ?e ?v) [?e :oa/rank ?v]]]";
        for (find, want) in [
            ("(sum ?v) .", json!(103.0)),
            ("(count ?v) .", json!(8)),
            ("(sum ?v) . :with ?e", json!(108.0)),
            ("(max ?v) .", json!(40.0)),
            ("(min ?v) .", json!(1.0)),
        ] {
            assert_eq!(or_and_rule(three, three_rule, find, ""), want, "{find}");
        }
        assert_eq!(
            or_and_rule(three, three_rule, "?v :order (desc ?v)", "[(> ?v 6)]"),
            json!([[40], [30], [12], [7.5]])
        );
        assert_eq!(
            or_and_rule(three, three_rule, "[?v ...]", "[(< ?v 6)]"),
            sorted(json!([1, 2.5, 5, 5.0]))
        );
    }

    /// A string arm: a column of values of several types keeps the untyped
    /// behaviour, as the rule form does: a number compares the numbers, a
    /// string compares every value as text. `(= ?v "c")` and `(!= ?v 5)`
    /// were SQL errors ("bigint = text"), `(< ?v "c")` compared only the
    /// strings, and count was one arm's.
    #[pg_test]
    fn test_or_string_arm_is_untyped() {
        setup();
        let or = "(or [?e :oa/age ?v] [?e :oa/name ?v])";
        let rule = "[[(num ?e ?v) [?e :oa/age ?v]] [(num ?e ?v) [?e :oa/name ?v]]]";
        assert_eq!(
            or_and_rule(or, rule, "[?v ...]", "[(= ?v \"c\")]"),
            json!(["c"])
        );
        assert_eq!(
            or_and_rule(or, rule, "[?v ...]", "[(!= ?v 5)]"),
            sorted(json!([30, 12, 40]))
        );
        assert_eq!(
            or_and_rule(or, rule, "[?v ...]", "[(> ?v 20)]"),
            sorted(json!([30, 40]))
        );
        assert_eq!(
            or_and_rule(or, rule, "[?v ...]", "[(< ?v \"c\")]"),
            sorted(json!([30, 12, 5, 40, "a", "b"]))
        );
        assert_eq!(or_and_rule(or, rule, "(count ?v) .", ""), json!(8));
        assert_eq!(or_and_rule(or, rule, "(max ?v) .", ""), json!("d"));
        or_and_rule(or, rule, "?v :order ?v", "");
    }
}
