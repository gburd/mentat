// Mozilla-era issues fixed in 1.18.0: pg_mentat agrees with the embedded
// engine on each (see docs/design/mozilla-issue-triage-2026-10-21.md).

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._test_error_msg(stmt TEXT) RETURNS TEXT
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN ''::TEXT;
             EXCEPTION WHEN OTHERS THEN
                 RETURN SQLERRM;
             END;
             $$",
        )
        .expect("create error_msg helper");
    }

    /// SQLERRM of `sql` run in a subtransaction ("" when it succeeds).
    fn error_message(sql: &str) -> String {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<String>(&format!("SELECT mentat._test_error_msg('{}')", escaped))
            .expect("error_msg call")
            .unwrap_or_default()
    }

    fn transact(edn: &str) -> String {
        error_message(&format!(
            "SELECT edn_t('{}'::TEXT)",
            edn.replace('\'', "''")
        ))
    }

    // #286: a new attribute needs :db/ident, :db/valueType and :db/cardinality.
    #[pg_test]
    fn new_attribute_needs_minimal_set() {
        setup();
        let e = transact(
            r#"[[:db/add "p" :db/ident :mi/prop] [:db/add "p" :db/valueType :db.type/string]]"#,
        );
        assert!(e.contains("missing :db/cardinality"), "{e}");
        let e = transact(
            r#"[[:db/add "p" :db/ident :mi/prop] [:db/add "p" :db/cardinality :db.cardinality/one]]"#,
        );
        assert!(e.contains("missing :db/valueType"), "{e}");
        let e = transact(
            r#"[[:db/add "p" :db/valueType :db.type/string] [:db/add "p" :db/cardinality :db.cardinality/one]]"#,
        );
        assert!(e.contains("missing :db/ident"), "{e}");

        // A bare ident is fine; so is the rest of the set in a later transaction.
        assert_eq!(transact(r#"[[:db/add "p" :db/ident :mi/prop]]"#), "");
        assert_eq!(
            transact("[[:db/add :mi/prop :db/valueType :db.type/string] [:db/add :mi/prop :db/cardinality :db.cardinality/one]]"),
            ""
        );
        // Altering an existing attribute needs only the changed piece.
        assert_eq!(transact("[[:db/add :mi/prop :db/index true]]"), "");
        assert_eq!(
            transact("[[:db/add :mi/prop :db/cardinality :db.cardinality/many]]"),
            ""
        );
        let row = Spi::get_one::<String>(
            "SELECT value_type::text || ' ' || cardinality::text || ' ' || indexed::text \
             FROM mentat.schema WHERE ident = ':mi/prop'",
        )
        .expect("schema row")
        .expect("non-NULL");
        assert_eq!(row, "string many true");
        // The type cannot change; re-asserting it can.
        let e = transact("[[:db/add :mi/prop :db/valueType :db.type/long]]");
        assert!(
            e.contains("Schema alteration for existing attribute :mi/prop is not valid"),
            "{e}"
        );
        assert_eq!(
            transact("[[:db/add :mi/prop :db/valueType :db.type/string]]"),
            ""
        );
    }

    fn tx(edn: &str) -> serde_json::Value {
        let raw = Spi::get_one::<String>(&format!(
            "SELECT edn_t('{}'::TEXT)::TEXT",
            edn.replace('\'', "''")
        ))
        .expect("transact")
        .expect("non-NULL report");
        serde_json::from_str(&raw).expect("parse report")
    }

    fn q(query: &str) -> serde_json::Value {
        let raw = Spi::get_one::<String>(&format!(
            "SELECT edn_q('{}'::TEXT, '{{}}'::jsonb)::TEXT",
            query.replace('\'', "''")
        ))
        .expect("query ran")
        .expect("non-NULL result");
        serde_json::from_str(&raw).expect("parse result")
    }

    // #249: min/max over refs, including transaction ids (agrees with the embedded engine).
    #[pg_test]
    fn min_max_ref() {
        setup();
        tx(
            "[{:db/ident :mi/r :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
             {:db/ident :mi/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let first = tx(r#"[{:db/id "a" :mi/n 1}]"#);
        let a = first["tempids"]["a"].as_i64().unwrap();
        let last = tx(r#"[{:db/id "b" :mi/n 2}]"#);
        let b = last["tempids"]["b"].as_i64().unwrap();
        tx(&format!(
            "[[:db/add {a} :mi/r {a}] [:db/add {a} :mi/r {b}]]"
        ));
        let first_tx = first["db-after"]["basis-t"].as_i64().unwrap();
        let last_tx = last["db-after"]["basis-t"].as_i64().unwrap();

        assert_eq!(
            q("[:find (max ?tx) . :where [_ :mi/n _ ?tx]]")["result"],
            last_tx
        );
        assert_eq!(
            q("[:find (min ?tx) . :where [_ :mi/n _ ?tx]]")["result"],
            first_tx
        );
        assert_eq!(
            q("[:find (max ?v) . :where [_ :mi/r ?v]]")["result"],
            a.max(b)
        );
        assert_eq!(
            q("[:find (min ?v) . :where [_ :mi/r ?v]]")["result"],
            a.min(b)
        );
        assert_eq!(
            q("[:find ?e (max ?v) :where [?e :mi/r ?v]]")["results"],
            serde_json::json!([[a, a.max(b)]])
        );
    }
}
