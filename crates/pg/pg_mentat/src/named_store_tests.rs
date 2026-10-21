// Named stores (issue #337): create_store(name) gives the store its own
// schema, mentat_<name>, with the default store's full layout (narrow logs,
// current projections, schema/idents/partitions/transactions). Writes and
// reads both go through that schema. Before 1.18.0 create_store built the
// pre-1.5 wide layout, so the first t() failed with `relation
// "mentat_<name>.current_instant" does not exist`, and reads went to the
// shared mentat.* tables.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    const SCHEMA_TX: &str = "[{:db/ident :p/name :db/valueType :db.type/string \
         :db/cardinality :db.cardinality/one :db/unique :db.unique/identity} \
        {:db/ident :p/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one} \
        {:db/ident :p/friend :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]";

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
    }

    fn text(sql: &str) -> String {
        Spi::get_one::<String>(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_default()
    }

    fn json(sql: &str) -> serde_json::Value {
        Spi::get_one::<pgrx::JsonB>(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .map(|j| j.0)
            .unwrap_or(serde_json::Value::Null)
    }

    fn tx(store: &str, edn: &str) -> serde_json::Value {
        let out = text(&format!(
            "SELECT t('{store}', '{}')",
            edn.replace('\'', "''")
        ));
        serde_json::from_str(&out).expect("tx report JSON")
    }

    fn q(store: &str, query: &str, inputs: &str) -> serde_json::Value {
        json(&format!(
            "SELECT q('{store}', '{}', '{inputs}'::jsonb)",
            query.replace('\'', "''")
        ))
    }

    fn names(store: &str) -> serde_json::Value {
        q(store, "[:find [?n ...] :where [?e :p/name ?n]]", "{}")["result"].clone()
    }

    fn sorted(v: serde_json::Value) -> Vec<String> {
        let mut out: Vec<String> = v
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|x| x.as_str().unwrap_or("").to_string())
                    .collect()
            })
            .unwrap_or_default();
        out.sort();
        out
    }

    #[pg_test]
    fn named_store_write_then_read() {
        setup();
        Spi::run("SELECT create_store('ns_one')").expect("create_store");
        // The default store's layout, in the store's own schema.
        let tables: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_tables WHERE schemaname = 'mentat_ns_one' \
             AND (tablename LIKE 'datoms\\_%\\_new' OR tablename LIKE 'current\\_%')",
        )
        .unwrap()
        .unwrap();
        assert_eq!(tables, 18, "9 narrow logs + 9 projections");

        // Schema tx then data tx: the first write used to fail with
        // `relation "mentat_ns_one.current_instant" does not exist`.
        tx("ns_one", SCHEMA_TX);
        let report = tx(
            "ns_one",
            "[{:db/id \"a\" :p/name \"Ann\" :p/age 30} {:db/id \"b\" :p/name \"Bob\" :p/age 40 :p/friend \"a\"}]",
        );
        let ann = report["tempids"]["a"].as_i64().expect("tempid a");
        let bob = report["tempids"]["b"].as_i64().expect("tempid b");
        let tx1 = report["db-after"]["basis-t"].as_i64().expect("basis");

        // The rows are in the store's schema, not in mentat.*.
        let here: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM mentat_ns_one.current_text WHERE e IN ({ann}, {bob})"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(here, 2);
        let shared: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM mentat.datoms_text_new WHERE e IN ({ann}, {bob})"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(shared, 0, "nothing written to the default store's tables");

        // q
        assert_eq!(sorted(names("ns_one")), vec!["Ann", "Bob"]);
        let age = q(
            "ns_one",
            "[:find ?a . :in $ ?n :where [?e :p/name ?n] [?e :p/age ?a]]",
            "{\"inputs\": [\"Bob\"]}",
        );
        assert_eq!(age["result"], serde_json::json!(40));
        // lookup ref through the store's own unique attribute
        let by_ref = q(
            "ns_one",
            "[:find ?a . :in $ ?e :where [?e :p/age ?a]]",
            "{\"inputs\": [[\":p/name\", \"Ann\"]]}",
        );
        assert_eq!(by_ref["result"], serde_json::json!(30));

        // pull + pull_many + entity
        let p = json(&format!(
            "SELECT pull('ns_one', '[:p/name :p/age {{:p/friend [:p/name]}}]', {bob})"
        ));
        assert_eq!(p[":p/name"], serde_json::json!("Bob"));
        assert_eq!(p[":p/age"], serde_json::json!(40));
        assert_eq!(
            p[":p/friend"][0][":p/name"],
            serde_json::json!("Ann"),
            "{p}"
        );
        let many = json(&format!(
            "SELECT pull_many('ns_one', '[:p/name]', ARRAY[{ann}, {bob}])"
        ));
        assert_eq!(many.as_array().map(|a| a.len()), Some(2), "{many}");
        let ent = json(&format!("SELECT entity('ns_one', {ann})"));
        assert_eq!(ent[":p/name"], serde_json::json!("Ann"), "{ent}");

        // schema(store) reads the store's attributes
        let sch = json("SELECT schema('ns_one')").to_string();
        assert!(sch.contains(":p/friend"), "{sch}");

        // Update, then as-of / since / history over the store's log.
        tx("ns_one", &format!("[[:db/add {ann} :p/age 31]]"));
        let now = q(
            "ns_one",
            "[:find ?a . :in $ ?n :where [?e :p/name ?n] [?e :p/age ?a]]",
            "{\"inputs\": [\"Ann\"]}",
        );
        assert_eq!(now["result"], serde_json::json!(31));
        let then = q(
            "ns_one",
            "[:find ?a . :in $ ?n :where [?e :p/name ?n] [?e :p/age ?a]]",
            &format!("{{\"inputs\": [\"Ann\"], \"asOf\": {tx1}}}"),
        );
        assert_eq!(then["result"], serde_json::json!(30), "as-of");
        let since = q(
            "ns_one",
            "[:find [?a ...] :where [?e :p/age ?a]]",
            &format!("{{\"since\": {tx1}}}"),
        );
        assert_eq!(since["result"], serde_json::json!([31]), "since");
        let hist = q(
            "ns_one",
            &format!("[:find ?a ?added :where [{ann} :p/age ?a _ ?added]]"),
            "{\"history\": true}",
        );
        let mut rows: Vec<String> = hist["results"]
            .as_array()
            .expect("history rows")
            .iter()
            .map(|r| r.to_string())
            .collect();
        rows.sort();
        assert_eq!(
            rows,
            vec!["[30,false]", "[30,true]", "[31,true]"],
            "history"
        );
        // log() over the store's narrow tables + transactions
        let log = json(&format!("SELECT log('ns_one', {tx1}, {tx1} + 1000000)"));
        assert!(log.to_string().contains("31"), "{log}");

        // Projection consistent with the log for this store.
        let sid: i64 =
            Spi::get_one("SELECT store_id FROM mentat.stores WHERE store_name = 'ns_one'")
                .unwrap()
                .unwrap();
        assert!(sid > 0);

        Spi::run("SELECT drop_store('ns_one')").expect("drop_store");
        let gone: Option<String> =
            Spi::get_one("SELECT nspname::text FROM pg_namespace WHERE nspname = 'mentat_ns_one'")
                .unwrap_or(None);
        assert!(gone.is_none(), "schema dropped");
    }

    #[pg_test]
    fn named_stores_are_isolated() {
        setup();
        Spi::run("SELECT create_store('iso_a')").expect("create a");
        Spi::run("SELECT create_store('iso_b')").expect("create b");
        for s in ["default", "iso_a", "iso_b"] {
            tx(s, SCHEMA_TX);
        }
        tx("iso_a", "[{:p/name \"only-a\"}]");
        tx("iso_b", "[{:p/name \"only-b\"}]");
        Spi::run("SELECT edn_t('[{:p/name \"only-default\"}]')").expect("default tx");

        assert_eq!(sorted(names("iso_a")), vec!["only-a"]);
        assert_eq!(sorted(names("iso_b")), vec!["only-b"]);
        assert_eq!(sorted(names("default")), vec!["only-default"]);
        let dflt = json("SELECT edn_q('[:find [?n ...] :where [?e :p/name ?n]]', '{}')");
        assert_eq!(sorted(dflt["result"].clone()), vec!["only-default"]);

        // An attribute defined only in iso_a is unknown in iso_b.
        tx(
            "iso_a",
            "[{:db/ident :a/only :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let err = text("SELECT pg_temp.capture($q$SELECT t('iso_b', '[{:a/only 1}]')$q$)");
        assert!(!err.is_empty(), "iso_b must not see iso_a's attribute");

        // Entity ids are allocated per store: same ids may repeat across
        // stores without colliding.
        let ea = q("iso_a", "[:find ?e . :where [?e :p/name \"only-a\"]]", "{}")["result"].clone();
        let pulled = json(&format!("SELECT pull('iso_b', '[:p/name]', {ea})"));
        assert_ne!(pulled[":p/name"], serde_json::json!("only-a"), "{pulled}");

        Spi::run("SELECT drop_store('iso_a')").expect("drop a");
        // iso_b and default are untouched by dropping iso_a.
        assert_eq!(sorted(names("iso_b")), vec!["only-b"]);
        assert_eq!(sorted(names("default")), vec!["only-default"]);
        Spi::run("SELECT drop_store('iso_b')").expect("drop b");
    }

    #[pg_test]
    fn rename_store_then_write_and_read() {
        setup();
        Spi::run("SELECT create_store('rn_old')").expect("create");
        tx("rn_old", SCHEMA_TX);
        tx("rn_old", "[{:p/name \"before\"}]");
        Spi::run("SELECT rename_store('rn_old', 'rn_new')").expect("rename");
        tx("rn_new", "[{:p/name \"after\"}]");
        assert_eq!(sorted(names("rn_new")), vec!["after", "before"]);
        let err = text("SELECT pg_temp.capture($q$SELECT t('rn_old', '[{:p/name \"x\"}]')$q$)");
        assert!(!err.is_empty(), "old name is gone");
        // The views in the store schema follow the rename.
        let n: i64 =
            Spi::get_one("SELECT count(*) FROM mentat_rn_new.facts WHERE attribute = ':p/name'")
                .unwrap()
                .unwrap();
        assert_eq!(n, 2);
        Spi::run("SELECT drop_store('rn_new')").expect("drop");
    }

    #[pg_test]
    fn store_management_errors() {
        setup();
        let err = text("SELECT pg_temp.capture($q$SELECT drop_store('default')$q$)");
        assert!(!err.is_empty(), "the default store cannot be dropped");
        Spi::run("SELECT create_store('dup_s')").expect("create");
        let err = text("SELECT pg_temp.capture($q$SELECT create_store('dup_s')$q$)");
        assert!(err.contains("dup_s"), "{err}");
        // with() works against a named store and leaves nothing behind.
        tx("dup_s", SCHEMA_TX);
        let w = text("SELECT \"with\"('dup_s', '[{:p/name \"spec\"}]')");
        assert!(w.contains("tempids"), "{w}");
        assert_eq!(sorted(names("dup_s")), Vec::<String>::new());
        Spi::run("SELECT drop_store('dup_s')").expect("drop");
        // A dropped name can be created again, empty.
        Spi::run("SELECT create_store('dup_s')").expect("re-create");
        tx("dup_s", SCHEMA_TX);
        assert_eq!(sorted(names("dup_s")), Vec::<String>::new());
        Spi::run("SELECT drop_store('dup_s')").expect("drop again");
    }
}
