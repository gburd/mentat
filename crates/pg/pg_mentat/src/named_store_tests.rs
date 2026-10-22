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

    /// The (store_id, tx) index on each of a schema's current_<type> tables
    /// (`since` reads the projection by tx; 1.18.0).
    fn tx_indexes(schema: &str) -> i64 {
        Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM pg_indexes WHERE schemaname = '{schema}' \
              AND indexname ~ '^idx_current_[a-z]+_tx$' AND indexdef LIKE '%(store_id, tx)'"
        ))
        .unwrap()
        .unwrap()
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
        assert_eq!(tx_indexes("mentat_ns_one"), 9, "idx_current_<type>_tx");

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
    #[pg_test]
    fn named_store_transactor_features() {
        setup();
        Spi::run("SELECT create_store('tf_s')").expect("create");
        tx("tf_s", SCHEMA_TX);
        tx(
            "tf_s",
            "[{:db/ident :p/note :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/noHistory true}]",
        );
        let r = tx("tf_s", "[{:db/id \"a\" :p/name \"Ann\" :p/age 1} {:db/id \"b\" :p/name \"Bob\" :p/friend \"a\"}]");
        let ann = r["tempids"]["a"].as_i64().unwrap();
        // Lookup ref in the entity position, resolved in the store.
        tx("tf_s", "[[:db/add [:p/name \"Ann\"] :p/age 2]]");
        // Upsert by unique identity: no new entity.
        let up = tx("tf_s", "[{:db/id \"x\" :p/name \"Ann\" :p/age 3}]");
        assert_eq!(up["tempids"]["x"].as_i64(), Some(ann), "{up}");
        // CAS reads the store's current_datoms.
        tx("tf_s", &format!("[[:db.fn/cas {ann} :p/age 3 4]]"));
        let err = text(&format!(
            "SELECT pg_temp.capture($q$SELECT t('tf_s', '[[:db.fn/cas {ann} :p/age 3 5]]')$q$)"
        ));
        assert!(!err.is_empty(), "cas against a stale value fails");
        // noHistory keeps one log row.
        tx("tf_s", &format!("[[:db/add {ann} :p/note \"n1\"]]"));
        tx("tf_s", &format!("[[:db/add {ann} :p/note \"n2\"]]"));
        let notes: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM mentat_tf_s.datoms_text_new WHERE e = {ann} \
             AND a = (SELECT entid FROM mentat_tf_s.idents WHERE ident = ':p/note')"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(notes, 1);
        // retractEntity removes Ann and Bob's ref to her.
        tx("tf_s", &format!("[[:db/retractEntity {ann}]]"));
        assert_eq!(sorted(names("tf_s")), vec!["Bob"]);
        let friends = q(
            "tf_s",
            "[:find ?f . :where [?b :p/name \"Bob\"] [?b :p/friend ?f]]",
            "{}",
        );
        assert_eq!(friends["result"], serde_json::Value::Null, "{friends}");
        // The projection agrees with the log (latest tx wins), per type.
        let mismatches: i64 = Spi::get_one(
            "SELECT count(*) FROM ( \
               (SELECT e, a, v FROM (SELECT DISTINCT ON (e, a, v) e, a, v, added \
                  FROM mentat_tf_s.datoms_long_new ORDER BY e, a, v, tx DESC) l WHERE added \
                EXCEPT SELECT e, a, v FROM mentat_tf_s.current_long) \
               UNION ALL \
               (SELECT e, a, v FROM mentat_tf_s.current_long \
                EXCEPT SELECT e, a, v FROM (SELECT DISTINCT ON (e, a, v) e, a, v, added \
                  FROM mentat_tf_s.datoms_long_new ORDER BY e, a, v, tx DESC) l WHERE added)) d",
        )
        .unwrap()
        .unwrap();
        assert_eq!(mismatches, 0);
        // The default store saw none of it.
        let leaked: i64 = Spi::get_one(
            "SELECT count(*) FROM mentat.current_text WHERE v IN ('Ann', 'Bob', 'n1', 'n2')",
        )
        .unwrap()
        .unwrap();
        assert_eq!(leaked, 0);
        Spi::run("SELECT drop_store('tf_s')").expect("drop");
    }

    // The 1.18.0 transactor checks (#152 lookup refs by a unique value, #286
    // alterations the data violates, #362 retractions of absent values,
    // #368 add/retract conflicts, the datom counts) on a named store. The
    // #286 and #362 checks read mentat.current_<type> with the store's id,
    // so on a named store they found nothing: a violating alteration went
    // through and a live retraction was dropped as absent.
    #[pg_test]
    fn named_store_lookup_refs_alterations_and_retractions() {
        setup();
        let dflt = || -> i64 {
            Spi::get_one(
                "SELECT (SELECT count(*) FROM mentat.datoms_text_new) \
                      + (SELECT count(*) FROM mentat.datoms_long_new) \
                      + (SELECT count(*) FROM mentat.datoms_ref_new) \
                      + (SELECT count(*) FROM mentat.schema)",
            )
            .unwrap()
            .unwrap()
        };
        let before = dflt();
        Spi::run("SELECT create_store('mz_s')").expect("create");
        tx("mz_s", SCHEMA_TX);
        tx(
            "mz_s",
            "[{:db/ident :p/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many} \
              {:db/ident :p/code :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
        );
        let r = tx(
            "mz_s",
            "[{:db/id \"a\" :p/name \"Ann\" :p/tags [\"x\" \"y\"] :p/code \"same\"} \
              {:db/id \"b\" :p/name \"Bob\" :p/code \"same\"}]",
        );
        let ann = r["tempids"]["a"].as_i64().unwrap();
        let capture = |edn: &str| {
            text(&format!(
                "SELECT pg_temp.capture($q$SELECT t('mz_s', '{}')$q$)",
                edn.replace('\'', "''")
            ))
        };

        // #152: [a v] lookup refs, in entity and in value position.
        tx("mz_s", "[[:db/add [:p/name \"Ann\"] :p/age 5]]");
        tx(
            "mz_s",
            "[[:db/add [:p/name \"Bob\"] :p/friend [:p/name \"Ann\"]]]",
        );
        let friend = q(
            "mz_s",
            "[:find ?n . :where [?b :p/name \"Bob\"] [?b :p/friend ?f] [?f :p/name ?n]]",
            "{}",
        );
        assert_eq!(friend["result"], "Ann", "{friend}");
        // A card-many ref's value [:p/name "Ann"] is one lookup ref, because
        // :p/name is unique/identity in this store (the default store has no
        // :p/name).
        tx("mz_s", "[{:p/name \"Cy\" :p/friend [:p/name \"Ann\"]}]");
        let cy = q(
            "mz_s",
            "[:find [?n ...] :where [?c :p/name \"Cy\"] [?c :p/friend ?f] [?f :p/name ?n]]",
            "{}",
        );
        assert_eq!(sorted(cy["result"].clone()), vec!["Ann"], "{cy}");

        // #286: alterations the store's data violates are refused.
        let e = capture("[[:db/add :p/tags :db/cardinality :db.cardinality/one]]");
        assert!(
            e.contains("Cannot alter schema attribute :p/tags to be :db.cardinality/one"),
            "{e:?}"
        );
        let e = capture(
            "[[:db/add :p/code :db/unique :db.unique/identity] [:db/add :p/code :db/index true]]",
        );
        assert!(
            e.contains("Cannot alter schema attribute :p/code to be :db.unique/identity"),
            "{e:?}"
        );

        // #362: a live retraction applies and an absent one is dropped.
        let r = tx(
            "mz_s",
            &format!("[[:db/retract {ann} :p/tags \"x\"] [:db/retract {ann} :p/tags \"zz\"]]"),
        );
        assert_eq!(
            (
                r["datoms_asserted"].as_i64(),
                r["datoms_retracted"].as_i64()
            ),
            (Some(0), Some(1)),
            "{r}"
        );
        let tags = q(
            "mz_s",
            &format!("[:find [?t ...] :where [{ann} :p/tags ?t]]"),
            "{}",
        );
        assert_eq!(sorted(tags["result"].clone()), vec!["y"]);
        // Now Ann holds one tag: the alteration goes through.
        tx(
            "mz_s",
            "[[:db/add :p/tags :db/cardinality :db.cardinality/one]]",
        );

        // #368: asserting and retracting one datom in one transaction.
        let e = capture(&format!(
            "[[:db/add {ann} :p/age 9] [:db/retract {ann} :p/age 9]]"
        ));
        assert!(
            e.contains("AddRetractConflict") && e.contains("a: :p/age"),
            "{e:?}"
        );

        assert_eq!(dflt(), before, "the default store is untouched");
        Spi::run("SELECT drop_store('mz_s')").expect("drop");
    }

    // A mixed-case name gets the quoted schema "mentat_MyStore". A text ->
    // regnamespace cast folds that to mentat_mystore: create_store('MyStore')
    // failed ("schema already exists"), and with a lower-case twin present
    // _store_grants re-owned the twin's functions.
    #[pg_test]
    fn mixed_case_store_name() {
        setup();
        Spi::run("SELECT create_store('MyStore')").expect("create mixed case");
        tx("MyStore", SCHEMA_TX);
        tx("MyStore", "[{:p/name \"upper\"}]");
        assert_eq!(sorted(names("MyStore")), vec!["upper"]);
        let in_schema: i64 =
            Spi::get_one("SELECT count(*) FROM \"mentat_MyStore\".current_text WHERE v = 'upper'")
                .unwrap()
                .unwrap();
        assert_eq!(in_schema, 1);
        let e = q(
            "MyStore",
            "[:find ?e . :where [?e :p/name \"upper\"]]",
            "{}",
        )["result"]
            .clone();
        let pulled = json(&format!("SELECT pull('MyStore', '[:p/name]', {e})"));
        assert_eq!(pulled[":p/name"], serde_json::json!("upper"), "{pulled}");

        Spi::run("SELECT rename_store('MyStore', 'MyOther')").expect("rename");
        tx("MyOther", "[{:p/name \"again\"}]");
        assert_eq!(sorted(names("MyOther")), vec!["again", "upper"]);
        let n: i64 = Spi::get_one(
            "SELECT count(*) FROM \"mentat_MyOther\".facts WHERE attribute = ':p/name'",
        )
        .unwrap()
        .unwrap();
        assert_eq!(n, 2);
        Spi::run("SELECT drop_store('MyOther')").expect("drop");
        let gone: i64 =
            Spi::get_one("SELECT count(*) FROM pg_namespace WHERE nspname = 'mentat_MyOther'")
                .unwrap()
                .unwrap();
        assert_eq!(gone, 0);
    }

    #[pg_test]
    fn mixed_case_store_with_lower_case_twin() {
        setup();
        Spi::run("SELECT create_store('twincase')").expect("create twin");
        // The twin's functions belong to another role: _store_grants for
        // "mentat_TwinCase" must not hand them to its caller.
        Spi::run(
            "CREATE ROLE ns_twin_owner NOLOGIN; \
             DO $$ DECLARE r RECORD; BEGIN \
               FOR r IN SELECT oid::regprocedure AS p FROM pg_proc WHERE pronamespace = \
                        (SELECT oid FROM pg_namespace WHERE nspname = 'mentat_twincase') LOOP \
                 EXECUTE format('ALTER ROUTINE %s OWNER TO ns_twin_owner', r.p); \
               END LOOP; END $$",
        )
        .expect("re-own twin functions");
        let twin_owned = "SELECT count(*) FROM pg_proc WHERE proowner = 'ns_twin_owner'::regrole \
             AND pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = 'mentat_twincase')";
        let before: i64 = Spi::get_one(twin_owned).unwrap().unwrap();
        assert!(before > 0);

        Spi::run("SELECT create_store('TwinCase')").expect("create mixed case");
        assert_eq!(Spi::get_one::<i64>(twin_owned).unwrap().unwrap(), before);
        for s in ["TwinCase", "twincase"] {
            tx(s, SCHEMA_TX);
        }
        tx("TwinCase", "[{:p/name \"upper\"}]");
        tx("twincase", "[{:p/name \"lower\"}]");
        assert_eq!(sorted(names("TwinCase")), vec!["upper"]);
        assert_eq!(sorted(names("twincase")), vec!["lower"]);
        Spi::run("SELECT drop_store('TwinCase')").expect("drop mixed case");
        assert_eq!(
            sorted(names("twincase")),
            vec!["lower"],
            "twin survives the drop"
        );
        Spi::run("SELECT drop_store('twincase')").expect("drop twin");
    }

    // regrole output is already quoted. Formatting it with %I (or
    // quote_ident) quoted it twice: GRANT ... TO """App-User""" failed, so
    // with such a role among the default store's grantees (the installing role
    // of a trusted install) every create_store failed, and the upgrade's
    // OWNER TO / AUTHORIZATION failed for a store owned by one.
    #[pg_test]
    fn role_names_that_need_quoting() {
        setup();
        Spi::run(
            "CREATE ROLE \"App-User\" NOLOGIN; \
             GRANT USAGE ON SCHEMA mentat TO \"App-User\" WITH GRANT OPTION; \
             GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA mentat TO \"App-User\" WITH GRANT OPTION; \
             GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA mentat TO \"App-User\" WITH GRANT OPTION",
        )
        .expect("installer-like role");
        // Grantee path: the store gets "App-User"'s grants.
        Spi::run("SELECT create_store('quoted_role')").expect("create_store");
        let usage: bool = Spi::get_one(
            "SELECT has_schema_privilege('App-User', 'mentat_quoted_role', 'USAGE WITH GRANT OPTION')",
        )
        .unwrap()
        .unwrap();
        assert!(usage);
        // Owner path (what the upgrade does): objects made by another role are
        // handed to the schema's owner.
        Spi::run(
            "ALTER SCHEMA mentat_quoted_role OWNER TO \"App-User\"; \
             SELECT mentat._store_grants('mentat_quoted_role')",
        )
        .expect("_store_grants for a quoted owner");
        let foreign: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_class WHERE relnamespace = \
               (SELECT oid FROM pg_namespace WHERE nspname = 'mentat_quoted_role') \
             AND relkind IN ('r', 'v', 'S') AND relowner <> '\"App-User\"'::regrole",
        )
        .unwrap()
        .unwrap();
        assert_eq!(foreign, 0, "every table, view and sequence is App-User's");
        // And the owner can use it.
        tx("quoted_role", SCHEMA_TX);
        Spi::run("SET LOCAL ROLE \"App-User\"").expect("set role");
        tx("quoted_role", "[{:p/name \"by-app-user\"}]");
        assert_eq!(sorted(names("quoted_role")), vec!["by-app-user"]);
        Spi::run("RESET ROLE").expect("reset role");
        Spi::run("SELECT drop_store('quoted_role')").expect("drop");
    }

    /// The upgrade's DO block, with @extschema@ resolved as PostgreSQL would.
    fn upgrade_do_block() -> String {
        upgrade_block("migrate")
    }

    /// The upgrade's `DO $tag$ ... $tag$;` block, @extschema@ resolved.
    fn upgrade_block(tag: &str) -> String {
        let script = include_str!("../sql/pg_mentat--1.17.2--1.18.0.sql");
        let (open, close) = (format!("DO ${tag}$"), format!("${tag}$;"));
        let start = script.find(&open).expect("DO block");
        let end = script[start..].find(&close).expect("DO block end") + start;
        let ext_schema = text(
            "SELECT extnamespace::regnamespace::text FROM pg_extension WHERE extname = 'pg_mentat'",
        );
        script[start..end + close.len()].replace("@extschema@", &ext_schema)
    }

    // The 1.17.2 -> 1.18.0 DO block runs as the bootstrap superuser. It must
    // not rebuild a schema just because mentat.stores (writable by the
    // installing role) names it, and must not read anything in an old store's
    // schema (its owner could have made any table a view over its own
    // function, which would then run as superuser). Before 1.18.0 shipped it
    // did both: it copied rows out of mentat_<name>_pre118.
    #[pg_test]
    fn upgrade_trusts_no_user_owned_relation() {
        setup();
        Spi::run(
            "CREATE TABLE public.ns_planted_ran (what TEXT); \
             CREATE FUNCTION public.ns_planted() RETURNS TABLE (ident TEXT, entid BIGINT) \
               LANGUAGE plpgsql AS $$ BEGIN INSERT INTO public.ns_planted_ran VALUES ('view'); END $$; \
             CREATE FUNCTION public.ns_planted_trg() RETURNS trigger \
               LANGUAGE plpgsql AS $$ BEGIN INSERT INTO public.ns_planted_ran VALUES ('trigger'); RETURN NULL; END $$; \
             CREATE TRIGGER ns_planted AFTER INSERT OR UPDATE ON mentat.cache_generation \
               FOR EACH STATEMENT EXECUTE FUNCTION public.ns_planted_trg(); \
             -- A 1.17 named store whose owner swapped its tables for views.
             INSERT INTO mentat.stores (store_name, schema_name) VALUES ('evil', 'mentat_evil'); \
             CREATE SCHEMA mentat_evil; \
             CREATE TABLE mentat_evil.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
               value_type_tag SMALLINT NOT NULL, v_ref BIGINT, v_bool BOOLEAN, v_long BIGINT, \
               v_double DOUBLE PRECISION, v_text TEXT, v_keyword TEXT, v_instant TIMESTAMPTZ, \
               v_uuid UUID, v_bytes BYTEA, tx BIGINT NOT NULL, added BOOLEAN NOT NULL DEFAULT TRUE); \
             INSERT INTO mentat_evil.datoms (e, a, value_type_tag, v_long, tx) \
               VALUES (1000001, 100, 2, 7, 1000000000001); \
             CREATE VIEW mentat_evil.idents AS SELECT * FROM public.ns_planted(); \
             CREATE VIEW mentat_evil.schema AS SELECT p.entid, p.ident, \
               'long'::mentat.value_type AS value_type, 'one'::mentat.cardinality_type AS cardinality, \
               NULL::mentat.unique_type AS unique_constraint, false AS indexed, false AS fulltext, \
               false AS component, false AS no_history FROM public.ns_planted() p; \
             CREATE VIEW mentat_evil.transactions AS SELECT entid AS tx, now() AS tx_instant \
               FROM public.ns_planted(); \
             -- A row pointing at a schema create_store() never made.
             CREATE SCHEMA billing; CREATE TABLE billing.invoices (id INT); \
             INSERT INTO mentat.stores (store_name, schema_name) VALUES ('billing', 'billing'); \
             -- A store schema without the old wide layout (datoms is a view).
             INSERT INTO mentat.stores (store_name, schema_name) VALUES ('odd', 'mentat_odd'); \
             CREATE SCHEMA mentat_odd; \
             CREATE VIEW mentat_odd.datoms AS SELECT 1::SMALLINT AS value_type_tag; \
             -- A store_id row for Row-Level Security, with no schema of its own.
             INSERT INTO mentat.stores (store_name, schema_name) VALUES ('rls_t', 'mentat_rls_test_t')",
        )
        .expect("1.17-style fixtures");

        Spi::run(&upgrade_do_block()).expect("the upgrade DO block");
        // Again: nothing left to do, and still nothing planted runs.
        Spi::run(&upgrade_do_block()).expect("the DO block again");
        let ran: i64 = Spi::get_one("SELECT count(*) FROM public.ns_planted_ran")
            .unwrap()
            .unwrap();
        assert_eq!(ran, 0, "a planted view or trigger ran during the upgrade");
        Spi::run("DROP TRIGGER ns_planted ON mentat.cache_generation").expect("drop trigger");

        // The old schema is kept as it was, under _pre118; the new one is empty.
        let kept: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_class WHERE relnamespace = \
               (SELECT oid FROM pg_namespace WHERE nspname = 'mentat_evil_pre118')",
        )
        .unwrap()
        .unwrap();
        assert_eq!(kept, 4, "datoms + 3 views kept");
        let copied: i64 = Spi::get_one("SELECT count(*) FROM mentat_evil.datoms_long_new")
            .unwrap()
            .unwrap();
        assert_eq!(copied, 0, "nothing copied out of the old schema");
        assert_eq!(
            tx_indexes("mentat_evil"),
            9,
            "a rebuilt store has idx_current_<type>_tx"
        );
        tx("evil", SCHEMA_TX);
        tx("evil", "[{:p/name \"rebuilt\"}]");
        assert_eq!(sorted(names("evil")), vec!["rebuilt"]);
        // Left alone.
        let untouched: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname IN ('billing', 'mentat_odd')",
        )
        .unwrap()
        .unwrap();
        assert_eq!(untouched, 2);
        let renamed: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_namespace WHERE nspname LIKE '%\\_pre118' AND nspname <> 'mentat_evil_pre118'",
        )
        .unwrap()
        .unwrap();
        assert_eq!(renamed, 0);
        assert_eq!(
            text("SELECT to_regclass('billing.invoices')::text"),
            "billing.invoices"
        );
    }

    // mentat_ + name must fit PostgreSQL's 63-character identifiers. 1.17
    // took up to 63; PostgreSQL cut the schema name short, and with this
    // branch's catalog lookups (nspname = text) create_store failed for
    // 57-63. The limit is 56 now.
    #[pg_test]
    fn store_name_length_limit() {
        setup();
        let max = "n".repeat(56);
        Spi::run(&format!("SELECT create_store('{max}')")).expect("56 characters");
        tx(&max, SCHEMA_TX);
        tx(&max, "[{:p/name \"long\"}]");
        assert_eq!(sorted(names(&max)), vec!["long"]);
        Spi::run(&format!("SELECT drop_store('{max}')")).expect("drop");
        let err = text(&format!(
            "SELECT pg_temp.capture($q$SELECT create_store('{}')$q$)",
            "n".repeat(57)
        ));
        assert!(err.contains("56"), "{err}");
    }

    // 1.17 stores with long names: 56 characters (its _pre118 name used to
    // truncate back to the store's own schema, so the upgrade aborted) and 60
    // (schema name truncated by PostgreSQL; the store was silently skipped).
    #[pg_test]
    fn upgrade_handles_long_store_names() {
        setup();
        let n56 = "l".repeat(56);
        let n60 = "m".repeat(60);
        let wide = "(e BIGINT NOT NULL, a BIGINT NOT NULL, value_type_tag SMALLINT NOT NULL, \
                    v_ref BIGINT, v_long BIGINT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL)";
        for n in [&n56, &n60] {
            // What 1.17's create_store left: the row keeps the full name, the
            // schema is whatever CREATE SCHEMA made of it.
            Spi::run(&format!(
                "INSERT INTO mentat.stores (store_name, schema_name) VALUES ('{n}', 'mentat_{n}'); \
                 CREATE SCHEMA \"mentat_{n}\"; CREATE TABLE \"mentat_{n}\".datoms {wide}"
            ))
            .expect("1.17 store");
        }
        Spi::run(&upgrade_do_block()).expect("the upgrade must not abort");
        // 56: rebuilt, old schema kept under a 63-character name.
        let pre = format!("{}_pre118", &format!("mentat_{n56}")[..56]);
        assert_eq!(pre.len(), 63);
        let kept: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM pg_class WHERE relname = 'datoms' AND relnamespace = \
               (SELECT oid FROM pg_namespace WHERE nspname = '{pre}')"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(kept, 1);
        tx(&n56, SCHEMA_TX);
        tx(&n56, "[{:p/name \"rebuilt\"}]");
        assert_eq!(sorted(names(&n56)), vec!["rebuilt"]);
        // 60: skipped, schema untouched.
        let truncated = &format!("mentat_{n60}")[..63];
        let left_alone: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM pg_class WHERE relname = 'datoms' AND relkind = 'r' AND relnamespace = \
               (SELECT oid FROM pg_namespace WHERE nspname = '{truncated}')"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(left_alone, 1);
    }

    // create_virtual_tables (create_store, rename_store, and the upgrade's DO
    // block, which pins search_path to pg_catalog) built the optional
    // trigram indexes with an unqualified gin_trgm_ops. With pg_trgm in a
    // schema off the path, create_store failed, and with pg_trgm anywhere the
    // 1.18.0 upgrade aborted: 'operator class "gin_trgm_ops" does not exist'.
    #[pg_test]
    fn pg_trgm_in_its_own_schema() {
        setup();
        if !pg_trgm_in_schema("ns_ext") {
            return;
        }
        let trgm_indexes = |schema: &str| -> i64 {
            Spi::get_one(&format!(
                "SELECT count(*) FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
                  WHERE c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = '{schema}') \
                    AND i.indclass[0] = (SELECT o.oid FROM pg_opclass o \
                          WHERE o.opcname = 'gin_trgm_ops' AND o.opcnamespace = 'ns_ext'::regnamespace)"
            ))
            .unwrap()
            .unwrap()
        };

        // A store made after pg_trgm, with ns_ext off the search path.
        Spi::run("SELECT create_store('trgm_new')").expect("create_store");
        assert_eq!(trgm_indexes("mentat_trgm_new"), 2);
        tx("trgm_new", SCHEMA_TX);
        tx("trgm_new", "[{:p/name \"fresh\"}]");
        assert_eq!(sorted(names("trgm_new")), vec!["fresh"]);

        // A 1.17 store rebuilt by the upgrade.
        Spi::run(
            "INSERT INTO mentat.stores (store_name, schema_name) VALUES ('trgm_old', 'mentat_trgm_old'); \
             CREATE SCHEMA mentat_trgm_old; \
             CREATE TABLE mentat_trgm_old.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
               value_type_tag SMALLINT NOT NULL, v_text TEXT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL)",
        )
        .expect("1.17 store");
        Spi::run(&upgrade_do_block()).expect("the upgrade must not abort");
        assert_eq!(trgm_indexes("mentat_trgm_old"), 2);
        tx("trgm_old", SCHEMA_TX);
        tx("trgm_old", "[{:p/name \"rebuilt\"}]");
        assert_eq!(sorted(names("trgm_old")), vec!["rebuilt"]);
    }

    /// Install pg_trgm in a schema of its own, off the search path. False if
    /// this server doesn't ship it.
    fn pg_trgm_in_schema(schema: &str) -> bool {
        let available: i64 =
            Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'pg_trgm'")
                .unwrap()
                .unwrap();
        if available == 0 {
            return false;
        }
        Spi::run(&format!(
            "DROP EXTENSION IF EXISTS pg_trgm CASCADE; \
             CREATE SCHEMA {schema}; CREATE EXTENSION pg_trgm SCHEMA {schema}"
        ))
        .expect("pg_trgm");
        true
    }

    // create_virtual_tables rewrote every "mentat.datoms_" in its SQL to the
    // store's schema, and the trigram index SQL already named the store's
    // schema: for a schema ending in "mentat" the rewrite matched inside it
    // (mentat_app_mentat_app_mentat.datoms_text_new). With pg_trgm installed,
    // create_store('app_mentat') and rename_store(x, 'x_mentat') failed, and
    // one such 1.17 store aborted ALTER EXTENSION UPDATE for the database.
    #[pg_test]
    fn store_names_ending_in_mentat() {
        setup();
        if !pg_trgm_in_schema("ns_trgm") {
            return;
        }
        let err = text("SELECT pg_temp.capture($q$SELECT create_store('x.mentat')$q$)");
        assert!(err.contains("letters, digits, and underscores"), "{err}");
        let check = |store: &str| {
            tx(store, SCHEMA_TX);
            tx(store, &format!("[{{:p/name \"in-{store}\"}}]"));
            assert_eq!(sorted(names(store)), vec![format!("in-{store}")]);
            let n: i64 = Spi::get_one(&format!(
                "SELECT count(*) FROM {}.facts WHERE attribute = ':p/name'",
                crate::functions::store_management::quote_ident(&format!("mentat_{store}"))
            ))
            .unwrap()
            .unwrap();
            assert_eq!(n, 1, "{store}");
        };
        for store in ["app_mentat", "mentat", "mentat_x"] {
            Spi::run(&format!("SELECT create_store('{store}')")).expect(store);
            check(store);
        }
        Spi::run("SELECT create_store('rn_src')").expect("create");
        Spi::run("SELECT rename_store('rn_src', 'x_mentat')").expect("rename to x_mentat");
        check("x_mentat");

        // The 1.17 -> 1.18 rebuild of such stores.
        for store in ["old_mentat", "mentat_old"] {
            Spi::run(&format!(
                "INSERT INTO mentat.stores (store_name, schema_name) VALUES ('{store}', 'mentat_{store}'); \
                 CREATE SCHEMA mentat_{store}; \
                 CREATE TABLE mentat_{store}.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
                   value_type_tag SMALLINT NOT NULL, v_text TEXT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL)"
            ))
            .expect("1.17 store");
        }
        Spi::run(&upgrade_do_block()).expect("the upgrade must not abort");
        for store in ["old_mentat", "mentat_old"] {
            check(store);
        }
    }

    // vector-near / turbovec-near / geom-* read mentat.attr_<entid>_* tables
    // that the mentat.attach_* helpers make for the default store only. On a
    // named store they joined the default store's rows by another store's
    // entids. They now refuse.
    #[pg_test]
    fn per_attribute_where_fns_refuse_named_stores() {
        setup();
        Spi::run("SELECT create_store('pa_s')").expect("create");
        tx("pa_s", SCHEMA_TX);
        for clause in [
            r#"[(vector-near $ :p/name "[1,0]" 3) [[?e ?d]]]"#,
            r#"[(turbovec-near $ :p/name "[1,0]" 3) [[?e ?d]]]"#,
            r#"[(geom-near $ :p/name "POINT(0 0)" 3) [[?e ?d]]]"#,
            r#"[(geom-within $ :p/name "POINT(0 0)" 1.0) [[?e ?d]]]"#,
            r#"[(geom-contains $ :p/name "POINT(0 0)") [[?e]]]"#,
            r#"[(geom-intersects $ :p/name "POINT(0 0)") [[?e]]]"#,
        ] {
            let query = format!("[:find ?e :where {clause}]");
            let err = text(&format!(
                "SELECT pg_temp.capture($q$SELECT q('pa_s', '{query}', '{{}}'::jsonb)$q$)"
            ));
            assert!(
                err.contains("not supported for named stores"),
                "{clause}: {err}"
            );
            // The default store is not refused (it fails later, if at all,
            // for a missing extension or table).
            let err = text(&format!(
                "SELECT pg_temp.capture($q$SELECT edn_q('{query}', '{{}}'::jsonb)$q$)"
            ));
            assert!(
                !err.contains("not supported for named stores"),
                "{clause}: {err}"
            );
        }
        Spi::run("SELECT drop_store('pa_s')").expect("drop");
    }

    // bm25-match took the attribute's entid from mentat.schema (the default
    // store) but read the named store's rows: it matched whatever attribute
    // had that entid in the named store.
    #[pg_test]
    fn bm25_match_uses_the_stores_attribute() {
        setup();
        let body = "[{:db/ident :bm/body :db/valueType :db.type/string \
                     :db/cardinality :db.cardinality/one}]";
        // Different entids for :bm/body in the two stores.
        tx("default", SCHEMA_TX);
        tx(
            "default",
            "[{:db/ident :bm/filler :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        tx("default", body);
        Spi::run("SELECT create_store('bm_s')").expect("create");
        tx("bm_s", body);
        let named: i64 =
            Spi::get_one("SELECT entid FROM mentat_bm_s.schema WHERE ident = ':bm/body'")
                .unwrap()
                .unwrap();
        let dflt: i64 = Spi::get_one("SELECT entid FROM mentat.schema WHERE ident = ':bm/body'")
            .unwrap()
            .unwrap();
        assert_ne!(named, dflt);
        let sql = json(
            r#"SELECT mentat_query_sql_store('bm_s',
                 '[:find ?e :where [(bm25-match $ :bm/body "fox" 5) [[?e ?s]]]]', '{}'::jsonb)"#,
        )["sql"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(sql.contains(&format!("t.a = {named} ")), "{sql}");
        assert!(sql.contains("mentat_bm_s.datoms_text_new"), "{sql}");
        Spi::run("SELECT drop_store('bm_s')").expect("drop");
    }

    // The [fts:lang=...] tag in an attribute's :db/doc was looked up with
    // store_id = 0, so a named store's (store_id N) was never found and its
    // fulltext search stemmed as English.
    #[pg_test]
    fn fts_language_tag_on_a_named_store() {
        setup();
        let attr = "[{:db/ident :ft/body :db/valueType :db.type/string \
                     :db/cardinality :db.cardinality/one :db/fulltext true \
                     :db/doc \"Body [fts:lang=german]\"}]";
        Spi::run("SELECT create_store('ft_s')").expect("create");
        tx("ft_s", attr);
        tx("default", attr);
        let query = "[:find ?e :where [(fulltext $ :ft/body \"haus\") [[?e ?v]]]]";
        for store in ["default", "ft_s"] {
            let sql = json(&format!(
                "SELECT mentat_query_sql_store('{store}', '{query}', '{{}}'::jsonb)"
            ))["sql"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert!(sql.contains("'german'"), "{store}: {sql}");
            assert!(!sql.contains("'english'"), "{store}: {sql}");
        }
        Spi::run("SELECT drop_store('ft_s')").expect("drop");
    }

    // log(store, ...) rendered attribute idents from the default store's
    // cache: a named store's attribute showed as a number, or as another
    // store's ident with the same entid.
    #[pg_test]
    fn log_renders_the_stores_idents() {
        setup();
        Spi::run("SELECT create_store('lg_s')").expect("create");
        let r = tx(
            "lg_s",
            "[{:db/ident :lg/only :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let t0 = r["db-after"]["basis-t"].as_i64().unwrap();
        let r = tx("lg_s", "[{:lg/only 42}]");
        let t1 = r["db-after"]["basis-t"].as_i64().unwrap();
        let log = json(&format!("SELECT log('lg_s', {t0}, {t1})"));
        let rows = log.as_array().cloned().unwrap_or_default();
        assert!(
            rows.iter().any(|d| d["a"] == ":lg/only" && d["v"] == 42),
            "{log}"
        );
        Spi::run("SELECT drop_store('lg_s')").expect("drop");
    }

    // subscribe(store, ...) put its trigger on the store's datoms view but
    // ran edn_q, so a t() on the named store NOTIFYed the default store's
    // result. The trigger keeps the hash of what it sent in a session
    // setting: it must be the hash of the named store's result.
    // (CREATE OR REPLACE TRIGGER, which subscribe uses, needs PostgreSQL 14.)
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscribe_on_a_named_store_queries_that_store() {
        setup();
        Spi::run("SELECT create_store('sb_s')").expect("create");
        tx("sb_s", SCHEMA_TX);
        tx("default", SCHEMA_TX);
        tx("default", "[{:p/name \"in-default\"}]");
        let query = "[:find [?n ...] :where [_ :p/name ?n]]";
        Spi::run(&format!("SELECT subscribe('sb_s', 'sb_names', '{query}')")).expect("subscribe");
        // Statement triggers on the store's datoms view fire once per t(),
        // after its data is written. They used to fire on the txInstant row,
        // written first, so a subscription sent the previous transaction's
        // result.
        Spi::run(
            "CREATE TABLE public.sb_fired (n BIGINT); \
             CREATE FUNCTION public.sb_fired() RETURNS trigger LANGUAGE plpgsql AS $$ \
               BEGIN INSERT INTO public.sb_fired SELECT count(*) FROM mentat_sb_s.current_text; \
               RETURN NULL; END $$; \
             CREATE TRIGGER sb_fired AFTER INSERT OR UPDATE OR DELETE ON mentat_sb_s.datoms \
               FOR EACH STATEMENT EXECUTE FUNCTION public.sb_fired()",
        )
        .expect("counting trigger");
        tx("sb_s", "[{:p/name \"in-named\"}]");
        let fired = json("SELECT jsonb_agg(n) FROM public.sb_fired");
        assert_eq!(fired, serde_json::json!([1]), "fired once, after the data");
        let sent = text("SELECT current_setting('mentat.sub_hash_sb_names', true)");
        let named = text(&format!(
            "SELECT md5(q('sb_s', '{query}', '{{}}'::jsonb)::text)"
        ));
        let dflt = text(&format!(
            "SELECT md5(edn_q('{query}', '{{}}'::jsonb)::text)"
        ));
        assert_ne!(named, dflt);
        assert_eq!(
            sent, named,
            "the trigger evaluated another store, or ran early"
        );
        Spi::run("SELECT unsubscribe('sb_s', 'sb_names')").expect("unsubscribe");
        Spi::run("DROP TRIGGER sb_fired ON mentat_sb_s.datoms").expect("drop counting trigger");
        Spi::run("SELECT drop_store('sb_s')").expect("drop");
    }

    // The default store's mentat.datoms view: its statement triggers fire
    // once per t(), after the data, too (t() used to send the txInstant row
    // through it first).
    #[pg_test]
    fn default_store_view_triggers_fire_after_the_data() {
        // Before setup(): other tests' bootstrap writes through this view,
        // and taking the lock after ours would deadlock with them.
        Spi::run("LOCK TABLE mentat.datoms IN SHARE ROW EXCLUSIVE MODE").expect("lock");
        setup();
        tx("default", SCHEMA_TX);
        Spi::run(
            "CREATE TABLE public.df_fired (n BIGINT); \
             CREATE FUNCTION public.df_fired() RETURNS trigger LANGUAGE plpgsql AS $$ \
               BEGIN INSERT INTO public.df_fired SELECT count(*) FROM mentat.current_text \
                 WHERE store_id = 0 AND v = 'df-name'; RETURN NULL; END $$; \
             CREATE TRIGGER df_fired AFTER INSERT OR UPDATE OR DELETE ON mentat.datoms \
               FOR EACH STATEMENT EXECUTE FUNCTION public.df_fired()",
        )
        .expect("counting trigger");
        tx("default", "[{:p/name \"df-name\"}]");
        let fired = json("SELECT jsonb_agg(n) FROM public.df_fired");
        assert_eq!(fired, serde_json::json!([1]), "once, after the data");
    }

    // The subscription trigger baked in the store's name: after rename_store
    // every t() on the store failed with "Store 'sb_a' not found", and
    // unsubscribe could not remove the trigger under either name.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscription_survives_rename_store() {
        setup();
        Spi::run("SELECT create_store('sbr_a')").expect("create");
        tx("sbr_a", SCHEMA_TX);
        let query = "[:find [?n ...] :where [_ :p/name ?n]]";
        Spi::run(&format!(
            "SELECT subscribe('sbr_a', 'sbr_names', '{query}')"
        ))
        .expect("subscribe");
        tx("sbr_a", "[{:p/name \"before\"}]");
        Spi::run("SELECT rename_store('sbr_a', 'sbr_b')").expect("rename");
        tx("sbr_b", "[{:p/name \"after\"}]");
        // The trigger ran q() on the renamed store (see
        // subscribe_on_a_named_store_queries_that_store for the hash).
        let sent = text("SELECT current_setting('mentat.sub_hash_sbr_names', true)");
        let now = text(&format!(
            "SELECT md5(q('sbr_b', '{query}', '{{}}'::jsonb)::text)"
        ));
        assert_eq!(sent, now);
        assert_eq!(sorted(names("sbr_b")), vec!["after", "before"]);
        let listed = json("SELECT list_subscriptions('sbr_b')");
        assert_eq!(listed.as_array().map(|a| a.len()), Some(1), "{listed}");
        Spi::run("SELECT unsubscribe('sbr_b', 'sbr_names')")
            .expect("unsubscribe under the new name");
        let triggers: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_trigger WHERE tgname = 'mentat_sub_trg_sbr_names'",
        )
        .unwrap()
        .unwrap();
        assert_eq!(triggers, 0);
        tx("sbr_b", "[{:p/name \"unsubscribed\"}]");

        // drop_store forgets the store's subscriptions: a new store of the
        // same name can subscribe again, and a rename can take the name.
        Spi::run(&format!(
            "SELECT subscribe('sbr_b', 'sbr_names', '{query}')"
        ))
        .expect("again");
        Spi::run("SELECT drop_store('sbr_b')").expect("drop");
        Spi::run("SELECT create_store('sbr_c')").expect("create c");
        Spi::run(&format!(
            "SELECT subscribe('sbr_c', 'sbr_names', '{query}')"
        ))
        .expect("sub c");
        Spi::run("SELECT rename_store('sbr_c', 'sbr_b')").expect("rename to a dropped name");
        Spi::run("SELECT unsubscribe('sbr_b', 'sbr_names')").expect("unsubscribe");
        Spi::run("SELECT drop_store('sbr_b')").expect("drop");
    }

    // Two stores could both subscribe under one name. The channel
    // (mentat_<name>) and the hash setting carry no store, so a listener for
    // one store's "names" got the other store's results. A name is now one
    // per database.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscription_names_are_shared_by_all_stores() {
        setup();
        Spi::run("SELECT create_store('sn_a'), create_store('sn_b')").expect("create");
        let query = "[:find ?e :where [?e :db/ident _]]";
        Spi::run(&format!("SELECT subscribe('sn_a', 'sn_names', '{query}')")).expect("first");
        let err = text(&format!(
            "SELECT pg_temp.capture($q$SELECT subscribe('sn_b', 'sn_names', '{query}')$q$)"
        ));
        assert!(
            err.contains("already used by store 'sn_a'"),
            "the second store must be refused: {err:?}"
        );
        Spi::run("SELECT unsubscribe('sn_a', 'sn_names')").expect("unsubscribe");
        Spi::run(&format!("SELECT subscribe('sn_b', 'sn_names', '{query}')")).expect("free again");
        Spi::run("SELECT drop_store('sn_a'), drop_store('sn_b')").expect("drop");
    }

    // The check above compared names case-sensitively, but LISTEN, the hash
    // setting and the trigger names fold case: subscribe('ka', 'Names') and
    // subscribe('kb', 'names') both passed, and `LISTEN mentat_Names` (as
    // printed) got kb's results. On one store 'Foo' and 'foo' shared one
    // trigger, so unsubscribe('Foo') dropped foo's.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscription_names_ignore_case() {
        setup();
        Spi::run("SELECT create_store('sc_a'), create_store('sc_b')").expect("create");
        let query = "[:find ?e :where [?e :db/ident _]]";
        let sub = |store: &str, name: &str| {
            text(&format!(
                "SELECT pg_temp.capture($q$SELECT subscribe('{store}', '{name}', '{query}')$q$)"
            ))
        };
        let made = text(&format!("SELECT subscribe('sc_a', 'Sc_Names', '{query}')"));
        assert!(
            made.contains("LISTEN \"mentat_Sc_Names\" to"),
            "quoted, as pg_notify sends it: {made:?}"
        );
        let err = sub("sc_b", "sc_names");
        assert!(
            err.contains("already used by store 'sc_a' as 'Sc_Names'"),
            "another store, other case: {err:?}"
        );
        assert_eq!(sub("sc_a", "Sc_Foo"), "");
        let err = sub("sc_a", "sc_foo");
        assert!(
            err.contains("already used by store 'sc_a' as 'Sc_Foo'"),
            "same store, other case: {err:?}"
        );
        let err = sub("sc_a", "Sc_Foo");
        assert!(err.contains("already exists for store 'sc_a'"), "{err:?}");
        let rows: i64 = Spi::get_one(
            "SELECT count(*) FROM mentat.subscriptions WHERE store_name IN ('sc_a', 'sc_b')",
        )
        .unwrap()
        .unwrap();
        assert_eq!(rows, 2, "Names and Foo only");
        Spi::run("SELECT unsubscribe('sc_a', 'Sc_Foo')").expect("unsubscribe");
        assert_eq!(sub("sc_b", "sc_foo"), "", "free again");
        Spi::run("SELECT drop_store('sc_a'), drop_store('sc_b')").expect("drop");
    }

    // The unique index behind subscribe()'s check: it holds for rows written
    // any way, and CREATE OR REPLACE TRIGGER (PostgreSQL 14) is not needed.
    #[pg_test]
    fn subscription_names_unique_index() {
        setup();
        let def = text(
            "SELECT indexdef FROM pg_indexes WHERE schemaname = 'mentat' \
              AND indexname = 'idx_subscriptions_lower_name'",
        );
        assert!(def.starts_with("CREATE UNIQUE INDEX"), "{def:?}");
        Spi::run(
            "INSERT INTO mentat.subscriptions (store_name, name, query) VALUES ('ui_a', 'Ui_Dup', 'q')",
        )
        .expect("first");
        let err = text(
            "SELECT pg_temp.capture($q$INSERT INTO mentat.subscriptions (store_name, name, query) \
               VALUES ('ui_b', 'uI_dUP', 'q')$q$)",
        );
        assert!(err.contains("idx_subscriptions_lower_name"), "{err:?}");
    }

    // Two subscribe() calls of one name on two stores, at once: the check
    // can't see the other's uncommitted row, so both succeeded and one
    // channel carried both stores' results. Two dblink sessions; everything
    // they do is committed, so the stores are dropped again at the end.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn concurrent_subscribes_of_one_name() {
        crate::ensure_extension_loaded();
        let available: i64 =
            Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'dblink'")
                .unwrap()
                .unwrap();
        if available == 0 {
            return;
        }
        Spi::run(
            "CREATE EXTENSION IF NOT EXISTS dblink; \
             CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("dblink");
        let conn = text(
            "SELECT pg_catalog.format('dbname=%s port=%s host=%s', current_database(), \
               current_setting('port'), split_part(current_setting('unix_socket_directories'), ',', 1))",
        );
        let on = |c: &str, sql: &str| {
            Spi::run(&format!(
                "SELECT dblink_exec('{c}', '{}')",
                sql.replace('\'', "''")
            ))
            .unwrap_or_else(|e| panic!("{c}: {sql}: {e}"))
        };
        let drop_stores = |c: &str| {
            for st in ["cs_a", "cs_b"] {
                on(
                    c,
                    &format!(
                        "DO $d$ BEGIN PERFORM drop_store('{st}'); \
                         EXCEPTION WHEN OTHERS THEN NULL; END $d$"
                    ),
                )
            }
        };
        Spi::run(&format!(
            "SELECT dblink_connect('ns_r1', '{conn}'), dblink_connect('ns_r2', '{conn}')"
        ))
        .expect("connect");
        on("ns_r1", "SET search_path TO mentat, public");
        on("ns_r2", "SET search_path TO mentat, public");
        drop_stores("ns_r1");
        on(
            "ns_r1",
            "DO $d$ BEGIN PERFORM create_store('cs_a'); PERFORM create_store('cs_b'); END $d$",
        );
        let query = "[:find ?e :where [?e :db/ident _]]";
        on("ns_r1", "BEGIN");
        on(
            "ns_r1",
            &format!("DO $d$ BEGIN PERFORM subscribe('cs_a', 'cs_name', '{query}'); END $d$"),
        );
        let pid2: i32 =
            Spi::get_one("SELECT p FROM dblink('ns_r2', 'SELECT pg_backend_pid()') AS t(p int)")
                .unwrap()
                .unwrap();
        Spi::run(&format!(
            "SELECT dblink_send_query('ns_r2', $q$SELECT subscribe('cs_b', 'cs_name', '{query}')$q$)"
        ))
        .expect("send");
        // Give the second session up to 10 s to reach the first's row.
        let mut waited = false;
        for _ in 0..200 {
            let n: i64 = Spi::get_one(&format!(
                "SELECT count(*) FROM pg_locks WHERE pid = {pid2} AND NOT granted"
            ))
            .unwrap()
            .unwrap();
            if n > 0 {
                waited = true;
                break;
            }
            Spi::run("SELECT pg_sleep(0.05)").unwrap();
        }
        on("ns_r1", "COMMIT");
        let second = text(
            "SELECT pg_temp.capture($q$SELECT * FROM dblink_get_result('ns_r2') AS t(r text)$q$)",
        );
        let held: Vec<String> = Spi::get_one::<Vec<String>>(
            "SELECT s FROM dblink('ns_r1', \
               'SELECT array_agg(store_name ORDER BY store_name) FROM mentat.subscriptions \
                 WHERE lower(name) = ''cs_name''') AS t(s text[])",
        )
        .unwrap()
        .unwrap_or_default();
        drop_stores("ns_r1");
        Spi::run("SELECT dblink_disconnect('ns_r1'), dblink_disconnect('ns_r2')").expect("close");
        assert_eq!(held, vec!["cs_a"], "one store holds the name");
        assert!(
            second.contains("already used by store 'cs_a'"),
            "the second subscribe must be refused: {second:?}"
        );
        assert!(waited, "the second subscribe waited for the first");
    }

    // A 1.17 database can hold subscription names that collide ignoring
    // case. The upgrade deletes none of them: it names each group and what
    // to unsubscribe, and leaves the unique index out until they are gone.
    // (A WARNING can't be caught, so each is raised as an error here.)
    #[pg_test]
    fn upgrade_reports_colliding_subscription_names() {
        setup();
        Spi::run(
            "DROP INDEX mentat.idx_subscriptions_lower_name; \
             INSERT INTO mentat.subscriptions (store_name, name, query) VALUES \
               ('du_a', 'Du_Dup', 'q1'), ('du_b', 'du_dup', 'q2'), ('du_b', 'DU_DUP', 'q3'), \
               ('du_c', 'Du_Foo', 'q4'), ('du_c', 'du_foo', 'q5'), ('du_a', 'du_solo', 'q6')",
        )
        .expect("1.17 rows");
        let raised = |what: &str| -> String {
            let block = upgrade_block("subnames").replace(
                &format!("RAISE WARNING 'pg_mentat upgrade: {what}"),
                &format!("RAISE EXCEPTION 'pg_mentat upgrade: {what}"),
            );
            text(&format!("SELECT pg_temp.capture($cap${block}$cap$)"))
        };
        let index = || -> i64 {
            Spi::get_one(
                "SELECT count(*) FROM pg_indexes WHERE schemaname = 'mentat' \
                  AND indexname = 'idx_subscriptions_lower_name'",
            )
            .unwrap()
            .unwrap()
        };
        let dup = raised("subscriptions");
        assert!(
            dup.contains(
                "'Du_Dup' on store 'du_a', 'du_dup' on store 'du_b', 'DU_DUP' on store 'du_b'"
            ) && dup.contains("run unsubscribe('du_b', 'du_dup'); unsubscribe('du_b', 'DU_DUP').")
                && !dup.contains("subscribe it again"),
            "{dup:?}"
        );
        let skipped = raised("index");
        assert!(
            skipped.contains("not created") && skipped.contains("subscribe() still refuses"),
            "{skipped:?}"
        );
        // As WARNINGs they don't stop the upgrade, and nothing is deleted.
        Spi::run(&upgrade_block("subnames")).expect("upgrade");
        assert_eq!(index(), 0);
        let rows: i64 =
            Spi::get_one("SELECT count(*) FROM mentat.subscriptions WHERE query ~ '^q'")
                .unwrap()
                .unwrap();
        assert_eq!(rows, 6, "no row deleted");

        // Two on one store share a trigger: unsubscribing one drops the
        // other's, so the keeper must be subscribed again.
        Spi::run("DELETE FROM mentat.subscriptions WHERE store_name = 'du_b'").unwrap();
        let foo = raised("subscriptions");
        assert!(
            foo.contains("run unsubscribe('du_c', 'du_foo').")
                && foo.contains("Then run unsubscribe('du_c', 'Du_Foo') and subscribe it again"),
            "{foo:?}"
        );

        // Once each name is held once, the upgrade builds the index.
        Spi::run("DELETE FROM mentat.subscriptions WHERE store_name = 'du_c' AND name = 'du_foo'")
            .unwrap();
        Spi::run(&upgrade_block("subnames")).expect("upgrade");
        assert_eq!(index(), 1);
    }

    // A subscription name is at most 48 characters, so its trigger name
    // (mentat_sub_trg_<name>) fits 63. 1.17 took 58: a 57-character name
    // made every t() on its store fail ("channel name too long"), and two
    // names over 48 that start the same way shared one trigger.
    // (subscribe() uses CREATE OR REPLACE TRIGGER, PostgreSQL 14+.)
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscription_names_are_at_most_48_characters() {
        setup();
        Spi::run("SELECT create_store('sl_s')").expect("create");
        tx("sl_s", SCHEMA_TX);
        let query = "[:find [?n ...] :where [_ :p/name ?n]]";
        let n48 = format!("sl_{}", "a".repeat(45));
        Spi::run(&format!("SELECT subscribe('sl_s', '{n48}', '{query}')")).expect("48 is fine");
        let trg: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM pg_trigger WHERE tgname = 'mentat_sub_trg_{n48}'"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(trg, 1, "the trigger name is not cut");
        tx("sl_s", "[{:p/name \"after-48\"}]");
        for n in [49, 52, 57, 58] {
            let name = format!("sl_{}", "b".repeat(n - 3));
            let err = text(&format!(
                "SELECT pg_temp.capture($$SELECT subscribe('sl_s', '{name}', '{query}')$$)"
            ));
            assert!(
                err.contains(&format!("is {n} characters; the limit is 48")),
                "{n}: {err:?}"
            );
        }
        let rows: i64 =
            Spi::get_one("SELECT count(*) FROM mentat.subscriptions WHERE name LIKE 'sl\\_b%'")
                .unwrap()
                .unwrap();
        assert_eq!(rows, 0);
        tx("sl_s", "[{:p/name \"after-refusals\"}]");
        // A longer name left by 1.17 can still be unsubscribed.
        let old = format!("sl_{}", "c".repeat(54));
        Spi::run(&format!(
            "INSERT INTO mentat.subscriptions (store_name, name, query) VALUES ('sl_s', '{old}', 'q')"
        ))
        .unwrap();
        Spi::run(&format!("SELECT unsubscribe('sl_s', '{old}')")).expect("unsubscribe old");
        Spi::run(&format!("SELECT unsubscribe('sl_s', '{n48}')")).expect("unsubscribe");
    }

    // The upgrade names each subscription over 48 characters in a WARNING
    // (the channel note for 57 and over), and changes no row.
    #[pg_test]
    fn upgrade_warns_on_long_subscription_names() {
        setup();
        let (n57, n50, n48) = (
            format!("ul_{}", "a".repeat(54)),
            format!("ul_{}", "b".repeat(47)),
            format!("ul_{}", "c".repeat(45)),
        );
        Spi::run(&format!(
            "INSERT INTO mentat.subscriptions (store_name, name, query) VALUES \
               ('ul_a', '{n57}', 'q1'), ('ul_b', '{n50}', 'q2'), ('ul_c', '{n48}', 'q3')"
        ))
        .expect("1.17 rows");
        let raised = || -> String {
            let block = upgrade_block("subnames").replace(
                "RAISE WARNING 'pg_mentat upgrade: subscription % on",
                "RAISE EXCEPTION 'pg_mentat upgrade: subscription % on",
            );
            text(&format!("SELECT pg_temp.capture($cap${block}$cap$)"))
        };
        let w = raised();
        assert!(
            w.contains(&format!(
                "subscription '{n57}' on store 'ul_a' has a name of 57 characters"
            )) && w.contains("channel name too long")
                && w.contains(&format!(
                    "Run unsubscribe('ul_a', '{n57}') and subscribe it again"
                )),
            "{w:?}"
        );
        Spi::run("UPDATE mentat.subscriptions SET store_name = 'ul_z' WHERE store_name = 'ul_a'")
            .unwrap();
        let w = raised();
        assert!(
            w.contains(&format!(
                "subscription '{n50}' on store 'ul_b' has a name of 50 characters"
            )) && w.contains("shares its trigger")
                && !w.contains("channel name too long"),
            "{w:?}"
        );
        // As WARNINGs they don't stop the upgrade; no row changes.
        Spi::run(&upgrade_block("subnames")).expect("upgrade");
        let names: Vec<String> = sorted(json(
            "SELECT jsonb_agg(name) FROM mentat.subscriptions WHERE name LIKE 'ul\\_%'",
        ));
        let mut want = vec![n57, n50, n48];
        want.sort();
        assert_eq!(names, want);
    }

    // An on_write materialized view's trigger baked in the store's schema,
    // and its body the store's name: after rename_store every t() on the
    // store failed with `schema "mentat_mv_a" does not exist`.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn on_write_matview_survives_rename_store() {
        setup();
        Spi::run("SELECT create_store('mvr_a')").expect("create");
        tx("mvr_a", SCHEMA_TX);
        // materialize's column inference is the next fix's subject.
        tx("default", SCHEMA_TX);
        tx("mvr_a", "[{:p/name \"one\"}]");
        Spi::run(
            "SELECT materialize('mvr_a', 'mvr_names', '[:find ?n :where [_ :p/name ?n]]', 'on_write')",
        )
        .expect("materialize");
        let rows = |schema: &str| -> Vec<String> {
            let v = json(&format!(
                "SELECT coalesce(jsonb_agg(n ORDER BY n), '[]') FROM {schema}.mvr_names"
            ));
            sorted(v)
        };
        tx("mvr_a", "[{:p/name \"two\"}]");
        // Refreshed after the transaction's data is in, not one behind.
        assert_eq!(rows("mentat_mvr_a"), vec!["one", "two"]);
        Spi::run("SELECT rename_store('mvr_a', 'mvr_b')").expect("rename");
        tx("mvr_b", "[{:p/name \"three\"}]");
        assert_eq!(rows("mentat_mvr_b"), vec!["one", "three", "two"]);
        Spi::run("SELECT refresh('mvr_b', 'mvr_names')").expect("refresh under the new name");
        let listed = json("SELECT list_matviews('mvr_b')");
        assert_eq!(listed.as_array().map(|a| a.len()), Some(1), "{listed}");
        Spi::run("SELECT drop_matview('mvr_b', 'mvr_names')").expect("drop_matview");
        tx("mvr_b", "[{:p/name \"four\"}]");
        // drop_store forgets the store's views: the name can be used again.
        Spi::run(
            "SELECT materialize('mvr_b', 'mvr_names', '[:find ?n :where [_ :p/name ?n]]', 'on_write')",
        )
        .expect("materialize again");
        Spi::run("SELECT drop_store('mvr_b')").expect("drop");
        Spi::run("SELECT create_store('mvr_b')").expect("create again");
        tx("mvr_b", SCHEMA_TX);
        Spi::run(
            "SELECT materialize('mvr_b', 'mvr_names', '[:find ?n :where [_ :p/name ?n]]', 'manual')",
        )
        .expect("materialize in the new store");
        Spi::run("SELECT drop_store('mvr_b')").expect("drop");
    }

    // (pull ?e [...]) in :find called edn_pull, which reads the default
    // store: on a named store it returned the default store's values for the
    // same entity id. Entity ids overlap across stores.
    #[pg_test]
    fn find_pull_on_a_named_store_reads_that_store() {
        setup();
        Spi::run("SELECT create_store('fp_s')").expect("create");
        tx("fp_s", SCHEMA_TX);
        tx("default", SCHEMA_TX);
        let e = tx("fp_s", "[{:db/id \"x\" :p/name \"in-named\" :p/age 7}]")["tempids"]["x"]
            .as_i64()
            .expect("tempid");
        tx(
            "default",
            &format!("[[:db/add {e} :p/name \"in-default\"]]"),
        );
        let got = q(
            "fp_s",
            "[:find (pull ?e [:p/name :p/age]) . :where [?e :p/name _]]",
            "{}",
        );
        assert_eq!(
            got["result"][":p/name"],
            serde_json::json!("in-named"),
            "{got}"
        );
        assert_eq!(got["result"][":p/age"], serde_json::json!(7), "{got}");
        let rel = q(
            "fp_s",
            "[:find ?n (pull ?e [:p/name]) :where [?e :p/name ?n]]",
            "{}",
        );
        assert_eq!(
            rel["results"][0][1][":p/name"],
            serde_json::json!("in-named"),
            "{rel}"
        );
        // The default store still pulls its own.
        let dflt = q(
            "default",
            "[:find (pull ?e [:p/name]) . :where [?e :p/name _]]",
            "{}",
        );
        assert_eq!(
            dflt["result"][":p/name"],
            serde_json::json!("in-default"),
            "{dflt}"
        );
        Spi::run("SELECT drop_store('fp_s')").expect("drop");
    }

    // A 1.17 subscription or materialized view on a named store stays with
    // the old tables, under _pre118, and stops working. The upgrade said
    // nothing about them; now it names each one. (A WARNING can't be caught,
    // so each is raised as an error here.)
    #[pg_test]
    fn upgrade_names_old_subscriptions_and_views() {
        setup();
        Spi::run(
            "INSERT INTO mentat.stores (store_name, schema_name) VALUES ('up_sv', 'mentat_up_sv'); \
             CREATE SCHEMA mentat_up_sv; \
             CREATE TABLE mentat_up_sv.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
               value_type_tag SMALLINT NOT NULL, v_text TEXT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL); \
             INSERT INTO mentat.subscriptions (store_name, name, query) VALUES ('up_sv', 'oldsub', '[:find ?e]'); \
             INSERT INTO mentat.materialized_views (store_name, view_name, datalog_query, refresh_policy) \
               VALUES ('up_sv', 'oldmv', '[:find ?e]', 'on_write')",
        )
        .expect("1.17 store with a subscription and a view");
        let raised = |what: &str| -> String {
            let block = upgrade_do_block().replace(
                &format!("RAISE WARNING 'pg_mentat upgrade: {what}"),
                &format!("RAISE EXCEPTION 'pg_mentat upgrade: {what}"),
            );
            text(&format!("SELECT pg_temp.capture($cap${block}$cap$)"))
        };
        let sub = raised("subscription");
        assert!(
            sub.contains("subscription \"oldsub\" on named store \"up_sv\"")
                && sub.contains("unsubscribe('up_sv', 'oldsub')"),
            "{sub:?}"
        );
        let mv = raised("materialized view");
        assert!(
            mv.contains("materialized view \"oldmv\" on named store \"up_sv\"")
                && mv.contains("drop_matview('up_sv', 'oldmv')"),
            "{mv:?}"
        );
        // As a WARNING it does not stop the upgrade.
        Spi::run(&upgrade_do_block()).expect("upgrade");
        assert_eq!(
            text("SELECT nspname::text FROM pg_namespace WHERE nspname = 'mentat_up_sv_pre118'"),
            "mentat_up_sv_pre118"
        );
    }

    // A find-pull on a named store under asOf / since called the 3-argument
    // pull(store, ...), which reads current state: an as-of query returned
    // the entity's current value (#364's bug, on named stores).
    #[pg_test]
    fn historical_find_pull_on_a_named_store() {
        setup();
        Spi::run("SELECT create_store('hp_s')").expect("create");
        tx("hp_s", SCHEMA_TX);
        tx("default", SCHEMA_TX);
        let r = tx("hp_s", "[{:db/id \"x\" :p/name \"v1\" :p/age 1}]");
        let e = r["tempids"]["x"].as_i64().expect("tempid");
        let t1 = r["db-after"]["basis-t"].as_i64().unwrap();
        let t2 = tx("hp_s", &format!("[[:db/add {e} :p/name \"v2\"]]"))["db-after"]["basis-t"]
            .as_i64()
            .unwrap();
        tx("hp_s", &format!("[[:db/add {e} :p/age 3]]"));
        tx(
            "default",
            &format!("[[:db/add {e} :p/name \"in-default\"] [:db/add {e} :p/age 99]]"),
        );
        let pull = |opts: &str| -> serde_json::Value {
            q(
                "hp_s",
                "[:find (pull ?e [:p/name :p/age]) . :where [?e :p/name _]]",
                opts,
            )["result"]
                .clone()
        };
        let as_of = pull(&format!("{{\"asOf\": {t1}}}"));
        assert_eq!(as_of[":p/name"], serde_json::json!("v1"), "{as_of}");
        assert_eq!(as_of[":p/age"], serde_json::json!(1), "{as_of}");
        // Since t2: only the current datoms transacted after t2 (:p/age 3).
        let since = q(
            "hp_s",
            "[:find (pull ?e [:p/name :p/age]) . :where [?e :p/age _]]",
            &format!("{{\"since\": {t2}}}"),
        )["result"]
            .clone();
        assert_eq!(since[":p/age"], serde_json::json!(3), "{since}");
        assert!(since.get(":p/name").is_none(), "{since}");
        let now = pull("{}");
        assert_eq!(now[":p/name"], serde_json::json!("v2"), "{now}");
        // The 4-argument pull directly.
        let direct = json(&format!(
            "SELECT pull('hp_s', '[:p/name]', {e}, '{{\"asOf\": {t1}}}'::jsonb)"
        ));
        assert_eq!(direct[":p/name"], serde_json::json!("v1"), "{direct}");
        Spi::run("SELECT drop_store('hp_s')").expect("drop");
    }

    // A hand-made mentat.stores row with a name validate_store_name reserves
    // passed the upgrade's name check; create_virtual_tables then refused it
    // and ALTER EXTENSION UPDATE aborted. Such rows are skipped now.
    #[pg_test]
    fn upgrade_skips_reserved_store_names() {
        setup();
        for name in ["pg_x", "PG_Y", "information_schema", "Default"] {
            Spi::run(&format!(
                "INSERT INTO mentat.stores (store_name, schema_name) VALUES ('{name}', 'mentat_{name}'); \
                 CREATE SCHEMA \"mentat_{name}\"; \
                 CREATE TABLE \"mentat_{name}\".datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
                   value_type_tag SMALLINT NOT NULL, v_text TEXT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL)"
            ))
            .expect("1.17-style row");
        }
        Spi::run(&upgrade_do_block()).expect("the upgrade must not abort");
        let renamed: i64 =
            Spi::get_one("SELECT count(*) FROM pg_namespace WHERE nspname LIKE '%\\_pre118'")
                .unwrap()
                .unwrap();
        assert_eq!(renamed, 0, "reserved-name stores are left alone");
        let wide: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_attribute a JOIN pg_class c ON c.oid = a.attrelid \
              WHERE c.relname = 'datoms' AND a.attname = 'value_type_tag' AND c.relnamespace IN \
                (SELECT oid FROM pg_namespace WHERE nspname IN \
                  ('mentat_pg_x', 'mentat_PG_Y', 'mentat_information_schema', 'mentat_Default'))",
        )
        .unwrap()
        .unwrap();
        assert_eq!(wide, 4);
    }

    // materialize(store, ...) worked out the view's columns with the default
    // store's mentat_query_sql: an attribute only the named store defines
    // failed with ":p/name is not registered".
    #[pg_test]
    fn materialize_infers_columns_from_the_store() {
        setup();
        Spi::run("SELECT create_store('mvc_s')").expect("create");
        tx("mvc_s", SCHEMA_TX);
        tx("mvc_s", "[{:p/name \"only-here\" :p/age 3}]");
        Spi::run(
            "SELECT materialize('mvc_s', 'mvc_v', '[:find ?n ?a :where [?e :p/name ?n] [?e :p/age ?a]]')",
        )
        .expect("materialize on an attribute the default store lacks");
        let row = json("SELECT to_jsonb(v) FROM mentat_mvc_s.mvc_v v");
        assert_eq!(row["n"], serde_json::json!("only-here"), "{row}");
        assert_eq!(row["a"], serde_json::json!("3"), "{row}");
        Spi::run("SELECT drop_store('mvc_s')").expect("drop");
    }

    // The trigram indexes were named idx_<store>_trgm_text / _keyword. From
    // 53 characters both names truncated to the same 63, so the keyword
    // index was skipped ("already exists"); and each rename_store added two
    // more under the new name.
    #[pg_test]
    fn trigram_indexes_long_names_and_renames() {
        setup();
        if !pg_trgm_in_schema("ns_trgm7") {
            return;
        }
        let per_table = |schema: &str| -> Vec<String> {
            let v = json(&format!(
                "SELECT coalesce(jsonb_agg(t.relname || ':' || c.relname ORDER BY t.relname, c.relname), '[]') \
                   FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
                   JOIN pg_class t ON t.oid = i.indrelid \
                   JOIN pg_opclass o ON o.oid = i.indclass[0] \
                  WHERE o.opcname = 'gin_trgm_ops' AND t.relnamespace = \
                    (SELECT oid FROM pg_namespace WHERE nspname = '{schema}')"
            ));
            sorted(v)
        };
        let expected = vec![
            "datoms_keyword_new:idx_datoms_keyword_new_trgm".to_string(),
            "datoms_text_new:idx_datoms_text_new_trgm".to_string(),
        ];
        let long = "k".repeat(56);
        Spi::run(&format!("SELECT create_store('{long}')")).expect("create");
        assert_eq!(per_table(&format!("mentat_{long}")), expected);
        Spi::run("SELECT create_store('tg_rn1')").expect("create");
        Spi::run("SELECT rename_store('tg_rn1', 'tg_rn2')").expect("rename");
        Spi::run("SELECT rename_store('tg_rn2', 'tg_rn3')").expect("rename");
        assert_eq!(per_table("mentat_tg_rn3"), expected);
        // A trigram index the store already has, under any name, is kept and
        // not doubled.
        Spi::run(
            "DROP INDEX mentat_tg_rn3.idx_datoms_text_new_trgm; \
             CREATE INDEX mine ON mentat_tg_rn3.datoms_text_new USING GIN (v ns_trgm7.gin_trgm_ops)",
        )
        .expect("own index");
        Spi::run("SELECT rename_store('tg_rn3', 'tg_rn4')").expect("rename");
        assert_eq!(
            per_table("mentat_tg_rn4"),
            vec![
                "datoms_keyword_new:idx_datoms_keyword_new_trgm".to_string(),
                "datoms_text_new:mine".to_string()
            ]
        );
    }

    // The attribute-not-found hint on a named store listed, and suggested,
    // the default store's attributes.
    #[pg_test]
    fn attribute_not_found_hint_lists_the_stores_attributes() {
        setup();
        tx(
            "default",
            "[{:db/ident :dflt/secret_attr :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
        );
        Spi::run("SELECT create_store('hint_s')").expect("create");
        tx(
            "hint_s",
            "[{:db/ident :hint/own_attr :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
        );
        for edn in ["[{:dflt/secret_atr \"x\"}]", "[{:hint/own_atr \"x\"}]"] {
            let err = text(&format!(
                "SELECT pg_temp.capture($q$SELECT t('hint_s', '{edn}')$q$)"
            ));
            assert!(err.contains("attribute-not-found"), "{err}");
            assert!(!err.contains(":dflt/secret_attr"), "{err}");
        }
        let err =
            text("SELECT pg_temp.capture($q$SELECT t('hint_s', '[{:hint/own_atr \"x\"}]')$q$)");
        assert!(err.contains("Did you mean ':hint/own_attr'?"), "{err}");
        Spi::run("SELECT drop_store('hint_s')").expect("drop");
    }

    // The upgrade (as superuser) handed a rebuilt store's tables, views,
    // sequences and functions to the schema's owner, but not the extended
    // statistics the tables' LIKE ... INCLUDING ALL copied.
    #[pg_test]
    fn upgrade_hands_statistics_to_the_store_owner() {
        setup();
        Spi::run(
            "CREATE ROLE ns_stat_owner NOLOGIN; \
             INSERT INTO mentat.stores (store_name, schema_name) VALUES ('stat_s', 'mentat_stat_s'); \
             CREATE SCHEMA mentat_stat_s AUTHORIZATION ns_stat_owner; \
             CREATE TABLE mentat_stat_s.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
               value_type_tag SMALLINT NOT NULL, v_text TEXT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL)",
        )
        .expect("1.17 store");
        Spi::run(&upgrade_do_block()).expect("upgrade");
        let ns = "(SELECT oid FROM pg_namespace WHERE nspname = 'mentat_stat_s')";
        let stats: i64 = Spi::get_one(&format!(
            "SELECT count(*) FROM pg_statistic_ext WHERE stxnamespace = {ns}"
        ))
        .unwrap()
        .unwrap();
        assert!(
            stats > 0,
            "the layout copies the default store's statistics"
        );
        let foreign: i64 = Spi::get_one(&format!(
            "SELECT (SELECT count(*) FROM pg_statistic_ext WHERE stxnamespace = {ns} \
                       AND stxowner <> 'ns_stat_owner'::regrole) \
                  + (SELECT count(*) FROM pg_class WHERE relnamespace = {ns} \
                       AND relkind IN ('r', 'v', 'S') AND relowner <> 'ns_stat_owner'::regrole) \
                  + (SELECT count(*) FROM pg_proc WHERE pronamespace = {ns} \
                       AND proowner <> 'ns_stat_owner'::regrole)"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(foreign, 0);
    }
}
