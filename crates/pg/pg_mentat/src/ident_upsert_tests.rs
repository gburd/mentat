// Re-asserting an existing :db/ident (#415). :db/ident is
// :db.unique/identity, so a map or list entity whose :db/ident names an
// existing ident upserts into the entity that has it, as in Datomic and on
// the embedded engine (tests/ident_upsert.rs). pg_mentat allocated a new
// entid for the map and moved the ident to it (`ON CONFLICT (ident) DO
// UPDATE SET entid = EXCLUDED.entid`), while the other datoms of the map went
// to the old entity: enum values split, and a tx-fn could not be redefined.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup(store: &str) {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        if store != "default" {
            Spi::run(&format!("SELECT create_store('{store}')")).expect("create_store");
        }
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
    }

    fn tx(store: &str, edn: &str) -> serde_json::Value {
        let out = Spi::get_one::<String>(&format!(
            "SELECT t('{store}', '{}')",
            edn.replace('\'', "''")
        ))
        .unwrap_or_else(|e| panic!("{edn}: {e}"))
        .expect("tx report");
        serde_json::from_str(&out).expect("tx report JSON")
    }

    fn tx_err(store: &str, edn: &str) -> String {
        let stmt = format!("SELECT t('{store}', '{}')", edn.replace('\'', "''"));
        Spi::get_one::<String>(&format!(
            "SELECT pg_temp.capture('{}')",
            stmt.replace('\'', "''")
        ))
        .expect("capture")
        .unwrap_or_default()
    }

    fn q(store: &str, query: &str) -> serde_json::Value {
        Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT q('{store}', '{}', '{{}}'::jsonb)",
            query.replace('\'', "''")
        ))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .map(|j| j.0)
        .unwrap_or(serde_json::Value::Null)["result"]
            .clone()
    }

    fn schema_of(store: &str) -> String {
        if store == "default" {
            "mentat".into()
        } else {
            format!("mentat_{store}")
        }
    }

    /// The entid `<store>.idents` maps `ident` to.
    fn ident_entid(store: &str, ident: &str) -> Option<i64> {
        Spi::get_one::<i64>(&format!(
            "SELECT (SELECT entid FROM {}.idents WHERE ident = '{ident}')",
            schema_of(store)
        ))
        .expect("idents")
    }

    /// The entity whose live :db/ident datom is `ident`.
    fn ident_entity(store: &str, ident: &str) -> serde_json::Value {
        q(
            store,
            &format!("[:find ?e . :where [?e :db/ident {ident}]]"),
        )
    }

    fn len(v: &serde_json::Value) -> usize {
        v.as_array().map(Vec::len).unwrap_or(0)
    }

    fn reassert_attribute_ident_with_doc(s: &str) {
        tx(
            s,
            "[{:db/ident :tf/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let n = ident_entid(s, ":tf/n").expect(":tf/n");
        tx(s, "[{:tf/n 7}]");
        let r = tx(s, r#"[{:db/ident :tf/n :db/doc "the n"}]"#);
        assert_eq!(r["tempids"], serde_json::json!({}), "{r}");
        assert_eq!(ident_entid(s, ":tf/n"), Some(n), "the ident did not move");
        let doc = q(
            s,
            "[:find ?d . :where [?a :db/ident :tf/n] [?a :db/doc ?d]]",
        );
        assert_eq!(doc, "the n");
        assert_eq!(ident_entity(s, ":tf/n"), n);
        tx(s, r#"[{:db/ident :tf/n :db/doc "again"}]"#);
        assert_eq!(ident_entid(s, ":tf/n"), Some(n));
        let doc = q(
            s,
            "[:find ?d . :where [?a :db/ident :tf/n] [?a :db/doc ?d]]",
        );
        assert_eq!(doc, "again");
        // Queries and writes of the attribute still work.
        tx(s, "[{:tf/n 8}]");
        assert_eq!(len(&q(s, "[:find [?v ...] :where [_ :tf/n ?v]]")), 2);
        // List form: the ident names the entity too.
        tx(
            s,
            r#"[[:db/add "x" :db/ident :tf/n] [:db/add "x" :db/doc "list"]]"#,
        );
        assert_eq!(ident_entid(s, ":tf/n"), Some(n));
        let doc = q(
            s,
            "[:find ?d . :where [?a :db/ident :tf/n] [?a :db/doc ?d]]",
        );
        assert_eq!(doc, "list");
    }

    #[pg_test]
    fn ident_reassert_attribute_with_doc() {
        setup("default");
        reassert_attribute_ident_with_doc("default");
    }

    #[pg_test]
    fn ident_reassert_attribute_with_doc_named_store() {
        setup("id415");
        reassert_attribute_ident_with_doc("id415");
    }

    fn reassert_enum_ident(s: &str) {
        tx(
            s,
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
              {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
        );
        tx(s, "[{:db/ident :color/red}]");
        let red = ident_entid(s, ":color/red").expect(":color/red");
        tx(s, r#"[{:c/name "car" :c/color :color/red}]"#);
        tx(s, r#"[{:db/ident :color/red :db/doc "red"}]"#);
        assert_eq!(ident_entid(s, ":color/red"), Some(red));
        tx(s, r#"[{:c/name "rose" :c/color :color/red}]"#);
        let q1 =
            "[:find [?n ...] :where [?c :db/ident :color/red] [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(len(&q(s, q1)), 2, "{}", q(s, q1));
        let q2 = "[:find [?n ...] :where [?e :c/color :color/red] [?e :c/name ?n]]";
        assert_eq!(len(&q(s, q2)), 2, "{}", q(s, q2));
        let q3 = "[:find [?c ...] :where [_ :c/color ?c]]";
        assert_eq!(q(s, q3), serde_json::json!([red]), "one red entity");
        // One existing and one new enum ident in one transaction.
        tx(
            s,
            r#"[{:db/ident :color/red :db/doc "still red"} {:db/ident :color/blue}]"#,
        );
        assert_eq!(ident_entid(s, ":color/red"), Some(red));
        let blue = ident_entid(s, ":color/blue").expect(":color/blue");
        assert_ne!(blue, red);
        assert_eq!(ident_entity(s, ":color/blue"), blue);
        assert_eq!(
            len(&q(s, "[:find [?e ...] :where [?e :db/ident :color/red]]")),
            1
        );
    }

    #[pg_test]
    fn ident_reassert_enum() {
        setup("default");
        reassert_enum_ident("default");
    }

    #[pg_test]
    fn ident_reassert_enum_named_store() {
        setup("id415e");
        reassert_enum_ident("id415e");
    }

    /// Redefining a tx-fn's :db/fn twice: the newest body runs. (Tx-fns run on
    /// the default store only, #389.)
    #[pg_test]
    fn ident_redefine_tx_fn() {
        setup("default");
        let s = "default";
        tx(
            s,
            "[{:db/ident :tf/v :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let def = |n: i64| {
            tx(
                s,
                &format!(r#"[{{:db/ident :x/p :db/fn "(fn [db e] [[:db/add e :tf/v {n}]])"}}]"#),
            )
        };
        def(1);
        let p = ident_entid(s, ":x/p").expect(":x/p");
        def(2);
        def(3);
        assert_eq!(ident_entid(s, ":x/p"), Some(p));
        let r = tx(s, r#"[[:x/p "e"]]"#);
        let e = r["tempids"]["e"].as_i64().expect("tempid e");
        assert_eq!(
            q(s, &format!("[:find ?v . :where [{e} :tf/v ?v]]")),
            serde_json::json!(3)
        );
    }

    fn idempotent_schema_install(s: &str) {
        let schema = r#"[{:db/ident :si/a :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/doc "a"}
                        {:db/ident :si/b :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
                        {:db/ident :si/u :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                        {:db/ident :si.e/x} {:db/ident :si.e/y :db/doc "y"}]"#;
        let idents = [":si/a", ":si/b", ":si/u", ":si.e/x", ":si.e/y"];
        tx(s, schema);
        let before: Vec<_> = idents.iter().map(|i| ident_entid(s, i)).collect();
        tx(s, r#"[{:si/a "v" :si/u "k" :si/b :si.e/x}]"#);
        tx(s, schema);
        tx(s, schema);
        let after: Vec<_> = idents.iter().map(|i| ident_entid(s, i)).collect();
        assert_eq!(before, after);
        for (i, e) in idents.iter().zip(&after) {
            assert_eq!(ident_entity(s, i), e.expect("entid"), "{i}");
        }
        let q1 = "[:find ?a . :where [?e :si/b :si.e/x] [?e :si/a ?a]]";
        assert_eq!(q(s, q1), "v");
    }

    #[pg_test]
    fn ident_idempotent_schema_install() {
        setup("default");
        idempotent_schema_install("default");
    }

    #[pg_test]
    fn ident_idempotent_schema_install_named_store() {
        setup("id415s");
        idempotent_schema_install("id415s");
    }

    /// Asserting an ident another entity holds, without retracting it there,
    /// is a unique conflict, not a silent move.
    #[pg_test]
    fn ident_taken_by_another_entity_is_an_error() {
        setup("default");
        let s = "default";
        tx(s, "[{:db/ident :r/a}]");
        let a = ident_entid(s, ":r/a").expect(":r/a");
        let r = tx(s, r#"[{:db/id "b" :db/doc "b"}]"#);
        let b = r["tempids"]["b"].as_i64().expect("b");
        let err = tx_err(s, &format!("[[:db/add {b} :db/ident :r/a]]"));
        assert!(!err.is_empty(), "stealing :r/a must fail");
        assert_eq!(ident_entid(s, ":r/a"), Some(a));
    }

    /// Datomic's rename: assert a new ident on the entity.
    #[pg_test]
    fn ident_rename() {
        setup("default");
        let s = "default";
        tx(s, "[{:db/ident :r/old}]");
        let e = ident_entid(s, ":r/old").expect(":r/old");
        tx(s, &format!("[[:db/add {e} :db/ident :r/new]]"));
        assert_eq!(ident_entid(s, ":r/new"), Some(e));
        assert_eq!(ident_entity(s, ":r/new"), e);
        assert_eq!(ident_entid(s, ":r/old"), None, "the old name is gone");
    }

    /// Moving an ident: retract it from its entity, then assert it on another
    /// in a later transaction (master's behavior, kept).
    #[pg_test]
    fn ident_move_after_retract() {
        setup("default");
        let s = "default";
        tx(s, "[{:db/ident :r/m}]");
        let a = ident_entid(s, ":r/m").expect(":r/m");
        let b = tx(s, r#"[{:db/id "b" :db/doc "b"}]"#)["tempids"]["b"]
            .as_i64()
            .expect("b");
        // In one transaction: still taken when the assertion is checked.
        let err = tx_err(
            s,
            &format!("[[:db/retract {a} :db/ident :r/m] [:db/add {b} :db/ident :r/m]]"),
        );
        assert!(!err.is_empty());
        assert_eq!(ident_entid(s, ":r/m"), Some(a));
        tx(s, &format!("[[:db/retract {a} :db/ident :r/m]]"));
        tx(s, &format!("[[:db/add {b} :db/ident :r/m]]"));
        assert_eq!(ident_entid(s, ":r/m"), Some(b));
        assert_eq!(ident_entity(s, ":r/m"), b);
    }

    /// Two maps that name one new ident are one entity.
    #[pg_test]
    fn ident_new_ident_twice_in_one_transaction() {
        setup("default");
        let s = "default";
        tx(
            s,
            r#"[{:db/ident :r/n :db/doc "n"} {:db/id "x" :db/ident :r/n}]"#,
        );
        let n = ident_entid(s, ":r/n").expect(":r/n");
        assert_eq!(ident_entity(s, ":r/n"), n);
        let doc = q(s, "[:find ?d . :where [?e :db/ident :r/n] [?e :db/doc ?d]]");
        assert_eq!(doc, "n");
    }
}
