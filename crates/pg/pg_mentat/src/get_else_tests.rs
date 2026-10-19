// get-else with more than one :where pattern (2026-10-18 report,
// pg_mentat 1.16.0 on PG 18.6).
//
// `(get-else $ ?e :attr default)` was compiled to `LEFT JOIN ... ON (ge.e =
// datomsN.e)` appended after a comma-separated FROM list. In PostgreSQL an
// explicit JOIN binds tighter than a comma, so `FROM d0, d1, d2 LEFT JOIN ge
// ON (...)` is `d0, d1, (d2 LEFT JOIN ge ON ...)`. The ON clause cannot see
// d0/d1, so every query whose entity variable was bound by a pattern other
// than the last FROM item failed with "invalid reference to FROM-clause entry
// for table datomsN". The fix compiles get-else to a correlated scalar
// subquery in the projection, which needs no join placement.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
    }

    /// The report's schema and data: two chats, only "b" has :t/private.
    fn setup_chats() {
        setup();
        Spi::run(
            "SELECT edn_t('[
              {:db/ident :t/kind    :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
              {:db/ident :t/value   :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
              {:db/ident :t/private :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
              {:db/ident :t/rank    :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
              {:db/ident :t/tag     :db/valueType :db.type/string  :db/cardinality :db.cardinality/many}
              {:db/ident :p/email   :db/valueType :db.type/string  :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity :db/index true}
              {:db/ident :t/person  :db/valueType :db.type/ref     :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        )
        .expect("schema");
        Spi::run(
            "SELECT edn_t('[{:db/id \"x\" :p/email \"x@example.com\"}
                            {:t/kind :chat :t/value \"a\" :t/person \"x\" :t/rank 3}
                            {:t/kind :chat :t/value \"b\" :t/person \"x\" :t/private true}
                            {:t/kind :mail :t/value \"c\" :t/private false}]'::TEXT)",
        )
        .expect("data");
    }

    fn rows_opts(q: &str, opts: &str) -> Vec<serde_json::Value> {
        let raw =
            Spi::get_one::<String>(&format!("SELECT edn_q('{q}'::TEXT, '{opts}'::jsonb)::TEXT"))
                .expect("query ran")
                .expect("non-NULL result");
        let j: serde_json::Value = serde_json::from_str(&raw).expect("parse json");
        let mut r = j["results"].as_array().cloned().expect("results array");
        r.sort_by_key(|v| v.to_string());
        r
    }

    fn rows(q: &str) -> Vec<serde_json::Value> {
        rows_opts(q, "{}")
    }

    fn j(v: serde_json::Value) -> serde_json::Value {
        v
    }

    // The report's query 1: one pattern (worked before the fix too).
    #[pg_test]
    fn get_else_one_pattern() {
        setup_chats();
        let r = rows("[:find ?v ?p :where [?c :t/value ?v] [(get-else $ ?c :t/private false) ?p]]");
        assert_eq!(
            r,
            vec![
                j(serde_json::json!(["a", false])),
                j(serde_json::json!(["b", true])),
                j(serde_json::json!(["c", false])),
            ]
        );
    }

    // The report's query 2: two patterns, entity bound by the FIRST pattern.
    // Before the fix: "invalid reference to FROM-clause entry for table datoms0".
    #[pg_test]
    fn get_else_two_patterns_entity_in_first_pattern() {
        setup_chats();
        let r = rows(
            "[:find ?v ?p :where [?c :t/kind :chat] [?c :t/value ?v] \
             [(get-else $ ?c :t/private false) ?p]]",
        );
        assert_eq!(
            r,
            vec![
                j(serde_json::json!(["a", false])),
                j(serde_json::json!(["b", true]))
            ]
        );
    }

    // The report's production shape: 4 patterns, the entity ?c is bound by
    // the SECOND pattern (datoms1), not the last FROM item.
    #[pg_test]
    fn get_else_four_patterns_entity_in_middle_pattern() {
        setup_chats();
        let r = rows(
            "[:find ?k ?v ?priv :where [?p :p/email \"x@example.com\"] [?c :t/person ?p] \
             [?c :t/kind ?k] [?c :t/value ?v] [(get-else $ ?c :t/private false) ?priv]]",
        );
        assert_eq!(
            r,
            vec![
                j(serde_json::json!([":chat", "a", false])),
                j(serde_json::json!([":chat", "b", true])),
            ]
        );
    }

    // Two get-else in one query, of different value types, both keyed on an
    // entity bound by a non-last pattern, plus a get-else on a variable bound
    // only as a ref VALUE (?p is datoms1.v).
    #[pg_test]
    fn get_else_twice_and_on_a_ref_value_variable() {
        setup_chats();
        let r = rows(
            "[:find ?v ?priv ?rank ?mail :where [?c :t/kind :chat] [?c :t/value ?v] \
             [?c :t/person ?p] \
             [(get-else $ ?c :t/private false) ?priv] \
             [(get-else $ ?c :t/rank 0) ?rank] \
             [(get-else $ ?p :p/email \"none\") ?mail]]",
        );
        assert_eq!(
            r,
            vec![
                j(serde_json::json!(["a", false, 3, "x@example.com"])),
                j(serde_json::json!(["b", true, 0, "x@example.com"])),
            ]
        );
    }

    // Aggregates over a get-else result, multi-pattern.
    #[pg_test]
    fn get_else_in_an_aggregate() {
        setup_chats();
        let r = rows(
            "[:find ?k (sum ?r) :with ?c :where [?c :t/kind ?k] [?c :t/value _] \
             [(get-else $ ?c :t/rank 0) ?r]]",
        );
        assert_eq!(
            r,
            vec![
                j(serde_json::json!([":chat", 3])),
                j(serde_json::json!([":mail", 0]))
            ]
        );
    }

    // as-of: the value current at T, not a later or superseded one.
    #[pg_test]
    fn get_else_as_of_sees_the_value_at_t() {
        setup_chats();
        let e: i64 = Spi::get_one(
            "SELECT e FROM mentat.current_text WHERE v = 'a' AND a = \
             (SELECT entid FROM mentat.idents WHERE ident = ':t/value')",
        )
        .expect("e")
        .expect("NULL");
        let t1: i64 = Spi::get_one("SELECT max(tx) FROM mentat.transactions")
            .expect("t1")
            .expect("NULL");
        // After T1: "a" becomes private, then its rank changes twice.
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {e} :t/private true]]'::TEXT)"
        ))
        .expect("tx2");
        Spi::run(&format!("SELECT edn_t('[[:db/add {e} :t/rank 7]]'::TEXT)")).expect("tx3");
        Spi::run(&format!("SELECT edn_t('[[:db/add {e} :t/rank 9]]'::TEXT)")).expect("tx4");
        let q = "[:find ?v ?p ?r :where [?c :t/kind :chat] [?c :t/value ?v] \
                 [(get-else $ ?c :t/private false) ?p] [(get-else $ ?c :t/rank 0) ?r]]";
        assert_eq!(
            rows_opts(q, &format!("{{\"asOf\": {t1}}}")),
            vec![
                j(serde_json::json!(["a", false, 3])),
                j(serde_json::json!(["b", true, 0])),
            ]
        );
        assert_eq!(
            rows(q),
            vec![
                j(serde_json::json!(["a", true, 9])),
                j(serde_json::json!(["b", true, 0]))
            ]
        );
    }

    // Datomic rejects get-else on a cardinality-many attribute; so do we,
    // with a clear error instead of silently multiplying rows.
    #[pg_test]
    fn get_else_on_cardinality_many_is_an_error() {
        setup_chats();
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
        let err = Spi::get_one::<String>(
            "SELECT pg_temp.capture($q$SELECT edn_q('[:find ?v ?t :where [?c :t/kind :chat] \
             [?c :t/value ?v] [(get-else $ ?c :t/tag \"none\") ?t]]'::TEXT, '{}'::jsonb)$q$)",
        )
        .expect("capture ran")
        .unwrap_or_default();
        assert!(
            err.contains("get-else is not supported for cardinality-many attribute :t/tag"),
            "got: {err}"
        );
    }

    // missing? already worked with several patterns; keep it that way, and
    // combined with get-else in the same query.
    #[pg_test]
    fn missing_and_get_else_together_multi_pattern() {
        setup_chats();
        let r = rows(
            "[:find ?v ?r :where [?c :t/kind :chat] [?c :t/value ?v] \
             [(missing? $ ?c :t/private)] [(get-else $ ?c :t/rank 0) ?r]]",
        );
        assert_eq!(r, vec![j(serde_json::json!(["a", 3]))]);
    }
}
