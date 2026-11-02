// #384: a tempid used only as a ref value, never the entity of an assertion (and not
// upserting), fails the transaction with Datomic's :db.error/tempid-not-an-entity.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::datum::DatumWithOid;
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(edn TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN PERFORM edn_t(edn); RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
        t(r#"[{:db/ident :v/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity :db/index true}
              {:db/ident :v/uv :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/value :db/index true}
              {:db/ident :v/likes :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
              {:db/ident :v/part :db/valueType :db.type/ref :db/cardinality :db.cardinality/one
               :db/isComponent true}]"#);
        t(r#"[{:v/id "a" :v/uv "ua"}]"#);
    }

    fn t(edn: &str) -> serde_json::Value {
        let r = Spi::get_one_with_args::<String>("SELECT edn_t($1)", &[DatumWithOid::from(edn)])
            .unwrap_or_else(|e| panic!("{edn}: {e}"))
            .expect("NULL");
        serde_json::from_str(&r).expect("tx report")
    }

    fn err(edn: &str) -> String {
        Spi::get_one_with_args::<String>(
            "SELECT pg_temp.capture($1)",
            &[DatumWithOid::from(edn)],
        )
        .expect("capture ran")
        .unwrap_or_default()
    }

    fn rejects(edn: &str, tempid: &str) {
        let e = err(edn);
        assert!(
            e.contains(":db.error/tempid-not-an-entity") && e.contains(&format!("\"{tempid}\"")),
            "{edn}: {e:?}"
        );
    }

    fn likes() -> i64 {
        Spi::get_one::<i64>(
            "SELECT count(*) FROM mentat.datoms \
             WHERE a = (SELECT entid FROM mentat.idents WHERE ident = ':v/likes')",
        )
        .expect("count")
        .unwrap_or(0)
    }

    #[pg_test]
    fn tempid_used_only_as_a_value_is_rejected() {
        setup();
        let before = likes();
        rejects(r#"[{:v/id "d8" :v/likes ["t1" "t2"]}]"#, "t1");
        // Map form, cardinality-many: `[:v/uv "ua"]` is a list (DataScript's identity-only
        // rule), so "ua" is a tempid used only as a value.
        rejects(r#"[{:v/id "e3" :v/likes [:v/uv "ua"]}]"#, "ua");
        rejects(r#"[[:db/add "l" :v/id "l"] [:db/add "l" :v/likes "t3"]]"#, "t3");
        // Only the value of a component attribute: still an entity with no datoms.
        rejects(r#"[{:v/id "c" :v/part "p"}]"#, "p");
        // A cas whose new value is a fresh tempid.
        rejects(r#"[[:db/cas [:v/id "a"] :v/part nil "q"]]"#, "q");
        // "datomic.tx" is an ordinary tempid; the error says so.
        let e = err(r#"[{:v/id "x" :v/part "datomic.tx"}]"#);
        assert!(e.contains("no reserved transaction tempid"), "{e}");
        assert_eq!(likes(), before, "nothing committed");
    }

    #[pg_test]
    fn tempid_with_a_datom_or_an_upsert_is_fine() {
        setup();
        // Value and entity.
        let r = t(r#"[{:v/id "d" :v/likes ["t1"]} {:db/id "t1" :v/id "t1"}]"#);
        assert!(r["tempids"]["t1"].as_i64().is_some());
        // Upserts to the existing :v/id "a" and is used as a value.
        let r = t(r#"[[:db/add "u" :v/id "a"] [:db/add "e" :v/id "e"] [:db/add "e" :v/likes "u"]]"#);
        let a = Spi::get_one::<pgrx::JsonB>(
            r#"SELECT edn_q('[:find ?e . :where [?e :v/id "a"]]', '{}'::jsonb)"#,
        )
        .expect("edn_q")
        .expect("NULL");
        assert_eq!(r["tempids"]["u"].as_i64(), a.0["result"].as_i64());
        // List form `[:db/add e a [:v/uv "ua"]]` is a lookup ref, not a tempid.
        t(r#"[[:db/add "y" :v/id "y"] [:db/add "y" :v/likes [:v/uv "ua"]]]"#);
    }

    /// A tx-function's expansion is checked like any other tx-data.
    #[cfg(feature = "script")]
    #[pg_test]
    fn tx_fn_expansion_is_checked() {
        setup();
        t(r#"[{:db/ident :v/like-new :db/fn "(fn [db who] [[:db/add [:v/id who] :v/likes \"n\"]])"}
              {:db/ident :v/like-made :db/fn "(fn [db who] [[:db/add [:v/id who] :v/likes \"n\"] {:db/id \"n\" :v/id \"n\"}])"}]"#);
        rejects(r#"[[:v/like-new "a"]]"#, "n");
        t(r#"[[:v/like-made "a"]]"#);
    }
}
