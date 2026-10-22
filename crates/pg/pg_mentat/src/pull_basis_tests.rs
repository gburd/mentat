// Pull on a historical basis (1.18.0). Before 1.18.0 edn_pull had no options
// and edn_q's find-pull emitted `edn_pull(pattern, e)` even under
// {"asOf": T} / {"since": T}, so a pull in a historical query returned the
// CURRENT values: a value changed after T came back changed.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;
    use serde_json::{json, Value};

    fn text(sql: &str) -> String {
        Spi::get_one::<String>(sql)
            .expect("query ran")
            .expect("non-NULL")
    }

    fn jsonv(sql: &str) -> Value {
        serde_json::from_str(&text(sql)).expect("json")
    }

    fn err(sql: &str) -> String {
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._pb_err(stmt TEXT) RETURNS TEXT
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN '';
             EXCEPTION WHEN OTHERS THEN
                 RETURN SQLERRM;
             END $$",
        )
        .expect("helper");
        text(&format!(
            "SELECT mentat._pb_err('{}')",
            sql.replace('\'', "''")
        ))
    }

    /// Alice (age 30, tags x y, Bob refers to her) at t1; at t2 her age is 31,
    /// tag x is retracted, tag z added, Bob's ref retracted. Returns
    /// (alice, bob, t1).
    fn setup() -> (i64, i64, i64) {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap");
        Spi::run(
            "SELECT edn_t('[
              {:db/ident :pb/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
              {:db/ident :pb/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
              {:db/ident :pb/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
              {:db/ident :pb/friend :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]'::TEXT)",
        )
        .expect("schema");
        // pg's map notation takes no vector for a card-many value: add each.
        let r1 = jsonv(
            "SELECT edn_t('[{:db/id \"a\" :pb/name \"Alice\" :pb/age 30}
                            [:db/add \"a\" :pb/tags \"x\"] [:db/add \"a\" :pb/tags \"y\"]
                            {:db/id \"b\" :pb/name \"Bob\" :pb/friend \"a\"}]'::TEXT)",
        );
        let a = r1["tempids"]["a"].as_i64().expect("a");
        let b = r1["tempids"]["b"].as_i64().expect("b");
        let t1 = r1["db-after"]["basis-t"].as_i64().expect("basis-t");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {a} :pb/age 31] [:db/retract {a} :pb/tags \"x\"]
                           [:db/add {a} :pb/tags \"z\"] [:db/retract {b} :pb/friend {a}]]'::TEXT)"
        ))
        .expect("tx2");
        (a, b, t1)
    }

    fn sorted(v: &Value) -> Vec<String> {
        let mut out: Vec<String> = v
            .as_array()
            .expect("array")
            .iter()
            .map(|x| x.to_string())
            .collect();
        out.sort();
        out
    }

    /// The bug: edn_q's find-pull under asOf returned the value changed after T.
    #[pg_test]
    fn find_pull_under_as_of_returns_the_old_value() {
        let (a, _, t1) = setup();
        let q = |opts: &str| {
            jsonv(&format!(
                "SELECT edn_q('[:find (pull ?e [:pb/age]) . :where [?e :pb/name \"Alice\"]]'::TEXT, \
                 '{opts}'::jsonb)::TEXT"
            ))
        };
        assert_eq!(q("{}")["result"][":pb/age"], json!(31));
        let then = q(&format!("{{\"asOf\": {t1}}}"));
        assert_eq!(then["result"][":pb/age"], json!(30), "{then}");
        assert_eq!(then["result"][":db/id"], json!(a));
        // A relation find with a pull column reads the basis too.
        let rel = jsonv(&format!(
            "SELECT edn_q('[:find ?n (pull ?e [:pb/age]) :where [?e :pb/name ?n] [?e :pb/age _]]'::TEXT, \
             '{{\"asOf\": {t1}}}'::jsonb)::TEXT"
        ));
        assert_eq!(rel["results"][0][1][":pb/age"], json!(30), "{rel}");
    }

    #[pg_test]
    fn edn_pull_options_read_the_basis() {
        let (a, b, t1) = setup();
        let pull = |eid: i64, pat: &str, opts: &str| {
            jsonv(&format!(
                "SELECT edn_pull('{pat}', {eid}, '{opts}'::jsonb)::TEXT"
            ))
        };
        let pat = "[:pb/name :pb/age :pb/tags :pb/_friend]";

        // The 2-arg form and {} are the current state.
        let now = jsonv(&format!("SELECT edn_pull('{pat}', {a})::TEXT"));
        assert_eq!(now, pull(a, pat, "{}"));
        assert_eq!(now[":pb/age"], json!(31));
        assert_eq!(sorted(&now[":pb/tags"]), ["\"y\"", "\"z\""]);
        assert!(now.get(":pb/_friend").is_none(), "{now}");

        let then = pull(a, pat, &format!("{{\"asOf\": {t1}}}"));
        assert_eq!(then[":pb/age"], json!(30), "{then}");
        assert_eq!(then[":pb/name"], json!("Alice"));
        assert_eq!(sorted(&then[":pb/tags"]), ["\"x\"", "\"y\""]);
        assert_eq!(then[":pb/_friend"], json!([{":db/id": b}]), "{then}");

        let since = pull(a, pat, &format!("{{\"since\": {t1}}}"));
        assert_eq!(since[":pb/age"], json!(31), "{since}");
        assert_eq!(sorted(&since[":pb/tags"]), ["\"z\""]);
        assert!(since.get(":pb/name").is_none(), "{since}");

        // Both bounds: S < tx <= T, not retracted by T (Datomic's
        // `(since (as-of db T) S)`). Nothing Alice has was asserted after t1
        // and by t1.
        let both = pull(a, pat, &format!("{{\"asOf\": {t1}, \"since\": {t1}}}"));
        assert!(both.get(":pb/age").is_none(), "{both}");
        assert!(both.get(":pb/tags").is_none(), "{both}");
        let t_last: i64 = text("SELECT max(tx)::TEXT FROM mentat.transactions")
            .parse()
            .expect("tx");
        let both = pull(a, pat, &format!("{{\"asOf\": {t_last}, \"since\": {t1}}}"));
        assert_eq!(both[":pb/age"], json!(31), "{both}");
        assert_eq!(sorted(&both[":pb/tags"]), ["\"z\""]);
        assert!(both.get(":pb/name").is_none(), "{both}");

        // A map spec follows the ref on the basis: Bob's friend as of t1.
        let bob = pull(
            b,
            "[{:pb/friend [:pb/age]}]",
            &format!("{{\"asOf\": {t1}}}"),
        );
        assert_eq!(
            bob[":pb/friend"],
            json!([{":db/id": a, ":pb/age": 30}]),
            "{bob}"
        );
        let bob_now = pull(b, "[{:pb/friend [:pb/age]}]", "{}");
        assert!(bob_now.get(":pb/friend").is_none(), "{bob_now}");

        // The basis does not leak into the next call.
        assert_eq!(
            jsonv(&format!("SELECT edn_pull('[:pb/age]', {a})::TEXT"))[":pb/age"],
            json!(31)
        );
    }

    #[pg_test]
    fn edn_pull_rejects_bad_options() {
        let (a, _, _) = setup();
        let e = err(&format!(
            "SELECT edn_pull('[:pb/age]', {a}, '{{\"asOf\": \"x\"}}'::jsonb)"
        ));
        assert!(e.contains("integer tx id"), "{e}");
        // An unknown key is an error, not a silent current-state pull: `as_of`
        // (time-travel.md's old spelling) used to return current values.
        let e = err(&format!(
            "SELECT edn_pull('[:pb/age]', {a}, '{{\"as_of\": 1}}'::jsonb)"
        ));
        assert!(e.contains("unknown option \"as_of\""), "{e}");
        let e = err(&format!(
            "SELECT edn_pull('[:pb/age]', {a}, '{{\"history\": true}}'::jsonb)"
        ));
        assert!(e.contains("does not apply to pull"), "{e}");
    }

    /// edn_q rejects an unknown option key or a non-integer asOf / since,
    /// like edn_pull and the SQLite / DuckDB edn_q. Before 1.18.0 it ignored
    /// them and read current state: {"as_of": t1} returned today's values.
    #[pg_test]
    fn edn_q_rejects_bad_options() {
        let (_, _, t1) = setup();
        let q = "[:find ?a . :where [_ :pb/age ?a]]";
        for (opts, want) in [
            (format!(r#"{{"as_of": {t1}}}"#), "unknown option \"as_of\""),
            (format!(r#"{{"asof": {t1}}}"#), "unknown option \"asof\""),
            (
                r#"{"asOf": "x"}"#.to_string(),
                "\"asOf\" must be an integer tx id",
            ),
            (
                r#"{"since": 1.5}"#.to_string(),
                "\"since\" must be an integer tx id",
            ),
            (
                r#"{"history": 1}"#.to_string(),
                "\"history\" must be a boolean",
            ),
        ] {
            for f in ["edn_q", "edn_q_rows"] {
                let e = err(&format!("SELECT {f}('{q}', '{opts}'::jsonb)"));
                assert!(e.contains(want), "{f} {opts}: {e}");
            }
        }
        // The documented keys still work.
        let ok = |opts: &str| jsonv(&format!("SELECT edn_q('{q}', '{opts}'::jsonb)::TEXT"));
        assert_eq!(ok(&format!(r#"{{"asOf": {t1}}}"#))["result"], json!(30));
        assert_eq!(ok("{}")["result"], json!(31));
        assert_eq!(
            ok(r#"{"inputs": [], "limit": 5, "offset": 0, "history": false, "since": null}"#)
                ["result"],
            json!(31)
        );
    }
}
