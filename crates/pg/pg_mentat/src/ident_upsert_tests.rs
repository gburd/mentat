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
        assert!(err.contains("the default store has 1 ident(s)"), "{err}");
        assert_eq!(moved_idents(s).len(), 1);
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
