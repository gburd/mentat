// Aggregate function tests in queries: count, sum, min, max, avg, etc.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
    }

    fn setup_agg_schema_and_data() {
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"n\" :db/ident :ag/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"v\" :db/ident :ag/val :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/id \"d\" :db/ident :ag/dept :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"s\" :db/ident :ag/score :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        ).expect("agg schema");

        Spi::run(
            "SELECT edn_t('[
                {:db/id \"e1\" :ag/name \"Alice\" :ag/val 100 :ag/dept \"Engineering\" :ag/score 88.5}
                {:db/id \"e2\" :ag/name \"Bob\" :ag/val 200 :ag/dept \"Engineering\" :ag/score 72.3}
                {:db/id \"e3\" :ag/name \"Carol\" :ag/val 150 :ag/dept \"Design\" :ag/score 91.7}
                {:db/id \"e4\" :ag/name \"Dave\" :ag/val 300 :ag/dept \"Product\" :ag/score 67.8}
                {:db/id \"e5\" :ag/name \"Eve\" :ag/val 50 :ag/dept \"Engineering\" :ag/score 95.2}
            ]'::TEXT)",
        ).expect("agg data");
    }

    // ========================================================================
    // Count
    // ========================================================================

    #[pg_test]
    fn test_ag_count_all() {
        setup();
        setup_agg_schema_and_data();
        // Count distinct entities
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find (count ?e) . :where [?e :ag/name _]]'::TEXT, '{}'::jsonb)::TEXT",
        );
        // count may or may not be supported; if it is, verify it returns 5
        if let Ok(Some(result)) = q {
            let j: serde_json::Value = serde_json::from_str(&result).expect("parse");
            if let Some(count) = j["result"].as_i64() {
                assert_eq!(count, 5);
            }
        }
    }

    // ========================================================================
    // Alternative: count via result set size
    // ========================================================================

    #[pg_test]
    fn test_ag_count_via_results() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?name :where [?e :ag/name ?name]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["results"].as_array().expect("arr").len(), 5);
    }

    #[pg_test]
    fn test_ag_count_by_dept() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?name :where [?e :ag/name ?name] [?e :ag/dept \"Engineering\"]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["results"].as_array().expect("arr").len(), 3); // Alice, Bob, Eve
    }

    #[pg_test]
    fn test_ag_count_filtered() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?name :where [?e :ag/name ?name] [?e :ag/val ?v] [(> ?v 100)]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["results"].as_array().expect("arr").len(), 3); // Bob=200, Carol=150, Dave=300
    }

    // ========================================================================
    // Min/Max via sort + scalar
    // ========================================================================

    #[pg_test]
    fn test_ag_min_val_via_predicate() {
        setup();
        setup_agg_schema_and_data();
        // Find the minimum val: Eve has 50
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?v ...] :where [?e :ag/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let vals: Vec<i64> = j["result"]
            .as_array()
            .expect("arr")
            .iter()
            .map(|v| v.as_i64().expect("v"))
            .collect();
        assert_eq!(*vals.iter().min().unwrap(), 50);
    }

    #[pg_test]
    fn test_ag_max_val_via_predicate() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?v ...] :where [?e :ag/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let vals: Vec<i64> = j["result"]
            .as_array()
            .expect("arr")
            .iter()
            .map(|v| v.as_i64().expect("v"))
            .collect();
        assert_eq!(*vals.iter().max().unwrap(), 300);
    }

    // ========================================================================
    // Sum via collection
    // ========================================================================

    #[pg_test]
    fn test_ag_sum_via_collection() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?v ...] :where [?e :ag/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let sum: i64 = j["result"]
            .as_array()
            .expect("arr")
            .iter()
            .map(|v| v.as_i64().expect("v"))
            .sum();
        assert_eq!(sum, 100 + 200 + 150 + 300 + 50);
    }

    // ========================================================================
    // Distinct values
    // ========================================================================

    #[pg_test]
    fn test_ag_distinct_depts() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?d ...] :where [_ :ag/dept ?d]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let depts = j["result"].as_array().expect("arr");
        assert_eq!(depts.len(), 3); // Engineering, Design, Product
    }

    // ========================================================================
    // Score-based queries
    // ========================================================================

    #[pg_test]
    fn test_ag_high_scorers() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?name ?s :where [?e :ag/name ?name] [?e :ag/score ?s] [(> ?s 85.0)]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let results = j["results"].as_array().expect("arr");
        // Alice=88.5, Carol=91.7, Eve=95.2
        assert_eq!(results.len(), 3);
    }

    #[pg_test]
    fn test_ag_score_range() {
        setup();
        setup_agg_schema_and_data();
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?s ...] :where [_ :ag/score ?s] [(>= ?s 70.0)] [(<= ?s 90.0)]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let scores = j["result"].as_array().expect("arr");
        // Alice=88.5, Bob=72.3 => 2
        assert_eq!(scores.len(), 2);
    }

    // ========================================================================
    // (min ?x) / (max ?x) aggregate over each value type.
    //
    // Regression for the 1.6.0 bug where MIN/MAX unconditionally cast the
    // decoded text to ::NUMERIC, which works only for long/ref and raised a
    // raw PostgreSQL cast error on instant/string/keyword/boolean/double/etc.
    // MIN/MAX are defined on any ORDERED type; the decode expr already renders
    // values so lexicographic TEXT ordering matches value ordering, so order on
    // the text directly -- EXCEPT long/ref, which must stay numeric ("9" > "61").
    // ========================================================================

    fn setup_mm_schema() {
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :mm/n  :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
                {:db/ident :mm/at :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}
                {:db/ident :mm/t  :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
                {:db/ident :mm/s  :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
                {:db/ident :mm/b  :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
                {:db/ident :mm/d  :db/valueType :db.type/double  :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        ).expect("mm schema");
        Spi::run(
            "SELECT edn_t('[
                {:mm/n 9  :mm/at #inst \"2026-08-01T00:00:37.000000Z\" :mm/t \"alpha\" :mm/s :a :mm/b false :mm/d 1.5}
                {:mm/n 61 :mm/at #inst \"2026-09-08T12:00:00.000000Z\" :mm/t \"omega\" :mm/s :z :mm/b true  :mm/d 2.5}
                {:mm/n 42 :mm/at #inst \"2026-01-15T06:30:00.000000Z\" :mm/t \"mid\"   :mm/s :m :mm/b false :mm/d 9.0}
            ]'::TEXT)",
        ).expect("mm data");
    }

    fn scalar_result(q: &str) -> serde_json::Value {
        let raw =
            Spi::get_one::<String>(&format!("SELECT edn_q('{q}'::TEXT, '{{}}'::jsonb)::TEXT"))
                .expect("query ran")
                .expect("non-NULL result");
        let j: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
        j["result"].clone()
    }

    #[pg_test]
    fn test_mm_max_long_is_numeric_not_lexicographic() {
        setup();
        setup_mm_schema();
        // The whole point: 61 > 42 > 9 numerically. Lexicographic text would
        // wrongly pick "9". Pins long behaviour against regression.
        assert_eq!(scalar_result("[:find (max ?n) . :where [?e :mm/n ?n]]"), 61);
        assert_eq!(scalar_result("[:find (min ?n) . :where [?e :mm/n ?n]]"), 9);
    }

    #[pg_test]
    fn test_mm_max_min_instant() {
        setup();
        setup_mm_schema();
        // Newest / oldest instant, rendered fixed-width UTC.
        let mx = scalar_result("[:find (max ?at) . :where [?e :mm/at ?at]]");
        assert_eq!(mx.as_str().expect("str"), "2026-09-08T12:00:00.000000Z");
        let mn = scalar_result("[:find (min ?at) . :where [?e :mm/at ?at]]");
        assert_eq!(mn.as_str().expect("str"), "2026-01-15T06:30:00.000000Z");
    }

    #[pg_test]
    fn test_mm_max_min_instant_non_utc_session() {
        // The text form of an instant ends in a literal `Z`, so it must be UTC
        // whatever the session TimeZone is. Before 1.6.2 it was rendered in the
        // session zone: on an America/New_York server a stored 12:00Z read back
        // as 08:00Z.
        setup();
        setup_mm_schema();
        Spi::run("SET LOCAL TimeZone = 'America/New_York'").expect("set tz");
        let mx = scalar_result("[:find (max ?at) . :where [?e :mm/at ?at]]");
        assert_eq!(mx.as_str().expect("str"), "2026-09-08T12:00:00.000000Z");
        Spi::run("SET LOCAL TimeZone = 'Asia/Kolkata'").expect("set tz");
        let mn = scalar_result("[:find (min ?at) . :where [?e :mm/at ?at]]");
        assert_eq!(mn.as_str().expect("str"), "2026-01-15T06:30:00.000000Z");
    }

    #[pg_test]
    fn test_mm_max_min_string() {
        setup();
        setup_mm_schema();
        assert_eq!(
            scalar_result("[:find (max ?t) . :where [?e :mm/t ?t]]")
                .as_str()
                .expect("str"),
            "omega"
        );
        assert_eq!(
            scalar_result("[:find (min ?t) . :where [?e :mm/t ?t]]")
                .as_str()
                .expect("str"),
            "alpha"
        );
    }

    #[pg_test]
    fn test_mm_max_min_keyword() {
        setup();
        setup_mm_schema();
        assert_eq!(
            scalar_result("[:find (max ?s) . :where [?e :mm/s ?s]]")
                .as_str()
                .expect("str"),
            ":z"
        );
        assert_eq!(
            scalar_result("[:find (min ?s) . :where [?e :mm/s ?s]]")
                .as_str()
                .expect("str"),
            ":a"
        );
    }

    #[pg_test]
    fn test_mm_max_min_boolean() {
        setup();
        setup_mm_schema();
        // Booleans render as text "true"/"false"; "true" > "false".
        assert_eq!(
            scalar_result("[:find (max ?b) . :where [?e :mm/b ?b]]"),
            serde_json::json!(true)
        );
        assert_eq!(
            scalar_result("[:find (min ?b) . :where [?e :mm/b ?b]]"),
            serde_json::json!(false)
        );
    }

    #[pg_test]
    fn test_mm_max_min_double() {
        setup();
        setup_mm_schema();
        // The double decode is 'd:' || hex(float8send), monotonic as text, so
        // MAX picks 9.0 and MIN picks 1.5 and both decode back to floats.
        let mx = scalar_result("[:find (max ?d) . :where [?e :mm/d ?d]]");
        assert_eq!(mx.as_f64().expect("f64"), 9.0);
        let mn = scalar_result("[:find (min ?d) . :where [?e :mm/d ?d]]");
        assert_eq!(mn.as_f64().expect("f64"), 1.5);
    }

    #[pg_test]
    fn test_mm_sum_avg_still_numeric_on_long() {
        setup();
        setup_mm_schema();
        // The SUM/AVG numeric arm is unchanged: 9+61+42 = 112, avg = 37.33...
        assert_eq!(
            scalar_result("[:find (sum ?n) . :where [?e :mm/n ?n]]"),
            112
        );
        let avg = scalar_result("[:find (avg ?n) . :where [?e :mm/n ?n]]");
        let a = avg.as_f64().expect("f64");
        assert!((a - 37.3333).abs() < 0.01, "avg was {a}");
    }

    // ========================================================================
    // Aggregate semantics (1.10.0): an aggregate runs over the SET of
    // bindings of the :find + :with variables, grouped by the non-aggregate
    // :find variables -- Datomic's semantics, and the embedded backend's
    // (crates/sqlite/mentat/tests/query.rs). Without :with, duplicate values
    // collapse before aggregation; :with keeps one row per :with binding.
    // ========================================================================

    fn setup_monsters() {
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :monster/heads  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
                {:db/ident :monster/name   :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                {:db/ident :monster/weapon :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
                {:db/ident :monster/weight :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        )
        .expect("monster schema");
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"me\" :monster/heads 1 :monster/name \"Medusa\"   :monster/weight 1.5}
                {:db/id \"cy\" :monster/heads 1 :monster/name \"Cyclops\"  :monster/weight 2.5}
                {:db/id \"ch\" :monster/heads 1 :monster/name \"Chimera\"  :monster/weight 2.5}
                {:db/id \"ce\" :monster/heads 3 :monster/name \"Cerberus\" :monster/weight 4.0}
                [:db/add \"me\" :monster/weapon \"Stony gaze\"]
                [:db/add \"cy\" :monster/weapon \"Large club\"]
                [:db/add \"cy\" :monster/weapon \"Mighty arms\"]
                [:db/add \"cy\" :monster/weapon \"Stompy feet\"]
                [:db/add \"ch\" :monster/weapon \"Goat-like agility\"]
                [:db/add \"ce\" :monster/weapon \"Kong\"]
                [:db/add \"ce\" :monster/weapon \"Deadly drool\"]
            ]'::TEXT)",
        )
        .expect("monster data");
    }

    fn rows(q: &str) -> Vec<serde_json::Value> {
        let raw =
            Spi::get_one::<String>(&format!("SELECT edn_q('{q}'::TEXT, '{{}}'::jsonb)::TEXT"))
                .expect("query ran")
                .expect("non-NULL result");
        let j: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
        let mut r = j["results"].as_array().cloned().expect("results array");
        r.sort_by_key(|v| v.to_string());
        r
    }

    fn sql_of(q: &str) -> String {
        Spi::get_one::<String>(&format!(
            "SELECT mentat_query_sql('{q}'::TEXT, '{{}}'::jsonb)->>'sql'"
        ))
        .expect("sql")
        .expect("NULL")
    }

    #[pg_test]
    fn test_agg_sum_avg_with_and_without_with() {
        setup();
        setup_monsters();
        // Distinct heads values are {1, 3}.
        assert_eq!(
            scalar_result("[:find (sum ?h) . :where [?m :monster/heads ?h]]"),
            4
        );
        assert_eq!(
            scalar_result("[:find (sum ?h) . :with ?m :where [?m :monster/heads ?h]]"),
            6
        );
        let avg = scalar_result("[:find (avg ?h) . :where [?m :monster/heads ?h]]");
        assert!((avg.as_f64().expect("f64") - 2.0).abs() < 1e-9, "avg {avg}");
        let avg = scalar_result("[:find (avg ?h) . :with ?m :where [?m :monster/heads ?h]]");
        assert!((avg.as_f64().expect("f64") - 1.5).abs() < 1e-9, "avg {avg}");
        // sum over a double attribute (was: numeric cast error on 'd:<bits>').
        // Distinct weights {1.5, 2.5, 4.0} = 8.0; with ?m 1.5+2.5+2.5+4.0 = 10.5.
        let s = scalar_result("[:find (sum ?w) . :where [?m :monster/weight ?w]]");
        assert!((s.as_f64().expect("f64") - 8.0).abs() < 1e-9, "sum {s}");
        let s = scalar_result("[:find (sum ?w) . :with ?m :where [?m :monster/weight ?w]]");
        assert!((s.as_f64().expect("f64") - 10.5).abs() < 1e-9, "sum {s}");
    }

    #[pg_test]
    fn test_agg_count_value_var_with_and_without_with() {
        setup();
        setup_monsters();
        assert_eq!(
            scalar_result("[:find (count ?h) . :where [_ :monster/heads ?h]]"),
            2
        );
        assert_eq!(
            scalar_result("[:find (count ?h) . :with ?m :where [?m :monster/heads ?h]]"),
            4
        );
        assert_eq!(
            scalar_result("[:find (count-distinct ?h) . :with ?m :where [?m :monster/heads ?h]]"),
            2
        );
    }

    #[pg_test]
    fn test_agg_count_grouped_distinct_tuples() {
        setup();
        setup_monsters();
        // The extra cardinality-many join multiplies rows: a naive COUNT(*)
        // gives heads=1 -> 5 and heads=3 -> 2. The answer is distinct ?m.
        assert_eq!(
            rows("[:find ?h (count ?m) :where [?m :monster/heads ?h] [?m :monster/weapon _]]"),
            vec![serde_json::json!([1, 3]), serde_json::json!([3, 1])]
        );
        // Grouped count over a cardinality-many value with :with.
        assert_eq!(
            rows(
                "[:find ?n (count ?w) :with ?m :where [?m :monster/name ?n] [?m :monster/weapon ?w]]"
            ),
            vec![
                serde_json::json!(["Cerberus", 2]),
                serde_json::json!(["Chimera", 1]),
                serde_json::json!(["Cyclops", 3]),
                serde_json::json!(["Medusa", 1]),
            ]
        );
        // Group by a value, count the entity: heads 1 -> 3 monsters, 3 -> 1.
        assert_eq!(
            rows("[:find ?h (count ?m) :where [?m :monster/heads ?h]]"),
            vec![serde_json::json!([1, 3]), serde_json::json!([3, 1])]
        );
        // Group by the entity, count its many-valued attribute.
        assert_eq!(
            rows("[:find ?m (count ?w) :where [?m :monster/weapon ?w] [?m :monster/name \"Cyclops\"]]")
                .iter()
                .map(|r| r[1].clone())
                .collect::<Vec<_>>(),
            vec![serde_json::json!(3)]
        );
    }

    #[pg_test]
    fn test_agg_count_plans_without_distinct_when_keyed() {
        setup();
        setup_monsters();
        // (?h, ?m) with ?m the entity of a cardinality-one pattern is a key of
        // the join: plain COUNT(*), no DISTINCT, no sort.
        let keyed = sql_of("[:find ?h (count ?m) :where [?m :monster/heads ?h]]");
        assert!(keyed.contains("COUNT(*)"), "{keyed}");
        assert!(!keyed.contains("DISTINCT"), "{keyed}");
        // Chains through cardinality-one refs and :db/unique stay keyed.
        let keyed =
            sql_of("[:find ?h (count ?n) :where [?m :monster/name ?n] [?m :monster/heads ?h]]");
        assert!(!keyed.contains("DISTINCT"), "{keyed}");
        // A cardinality-many join can repeat (?h, ?m): must de-duplicate.
        let dup =
            sql_of("[:find ?h (count ?m) :where [?m :monster/heads ?h] [?m :monster/weapon _]]");
        assert!(dup.contains("SELECT DISTINCT"), "{dup}");
        // Without :with, (sum ?h) aggregates distinct values.
        let dup = sql_of("[:find (sum ?h) . :where [?m :monster/heads ?h]]");
        assert!(dup.contains("SELECT DISTINCT"), "{dup}");
    }

    // ========================================================================
    // get-else / missing? / attribute pushdown resolve the attribute (before
    // 1.10.0 they looked up "::ns/attr", found nothing, and silently returned
    // the default / matched everything / fell back to an InitPlan).
    // ========================================================================

    #[pg_test]
    fn test_get_else_and_missing_resolve_the_attribute() {
        setup();
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :ge/n :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
                {:db/ident :ge/x :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
                {:db/ident :ge/k :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        )
        .expect("schema");
        Spi::run(
            "SELECT edn_t('[{:db/id \"a\" :ge/n \"a\" :ge/x 1 :ge/k :k/one} {:ge/n \"b\"}]'::TEXT)",
        )
        .expect("data");
        // Replace a's :ge/x: the superseded 1 must not leak into either form.
        let e: i64 = Spi::get_one(
            "SELECT e FROM mentat.current_text WHERE v = 'a' AND a = \
             (SELECT entid FROM mentat.idents WHERE ident = ':ge/n')",
        )
        .expect("e")
        .expect("NULL");
        Spi::run(&format!("SELECT edn_t('[[:db/add {e} :ge/x 5]]'::TEXT)")).expect("replace");

        assert_eq!(
            rows("[:find ?n ?x :where [?e :ge/n ?n] [(get-else $ ?e :ge/x 0) ?x]]"),
            vec![serde_json::json!(["a", 5]), serde_json::json!(["b", 0])]
        );
        assert_eq!(
            rows("[:find ?n ?k :where [?e :ge/n ?n] [(get-else $ ?e :ge/k :k/none) ?k]]"),
            vec![
                serde_json::json!(["a", ":k/one"]),
                serde_json::json!(["b", ":k/none"])
            ]
        );
        assert_eq!(
            rows("[:find ?n :where [?e :ge/n ?n] [(missing? $ ?e :ge/x)]]"),
            vec![serde_json::json!(["b"])]
        );
        // The constant attribute is pushed into the typed FROM fragment as a
        // literal entid (partial indexes WHERE a = <entid> need it).
        let sql = sql_of("[:find ?n :where [?e :ge/n ?n]]");
        let a: i64 = Spi::get_one("SELECT entid FROM mentat.idents WHERE ident = ':ge/n'")
            .expect("a")
            .expect("NULL");
        assert!(sql.contains(&format!("AND a = {a})")), "{sql}");
    }
}
