// #414: Datomic's reserved tempid "datomic.tx" names the transaction being committed.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::datum::DatumWithOid;
    use pgrx::prelude::*;

    const SCHEMA: &str = r#"[{:db/ident :v/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                               :db/unique :db.unique/identity :db/index true}
                              {:db/ident :v/uv :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                               :db/unique :db.unique/value :db/index true}
                              {:db/ident :v/src :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                              {:db/ident :v/likes :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
                              {:db/ident :v/tx :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]"#;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(edn TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN PERFORM edn_t(edn); RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
        t(SCHEMA);
        t(r#"[{:v/id "a" :v/uv "ua"}]"#);
    }

    fn ts(store: &str, edn: &str) -> serde_json::Value {
        let r = Spi::get_one_with_args::<String>(
            "SELECT t($1, $2)",
            &[DatumWithOid::from(store), DatumWithOid::from(edn)],
        )
        .unwrap_or_else(|e| panic!("{edn}: {e}"))
        .expect("NULL");
        serde_json::from_str(&r).expect("tx report")
    }

    fn t(edn: &str) -> serde_json::Value {
        ts("default", edn)
    }

    fn err(edn: &str) -> String {
        Spi::get_one_with_args::<String>("SELECT pg_temp.capture($1)", &[DatumWithOid::from(edn)])
            .expect("capture ran")
            .unwrap_or_default()
    }

    fn qs(store: &str, query: &str, inputs: &str) -> serde_json::Value {
        Spi::get_one_with_args::<pgrx::JsonB>(
            "SELECT q($1, $2, $3::jsonb)",
            &[
                DatumWithOid::from(store),
                DatumWithOid::from(query),
                DatumWithOid::from(inputs),
            ],
        )
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .expect("NULL")
        .0["result"]
            .clone()
    }

    fn q(query: &str) -> serde_json::Value {
        qs("default", query, "{}")
    }

    fn tx(r: &serde_json::Value) -> i64 {
        r["db-after"]["basis-t"].as_i64().expect("basis-t")
    }

    #[pg_test]
    fn annotates_the_transaction_in_map_and_list_form() {
        setup();
        let r = t(r#"[{:db/id "datomic.tx" :v/src "import"} {:v/id "b"}]"#);
        assert_eq!(r["tempids"]["datomic.tx"].as_i64(), Some(tx(&r)), "{r}");
        assert_eq!(
            q(&format!("[:find ?s . :where [{} :v/src ?s]]", tx(&r))),
            serde_json::json!("import")
        );
        // Read back joined to the data the transaction wrote.
        assert_eq!(
            q(r#"[:find ?s . :where [?e :v/id "b" ?tx] [?tx :v/src ?s]]"#),
            serde_json::json!("import")
        );
        // In the report's tx-data, on the transaction entity.
        let src = Spi::get_one::<i64>("SELECT entid FROM mentat.idents WHERE ident = ':v/src'")
            .expect("ident")
            .expect("NULL");
        assert!(
            r["tx-data"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d[0].as_i64() == Some(tx(&r))
                    && d[1].as_i64() == Some(src)
                    && d[2] == serde_json::json!("import")),
            "{r}"
        );

        let r = t(r#"[[:db/add "datomic.tx" :v/src "list"] [:db/add "c" :v/id "c"]]"#);
        assert_eq!(r["tempids"]["datomic.tx"].as_i64(), Some(tx(&r)), "{r}");
        // Through the log (its range is (start, end]).
        let log = Spi::get_one_with_args::<pgrx::JsonB>(
            "SELECT log('default', $1 - 1, $1)",
            &[DatumWithOid::from(tx(&r))],
        )
        .expect("log")
        .expect("NULL")
        .0;
        assert!(
            log.as_array()
                .unwrap()
                .iter()
                .any(|d| d["e"].as_i64() == Some(tx(&r)) && d["v"] == serde_json::json!("list")),
            "{log}"
        );

        // Not used: not reported.
        let r = t(r#"[{:v/id "d"}]"#);
        assert!(r["tempids"].get("datomic.tx").is_none(), "{r}");
    }

    #[pg_test]
    fn as_a_ref_value_and_as_of() {
        setup();
        let before = tx(&t(r#"[{:v/id "x"}]"#));
        let r = t(r#"[{:v/id "x" :v/tx "datomic.tx"} {:db/id "datomic.tx" :v/src "s"}]"#);
        assert_eq!(
            q(r#"[:find ?t . :where [?e :v/id "x"] [?e :v/tx ?t]]"#),
            serde_json::json!(tx(&r))
        );
        // Used only as a value, it is still the transaction (it has a :db/txInstant).
        let r2 = t(r#"[{:v/id "y" :v/tx "datomic.tx"}]"#);
        assert_eq!(r2["tempids"]["datomic.tx"].as_i64(), Some(tx(&r2)), "{r2}");

        let query = r#"[:find ?s . :where [?e :v/id "x"] [?e :v/tx ?t] [?t :v/src ?s]]"#;
        let as_of = |t: i64| qs("default", query, &format!("{{\"asOf\": {t}}}"));
        assert_eq!(as_of(before), serde_json::Value::Null);
        assert_eq!(as_of(tx(&r)), serde_json::json!("s"));
    }

    #[pg_test]
    fn a_value_only_tempid_beside_it_still_fails() {
        setup();
        let e = err(r#"[{:db/id "datomic.tx" :v/likes ["t9"]}]"#);
        assert!(
            e.contains(":db.error/tempid-not-an-entity") && e.contains("\"t9\""),
            "{e}"
        );
    }

    #[pg_test]
    fn explicit_tx_instant() {
        setup();
        let r = t(
            r#"[{:db/id "datomic.tx" :db/txInstant #inst "2017-06-16T00:56:41.257Z"} {:v/id "i"}]"#,
        );
        let t_ = tx(&r);
        let micros = 1_497_574_601_257_000_i64;
        // The report, the log, the current state and mentat.transactions all have it,
        // once.
        let inst: Vec<_> = r["tx-data"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d[1].as_i64() == Some(50))
            .collect();
        assert_eq!(inst.len(), 1, "{r}");
        assert_eq!(inst[0][2]["v"].as_i64(), Some(micros), "{r}");
        let one = |sql: &str| {
            Spi::get_one_with_args::<i64>(sql, &[DatumWithOid::from(t_)])
                .expect(sql)
                .expect("NULL")
        };
        let micros_of = crate::types::instant::sql_micros_from_instant;
        let at = format!("{} = {micros}", micros_of("v"));
        assert_eq!(
            one(&format!(
                "SELECT count(*) FROM mentat.datoms_instant_new WHERE e = $1 AND a = 50 AND {at}"
            )),
            1
        );
        assert_eq!(
            one("SELECT count(*) FROM mentat.datoms_instant_new WHERE e = $1 AND a = 50"),
            1
        );
        assert_eq!(
            one(&format!(
                "SELECT count(*) FROM mentat.current_instant WHERE e = $1 AND a = 50 AND {at}"
            )),
            1
        );
        assert_eq!(
            one(&format!(
                "SELECT count(*) FROM mentat.transactions WHERE tx = $1 AND {} = {micros}",
                micros_of("tx_instant")
            )),
            1
        );
        // Two different instants are a cardinality conflict.
        let e = err(
            r#"[[:db/add "datomic.tx" :db/txInstant #inst "2017-06-16T00:00:00Z"]
                [:db/add "datomic.tx" :db/txInstant #inst "2018-06-16T00:00:00Z"]]"#,
        );
        assert!(e.contains(":db.error/cardinality-violation"), "{e}");
    }

    #[pg_test]
    fn a_unique_value_another_entity_holds_is_a_conflict() {
        setup();
        for (edn, attr, value) in [
            (r#"[{:db/id "datomic.tx" :v/id "a"}]"#, ":v/id", "\"a\""),
            (r#"[{:db/id "datomic.tx" :v/uv "ua"}]"#, ":v/uv", "\"ua\""),
            (
                r#"[{:db/id "datomic.tx" :v/uv "n"} {:v/id "p" :v/uv "n"}]"#,
                ":v/uv",
                "\"n\"",
            ),
            (
                r#"[{:db/id "datomic.tx" :v/id "n"} {:db/id "p" :v/id "n"}]"#,
                ":v/id",
                "\"n\"",
            ),
        ] {
            let e = err(edn);
            assert!(
                e.contains(":db.error/unique-conflict")
                    && e.contains(&format!("'{attr}'"))
                    && e.contains(&format!("value {value} already exists")),
                "{edn}: {e}"
            );
        }
        assert_eq!(
            q("[:find (count ?e) . :where [?e :v/uv _]]"),
            serde_json::json!(1)
        );
        t(r#"[{:db/id "datomic.tx" :v/uv "free"}]"#);
    }

    #[pg_test]
    fn on_a_named_store() {
        setup();
        Spi::run("SELECT create_store('txtemp')").expect("create_store");
        ts("txtemp", SCHEMA);
        let r = ts(
            "txtemp",
            r#"[{:db/id "datomic.tx" :v/src "named"} {:v/id "n1" :v/tx "datomic.tx"}]"#,
        );
        assert_eq!(r["tempids"]["datomic.tx"].as_i64(), Some(tx(&r)), "{r}");
        assert_eq!(
            qs(
                "txtemp",
                r#"[:find ?s . :where [?e :v/id "n1" ?tx] [?tx :v/src ?s] [?e :v/tx ?tx]]"#,
                "{}"
            ),
            serde_json::json!("named")
        );
        // Not on the default store.
        assert_eq!(
            q("[:find (count ?s) . :where [_ :v/src ?s]]"),
            serde_json::json!(0)
        );
    }

    /// The transaction's own annotations are not entid collisions; a real one still is.
    #[pg_test]
    fn collision_report_skips_annotations() {
        setup();
        t(r#"[{:db/id "datomic.tx" :v/src "note"} {:v/id "k"}]"#);
        let count = || {
            Spi::get_one::<i64>("SELECT mentat.entid_collision_count()")
                .expect("count")
                .expect("NULL")
        };
        assert_eq!(count(), 0, "an annotation is not a collision");
        assert_eq!(
            Spi::get_one::<i64>("SELECT mentat.repair_entid_collisions(false)")
                .expect("repair")
                .expect("NULL"),
            0
        );
        assert_eq!(
            q(r#"[:find ?s . :where [?e :v/id "k" ?tx] [?tx :v/src ?s]]"#),
            serde_json::json!("note"),
            "the annotation stays on the transaction"
        );

        // A user entity whose id is also a transaction id (an overflowed sequence) is.
        let u = t(r#"[{:db/id "u" :v/src "needle"}]"#)["tempids"]["u"]
            .as_i64()
            .expect("u");
        Spi::run(&format!(
            "INSERT INTO mentat.transactions (tx, tx_instant) VALUES ({u}, NOW()); \
             INSERT INTO mentat.datoms_instant_new (store_id, e, a, v, tx, added) \
             VALUES (0, {u}, 50, NOW(), {u}, true)"
        ))
        .expect("forge");
        assert_eq!(count(), 1, "a real collision is still reported");
    }

    /// A tx-function may return {:db/id "datomic.tx" ...}.
    #[cfg(feature = "script")]
    #[pg_test]
    fn from_a_tx_fn() {
        setup();
        t(r#"[{:db/ident :v/stamp :db/fn "(fn [db who] [{:db/id \"datomic.tx\" :v/src who}])"}]"#);
        let r = t(r#"[[:v/stamp "bob"] {:v/id "f"}]"#);
        assert_eq!(
            q(r#"[:find ?s . :where [?e :v/id "f" ?tx] [?tx :v/src ?s]]"#),
            serde_json::json!("bob")
        );
        assert_eq!(r["tempids"]["datomic.tx"].as_i64(), Some(tx(&r)), "{r}");
    }
}
