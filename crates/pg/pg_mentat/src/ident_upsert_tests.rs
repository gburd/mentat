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

    /// Redefining a tx-fn's :db/fn twice: the newest body runs.
    fn redefine_tx_fn(s: &str) {
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

    #[pg_test]
    fn ident_redefine_tx_fn() {
        setup("default");
        redefine_tx_fn("default");
    }

    #[pg_test]
    fn ident_redefine_tx_fn_named_store() {
        setup("id415f");
        redefine_tx_fn("id415f");
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

    /// Datomic's documented rename form names the entity by its old ident
    /// ([{:db/id :old :db/ident :new}]); the old ident resolves for the rest
    /// of the transaction, as against the database before it.
    fn rename_by_old_ident(s: &str) {
        tx(
            s,
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
              {:db/ident :c/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        tx(s, "[{:db/ident :color/blue} {:db/ident :color/red}]");
        let blue = ident_entid(s, ":color/blue").unwrap();
        let red = ident_entid(s, ":color/red").unwrap();
        let n = ident_entid(s, ":c/n").unwrap();
        tx(
            s,
            r#"[{:db/id :color/blue :db/ident :color/azure} {:db/id "sea" :c/color :color/blue}]"#,
        );
        assert_eq!(ident_entid(s, ":color/azure"), Some(blue));
        assert_eq!(ident_entity(s, ":color/azure"), blue);
        assert_eq!(ident_entid(s, ":color/blue"), None);
        assert_eq!(
            q(s, "[:find ?c . :where [_ :c/color ?c]]"),
            blue,
            "the old ident named the entity in its own transaction"
        );
        tx(s, "[[:db/add :color/red :db/ident :color/scarlet]]");
        assert_eq!(ident_entid(s, ":color/scarlet"), Some(red));
        // An attribute: its schema row is renamed with it.
        tx(s, "[{:db/id :c/n :db/ident :c/num}]");
        assert_eq!(ident_entid(s, ":c/num"), Some(n));
        tx(s, r#"[{:db/id "x" :c/num 1}]"#);
        assert_eq!(q(s, "[:find ?v . :where [_ :c/num ?v]]"), 1);
        // In a later transaction the old ident is gone (no alias).
        let err = tx_err(s, r#"[{:db/id :color/blue :db/doc "b"}]"#);
        assert!(err.contains(":color/blue"), "{err}");
    }

    #[pg_test]
    fn ident_rename_by_old_ident() {
        setup("default");
        rename_by_old_ident("default");
    }

    #[pg_test]
    fn ident_rename_by_old_ident_named_store() {
        setup("id415n");
        rename_by_old_ident("id415n");
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

    /// A map that names a new ident and upserts through another
    /// :db.unique/identity attribute: the ident goes to the entity the map
    /// upserts into, with the rest of the map.
    fn new_ident_on_an_upserted_entity(s: &str) {
        tx(
            s,
            "[{:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity}
              {:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]",
        );
        tx(s, r#"[{:c/name "sea"}]"#);
        let sea = q(s, r#"[:find ?e . :where [?e :c/name "sea"]]"#)
            .as_i64()
            .unwrap();
        let r = tx(s, r#"[{:db/ident :color/brand-new :c/name "sea"}]"#);
        assert_eq!(ident_entid(s, ":color/brand-new"), Some(sea), "{r}");
        assert_eq!(ident_entity(s, ":color/brand-new"), sea);
        assert!(moved_idents(s).is_empty());
        tx(s, r#"[{:c/name "car" :c/color :color/brand-new}]"#);
        assert_eq!(
            q(
                s,
                "[:find ?n . :where [?c :db/ident :color/brand-new] [?e :c/color ?c] [?e :c/name ?n]]"
            ),
            "car"
        );
        // Through a tempid, and an existing ident upserting the same way.
        tx(s, r#"[{:db/id "t" :db/ident :color/other :c/name "lake"}]"#);
        tx(s, r#"[{:db/id "u" :db/ident :color/third :c/name "lake"}]"#);
        let lake = q(s, r#"[:find ?e . :where [?e :c/name "lake"]]"#)
            .as_i64()
            .unwrap();
        assert_eq!(ident_entid(s, ":color/third"), Some(lake));
        assert_eq!(ident_entid(s, ":color/other"), None, "renamed");
        // An ident another entity holds still can't be taken this way.
        let err = tx_err(s, r#"[{:db/ident :color/third :c/name "sea"}]"#);
        assert!(err.contains("Conflicting upsert"), "{err}");
        assert_eq!(ident_entid(s, ":color/third"), Some(lake));
        // The transaction never upserts, through :db/ident either.
        let err = tx_err(s, r#"[{:db/id "datomic.tx" :db/ident :color/third}]"#);
        assert!(err.contains(":db/ident of entity"), "{err}");
        assert_eq!(ident_entid(s, ":color/third"), Some(lake));
    }

    #[pg_test]
    fn ident_new_ident_on_an_upserted_entity() {
        setup("default");
        new_ident_on_an_upserted_entity("default");
    }

    #[pg_test]
    fn ident_new_ident_on_an_upserted_entity_named_store() {
        setup("id415u");
        new_ident_on_an_upserted_entity("id415u");
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

    /// A store as 1.18.0 left it after `[{:db/ident I :db/doc ...}]`: the
    /// idents row names a new entity N, and the :db/ident datom stays on O.
    /// (The transactor no longer writes this, so the test moves the row as
    /// the old ON CONFLICT did, then writes through it.)
    fn break_like_118(s: &str, ident: &str) -> (i64, i64) {
        let owner = ident_entid(s, ident).expect("ident");
        let moved = Spi::get_one::<i64>(&format!(
            "SELECT nextval('{}.partition_user_seq')",
            schema_of(s)
        ))
        .unwrap()
        .unwrap();
        Spi::run(&format!(
            "UPDATE {}.idents SET entid = {moved} WHERE ident = '{ident}'; \
             UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = '{s}'",
            schema_of(s)
        ))
        .expect("move the ident");
        (owner, moved)
    }

    fn moved_idents(s: &str) -> Vec<serde_json::Value> {
        let r = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.ident), '[]') \
               FROM mentat.moved_idents('{s}') m"
        ))
        .unwrap()
        .unwrap();
        r.0.as_array().cloned().unwrap_or_default()
    }

    fn repair(s: &str, dry_run: bool) -> Vec<serde_json::Value> {
        let r = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT COALESCE(jsonb_agg(to_jsonb(m) ORDER BY m.ident), '[]') \
               FROM mentat.repair_moved_idents('{s}', {dry_run}) m"
        ))
        .unwrap()
        .unwrap();
        r.0.as_array().cloned().unwrap_or_default()
    }

    fn repair_moved_enum(s: &str) {
        tx(
            s,
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
              {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
              {:db/ident :c/rank :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        tx(
            s,
            "[{:db/ident :color/red} {:db/ident :color/blue} {:db/ident :color/green}]",
        );
        tx(s, r#"[{:c/name "car" :c/color :color/red}]"#);
        assert!(moved_idents(s).is_empty(), "a healthy store has none");
        let (red, red2) = break_like_118(s, ":color/red");
        // Written after the move: a ref to N, and a datom on N.
        tx(
            s,
            r#"[{:c/name "rose" :c/color :color/red} [:db/add :color/red :c/rank 1]]"#,
        );
        // green moved too, and both entities got :c/rank: not repairable.
        tx(s, "[[:db/add :color/green :c/rank 3]]");
        let (_green, green2) = break_like_118(s, ":color/green");
        tx(s, "[[:db/add :color/green :c/rank 4]]");

        let found = moved_idents(s);
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(found[1]["ident"], ":color/red");
        assert_eq!(found[1]["owner"], red);
        assert_eq!(found[1]["moved_to"], red2);
        assert_eq!(found[1]["n_datoms"], 1);
        assert_eq!(found[1]["n_refs"], 1);
        assert_eq!(found[1]["blocker"], serde_json::Value::Null);
        assert_eq!(found[0]["moved_to"], green2);
        assert!(found[0]["blocker"].is_string(), "{found:?}");
        // The split: the datom query finds only the car.
        let q1 =
            "[:find [?n ...] :where [?c :db/ident :color/red] [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(len(&q(s, q1)), 1);

        let dry = repair(s, true);
        assert_eq!(dry[1]["action"], "would repair", "{dry:?}");
        assert_eq!(moved_idents(s).len(), 2, "a dry run changes nothing");
        let done = repair(s, false);
        assert_eq!(done[1]["action"], "repaired", "{done:?}");
        assert!(done[0]["action"].as_str().unwrap().starts_with("skipped"));

        assert_eq!(ident_entid(s, ":color/red"), Some(red));
        assert_eq!(len(&q(s, q1)), 2, "{}", q(s, q1));
        let q2 = "[:find [?n ...] :where [?e :c/color :color/red] [?e :c/name ?n]]";
        assert_eq!(len(&q(s, q2)), 2);
        assert_eq!(
            q(
                s,
                "[:find ?r . :where [?c :db/ident :color/red] [?c :c/rank ?r]]"
            ),
            1
        );
        let left = moved_idents(s);
        assert_eq!(left.len(), 1, "only green is left: {left:?}");
        // New writes resolve to the owner, and history keeps the tx.
        tx(s, r#"[{:c/name "fire" :c/color :color/red}]"#);
        assert_eq!(len(&q(s, q1)), 3);
        let h = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT q('{s}', '[:find ?e :where [?e :c/color {red2} _ _]]', '{{\"history\": true}}'::jsonb)"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(
            len(&h.0["results"]),
            0,
            "no ref to N is left in the log: {}",
            h.0
        );
    }

    /// Before the repair, re-asserting the ident names its owner, and the
    /// transactor does not move the row from an entity that has data.
    #[pg_test]
    fn ident_reassert_on_a_moved_ident() {
        setup("default");
        let s = "default";
        tx(
            s,
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]",
        );
        tx(s, "[{:db/ident :color/red} {:db/ident :color/blue}]");
        let (red, _) = break_like_118(s, ":color/red");
        tx(s, "[{:c/color :color/red}]");
        let err = tx_err(s, r#"[{:db/ident :color/red :db/doc "r"}]"#);
        assert!(err.contains("moved_idents('default')"), "{err}");
        // An empty moved row is repointed by the re-assert itself.
        let (blue, _) = break_like_118(s, ":color/blue");
        tx(s, r#"[{:db/ident :color/blue :db/doc "b"}]"#);
        assert_eq!(ident_entid(s, ":color/blue"), Some(blue));
        assert_ne!(ident_entid(s, ":color/red"), Some(red));
    }

    /// The 1.18.0 -> 1.19.0 upgrade makes the same functions as a fresh
    /// install, and its check runs on a store with a moved ident.
    #[pg_test]
    fn ident_upgrade_section() {
        let fresh = include_str!("../sql/32_moved_idents.sql");
        let upgrade = include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql");
        let funcs = &fresh[fresh.find("-- mentat.moved_idents(store TEXT").unwrap()..];
        assert!(
            upgrade.contains(funcs.trim_end()),
            "the upgrade's functions differ from 32_moved_idents.sql"
        );
        setup("default");
        let s = "default";
        tx(s, "[{:db/ident :color/red}]");
        break_like_118(s, ":color/red");
        break_bootstrap_like_118(s);
        let start = upgrade.find("DO $movedidents$").unwrap();
        let end = upgrade.rfind("$movedidents$;").unwrap() + "$movedidents$".len();
        // The block's WARNING, raised as an error so the test can read it.
        Spi::run("SET LOCAL client_min_messages = warning").unwrap();
        let block = upgrade[start..end].replace("RAISE WARNING", "RAISE EXCEPTION");
        let err = Spi::get_one::<String>(&format!(
            "SELECT pg_temp.capture('{}')",
            block.replace('\'', "''")
        ))
        .unwrap()
        .unwrap_or_default();
        assert!(err.contains("the default store has 2 ident(s)"), "{err}");
        assert_eq!(moved_idents(s).len(), 2);
    }

    /// `[?e <attr> ?v ?tx ?added]` over the whole history, sorted.
    fn history(s: &str, attr: &str) -> Vec<(i64, i64, i64, bool)> {
        let h = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT q('{s}', '[:find ?e ?v ?tx ?added :where [?e {attr} ?v ?tx ?added]]', \
             '{{\"history\": true}}'::jsonb)"
        ))
        .unwrap()
        .unwrap();
        let mut rows: Vec<(i64, i64, i64, bool)> = h.0["results"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| {
                (
                    r[0].as_i64().unwrap(),
                    r[1].as_i64().unwrap(),
                    r[2].as_i64().unwrap(),
                    r[3].as_bool().unwrap(),
                )
            })
            .collect();
        rows.sort();
        rows
    }

    fn last_tx(report: &serde_json::Value) -> i64 {
        report["db-after"]["basis-t"].as_i64().expect("basis-t")
    }

    /// Re-running a data load after the move wrote, in one transaction, a
    /// retraction of [car :c/color O] and an assertion of [car :c/color N]:
    /// one datom once N is O. The repair keeps neither, so car's color is O,
    /// asserted in its first transaction and never retracted.
    fn repair_same_tx_pair(s: &str) {
        tx(
            s,
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
              {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity}]",
        );
        tx(s, "[{:db/ident :color/red}]");
        let first = last_tx(&tx(s, r#"[{:c/name "car" :c/color :color/red}]"#));
        let (red, red2) = break_like_118(s, ":color/red");
        let reload = last_tx(&tx(s, r#"[{:c/name "car" :c/color :color/red}]"#));
        let car = q(s, r#"[:find ?e . :where [?e :c/name "car"]]"#)
            .as_i64()
            .unwrap();
        assert_eq!(
            history(s, ":c/color"),
            [
                (car, red, first, true),
                (car, red, reload, false),
                (car, red2, reload, true)
            ]
        );
        let found = moved_idents(s);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0]["blocker"], serde_json::Value::Null, "{found:?}");

        let done = repair(s, false);
        assert_eq!(done[0]["action"], "repaired", "{done:?}");
        assert!(moved_idents(s).is_empty());
        assert_eq!(history(s, ":c/color"), [(car, red, first, true)]);
        let q1 =
            "[:find [?n ...] :where [?c :db/ident :color/red] [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(q(s, q1), serde_json::json!(["car"]));
        for t in [first, reload] {
            let r = Spi::get_one::<pgrx::JsonB>(&format!(
                "SELECT q('{s}', '{q1}', '{{\"asOf\": {t}}}'::jsonb)"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(r.0["result"], serde_json::json!(["car"]), "as of {t}");
        }
    }

    #[pg_test]
    fn ident_repair_same_tx_pair() {
        setup("default");
        repair_same_tx_pair("default");
    }

    #[pg_test]
    fn ident_repair_same_tx_pair_named_store() {
        setup("id415p");
        repair_same_tx_pair("id415p");
    }

    /// Rows of the store's current_<type> tables that the log's latest-tx-wins
    /// rule disagrees with (mentat.verify_current_projection reads only the
    /// default store's tables).
    fn projection_drift(s: &str) -> i64 {
        let sch = schema_of(s);
        [
            "ref", "boolean", "long", "double", "instant", "text", "keyword", "uuid", "bytes",
        ]
        .iter()
        .map(|ty| {
            Spi::get_one::<i64>(&format!(
                "WITH l AS (SELECT store_id, e, a, v FROM (SELECT DISTINCT ON (store_id, e, a, v) \
                   store_id, e, a, v, added FROM {sch}.datoms_{ty}_new \
                   ORDER BY store_id, e, a, v, tx DESC) x WHERE added), \
                 c AS (SELECT store_id, e, a, v FROM {sch}.current_{ty}) \
                 SELECT count(*) FROM ((SELECT * FROM l EXCEPT SELECT * FROM c) \
                   UNION ALL (SELECT * FROM c EXCEPT SELECT * FROM l)) d"
            ))
            .unwrap()
            .unwrap()
        })
        .sum()
    }

    /// `query`'s result now and as of `t`.
    fn now_and_as_of(s: &str, query: &str, t: i64) -> (serde_json::Value, serde_json::Value) {
        let r = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT q('{s}', '{query}', '{{\"asOf\": {t}}}'::jsonb)"
        ))
        .unwrap()
        .unwrap();
        (q(s, query), r.0["result"].clone())
    }

    fn sorted(v: serde_json::Value) -> Vec<String> {
        let mut r: Vec<String> = v
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.to_string())
            .collect();
        r.sort();
        r
    }

    fn history_schema(s: &str) {
        tx(
            s,
            "[{:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity}
              {:db/ident :c/tags :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
              {:db/ident :c/label :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
        );
        tx(s, "[{:db/ident :color/red} {:db/ident :color/blue}]");
    }

    /// Values asserted and retracted through N after the move: in the merged
    /// history they are retracted, so the current state the repair leaves
    /// matches the log (as of the latest transaction), with no ghost of a
    /// value retracted through N. The ref side is also what the documented
    /// hand fix of a blocker does (retract the value written through N).
    fn repair_history_on_moved(s: &str) {
        history_schema(s);
        let t1 = last_tx(&tx(
            s,
            r#"[{:c/name "x" :c/tags [:color/red]} [:db/add :color/blue :c/label "a"]]"#,
        ));
        let (red, _) = break_like_118(s, ":color/red");
        let (blue, _) = break_like_118(s, ":color/blue");
        tx(s, r#"[{:c/name "x" :c/tags [:color/red]}]"#);
        let t3 = last_tx(&tx(
            s,
            r#"[[:db/retract [:c/name "x"] :c/tags :color/red]]"#,
        ));
        tx(s, r#"[[:db/add :color/red :c/label "warm"]]"#);
        tx(s, r#"[[:db/retract :color/red :c/label "warm"]]"#);
        tx(s, r#"[[:db/add :color/red :c/label "hot"]]"#);
        // Two labels for blue: the documented hand fix is to retract one.
        tx(s, r#"[[:db/add :color/blue :c/label "b"]]"#);
        let found = moved_idents(s);
        assert!(
            found[0]["blocker"]
                .as_str()
                .unwrap_or_default()
                .contains("attribute"),
            "{found:?}"
        );
        let last = last_tx(&tx(s, r#"[[:db/retract :color/blue :c/label "b"]]"#));
        assert_eq!(projection_drift(s), 0, "before");
        let found = moved_idents(s);
        assert!(found.iter().all(|m| m["blocker"].is_null()), "{found:?}");

        let done = repair(s, false);
        assert!(done.iter().all(|d| d["action"] == "repaired"), "{done:?}");
        assert!(moved_idents(s).is_empty());
        assert_eq!(projection_drift(s), 0, "the current state is the log's");
        let tags = "[:find ?n ?c :where [?e :c/tags ?c] [?e :c/name ?n]]";
        let (now, then) = now_and_as_of(s, tags, last);
        assert_eq!(now, serde_json::json!([]), "x's red was retracted");
        assert_eq!(now, then);
        let labels = "[:find ?c ?l :where [?c :c/label ?l]]";
        let (now, then) = now_and_as_of(s, labels, last);
        assert_eq!(
            sorted(now.clone()),
            sorted(serde_json::json!([[red, "hot"], [blue, "a"]])),
            "{now}"
        );
        assert_eq!(sorted(now), sorted(then));
        // The re-load asserted a value x had: no row is left for it, and
        // x's red is retracted when it was.
        let x = q(s, r#"[:find ?e . :where [?e :c/name "x"]]"#)
            .as_i64()
            .unwrap();
        assert_eq!(
            history(s, ":c/tags"),
            [(x, red, t1, true), (x, red, t3, false)]
        );
    }

    /// One entity refers to both through a cardinality-many attribute: the
    /// two values are one once N is O.
    fn repair_refers_to_both(s: &str) {
        history_schema(s);
        let t1 = last_tx(&tx(s, r#"[{:c/name "y" :c/tags [:color/red]}]"#));
        let (red, _) = break_like_118(s, ":color/red");
        tx(s, r#"[{:c/name "y" :c/tags [:color/red]}]"#);
        let found = moved_idents(s);
        assert_eq!(found[0]["blocker"], serde_json::Value::Null, "{found:?}");
        let done = repair(s, false);
        assert_eq!(done[0]["action"], "repaired", "{done:?}");
        assert_eq!(projection_drift(s), 0);
        let y = q(s, r#"[:find ?e . :where [?e :c/name "y"]]"#)
            .as_i64()
            .unwrap();
        assert_eq!(history(s, ":c/tags"), [(y, red, t1, true)]);
        assert_eq!(
            q(s, "[:find ?n ?c :where [?e :c/tags ?c] [?e :c/name ?n]]"),
            serde_json::json!([["y", red]])
        );
    }

    #[pg_test]
    fn ident_repair_refers_to_both() {
        setup("default");
        repair_refers_to_both("default");
    }

    #[pg_test]
    fn ident_repair_refers_to_both_named_store() {
        setup("id415b");
        repair_refers_to_both("id415b");
    }

    /// A :db.unique ref attribute: one entity refers to O, another to N. Once
    /// N is O both would hold one unique value, so the ident is not repaired.
    /// One entity holding both (cardinality many) is one value after the merge.
    fn repair_unique_ref(s: &str) {
        tx(
            s,
            "[{:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity}
              {:db/ident :c/uv :db/valueType :db.type/ref :db/cardinality :db.cardinality/one
               :db/unique :db.unique/value}
              {:db/ident :c/ui :db/valueType :db.type/ref :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity}
              {:db/ident :c/um :db/valueType :db.type/ref :db/cardinality :db.cardinality/many
               :db/unique :db.unique/value}]",
        );
        tx(
            s,
            "[{:db/ident :color/red} {:db/ident :color/blue} {:db/ident :color/green}]",
        );
        tx(
            s,
            r#"[{:c/name "y" :c/uv :color/red} {:c/name "y2" :c/ui :color/blue}
                {:c/name "z" :c/um [:color/green]}]"#,
        );
        break_like_118(s, ":color/red");
        break_like_118(s, ":color/blue");
        let (green, _) = break_like_118(s, ":color/green");
        tx(
            s,
            r#"[{:c/name "x" :c/uv :color/red} {:c/name "x2" :c/ui :color/blue}
                {:c/name "z" :c/um [:color/green]}]"#,
        );
        let e = |n: &str| {
            q(s, &format!(r#"[:find ?e . :where [?e :c/name "{n}"]]"#))
                .as_i64()
                .unwrap()
        };
        let found = moved_idents(s);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(
            found[0]["blocker"],
            format!(
                "entities {} and {} would both hold :c/ui :color/blue",
                e("y2"),
                e("x2")
            ),
            "{found:?}"
        );
        assert_eq!(found[1]["blocker"], serde_json::Value::Null, "{found:?}");
        assert_eq!(
            found[2]["blocker"],
            format!(
                "entities {} and {} would both hold :c/uv :color/red",
                e("y"),
                e("x")
            ),
            "{found:?}"
        );
        let done = repair(s, false);
        let actions: Vec<&str> = done.iter().map(|d| d["action"].as_str().unwrap()).collect();
        assert!(actions[0].starts_with("skipped: entities"), "{done:?}");
        assert_eq!(actions[1], "repaired", "{done:?}");
        assert!(actions[2].starts_with("skipped: entities"), "{done:?}");
        assert_eq!(projection_drift(s), 0);
        assert_eq!(
            q(s, r#"[:find [?c ...] :where [?e :c/um ?c] [?e :c/name "z"]]"#),
            serde_json::json!([green])
        );
    }

    /// What the repair changes beyond moving N's datoms onto O, in its notes
    /// (dry run and real run alike): a cardinality-one attribute with a value
    /// through each entity at once in the past, a unique value two entities
    /// held at once in the past, and a value live through O now whose later
    /// retraction through N wins.
    fn repair_notes(s: &str) {
        history_schema(s);
        tx(
            s,
            "[{:db/ident :c/uv :db/valueType :db.type/ref :db/cardinality :db.cardinality/one
               :db/unique :db.unique/value}]",
        );
        tx(
            s,
            r#"[{:c/name "x" :c/tags [:color/red]} {:c/name "y" :c/uv :color/red}
                [:db/add :color/red :c/label "a"]]"#,
        );
        let (red, _) = break_like_118(s, ":color/red");
        let t2 = last_tx(&tx(s, r#"[[:db/add :color/red :c/label "b"]]"#));
        let t3 = last_tx(&tx(s, &format!(r#"[[:db/add {red} :c/label "c"]]"#)));
        let t4 = last_tx(&tx(s, r#"[[:db/retract :color/red :c/label "b"]]"#));
        tx(s, r#"[{:c/name "x" :c/tags [:color/red]}]"#);
        tx(s, r#"[[:db/retract [:c/name "x"] :c/tags :color/red]]"#);
        let t7 = last_tx(&tx(s, r#"[{:c/name "z" :c/uv :color/red}]"#));
        let t8 = last_tx(&tx(
            s,
            &format!(r#"[[:db/retract [:c/name "y"] :c/uv {red}]]"#),
        ));
        let e = |n: &str| {
            q(s, &format!(r#"[:find ?e . :where [?e :c/name "{n}"]]"#))
                .as_i64()
                .unwrap()
        };
        let (x, y, z) = (e("x"), e("y"), e("z"));
        let expected = [
            format!(
                "past overlap: from tx {t2} until tx {t3}, :color/red has two values of \
                 :c/label: \"a\" and \"b\""
            ),
            format!(
                "past overlap: from tx {t3} until tx {t4}, :color/red has two values of \
                 :c/label: \"b\" and \"c\""
            ),
            format!(
                "past overlap: from tx {t7} until tx {t8}, entities {y} and {z} both hold \
                 :c/uv :color/red"
            ),
            format!(
                "[{x} :c/tags :color/red] is live now and retracted after the repair \
                 (a later retraction wins)"
            ),
        ]
        .join("\n");
        let dry = repair(s, true);
        assert_eq!(dry.len(), 1, "{dry:?}");
        assert_eq!(dry[0]["action"], "would repair");
        assert_eq!(dry[0]["notes"], expected.as_str());
        let done = repair(s, false);
        assert_eq!(done[0]["action"], "repaired", "{done:?}");
        assert_eq!(done[0]["notes"], expected.as_str());
        assert_eq!(projection_drift(s), 0);
        // As the notes say.
        let labels = "[:find [?l ...] :where [?c :db/ident :color/red] [?c :c/label ?l]]";
        let (now, then) = now_and_as_of(s, labels, t2);
        assert_eq!(now, serde_json::json!(["c"]));
        assert_eq!(sorted(then), ["\"a\"", "\"b\""]);
        let holders = "[:find [?e ...] :where [?e :c/uv :color/red]]";
        let (now, then) = now_and_as_of(s, holders, t7);
        assert_eq!(now, serde_json::json!([z]));
        assert_eq!(len(&then), 2, "{then}");
        assert_eq!(
            q(s, "[:find ?n ?c :where [?e :c/tags ?c] [?e :c/name ?n]]"),
            serde_json::json!([])
        );
        // Nothing to note: no notes.
        break_like_118(s, ":color/blue");
        tx(s, r#"[{:c/name "w" :c/tags [:color/blue]}]"#);
        let dry = repair(s, true);
        assert_eq!(dry[0]["notes"], serde_json::Value::Null, "{dry:?}");
    }

    #[pg_test]
    fn ident_repair_notes() {
        setup("default");
        repair_notes("default");
    }

    #[pg_test]
    fn ident_repair_notes_named_store() {
        setup("id415n");
        repair_notes("id415n");
    }

    /// A store as 1.18.0 left it after `[{:db/ident :db/doc :db/doc "d"}]`.
    /// :db/doc is a bootstrap ident (schema row 19), and an installed store has
    /// no :db/ident datoms for those. So the map made a new entity X with
    /// [X :db/ident :db/doc] and [X :db/doc "d"], and the idents row moved to
    /// X. The schema row stayed 19.
    fn break_bootstrap_like_118(s: &str) -> i64 {
        let sch = schema_of(s);
        let sid = format!("(SELECT store_id FROM mentat.stores WHERE store_name = '{s}')");
        // As on an installed store (the tests' bootstrap_schema() writes them).
        Spi::run(&format!(
            "DELETE FROM {sch}.datoms_keyword_new WHERE store_id = {sid} AND a = 10 AND e < 100; \
             DELETE FROM {sch}.current_keyword WHERE store_id = {sid} AND a = 10 AND e < 100"
        ))
        .expect("drop the bootstrap ident datoms");
        let r = tx(s, r#"[{:db/id "x" :db/doc "d"}]"#);
        let x = r["tempids"]["x"].as_i64().expect("x");
        let t = last_tx(&r);
        Spi::run(&format!(
            "INSERT INTO {sch}.datoms_keyword_new (store_id, e, a, v, tx, added) \
               VALUES ({sid}, {x}, 10, 'db/doc', {t}, true); \
             INSERT INTO {sch}.current_keyword (store_id, e, a, v, tx) \
               VALUES ({sid}, {x}, 10, 'db/doc', {t}); \
             UPDATE {sch}.idents SET entid = {x} WHERE ident = ':db/doc'; \
             UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = '{s}'"
        ))
        .expect("move :db/doc");
        x
    }

    fn repair_moved_bootstrap_ident(s: &str) {
        let x = break_bootstrap_like_118(s);
        let found = moved_idents(s);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0]["ident"], ":db/doc");
        assert_eq!(found[0]["owner"], 19);
        assert_eq!(found[0]["moved_to"], x);
        assert_eq!(found[0]["n_datoms"], 2);
        assert_eq!(found[0]["blocker"], serde_json::Value::Null, "{found:?}");
        let done = repair(s, false);
        assert_eq!(done[0]["action"], "repaired", "{done:?}");
        assert!(moved_idents(s).is_empty());
        assert_eq!(projection_drift(s), 0);
        assert_eq!(ident_entid(s, ":db/doc"), Some(19));
        assert_eq!(ident_entity(s, ":db/doc"), 19);
        let doc = "[:find ?d . :where [?a :db/ident :db/doc] [?a :db/doc ?d]]";
        assert_eq!(q(s, doc), "d");
        // Re-asserting it now upserts into 19.
        tx(s, r#"[{:db/ident :db/doc :db/doc "again"}]"#);
        assert_eq!(ident_entid(s, ":db/doc"), Some(19));
        assert_eq!(q(s, doc), "again");
    }

    #[pg_test]
    fn ident_repair_moved_bootstrap_ident() {
        setup("default");
        repair_moved_bootstrap_ident("default");
    }

    #[pg_test]
    fn ident_repair_moved_bootstrap_ident_named_store() {
        setup("id415s");
        repair_moved_bootstrap_ident("id415s");
    }

    #[pg_test]
    fn ident_repair_unique_ref() {
        setup("default");
        repair_unique_ref("default");
    }

    #[pg_test]
    fn ident_repair_unique_ref_named_store() {
        setup("id415q");
        repair_unique_ref("id415q");
    }

    #[pg_test]
    fn ident_repair_history_on_moved() {
        setup("default");
        repair_history_on_moved("default");
        assert_eq!(
            Spi::get_one::<i64>("SELECT mentat.verify_current_projection()").unwrap(),
            Some(0)
        );
    }

    #[pg_test]
    fn ident_repair_history_on_moved_named_store() {
        setup("id415h");
        repair_history_on_moved("id415h");
    }

    #[pg_test]
    fn ident_repair_moved_idents() {
        setup("default");
        repair_moved_enum("default");
    }

    #[pg_test]
    fn ident_repair_moved_idents_named_store() {
        setup("id415r");
        repair_moved_enum("id415r");
    }
}
