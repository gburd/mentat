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
        script_block(include_str!("../sql/pg_mentat--1.17.2--1.18.0.sql"), tag)
    }

    /// The 1.18.0 -> 1.19.0 upgrade's `DO $tag$ ... $tag$;` block.
    fn upgrade_119_block(tag: &str) -> String {
        script_block(include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql"), tag)
    }

    fn script_block(script: &str, tag: &str) -> String {
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
        assert!(sql.contains("mentat_bm_s.current_text"), "{sql}");
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

    /// The sorted rows of a FindRel q(store, ...), as compact JSON.
    fn rel(store: &str, query: &str, inputs: &str) -> Vec<String> {
        let mut v: Vec<String> = q(store, query, inputs)["results"]
            .as_array()
            .unwrap_or_else(|| panic!("{store}: {query}: not a relation"))
            .iter()
            .map(|r| r.to_string())
            .collect();
        v.sort();
        v
    }

    // Rule CTEs (recursive and not), not / not-join / or-join subqueries,
    // 5-place history patterns inside them, untyped comparisons and
    // lookup-ref :in values (also for a variable only a rule or a not binds)
    // read the named store, while the default store holds a different graph
    // under the same names.
    #[pg_test]
    fn rules_not_history_and_in_lookup_refs_on_a_named_store() {
        setup();
        Spi::run("SELECT create_store('rq_s')").expect("create");
        let id = |store: &str, edn: &str| -> i64 {
            tx(store, edn)["tempids"]["x"].as_i64().expect("tempid x")
        };
        // rq_s: a -> b -> c (friends); a's age 1 was replaced by 2. Made in
        // the order c, b, a, so a's entid is not the default store's a's.
        tx("rq_s", SCHEMA_TX);
        let c = id("rq_s", "[{:db/id \"x\" :p/name \"c\"}]");
        let b = id(
            "rq_s",
            &format!("[{{:db/id \"x\" :p/name \"b\" :p/friend {c}}}]"),
        );
        let a = id(
            "rq_s",
            &format!("[{{:db/id \"x\" :p/name \"a\" :p/age 1 :p/friend {b}}}]"),
        );
        tx("rq_s", &format!("[[:db/add {a} :p/age 2]]"));
        // default: a <-> c (friends); b's age 1 was replaced by 3; d.
        tx("default", SCHEMA_TX);
        let da = id("default", "[{:db/id \"x\" :p/name \"a\" :p/age 1}]");
        let db = id("default", "[{:db/id \"x\" :p/name \"b\" :p/age 1}]");
        let dc = id(
            "default",
            &format!("[{{:db/id \"x\" :p/name \"c\" :p/friend {da}}}]"),
        );
        tx("default", &format!("[[:db/add {da} :p/friend {dc}]]"));
        tx("default", "[{:db/id \"x\" :p/name \"d\"}]");
        tx("default", &format!("[[:db/add {db} :p/age 3]]"));
        assert_ne!(a, da);

        let lookup_a = r#"{"inputs": [[":p/name", "a"]]}"#;
        let reach = "[[(reach ?x ?y) [?x :p/friend ?y]] \
                      [(reach ?x ?y) [?x :p/friend ?z] (reach ?z ?y)]]";
        let fname = "[[(fname ?x ?n) [?x :p/friend ?y] [?y :p/name ?n]]]";
        let dropped = "[[(dropped ?e) [?e :p/age 1 _ ?op] [(= ?op false)]]]";
        let val = "[[(val ?e ?v) [?e :p/age ?v]] [(val ?e ?v) [?e :p/name ?v]]]";
        let fr = "[[(fr ?x ?y) [?x :p/friend ?y]]]";
        // (query, inputs, rows on rq_s, rows on default)
        let cases: Vec<(String, &str, Vec<&str>, Vec<&str>)> = vec![
            // A recursive rule.
            (
                format!("[:find ?n :rules {reach} :where [?x :p/name \"a\"] (reach ?x ?y) [?y :p/name ?n]]"),
                "{}",
                vec![r#"["b"]"#, r#"["c"]"#],
                vec![r#"["a"]"#, r#"["c"]"#],
            ),
            // A non-recursive rule with a string head value, invoked twice.
            (
                format!(
                    "[:find ?n1 ?n2 :rules {fname} :where [?a :p/name \"a\"] (fname ?a ?n1) \
                     [?b :p/name ?n1] (fname ?b ?n2)]"
                ),
                "{}",
                vec![r#"["b","c"]"#],
                vec![r#"["c","a"]"#],
            ),
            // A rule column of longs and strings.
            (
                format!("[:find ?v :rules {val} :where [?e :p/name \"a\"] (val ?e ?v)]"),
                "{}",
                vec!["[2]", r#"["a"]"#],
                vec!["[1]", r#"["a"]"#],
            ),
            // A 5-place history pattern in a rule body.
            (
                format!("[:find ?n :rules {dropped} :where [?e :p/name ?n] (dropped ?e)]"),
                "{}",
                vec![r#"["a"]"#],
                vec![r#"["b"]"#],
            ),
            // not-join.
            (
                "[:find ?n :where [?e :p/name ?n] (not-join [?e] [?e :p/friend ?f])]".into(),
                "{}",
                vec![r#"["c"]"#],
                vec![r#"["b"]"#, r#"["d"]"#],
            ),
            // A 5-place history pattern inside not and not-join.
            (
                "[:find ?n :where [?e :p/name ?n] (not [?e :p/age 1 _ ?op] [(= ?op false)])]".into(),
                "{}",
                vec![r#"["b"]"#, r#"["c"]"#],
                vec![r#"["a"]"#, r#"["c"]"#, r#"["d"]"#],
            ),
            (
                "[:find ?n :where [?e :p/name ?n] \
                 (not-join [?e] [?e :p/age 1 ?tx ?op] [(= ?op false)])]"
                    .into(),
                "{}",
                vec![r#"["b"]"#, r#"["c"]"#],
                vec![r#"["a"]"#, r#"["c"]"#, r#"["d"]"#],
            ),
            // ... and inside or-join.
            (
                "[:find ?n :where [?e :p/name ?n] \
                 (or-join [?e] (and [?e :p/age 1 _ ?op] [(= ?op false)]) [?e :p/friend ?f])]"
                    .into(),
                "{}",
                vec![r#"["a"]"#, r#"["b"]"#],
                vec![r#"["a"]"#, r#"["b"]"#, r#"["c"]"#],
            ),
            // An untyped comparison (the friend refs aren't numbers).
            (
                "[:find ?n :where [?e :p/name ?n] [?e ?attr ?v] [(> ?v 1)] [(< ?v 100)]]".into(),
                "{}",
                vec![r#"["a"]"#],
                vec![r#"["b"]"#],
            ),
            // A lookup-ref :in value, for a pattern variable ...
            (
                "[:find ?n :in ?x :where [?x :p/friend ?f] [?f :p/name ?n]]".into(),
                lookup_a,
                vec![r#"["b"]"#],
                vec![r#"["c"]"#],
            ),
            // ... a variable only a rule invocation binds ...
            (
                format!("[:find ?n :in ?x :rules {fr} :where (fr ?x ?y) [?y :p/name ?n]]"),
                lookup_a,
                vec![r#"["b"]"#],
                vec![r#"["c"]"#],
            ),
            // ... and a variable only a not binds.
            (
                "[:find ?n :in ?x :where [?e :p/name ?n] (not [?x :p/friend ?e])]".into(),
                lookup_a,
                vec![r#"["a"]"#, r#"["c"]"#],
                vec![r#"["a"]"#, r#"["b"]"#, r#"["d"]"#],
            ),
        ];
        let want = |rows: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = rows.iter().map(|r| r.to_string()).collect();
            v.sort();
            v
        };
        for (query, inputs, named, dflt) in &cases {
            assert_eq!(rel("rq_s", query, inputs), want(named), "rq_s: {query}");
            assert_eq!(
                rel("default", query, inputs),
                want(dflt),
                "default: {query}"
            );
        }
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

    // materialize / refresh / drop_matview validated the store name as
    // create_store does, which refuses "default": materialize('default', ...)
    // (the form docs/src/sql-functions.md shows) always failed with
    // ":db.error/invalid-store-name ... 'default' is a reserved name".
    #[pg_test]
    fn materialize_on_the_default_store() {
        setup();
        tx("default", SCHEMA_TX);
        tx("default", "[{:p/name \"one\"}]");
        Spi::run("SELECT materialize('default', 'mvd_names', '[:find ?n :where [_ :p/name ?n]]')")
            .expect("materialize('default', ...)");
        let rows = || {
            sorted(json(
                "SELECT coalesce(jsonb_agg(n), '[]') FROM mentat.mvd_names",
            ))
        };
        assert_eq!(rows(), vec!["one"]);
        tx("default", "[{:p/name \"two\"}]");
        assert_eq!(rows(), vec!["one"], "manual: refreshed only on request");
        Spi::run("SELECT refresh('default', 'mvd_names')").expect("refresh");
        assert_eq!(rows(), vec!["one", "two"]);
        let listed = json("SELECT list_matviews('default')");
        assert_eq!(listed.as_array().map(|a| a.len()), Some(1), "{listed}");
        Spi::run("SELECT drop_matview('default', 'mvd_names')").expect("drop_matview");
        let gone: i64 =
            Spi::get_one("SELECT count(*) FROM pg_matviews WHERE matviewname = 'mvd_names'")
                .unwrap()
                .unwrap();
        assert_eq!(gone, 0);
    }

    // On PostgreSQL 17 CREATE / REFRESH MATERIALIZED VIEW run the view's query
    // with search_path = pg_catalog, pg_temp, and the SQL edn_q generates
    // for a find-pull called pull(...) / edn_pull(...) unqualified: "function
    // pull(text, text, bigint) does not exist" (#382). Through materialize
    // and create_datalog_materialized_view, on the default store and on a
    // named store, created and refreshed. Also with the caller's own path
    // pinned, as a pinned trigger function runs it.
    #[pg_test]
    fn find_pull_in_a_materialized_view() {
        setup();
        Spi::run("SELECT create_store('mvp_s')").expect("create");
        let pull_q = "[:find (pull ?e [:p/name]) :where [?e :p/name _]]";
        for store in ["default", "mvp_s"] {
            tx(store, SCHEMA_TX);
            tx(store, &format!("[{{:p/name \"{store}-1\"}}]"));
            let schema = if store == "default" {
                "mentat"
            } else {
                "mentat_mvp_s"
            };
            Spi::run(&format!(
                "SELECT materialize('{store}', 'mvp_v', '{pull_q}')"
            ))
            .unwrap_or_else(|e| panic!("{store}: materialize: {e}"));
            let rows = || -> Vec<String> {
                let v = json(&format!(
                    "SELECT coalesce(jsonb_agg(p ORDER BY p::TEXT), '[]') FROM \
                     (SELECT (\"(pull ?e [ :p_name ])\")::jsonb->>':p/name' AS p FROM {schema}.mvp_v) s"
                ));
                sorted(v)
            };
            assert_eq!(rows(), vec![format!("{store}-1")], "{store}");
            tx(store, &format!("[{{:p/name \"{store}-2\"}}]"));
            Spi::run(&format!("SELECT refresh('{store}', 'mvp_v')"))
                .unwrap_or_else(|e| panic!("{store}: refresh: {e}"));
            assert_eq!(
                rows(),
                vec![format!("{store}-1"), format!("{store}-2")],
                "{store}"
            );
            Spi::run(&format!("SELECT drop_matview('{store}', 'mvp_v')")).expect("drop");
        }
        Spi::run(&format!(
            "SELECT mentat.create_datalog_materialized_view('mvp_d', '{pull_q}')"
        ))
        .expect("create_datalog_materialized_view");
        Spi::run("REFRESH MATERIALIZED VIEW mvp_d").expect("refresh mvp_d");
        Spi::run("SELECT mentat.drop_datalog_view('mvp_d', false, true)")
            .expect("drop_datalog_view");
        // The caller's path pinned to pg_catalog, as in a pinned trigger.
        Spi::run("SELECT pg_catalog.set_config('search_path', 'pg_catalog, pg_temp', true)")
            .expect("pin");
        let got = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT public.q('mvp_s', '{pull_q}', '{{}}'::jsonb)"
        ))
        .expect("q under a pinned path")
        .expect("NULL")
        .0;
        assert_eq!(got["result"].as_array().map(|a| a.len()), Some(2), "{got}");
        Spi::run("SELECT pg_catalog.set_config('search_path', '\"$user\", public', true)")
            .expect("unpin");
        Spi::run("SELECT drop_store('mvp_s')").expect("drop");
    }

    // The contrib functions edn_q generates calls to (fuzzystrmatch's
    // levenshtein / soundex / metaphone, pg_trgm's similarity) were named
    // bare, so they resolved only through the search path: in a view on
    // PostgreSQL 17 (CREATE / REFRESH MATERIALIZED VIEW pin it to
    // pg_catalog), in a pinned trigger, or with the extension in a schema
    // off the path, "function levenshtein(text, text) does not exist"
    // (#382). They are named through their extension's schema.
    #[pg_test]
    fn extension_functions_resolve_through_their_schema() {
        setup();
        let available: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_available_extensions \
              WHERE name IN ('fuzzystrmatch', 'pg_trgm')",
        )
        .unwrap()
        .unwrap();
        if available < 2 {
            return;
        }
        Spi::run(
            "DROP EXTENSION IF EXISTS fuzzystrmatch CASCADE; DROP EXTENSION IF EXISTS pg_trgm CASCADE; \
             CREATE SCHEMA ext_off_path; \
             CREATE EXTENSION fuzzystrmatch SCHEMA ext_off_path; \
             CREATE EXTENSION pg_trgm SCHEMA ext_off_path",
        )
        .expect("extensions off the path");
        Spi::run("SELECT create_store('xq_s')").expect("create");
        let lev = "[:find ?n ?d :where [?e :p/name ?n] [(levenshtein ?n \"Alice\") ?d] [(< ?d 2)]]";
        let codes = "[:find ?s ?m :where [?e :p/name \"Alicia\"] [(soundex \"Alicia\") ?s] \
                     [(metaphone \"Alicia\" 4) ?m]]";
        let sim = "[:find ?n :where [(similar-to $ :p/name \"Alice\" 0.3) [[?e ?n ?s]]]]";
        for store in ["default", "xq_s"] {
            tx(store, SCHEMA_TX);
            tx(
                store,
                "[{:p/name \"Alice\"} {:p/name \"Alicia\"} {:p/name \"Bob\"}]",
            );
            assert_eq!(
                q(store, lev, "{}")["result"],
                serde_json::json!([["Alice", 0]]),
                "{store}"
            );
            assert_eq!(
                q(store, codes, "{}")["result"],
                serde_json::json!([["A420", "ALX"]]),
                "{store}"
            );
            let near = sorted(
                q(store, &sim.replace(":find ?n", ":find [?n ...]"), "{}")["result"].clone(),
            );
            assert_eq!(near, vec!["Alice", "Alicia"], "{store}");
            // In a materialized view (PostgreSQL 17 pins its search path).
            let schema = if store == "default" {
                "mentat"
            } else {
                "mentat_xq_s"
            };
            for (view, query) in [("xq_lev", lev), ("xq_sim", sim)] {
                Spi::run(&format!(
                    "SELECT materialize('{store}', '{view}', '{query}')"
                ))
                .unwrap_or_else(|e| panic!("{store} {view}: {e}"));
                Spi::run(&format!("SELECT refresh('{store}', '{view}')"))
                    .unwrap_or_else(|e| panic!("{store} {view}: refresh: {e}"));
                let n: i64 = Spi::get_one(&format!("SELECT count(*) FROM {schema}.{view}"))
                    .unwrap()
                    .unwrap();
                assert_eq!(n, if view == "xq_lev" { 1 } else { 2 }, "{store} {view}");
                Spi::run(&format!("SELECT drop_matview('{store}', '{view}')")).expect("drop");
            }
        }
        Spi::run("SELECT drop_store('xq_s')").expect("drop");
        Spi::run("DROP EXTENSION fuzzystrmatch; DROP EXTENSION pg_trgm; DROP SCHEMA ext_off_path")
            .expect("cleanup");
    }

    /// extension_schema for a SQL caller (a dblink session below). It first
    /// does what this backend's next lock would: process pending invalidations.
    #[pg_extern]
    fn xs_extension_schema(name: &str) -> Option<String> {
        // SAFETY: what any lock acquisition does; no state of ours is held.
        unsafe { pg_sys::AcceptInvalidationMessages() };
        crate::functions::virtual_tables::extension_schema(name)
    }

    // extension_schema caches per backend; a pg_proc / pg_namespace change
    // clears the cache. The refill read pg_extension with the caller's
    // snapshot: when another session's ALTER EXTENSION ... SET SCHEMA
    // committed during a statement or REPEATABLE READ transaction that
    // started earlier, the old schema went back into the cache for the rest
    // of the backend's life (edn_q then named public.levenshtein: "does not
    // exist"). Session B reads, session A runs the DDL; both are dblink
    // connections, so it is committed and cleaned up at the end. `seg`: no
    // other test uses it.
    #[pg_test]
    fn extension_schema_sees_ddl_committed_during_the_transaction() {
        crate::ensure_extension_loaded();
        let available: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_available_extensions WHERE name IN ('dblink', 'seg')",
        )
        .unwrap()
        .unwrap();
        if available < 2 {
            return;
        }
        Spi::run("CREATE EXTENSION IF NOT EXISTS dblink").expect("dblink");
        let conn = text(
            "SELECT pg_catalog.format('dbname=%s port=%s host=%s', current_database(), \
               current_setting('port'), split_part(current_setting('unix_socket_directories'), ',', 1))",
        );
        Spi::run(&format!(
            "SELECT dblink_connect('xs_a', '{conn}'), dblink_connect('xs_b', '{conn}')"
        ))
        .expect("connect");
        let on = |c: &str, sql: &str| {
            Spi::run(&format!(
                "SELECT dblink_exec('{c}', '{}')",
                sql.replace('\'', "''")
            ))
            .unwrap_or_else(|e| panic!("{c}: {sql}: {e}"))
        };
        let in_b = |sql: &str| -> Option<String> {
            Spi::get_one(&format!(
                "SELECT s FROM dblink('xs_b', '{}') AS t(s text)",
                sql.replace('\'', "''")
            ))
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
        };
        let schema = || in_b("SELECT tests.xs_extension_schema('seg')");
        on("xs_a", "DROP EXTENSION IF EXISTS seg");
        on("xs_a", "DROP SCHEMA IF EXISTS xs_one, xs_two, \"xs Two\"");
        on("xs_a", "CREATE SCHEMA xs_one");
        on("xs_a", "CREATE SCHEMA xs_two");
        on("xs_a", "CREATE EXTENSION seg SCHEMA xs_one");
        on("xs_b", "BEGIN ISOLATION LEVEL REPEATABLE READ");
        let first = schema();
        on("xs_a", "ALTER EXTENSION seg SET SCHEMA xs_two");
        let in_tx = schema();
        let xid = in_b("SELECT txid_current_if_assigned()::text");
        on("xs_b", "COMMIT");
        let after = schema();
        on("xs_a", "ALTER SCHEMA xs_two RENAME TO \"xs Two\"");
        let renamed = schema();
        on("xs_a", "DROP EXTENSION seg");
        let dropped = schema();
        on("xs_a", "DROP SCHEMA xs_one, \"xs Two\"");
        Spi::run("SELECT dblink_disconnect('xs_a'), dblink_disconnect('xs_b')").expect("close");
        assert_eq!(first.as_deref(), Some("xs_one"));
        assert_eq!(
            in_tx.as_deref(),
            Some("xs_two"),
            "moved during the transaction"
        );
        assert_eq!(xid, None, "the read assigns no transaction id");
        assert_eq!(after.as_deref(), Some("xs_two"), "after the transaction");
        assert_eq!(renamed.as_deref(), Some("\"xs Two\""), "schema renamed");
        assert_eq!(dropped, None, "dropped");
    }

    // recursive / drop_recursive / list_recursive refused "default" as a
    // reserved store name, as materialize did (#406).
    #[pg_test]
    fn recursive_on_the_default_and_a_named_store() {
        setup();
        Spi::run("SELECT create_store('rq_a')").expect("create");
        for (store, schema) in [("default", "mentat"), ("rq_a", "mentat_rq_a")] {
            Spi::run(&format!(
                "SELECT recursive('{store}', 'rq_nums', 'r', 'SELECT 1 AS n', \
                 'SELECT n + 1 FROM r WHERE n < 3')"
            ))
            .unwrap_or_else(|e| panic!("recursive('{store}', ...): {e}"));
            let n: i64 = Spi::get_one(&format!("SELECT sum(n) FROM {schema}.rq_nums"))
                .unwrap()
                .unwrap();
            assert_eq!(n, 6, "{store}");
            let listed = json(&format!("SELECT list_recursive('{store}')"));
            assert_eq!(listed.as_array().map(|a| a.len()), Some(1), "{listed}");
            Spi::run(&format!("SELECT drop_recursive('{store}', 'rq_nums')"))
                .unwrap_or_else(|e| panic!("drop_recursive('{store}', ...): {e}"));
            let gone: i64 = Spi::get_one(&format!(
                "SELECT count(*) FROM pg_views WHERE schemaname = '{schema}' AND viewname = 'rq_nums'"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(gone, 0, "{store}");
        }
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

    // The trigger functions subscribe() and materialize(..., 'on_write')
    // generate run as the writer, and had no pinned search_path: a q(), md5()
    // or format() in a schema ahead of pg_mentat's (or of pg_catalog, when
    // the path names it) on the writer's path was called instead of
    // pg_mentat's / pg_catalog's (#383).
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn generated_triggers_pin_the_search_path() {
        setup();
        Spi::run("SELECT create_store('sp_s')").expect("create");
        tx("sp_s", SCHEMA_TX);
        tx("sp_s", "[{:p/name \"one\"}]");
        let query = "[:find [?n ...] :where [_ :p/name ?n]]";
        Spi::run(&format!("SELECT subscribe('sp_s', 'sp_names', '{query}')")).expect("subscribe");
        Spi::run(
            "SELECT materialize('sp_s', 'sp_mv', '[:find ?n :where [_ :p/name ?n]]', 'on_write')",
        )
        .expect("materialize");
        Spi::run(
            "CREATE TABLE public.sp_hits (f TEXT); CREATE SCHEMA sp_other; \
             CREATE FUNCTION sp_other.q(TEXT, TEXT, JSONB) RETURNS JSONB LANGUAGE sql AS \
               $$ INSERT INTO public.sp_hits VALUES ('q') RETURNING '{}'::jsonb $$; \
             CREATE FUNCTION sp_other.md5(TEXT) RETURNS TEXT LANGUAGE sql AS \
               $$ INSERT INTO public.sp_hits VALUES ('md5') RETURNING 'x' $$; \
             CREATE FUNCTION sp_other.format(TEXT, NAME, TEXT) RETURNS TEXT LANGUAGE sql AS \
               $$ INSERT INTO public.sp_hits VALUES ('format') RETURNING 'SELECT 1' $$",
        )
        .expect("functions in another schema");
        Spi::run("SET search_path TO sp_other, pg_catalog, mentat, public").expect("path");
        tx("sp_s", "[{:p/name \"two\"}]");
        Spi::run("SET search_path TO mentat, public").expect("path back");
        let hits = json("SELECT coalesce(jsonb_agg(f), '[]') FROM public.sp_hits");
        assert_eq!(hits, serde_json::json!([]), "called from the triggers");
        let sent = text("SELECT current_setting('mentat.sub_hash_sp_names', true)");
        let now = text(&format!(
            "SELECT md5(q('sp_s', '{query}', '{{}}'::jsonb)::text)"
        ));
        assert_eq!(sent, now);
        let rows = sorted(json(
            "SELECT coalesce(jsonb_agg(n), '[]') FROM mentat_sp_s.sp_mv",
        ));
        assert_eq!(rows, vec!["one", "two"]);
        for (f, store) in [
            ("mentat_sub_sp_names", "sp_s"),
            ("mentat_matview_refresh_sp_mv", "sp_s"),
        ] {
            let cfg = text(&format!(
                "SELECT array_to_string(proconfig, ',') FROM pg_proc \
                 WHERE proname = '{f}' AND pronamespace = 'mentat_{store}'::regnamespace"
            ));
            assert_eq!(cfg, "search_path=pg_catalog, pg_temp", "{f}");
        }
        Spi::run("SELECT drop_store('sp_s')").expect("drop");
    }

    // subscribe() put the query into a $$-quoted function body with only '
    // doubled: a query containing $$ ended the body early, so subscribe
    // failed with a syntax error (or ran the rest as SQL) (#383).
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscription_query_with_dollar_quotes() {
        setup();
        Spi::run("SELECT create_store('dq_s')").expect("create");
        tx("dq_s", SCHEMA_TX);
        let query =
            r#"[:find [?n ...] :where [_ :p/name ?n] [(!= ?n "it's $$ a \"$mentat_sub0$\" \\")]]"#;
        Spi::run(&format!(
            "SELECT subscribe('dq_s', 'dq_names', $q${query}$q$)"
        ))
        .expect("subscribe a query containing $$");
        tx("dq_s", "[{:p/name \"one\"}]");
        let sent = text("SELECT current_setting('mentat.sub_hash_dq_names', true)");
        let now = text(&format!(
            "SELECT md5(q('dq_s', $q${query}$q$, '{{}}'::jsonb)::text)"
        ));
        assert_eq!(sent, now);
        assert_eq!(
            text("SELECT query FROM mentat.subscriptions WHERE name = 'dq_names'"),
            query
        );
        Spi::run("SELECT drop_store('dq_s')").expect("drop");
    }

    // With the trigger functions' path pinned to pg_catalog, pg_temp, a
    // find-pull subscription and a find-pull on_write view still fire on t():
    // the generated pull() / edn_pull() call is qualified (#382, #383). The
    // view on the default store (edn_pull) and on a named store (pull); the
    // subscription on the named store (subscribe refuses 'default').
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn find_pull_triggers_fire_under_the_pinned_path() {
        // Before setup(), as in default_store_view_triggers_fire_after_the_data:
        // a default-store view's trigger goes on mentat.datoms.
        Spi::run("LOCK TABLE mentat.datoms IN SHARE ROW EXCLUSIVE MODE").expect("lock");
        setup();
        Spi::run("SELECT create_store('fpt_s')").expect("create");
        let pull_q = "[:find (pull ?e [:p/name]) :where [?e :p/name _]]";
        tx("fpt_s", SCHEMA_TX);
        tx("default", SCHEMA_TX);
        Spi::run(&format!(
            "SELECT subscribe('fpt_s', 'fpt_names', '{pull_q}')"
        ))
        .expect("subscribe");
        for (store, schema) in [("default", "mentat"), ("fpt_s", "mentat_fpt_s")] {
            Spi::run(&format!(
                "SELECT materialize('{store}', 'fpt_mv', '{pull_q}', 'on_write')"
            ))
            .unwrap_or_else(|e| panic!("{store}: materialize: {e}"));
            tx(store, &format!("[{{:p/name \"{store}-1\"}}]"));
            let rows = sorted(json(&format!(
                "SELECT coalesce(jsonb_agg(p), '[]') FROM \
                 (SELECT (\"(pull ?e [ :p_name ])\")::jsonb->>':p/name' AS p FROM {schema}.fpt_mv) s"
            )));
            assert_eq!(rows, vec![format!("{store}-1")], "{store}");
            Spi::run(&format!("SELECT drop_matview('{store}', 'fpt_mv')")).expect("drop_matview");
        }
        let sent = text("SELECT current_setting('mentat.sub_hash_fpt_names', true)");
        let now = text(&format!(
            "SELECT md5(q('fpt_s', '{pull_q}', '{{}}'::jsonb)::text)"
        ));
        assert_eq!(sent, now);
        assert_eq!(
            q("fpt_s", pull_q, "{}")["results"][0][0][":p/name"],
            serde_json::json!("fpt_s-1")
        );
        Spi::run("SELECT drop_store('fpt_s')").expect("drop");
    }

    // subscribe / unsubscribe validated the store name as create_store does,
    // which refuses "default": subscribe('default', ...) (the form
    // docs/src/sql-functions.md shows) always failed with
    // ":db.error/invalid-store-name ... 'default' is a reserved name". A
    // default-store subscription fires on edn_t, also a find-pull one under
    // the pinned path (#383).
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscribe_on_the_default_store() {
        // Before setup(): the trigger goes on mentat.datoms (see
        // default_store_view_triggers_fire_after_the_data).
        Spi::run("LOCK TABLE mentat.datoms IN SHARE ROW EXCLUSIVE MODE").expect("lock");
        setup();
        tx("default", SCHEMA_TX);
        let names_q = "[:find [?n ...] :where [_ :p/name ?n]]";
        let pull_q = "[:find (pull ?e [:p/name]) :where [?e :p/name _]]";
        for (sub, query) in [("sd_names", names_q), ("sd_pull", pull_q)] {
            Spi::run(&format!("SELECT subscribe('default', '{sub}', '{query}')"))
                .unwrap_or_else(|e| panic!("{sub}: subscribe('default', ...): {e}"));
        }
        Spi::run("SELECT edn_t('[{:p/name \"sd-1\"}]')").expect("edn_t");
        for (sub, query) in [("sd_names", names_q), ("sd_pull", pull_q)] {
            let sent = text(&format!(
                "SELECT current_setting('mentat.sub_hash_{sub}', true)"
            ));
            let now = text(&format!(
                "SELECT md5(edn_q('{query}', '{{}}'::jsonb)::text)"
            ));
            assert_eq!(sent, now, "{sub}");
        }
        let listed = json("SELECT list_subscriptions('default')");
        assert_eq!(listed.as_array().map(|a| a.len()), Some(2), "{listed}");
        for sub in ["sd_names", "sd_pull"] {
            Spi::run(&format!("SELECT unsubscribe('default', '{sub}')"))
                .unwrap_or_else(|e| panic!("{sub}: unsubscribe('default', ...): {e}"));
        }
        let left: i64 = Spi::get_one(
            "SELECT count(*) FROM pg_trigger WHERE tgname LIKE 'mentat\\_sub\\_trg\\_sd\\_%'",
        )
        .unwrap()
        .unwrap();
        assert_eq!(left, 0);
    }

    // 1.17's drop_store kept the store's subscription and materialized-view
    // rows. A leftover row blocked a later store of that name: rename_store
    // to it failed with a raw duplicate-key error, and subscribe() said
    // "already exists" (#380). The 1.19.0 upgrade deletes rows of stores that
    // no longer exist; rename_store explains what is in the way.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn upgrade_deletes_rows_of_dropped_stores() {
        setup();
        Spi::run(
            "CREATE TABLE public.or_ran (n INT); \
             CREATE FUNCTION public.or_trg() RETURNS trigger LANGUAGE plpgsql AS \
               $$ BEGIN INSERT INTO public.or_ran VALUES (1); RETURN NULL; END $$; \
             CREATE TRIGGER or_trg AFTER DELETE ON mentat.subscriptions \
               FOR EACH STATEMENT EXECUTE FUNCTION public.or_trg(); \
             INSERT INTO mentat.subscriptions (store_name, name, query) \
               VALUES ('or_gone', 'or_sub1', '[:find ?e :where [?e :db/ident _]]'); \
             INSERT INTO mentat.materialized_views (store_name, view_name, datalog_query) \
               VALUES ('or_gone', 'or_mv', '[:find ?e :where [?e :db/ident _]]')",
        )
        .expect("rows a 1.17 drop_store left");
        Spi::run("SELECT create_store('or_live')").expect("create");
        Spi::run(
            "SELECT subscribe('or_live', 'or_live_sub', '[:find ?e :where [?e :db/ident _]]')",
        )
        .expect("a live store's subscription");
        // Same view name as the leftover: rename_store's UPDATE hit
        // materialized_views_store_name_view_name_key.
        Spi::run("SELECT materialize('or_live', 'or_mv', '[:find ?e :where [?e :db/ident _]]')")
            .expect("a live store's view");
        Spi::run("SELECT materialize('default', 'or_dmv', '[:find ?e :where [?e :db/ident _]]')")
            .expect("a default-store view");

        // Before the upgrade: rename_store says what is in the way.
        let err = text("SELECT pg_temp.capture($$SELECT rename_store('or_live', 'or_gone')$$)");
        assert!(
            err.contains("subscriptions (or_sub1)")
                && err.contains("materialized views (or_mv)")
                && err.contains("left by a dropped store"),
            "{err:?}"
        );
        assert_eq!(
            text("SELECT schema_name FROM mentat.stores WHERE store_name = 'or_live'"),
            "mentat_or_live",
            "nothing renamed"
        );

        let block = upgrade_119_block("orphans");
        Spi::run(&block).expect("the upgrade DO block");
        Spi::run(&block).expect("again");
        let left = |table: &str, store: &str| -> i64 {
            Spi::get_one(&format!(
                "SELECT count(*) FROM mentat.{table} WHERE store_name = '{store}'"
            ))
            .unwrap()
            .unwrap()
        };
        assert_eq!(left("subscriptions", "or_gone"), 0);
        assert_eq!(left("materialized_views", "or_gone"), 0);
        assert_eq!(
            left("subscriptions", "or_live"),
            1,
            "a live store's row stays"
        );
        assert_eq!(
            left("materialized_views", "default"),
            1,
            "the default store's row stays"
        );
        let ran: i64 = Spi::get_one("SELECT count(*) FROM public.or_ran")
            .unwrap()
            .unwrap();
        assert_eq!(
            ran, 0,
            "a trigger on mentat.subscriptions ran during the upgrade"
        );
        Spi::run("DROP TRIGGER or_trg ON mentat.subscriptions").expect("drop trigger");
        assert_eq!(
            text("SELECT current_setting('session_replication_role')"),
            "origin"
        );

        // A new store of that name subscribes and materializes.
        Spi::run("SELECT create_store('or_gone')").expect("create or_gone");
        tx("or_gone", SCHEMA_TX);
        Spi::run("SELECT subscribe('or_gone', 'or_sub1', '[:find ?e :where [?e :db/ident _]]')")
            .expect("subscribe under the freed name");
        Spi::run("SELECT materialize('or_gone', 'or_mv', '[:find ?e :where [?e :db/ident _]]')")
            .expect("materialize under the freed name");
        Spi::run("SELECT drop_store('or_gone')").expect("drop");
        Spi::run("SELECT rename_store('or_live', 'or_gone')").expect("rename to the freed name");
        assert_eq!(
            left("subscriptions", "or_gone"),
            1,
            "the row followed the rename"
        );
        Spi::run("SELECT drop_store('or_gone')").expect("drop");
        Spi::run("SELECT drop_matview('default', 'or_dmv')").expect("drop_matview");
    }

    // 1.17's create_store('x') made idx_x_fts_text (and with pg_trgm
    // idx_x_trgm_text / idx_x_trgm_keyword) on the default store's
    // mentat.datoms_text_new / datoms_keyword_new; the 1.18.0 upgrade kept
    // them, so every default-store write kept paying for them (#381). The
    // 1.19.0 upgrade drops them, and only them.
    //
    // The planted indexes go on stand-in tables (the block's two table names
    // swapped): CREATE / DROP INDEX on the shared tables would lock them
    // against every other test. The unchanged block then runs on the real
    // tables, which hold no such index, and must leave them as they are.
    #[pg_test]
    fn upgrade_drops_old_store_indexes_on_the_default_tables() {
        setup();
        let indexes = |tables: &[&str]| -> Vec<String> {
            let list = tables
                .iter()
                .map(|t| format!("'mentat.{t}'::regclass"))
                .collect::<Vec<_>>()
                .join(", ");
            sorted(json(&format!(
                "SELECT coalesce(jsonb_agg(c.relname::text), '[]') \
                   FROM pg_index i JOIN pg_class c ON c.oid = i.indexrelid \
                  WHERE i.indrelid IN ({list})"
            )))
        };
        let real = ["datoms_text_new", "datoms_keyword_new", "datoms_long_new"];
        let own = indexes(&real);
        assert!(
            own.contains(&"idx_datoms_text_new_fts".to_string()),
            "{own:?}"
        );
        let long = "Ix_".to_string() + &"l".repeat(53);
        Spi::run(&format!(
            "INSERT INTO mentat.stores (store_name, schema_name) VALUES \
               ('Ix_S', 'mentat_Ix_S'), ('{long}', 'mentat_{long}'); \
             CREATE SCHEMA mentat_ix_old_pre118; \
             CREATE TABLE mentat.ix_text (v TEXT, added BOOLEAN); \
             CREATE TABLE mentat.ix_keyword (v TEXT, added BOOLEAN); \
             CREATE TABLE mentat.ix_long (v BIGINT); \
             CREATE INDEX ix_text_own ON mentat.ix_text (v); \
             CREATE INDEX idx_ix_s_fts_text ON mentat.ix_text \
               USING GIN (to_tsvector('english', COALESCE(v, ''))) WHERE added = true; \
             CREATE INDEX idx_ix_s_trgm_text ON mentat.ix_text (v) WHERE added = true; \
             CREATE INDEX idx_ix_old_trgm_keyword ON mentat.ix_keyword (v) WHERE added = true; \
             CREATE INDEX idx_{long}_trgm_keyword ON mentat.ix_keyword (v) WHERE added = true; \
             CREATE INDEX idx_ix_s_trgm_keyword ON mentat.ix_long (v); \
             CREATE INDEX idx_ix_gone_fts_text ON mentat.ix_text (v) WHERE added = true"
        ))
        .expect("1.17-style indexes");
        let real_block = upgrade_119_block("oldidx");
        let mut block = real_block.clone();
        for (from, to) in [
            ("datoms_text_new", "ix_text"),
            ("datoms_keyword_new", "ix_keyword"),
        ] {
            let from = format!("'mentat.{from}'::regclass");
            assert_eq!(block.matches(&from).count(), 2, "{from}");
            block = block.replace(&from, &format!("'mentat.{to}'::regclass"));
        }
        let warned = text(&format!(
            "SELECT pg_temp.capture($cap${}$cap$)",
            block.replace(
                "RAISE WARNING 'pg_mentat upgrade: indexes",
                "RAISE EXCEPTION 'pg_mentat upgrade: indexes"
            )
        ));
        assert!(
            warned.contains("indexes mentat.idx_ix_gone_fts_text on") && !warned.contains("ix_s"),
            "{warned:?}"
        );
        Spi::run(&block).expect("the upgrade DO block");
        Spi::run(&block).expect("again");
        // An index on another table, or with a name no store gives, stays.
        assert_eq!(
            indexes(&["ix_text", "ix_keyword", "ix_long"]),
            vec![
                "idx_ix_gone_fts_text",
                "idx_ix_s_trgm_keyword",
                "ix_text_own"
            ]
        );
        Spi::run(&real_block).expect("the upgrade DO block, as shipped");
        assert_eq!(indexes(&real), own, "the default store's own indexes");
        Spi::run(
            "DROP TABLE mentat.ix_text, mentat.ix_keyword, mentat.ix_long; \
             DROP SCHEMA mentat_ix_old_pre118; \
             DELETE FROM mentat.stores WHERE store_name LIKE 'Ix\\_%'",
        )
        .expect("clean up");
    }

    // Anyone who can connect can LISTEN, so a subscription's payload (its
    // query result) reached roles with no access to the store (#379):
    // subscribe(..., token_only => true) sends {"tx", "hash"} instead. And a
    // result over the NOTIFY limit made pg_notify raise in the trigger, so
    // every t() on the store failed (#400): it is sent as a token with
    // "too_large". Real sessions (dblink): a NOTIFY is delivered on commit.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn subscription_token_payloads() {
        crate::ensure_extension_loaded();
        let available: i64 =
            Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'dblink'")
                .unwrap()
                .unwrap();
        if available == 0 {
            return;
        }
        Spi::run("CREATE EXTENSION IF NOT EXISTS dblink").expect("dblink");
        let conn = text(
            "SELECT pg_catalog.format('dbname=%s port=%s host=%s', current_database(), \
               current_setting('port'), split_part(current_setting('unix_socket_directories'), ',', 1))",
        );
        Spi::run(&format!(
            "SELECT dblink_connect('tk_w', '{conn}'), dblink_connect('tk_l', '{conn}'), \
                    dblink_connect('tk_w2', '{conn}')"
        ))
        .expect("connect");
        let on = |c: &str, sql: &str| {
            Spi::run(&format!(
                "SELECT dblink_exec('{c}', '{}')",
                sql.replace('\'', "''")
            ))
            .unwrap_or_else(|e| panic!("{c}: {sql}: {e}"))
        };
        let write_on = |c: &str, edn: &str| -> serde_json::Value {
            let sql = format!("SELECT t('tk_s', '{}')", edn.replace('\'', "''"));
            let out = text(&format!(
                "SELECT r FROM dblink('{c}', '{}') AS x(r TEXT)",
                sql.replace('\'', "''")
            ));
            serde_json::from_str(&out).expect("tx report")
        };
        let write = |edn: &str| write_on("tk_w", edn);
        // (channel, payload) received by the listener, waiting up to 10 s
        // for `n` of them.
        let received = |n: usize| -> Vec<(String, serde_json::Value)> {
            let mut got = Vec::new();
            for _ in 0..200 {
                let v = json(
                    "SELECT coalesce(jsonb_agg(jsonb_build_array(notify_name, extra)), '[]') \
                       FROM dblink_get_notify('tk_l')",
                );
                for p in v.as_array().unwrap() {
                    got.push((
                        p[0].as_str().unwrap().to_string(),
                        serde_json::from_str(p[1].as_str().unwrap()).expect("JSON payload"),
                    ));
                }
                if got.len() >= n {
                    break;
                }
                Spi::run("SELECT pg_sleep(0.05)").unwrap();
            }
            got.sort_by(|a, b| a.0.cmp(&b.0));
            got
        };
        on("tk_w", "SET search_path TO mentat, public");
        on(
            "tk_w",
            "DO $d$ BEGIN PERFORM drop_store('tk_s'); EXCEPTION WHEN OTHERS THEN NULL; END $d$",
        );
        on("tk_w", "DO $d$ BEGIN PERFORM create_store('tk_s'); END $d$");
        write(SCHEMA_TX);
        let query = "[:find [?n ...] :where [_ :p/name ?n]]";
        on(
            "tk_w",
            &format!("DO $d$ BEGIN PERFORM subscribe('tk_s', 'tk_res', '{query}'); END $d$"),
        );
        on(
            "tk_w",
            &format!(
                "DO $d$ BEGIN PERFORM subscribe('tk_s', 'tk_tok', '{query}', token_only => true); END $d$"
            ),
        );
        on("tk_l", "LISTEN mentat_tk_res");
        on("tk_l", "LISTEN mentat_tk_tok");
        let hash_on = |c: &str| {
            text(&format!(
                "SELECT h FROM dblink('{c}', $q$SELECT md5(q('tk_s', '{query}', '{{}}'::jsonb)::text)$q$) \
                   AS x(h TEXT)"
            ))
        };
        let hash = || hash_on("tk_w");

        let tx1 = write("[{:p/name \"one\"}]")["db-after"]["basis-t"].clone();
        let got = received(2);
        assert_eq!(
            got,
            vec![
                (
                    "mentat_tk_res".into(),
                    serde_json::json!({"result": ["one"]})
                ),
                (
                    "mentat_tk_tok".into(),
                    serde_json::json!({"tx": tx1, "hash": hash()})
                ),
            ],
            "the default payload is the result; token_only sends tx and hash"
        );

        // A result over the limit: the write succeeds, both send a token.
        let tx2 = write(&format!("[{{:p/name \"{}\"}}]", "x".repeat(9000)))["db-after"]["basis-t"]
            .clone();
        assert!(tx2.as_i64().is_some(), "the write must not fail");
        let h = hash();
        assert_eq!(
            received(2),
            vec![
                (
                    "mentat_tk_res".into(),
                    serde_json::json!({"tx": tx2, "hash": h, "too_large": true})
                ),
                (
                    "mentat_tk_tok".into(),
                    serde_json::json!({"tx": tx2, "hash": h})
                ),
            ]
        );

        // "tx" came from currval of the store's tx sequence, which is the
        // session's last t(), not this transaction's: another session writes
        // (tx3), then tk_w changes nothing through the view, and its trigger
        // sees a new result and sent tk_w's old tx2 with tx3's hash.
        let tok = |tx: serde_json::Value, h: String| {
            vec![
                (
                    "mentat_tk_res".to_string(),
                    serde_json::json!({"tx": tx, "hash": h, "too_large": true}),
                ),
                (
                    "mentat_tk_tok".to_string(),
                    serde_json::json!({"tx": tx, "hash": h}),
                ),
            ]
        };
        on("tk_w2", "SET search_path TO mentat, public");
        let tx3 = write_on("tk_w2", "[{:p/name \"three\"}]")["db-after"]["basis-t"].clone();
        assert_eq!(received(2), tok(tx3, hash()));
        on("tk_w", "DELETE FROM mentat_tk_s.datoms WHERE false");
        assert_eq!(
            received(2),
            tok(serde_json::Value::Null, hash()),
            "no t() in this transaction: tx is null, not the session's last"
        );
        // A raw statement in the same transaction as a t() reports that t().
        on("tk_w", "BEGIN");
        let tx4 = write("[{:p/name \"four\"}]")["db-after"]["basis-t"].clone();
        let h4 = hash();
        let tx5 = write_on("tk_w2", "[{:p/name \"five\"}]")["db-after"]["basis-t"].clone();
        assert_eq!(received(2), tok(tx5, hash_on("tk_w2")), "tk_w2's own write");
        on("tk_w", "DELETE FROM mentat_tk_s.datoms WHERE false");
        let h5 = hash();
        on("tk_w", "COMMIT");
        let mut both = tok(tx4.clone(), h4);
        both.extend(tok(tx4, h5));
        both.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(received(4), both);
        on("tk_w", "DO $d$ BEGIN PERFORM drop_store('tk_s'); END $d$");
        Spi::run(
            "SELECT dblink_disconnect('tk_w'), dblink_disconnect('tk_l'), \
                    dblink_disconnect('tk_w2')",
        )
        .expect("disconnect");
    }

    // edn_t and t() record their transaction's tx, by schema, for the
    // subscription trigger's token (mentat.current_tx, transaction-local).
    #[pg_test]
    fn transactor_records_the_current_tx() {
        setup();
        tx("default", SCHEMA_TX);
        let r: serde_json::Value =
            serde_json::from_str(&text("SELECT edn_t('[{:p/name \"ct-1\"}]')")).unwrap();
        assert_eq!(
            text("SELECT current_setting('mentat.current_tx', true)"),
            format!("mentat:{}", r["db-after"]["basis-t"])
        );
        Spi::run("SELECT create_store('ct_s')").expect("create");
        let r = tx("ct_s", SCHEMA_TX);
        assert_eq!(
            text("SELECT current_setting('mentat.current_tx', true)"),
            format!("mentat_ct_s:{}", r["db-after"]["basis-t"])
        );
        Spi::run("SELECT drop_store('ct_s')").expect("drop");
    }

    // mentat.refresh_datalog_view could not be called on any version: its
    // parameter `concurrently` is a keyword in a PL/pgSQL IF, so the body
    // failed with "syntax error at end of input" (#398). Without and with
    // concurrently (a unique index on the view), and through the 1.19.0
    // upgrade's definition.
    #[pg_test]
    fn refresh_datalog_view_with_and_without_concurrently() {
        setup();
        tx("default", SCHEMA_TX);
        tx("default", "[{:p/name \"rv-1\"}]");
        let rows =
            || -> Vec<String> { sorted(json("SELECT coalesce(jsonb_agg(n), '[]') FROM rv_names")) };
        let script = include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql");
        let upgrade = &script[script
            .find("DROP FUNCTION IF EXISTS mentat.refresh_datalog_view")
            .expect("upgrade section")..];
        let upgrade = &upgrade[..upgrade.find("$$ LANGUAGE plpgsql;").unwrap() + 20];
        for (round, install) in [("fresh install", None), ("upgrade", Some(upgrade))] {
            if let Some(sql) = install {
                // Outside the extension script a member can't be dropped.
                Spi::run(
                    "ALTER EXTENSION pg_mentat DROP FUNCTION mentat.refresh_datalog_view(TEXT, BOOLEAN)",
                )
                .expect("detach");
                Spi::run(sql).unwrap_or_else(|e| panic!("{round}: {e}"));
            }
            Spi::run(
                "SELECT mentat.create_datalog_materialized_view('rv_names', \
                   '[:find ?n :where [_ :p/name ?n]]')",
            )
            .expect("create_datalog_materialized_view");
            assert_eq!(rows(), vec!["rv-1"], "{round}");
            tx("default", &format!("[{{:p/name \"rv-{round}\"}}]"));
            Spi::run("SELECT mentat.refresh_datalog_view('rv_names')")
                .unwrap_or_else(|e| panic!("{round}: refresh: {e}"));
            assert_eq!(
                rows(),
                vec!["rv-1".to_string(), format!("rv-{round}")],
                "{round}"
            );
            Spi::run("CREATE UNIQUE INDEX ON rv_names (n)").expect("unique index");
            tx("default", &format!("[{{:p/name \"rv-{round}-c\"}}]"));
            Spi::run("SELECT mentat.refresh_datalog_view('rv_names', true)")
                .unwrap_or_else(|e| panic!("{round}: refresh concurrently: {e}"));
            Spi::run("SELECT mentat.refresh_datalog_view('rv_names', p_concurrently => false)")
                .unwrap_or_else(|e| panic!("{round}: named argument: {e}"));
            assert_eq!(rows().len(), 3, "{round}");
            Spi::run("SELECT mentat.drop_datalog_view('rv_names', false, true)").expect("drop");
            tx(
                "default",
                &format!(
                    "[[:db/retract [:p/name \"rv-{round}\"] :p/name \"rv-{round}\"] \
                          [:db/retract [:p/name \"rv-{round}-c\"] :p/name \"rv-{round}-c\"]]"
                ),
            );
        }
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

    // Trigger functions made before 1.19.0 have no pinned search_path (#383),
    // and the upgrade doesn't regenerate them: it names them in a WARNING.
    #[cfg(not(feature = "pg13"))]
    #[pg_test]
    fn upgrade_names_unpinned_trigger_functions() {
        setup();
        Spi::run("SELECT create_store('up_pin')").expect("create");
        tx("up_pin", SCHEMA_TX);
        let query = "[:find ?n :where [_ :p/name ?n]]";
        Spi::run(&format!(
            "SELECT subscribe('up_pin', 'up_pin_old', '{query}')"
        ))
        .expect("subscribe");
        Spi::run(&format!(
            "SELECT subscribe('up_pin', 'up_pin_new', '{query}')"
        ))
        .expect("subscribe");
        Spi::run(&format!(
            "SELECT materialize('up_pin', 'up_pin_mv', '{query}', 'on_write')"
        ))
        .expect("materialize");
        // What a 1.18.0 subscribe / materialize made: no SET search_path.
        Spi::run(
            "ALTER FUNCTION mentat_up_pin.mentat_sub_up_pin_old() RESET search_path; \
             ALTER FUNCTION mentat_up_pin.mentat_matview_refresh_up_pin_mv() RESET search_path",
        )
        .expect("unpin");
        let block = upgrade_119_block("unpinned").replace("RAISE WARNING", "RAISE EXCEPTION");
        let msg = text(&format!("SELECT pg_temp.capture($cap${block}$cap$)"));
        assert!(
            msg.contains(
                "trigger functions mentat_up_pin.mentat_matview_refresh_up_pin_mv(), \
                 mentat_up_pin.mentat_sub_up_pin_old() were made before 1.19.0"
            ),
            "{msg:?}"
        );
        assert!(!msg.contains("up_pin_new"), "{msg:?}");
        Spi::run("SELECT drop_store('up_pin')").expect("drop");
        let none = text(&format!("SELECT pg_temp.capture($cap${block}$cap$)"));
        assert!(!none.contains("up_pin"), "{none:?}");
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
            "current_keyword:idx_current_keyword_trgm".to_string(),
            "current_text:idx_current_text_trgm".to_string(),
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
            "DROP INDEX mentat_tg_rn3.idx_current_text_trgm; \
             CREATE INDEX mine ON mentat_tg_rn3.current_text USING GIN (v ns_trgm7.gin_trgm_ops)",
        )
        .expect("own index");
        Spi::run("SELECT rename_store('tg_rn3', 'tg_rn4')").expect("rename");
        assert_eq!(
            per_table("mentat_tg_rn4"),
            vec![
                "current_keyword:idx_current_keyword_trgm".to_string(),
                "current_text:mine".to_string()
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

    /// A store's views and helper functions, each with the rows it returns
    /// as sorted text, after a card-one replace and a retraction (#395).
    /// Values are distinct per object so a stale row can't hide.
    fn store_objects_read_live_values(store: &str) {
        let schema = crate::functions::store_management::quote_ident(&format!("mentat_{store}"));
        tx(store, SCHEMA_TX);
        tx(
            store,
            "[{:db/ident :p/tag :db/valueType :db.type/string :db/cardinality :db.cardinality/many}]",
        );
        let r = tx(
            store,
            "[{:db/id \"a\" :p/name \"ann\" :p/age 1 :p/tag [\"fox jumps\" \"t2\"]} \
              {:db/id \"b\" :p/name \"bob\" :p/age 7 :p/friend \"a\"} {:db/id \"c\" :p/name \"cy\"}]",
        );
        let (a, b, c) = (
            r["tempids"]["a"].as_i64().unwrap(),
            r["tempids"]["b"].as_i64().unwrap(),
            r["tempids"]["c"].as_i64().unwrap(),
        );
        let tx1 = r["db-after"]["basis-t"].as_i64().unwrap();
        tx(
            store,
            &format!(
                "[[:db/add {a} :p/name \"anna\"] [:db/add {a} :p/age 2] \
                  [:db/retract {a} :p/tag \"fox jumps\"] [:db/add {a} :p/tag \"lazy dog\"] \
                  [:db/retract {b} :p/age 7] [:db/retract {b} :p/friend {a}] [:db/add {b} :p/friend {c}]]"
            ),
        );
        // c's only value replaced: c was still created at tx1.
        let tx3 = tx(store, &format!("[[:db/add {c} :p/name \"cyd\"]]"))["db-after"]["basis-t"]
            .as_i64()
            .unwrap();
        let first_last = text(&format!(
            "SELECT e.first_tx || ' ' || e.last_tx || ' ' || (e.first_ts = t.tx_instant) \
               FROM {schema}.entities e JOIN {schema}.transactions t ON t.tx = {tx1} \
              WHERE e.entity_id = {c}"
        ));
        assert_eq!(
            first_last,
            format!("{tx1} {tx3} true"),
            "entities: first_tx of {c}"
        );
        let rows = |sql: &str| -> Vec<String> {
            let mut out: Vec<String> = Spi::connect(|client| {
                client
                    .select(sql, None, &[])
                    .unwrap_or_else(|e| panic!("{sql}: {e}"))
                    .map(|row| row.get::<String>(1).unwrap().unwrap_or_default())
                    .collect()
            });
            out.sort();
            out
        };
        let mine = format!("{a}, {b}, {c}");
        let in_mine = |col: &str| format!("{col} IN ({mine})");
        let s = |v: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = v.iter().map(|x| x.to_string()).collect();
            v.sort();
            v
        };
        assert_eq!(
            rows(&format!(
                "SELECT entity_id || ' ' || attribute || ' ' || value FROM {schema}.facts \
                  WHERE {} AND attribute LIKE ':p/%'",
                in_mine("entity_id")
            )),
            s(&[
                &format!("{a} :p/name anna"),
                &format!("{a} :p/age 2"),
                &format!("{a} :p/tag t2"),
                &format!("{a} :p/tag lazy dog"),
                &format!("{b} :p/name bob"),
                &format!("{b} :p/friend {c}"),
                &format!("{c} :p/name cyd"),
            ]),
            "facts"
        );
        assert_eq!(
            rows(&format!(
                "SELECT entity_id || ' ' || value FROM {schema}.text_values \
                  WHERE attribute = ':p/name'"
            )),
            s(&[
                &format!("{a} anna"),
                &format!("{b} bob"),
                &format!("{c} cyd")
            ]),
            "text_values"
        );
        assert_eq!(
            rows(&format!(
                "SELECT entity_id || ' ' || value FROM {schema}.numeric_values \
                  WHERE attribute = ':p/age'"
            )),
            s(&[&format!("{a} 2")]),
            "numeric_values"
        );
        for view in ["entity_references", "graph_edges"] {
            let (src, dst) = if view == "graph_edges" {
                ("source", "target")
            } else {
                ("source_entity", "target_entity")
            };
            assert_eq!(
                rows(&format!(
                    "SELECT {src} || ' ' || {dst} FROM {schema}.{view} WHERE {}",
                    in_mine(src)
                )),
                s(&[&format!("{b} {c}")]),
                "{view}"
            );
        }
        assert_eq!(
            rows(&format!(
                "SELECT source_entity || ' ' || target_entity FROM {schema}.reverse_references \
                  WHERE {}",
                in_mine("target_entity")
            )),
            s(&[&format!("{b} {c}")]),
            "reverse_references"
        );
        assert_eq!(
            rows(&format!(
                "SELECT value FROM {schema}.searchable_text \
                  WHERE search_vector @@ plainto_tsquery('english', 'fox') \
                     OR search_vector @@ plainto_tsquery('english', 'dog')"
            )),
            s(&["lazy dog"]),
            "searchable_text"
        );
        assert_eq!(
            rows(&format!("SELECT value FROM {schema}.find_text('fox')")),
            s(&[]),
            "find_text"
        );
        assert_eq!(
            rows(&format!("SELECT value FROM {schema}.find_text('dog')")),
            s(&["lazy dog"]),
            "find_text"
        );
        assert_eq!(
            rows(&format!(
                "SELECT entity_id::TEXT FROM {schema}.entities_with_attribute(':p/age')"
            )),
            s(&[&a.to_string()]),
            "entities_with_attribute"
        );
        for (attr, value, want) in [
            (":p/name", "ann".to_string(), vec![]),
            (":p/name", "anna".to_string(), vec![a.to_string()]),
            (":p/age", "7".to_string(), vec![]),
            (":p/age", "2".to_string(), vec![a.to_string()]),
            (":p/friend", a.to_string(), vec![]),
            (":p/friend", c.to_string(), vec![b.to_string()]),
        ] {
            assert_eq!(
                rows(&format!(
                    "SELECT entity_id::TEXT FROM {schema}.lookup_entity('{attr}', '{value}')"
                )),
                want,
                "lookup_entity {attr} {value}"
            );
        }
        for (e, attr, want) in [
            (a, ":p/name", Some("anna".to_string())),
            (a, ":p/age", Some("2".to_string())),
            (b, ":p/age", None),
            (b, ":p/friend", Some(c.to_string())),
        ] {
            assert_eq!(
                Spi::get_one::<String>(&format!("SELECT {schema}.entity_value({e}, '{attr}')"))
                    .unwrap(),
                want,
                "entity_value {e} {attr}"
            );
        }
        assert_eq!(
            rows(&format!(
                "SELECT entity_id || ' ' || attribute_count FROM {schema}.entities WHERE {}",
                in_mine("entity_id")
            )),
            s(&[&format!("{a} 3"), &format!("{b} 2"), &format!("{c} 1")]),
            "entities"
        );
        assert_eq!(
            rows(&format!(
                "SELECT ident || ' ' || entity_count || ' ' || datom_count FROM {schema}.schema_summary \
                  WHERE ident IN (':p/age', ':p/tag', ':p/friend')"
            )),
            s(&[":p/age 1 1", ":p/friend 1 1", ":p/tag 1 2"]),
            "schema_summary"
        );
        assert_eq!(
            rows(&format!(
                "SELECT attribute || ' ' || entity_count || ' ' || datom_count \
                   FROM {schema}.count_by_attribute() WHERE attribute IN (':p/age', ':p/tag')"
            )),
            s(&[":p/age 1 1", ":p/tag 1 2"]),
            "count_by_attribute"
        );
        // History views keep what was replaced.
        assert_eq!(
            rows(&format!(
                "SELECT value FROM {schema}.recent_changes WHERE attribute = ':p/name' AND {}",
                in_mine("entity_id")
            )),
            s(&["ann", "anna", "bob", "cy", "cyd"]),
            "recent_changes"
        );
    }

    // Since 1.5.0 the log is append-only: a replaced or retracted value
    // keeps its added = true row. A named store's views and helper functions
    // read the log with only `added = true`, so they returned values
    // entities no longer have (#395).
    #[pg_test]
    fn store_views_read_current_state() {
        setup();
        Spi::run("SELECT create_store('cur_views')").expect("create_store");
        store_objects_read_live_values("cur_views");
        Spi::run("SELECT create_store('Cur_Mixed')").expect("create_store");
        store_objects_read_live_values("Cur_Mixed");
    }

    /// A store's views and functions, but for the layout's datoms /
    /// current_datoms: (kind, name, definition with the schema and store
    /// name written as STORE, owner, member of an extension, grants).
    fn store_object_defs(store: &str) -> Vec<(String, String, String, String, bool, String)> {
        let schema = format!("mentat_{store}");
        let sql = format!(
            "SELECT 'view', c.relname::TEXT, pg_get_viewdef(c.oid), c.relowner::regrole::TEXT, \
                    EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_class'::regclass \
                              AND d.objid = c.oid AND d.deptype = 'e'), \
                    coalesce(c.relacl::TEXT, '') \
               FROM pg_class c \
              WHERE c.relnamespace = (SELECT oid FROM pg_namespace WHERE nspname = '{schema}') \
                AND c.relkind = 'v' AND c.relname NOT IN ('datoms', 'current_datoms') \
             UNION ALL \
             SELECT 'function', p.proname || '(' || pg_get_function_identity_arguments(p.oid) || ')', \
                    p.prosrc, p.proowner::regrole::TEXT, \
                    EXISTS (SELECT 1 FROM pg_depend d WHERE d.classid = 'pg_proc'::regclass \
                              AND d.objid = p.oid AND d.deptype = 'e'), \
                    coalesce(p.proacl::TEXT, '') \
               FROM pg_proc p \
              WHERE p.pronamespace = (SELECT oid FROM pg_namespace WHERE nspname = '{schema}') \
              ORDER BY 1, 2"
        );
        Spi::connect(|client| {
            client
                .select(&sql, None, &[])
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                .map(|row| {
                    let t = |i| row.get::<String>(i).unwrap().unwrap_or_default();
                    (
                        t(1),
                        t(2),
                        t(3).replace(&schema, "STORE")
                            .replace(&format!("'{store}'"), "'STORE'"),
                        t(4),
                        row.get::<bool>(5).unwrap().unwrap(),
                        t(6),
                    )
                })
                .collect()
        })
    }

    /// Give store `store` the views and functions 1.18.0's create_store made
    /// (src/tests/store_views_1_18_0.sql, taken from the released build) and,
    /// with pg_trgm (in schema ns_vt), its trigram indexes on the log.
    fn make_1_18_store(store: &str, trgm: bool) {
        Spi::run(&format!("SELECT create_store('{store}')")).expect("create_store");
        let old = include_str!("tests/store_views_1_18_0.sql")
            .replace("mentat_v118", &format!("mentat_{store}"))
            .replace("'v118'", &format!("'{store}'"));
        Spi::run(&old).expect("1.18.0 definitions");
        Spi::run(&format!(
            "DROP INDEX IF EXISTS mentat_{store}.idx_current_text_trgm; \
             DROP INDEX IF EXISTS mentat_{store}.idx_current_keyword_trgm; \
             DROP INDEX IF EXISTS mentat_{store}.idx_current_text_fts"
        ))
        .expect("drop 1.19 indexes");
        if trgm {
            Spi::run(&format!(
                "CREATE INDEX idx_datoms_text_new_trgm ON mentat_{store}.datoms_text_new \
                   USING GIN (v ns_vt.gin_trgm_ops) WHERE added = true; \
                 CREATE INDEX idx_datoms_keyword_new_trgm ON mentat_{store}.datoms_keyword_new \
                   USING GIN (v ns_vt.gin_trgm_ops) WHERE added = true"
            ))
            .expect("1.18.0 trigram indexes");
        }
    }

    /// The trigram indexes in a store's schema, as table.index.
    fn trgm_indexes(store: &str) -> Vec<String> {
        let sql = format!(
            "SELECT t.relname || '.' || c.relname FROM pg_index i \
               JOIN pg_class c ON c.oid = i.indexrelid JOIN pg_class t ON t.oid = i.indrelid \
               JOIN pg_opclass o ON o.oid = i.indclass[0] \
              WHERE o.opcname = 'gin_trgm_ops' AND c.relnamespace = \
                    (SELECT oid FROM pg_namespace WHERE nspname = 'mentat_{store}') \
              ORDER BY 1"
        );
        Spi::connect(|client| {
            client
                .select(&sql, None, &[])
                .unwrap()
                .map(|r| r.get::<String>(1).unwrap().unwrap_or_default())
                .collect()
        })
    }

    // The 1.18.0 -> 1.19.0 upgrade replaces a 1.18 store's views and helper
    // functions with ones that read the projections (#395), and only the
    // ones the store has. Each keeps its owner, grants, extension membership
    // and the user views built on it; one whose columns its owner changed is
    // left alone; the trigram indexes move to the projections. The result is
    // what a fresh store has, and a second run changes nothing.
    #[pg_test]
    fn upgrade_replaces_a_1_18_stores_views() {
        setup();
        let trgm = pg_trgm_in_schema("ns_vt");
        make_1_18_store("vt_old", trgm);
        make_1_18_store("vt_part", false);
        make_1_18_store("vt_live", false);
        // vt_part: two objects its owner dropped, one it changed.
        Spi::run(
            "DROP VIEW mentat_vt_part.searchable_text; \
             DROP FUNCTION mentat_vt_part.find_text(TEXT); \
             DROP VIEW mentat_vt_part.graph_edges; \
             CREATE VIEW mentat_vt_part.graph_edges AS SELECT 1 AS mine",
        )
        .expect("vt_part");
        // vt_old: another owner, a grant to a reader, a user view on facts,
        // and one object that is a member of pg_mentat.
        Spi::run(
            "CREATE ROLE ns_vt_owner NOLOGIN; CREATE ROLE ns_vt_reader NOLOGIN; \
             GRANT USAGE ON SCHEMA mentat TO ns_vt_owner; GRANT SELECT ON mentat.stores TO ns_vt_owner; \
             ALTER SCHEMA mentat_vt_old OWNER TO ns_vt_owner; \
             SELECT mentat._store_grants('mentat_vt_old'); \
             GRANT USAGE ON SCHEMA mentat_vt_old TO ns_vt_reader; \
             GRANT SELECT ON mentat_vt_old.text_values TO ns_vt_reader; \
             ALTER EXTENSION pg_mentat ADD VIEW mentat_vt_old.attributes",
        )
        .expect("owners and grants");
        tx("vt_old", SCHEMA_TX);
        let a = tx("vt_old", "[{:db/id \"a\" :p/name \"ann\" :p/age 1}]")["tempids"]["a"]
            .as_i64()
            .unwrap();
        tx(
            "vt_old",
            &format!("[[:db/add {a} :p/name \"anna\"] [:db/retract {a} :p/age 1]]"),
        );
        Spi::run(
            "CREATE VIEW public.vt_user_facts AS \
               SELECT entity_id, value FROM mentat_vt_old.facts WHERE attribute = ':p/name'",
        )
        .expect("user view");
        let user_view = || {
            sorted(json(
                "SELECT coalesce(jsonb_agg(value), '[]') FROM public.vt_user_facts",
            ))
        };
        let granted = || {
            sorted(json(
                "SELECT coalesce(jsonb_agg(value), '[]') FROM mentat_vt_old.text_values \
                  WHERE attribute = ':p/name'",
            ))
        };
        assert_eq!(user_view(), vec!["ann", "anna"], "1.18.0 reads the log");
        let before = store_object_defs("vt_old");
        let part_before = store_object_defs("vt_part");

        let block = upgrade_119_block("views");
        Spi::run(&block).expect("upgrade");

        assert_eq!(user_view(), vec!["anna"], "the user view on facts");
        Spi::run("SET LOCAL ROLE ns_vt_reader").expect("set role");
        assert_eq!(
            granted(),
            vec!["anna"],
            "text_values, as the role granted it"
        );
        Spi::run("RESET ROLE").expect("reset role");
        assert_eq!(
            text(&format!(
                "SELECT coalesce(mentat_vt_old.entity_value({a}, ':p/age'), 'none')"
            )),
            "none"
        );
        // The same objects, owners, grants and memberships.
        type Defs = Vec<(String, String, String, String, bool, String)>;
        let meta = |v: &Defs| -> Vec<(String, String, String, bool, String)> {
            v.iter()
                .map(|(k, n, _, o, m, acl)| (k.clone(), n.clone(), o.clone(), *m, acl.clone()))
                .collect()
        };
        let after = store_object_defs("vt_old");
        assert_eq!(meta(&after), meta(&before));
        assert!(
            after.iter().all(|(_, _, _, o, _, _)| o == "ns_vt_owner"),
            "{after:?}"
        );
        let members: Vec<&str> = after
            .iter()
            .filter(|(.., m, _)| *m)
            .map(|(_, n, ..)| n.as_str())
            .collect();
        assert_eq!(members, vec!["attributes"]);

        // vt_part: nothing made; the view its owner changed is left alone.
        let part_after = store_object_defs("vt_part");
        assert_eq!(meta(&part_after), meta(&part_before));
        let def = |v: &Defs, name: &str| {
            v.iter()
                .find(|(_, n, ..)| n == name)
                .map(|x| x.2.clone())
                .unwrap()
        };
        assert_eq!(
            def(&part_after, "graph_edges"),
            def(&part_before, "graph_edges")
        );

        // What a fresh store has.
        Spi::run("SELECT create_store('vt_new')").expect("fresh store");
        let defs = |v: &Defs| -> Vec<(String, String, String)> {
            v.iter()
                .map(|(k, n, d, ..)| (k.clone(), n.clone(), d.clone()))
                .collect()
        };
        let fresh = store_object_defs("vt_new");
        assert_eq!(
            defs(&after).iter().map(|d| d.1.clone()).collect::<Vec<_>>(),
            defs(&fresh).iter().map(|d| d.1.clone()).collect::<Vec<_>>()
        );
        for (old, new) in defs(&after).iter().zip(defs(&fresh).iter()) {
            assert_eq!(old, new);
        }
        for name in [
            "facts",
            "references",
            "entity_value(eid bigint, attr_ident text)",
        ] {
            assert_eq!(def(&part_after, name), def(&fresh, name));
        }
        if trgm {
            assert_eq!(trgm_indexes("vt_old"), trgm_indexes("vt_new"));
            assert_eq!(
                trgm_indexes("vt_old"),
                vec![
                    "current_keyword.idx_current_keyword_trgm",
                    "current_text.idx_current_text_trgm"
                ]
            );
        }
        assert!(trgm_indexes("vt_part").is_empty());

        // Twice changes nothing.
        Spi::run(&block).expect("upgrade again");
        assert_eq!(store_object_defs("vt_old"), after);
        assert_eq!(store_object_defs("vt_part"), part_after);
        assert_eq!(trgm_indexes("vt_old"), trgm_indexes("vt_new"));

        // An upgraded store's objects return only live values.
        store_objects_read_live_values("vt_live");
    }

    /// Excise `ids` from `store` as 1.5.0 - 1.18.0 did (#396): delete the log
    /// rows and log the excision; the projection, fulltext text and idents
    /// stay.
    fn excise_as_1_18(store: &str, ids: &[i64]) {
        let sch = if store == "default" {
            "mentat".to_string()
        } else {
            format!("mentat_{store}")
        };
        let list = ids
            .iter()
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        for ty in [
            "ref", "boolean", "long", "double", "instant", "text", "keyword", "uuid", "bytes",
        ] {
            Spi::run(&format!(
                "DELETE FROM \"{sch}\".datoms_{ty}_new WHERE e IN ({list})"
            ))
            .expect("log rows");
        }
        Spi::run(&format!(
            "INSERT INTO mentat.excision_log (store_name, entity_ids) VALUES ('{store}', ARRAY[{list}])"
        ))
        .expect("excision_log");
    }

    // 1.5.0 - 1.18.0's mentat_excise left the projection rows of the
    // entities it excised (#396). The 1.18 store views read the log, which
    // hid them; the 1.19 views read the projection, so the upgrade deletes
    // them first: they don't come back in facts, text_values, datom_text_like,
    // current() or q(). A value asserted for the same id after the excision
    // stays, another entity is untouched, and a store the log names that is
    // gone gets a WARNING.
    #[pg_test]
    fn upgrade_deletes_what_old_excisions_left() {
        setup();
        make_1_18_store("ex_old", false);
        let mut eve = std::collections::HashMap::new();
        for store in ["default", "ex_old"] {
            tx(
                store,
                "[{:db/ident :ex/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one \
                   :db/unique :db.unique/identity :db/fulltext true} \
                  {:db/ident :ex/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
            );
            let r = tx(
                store,
                "[{:db/id \"e\" :ex/name \"eve secret\" :ex/age 50} {:db/id \"f\" :ex/name \"fay\" :ex/age 3} \
                  {:db/id \"g\" :ex/name \"gus\"} {:db/id \"k\" :db/ident :ex/kmauve}]",
            );
            let id = |t: &str| r["tempids"][t].as_i64().unwrap();
            // g: excised, then given a value again under the same id.
            excise_as_1_18(store, &[id("e"), id("g"), id("k")]);
            tx(
                store,
                &format!("[[:db/add {} :ex/name \"gus again\"]]", id("g")),
            );
            eve.insert(store, (id("e"), id("f"), id("g")));
        }
        Spi::run("INSERT INTO mentat.excision_log (store_name, entity_ids) VALUES ('ex_gone', ARRAY[1000001])")
            .expect("gone store");
        let (e, _, _) = eve["ex_old"];
        let shows = || -> i64 {
            Spi::get_one::<i64>(&format!(
                "SELECT (SELECT count(*) FROM mentat_ex_old.facts WHERE entity_id = {e}) \
                      + (SELECT count(*) FROM mentat_ex_old.text_values WHERE value = 'eve secret')"
            ))
            .unwrap()
            .unwrap()
        };
        assert_eq!(shows(), 0, "1.18 views read the log");

        let block = upgrade_119_block("excised");
        Spi::run(&block).expect("excised");
        Spi::run(&upgrade_119_block("views")).expect("views");
        assert_eq!(shows(), 0, "facts / text_values after the upgrade");

        for store in ["default", "ex_old"] {
            let (e, f, g) = eve[store];
            let sch = if store == "default" {
                "mentat"
            } else {
                "mentat_ex_old"
            };
            let left = Spi::get_one::<i64>(&format!(
                "SELECT (SELECT count(*) FROM {sch}.current_datoms WHERE e = {e}) \
                      + (SELECT count(*) FROM {sch}.fulltext WHERE text_value = 'eve secret') \
                      + (SELECT count(*) FROM {sch}.idents WHERE ident = ':ex/kmauve')"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(left, 0, "{store}: rows of excised entities");
            assert_eq!(
                q(store, &format!("[:find ?a ?v :where [{e} ?a ?v]]"), "{}")["result"],
                serde_json::json!([]),
                "{store}: q"
            );
            assert_eq!(
                names_of(store),
                vec!["fay".to_string(), "gus again".to_string()],
                "{store}: q, the others"
            );
        }
        // current() reads the default store.
        let (e, f, g) = eve["default"];
        for (id, attr, want) in [
            (e, ":ex/name", "-"),
            (e, ":ex/age", "-"),
            (f, ":ex/age", "3"),
            (g, ":ex/name", "gus again"),
        ] {
            assert_eq!(
                text(&format!(
                    "SELECT coalesce(mentat.current({id}, '{attr}'), '-')"
                )),
                want,
                "current({id}, {attr})"
            );
        }
        assert_eq!(
            Spi::get_one::<i64>(
                "SELECT count(*) FROM mentat.datom_text_like(':ex/name', '%secret%')"
            )
            .unwrap()
            .unwrap(),
            0
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT mentat.verify_current_projection(0)")
                .unwrap()
                .unwrap(),
            0
        );
        let msg = text(&format!(
            "SELECT pg_temp.capture($cap${}$cap$)",
            block.replace("RAISE WARNING", "RAISE EXCEPTION")
        ));
        assert!(
            msg.contains("mentat.excision_log lists entities excised from 'ex_gone'"),
            "{msg:?}"
        );
        // A table the store's owner gave a DELETE trigger: the store is
        // skipped, the trigger doesn't run.
        Spi::run(
            "CREATE TABLE public.ex_ran (x TEXT); \
             CREATE FUNCTION public.ex_trg() RETURNS trigger LANGUAGE plpgsql AS \
               $$ BEGIN INSERT INTO public.ex_ran VALUES ('ran'); RETURN OLD; END $$; \
             CREATE TRIGGER ex_trg BEFORE DELETE ON mentat_ex_old.current_long \
               FOR EACH ROW EXECUTE FUNCTION public.ex_trg()",
        )
        .expect("trigger");
        let msg = text(&format!(
            "SELECT pg_temp.capture($cap${}$cap$)",
            block.replace("RAISE WARNING", "RAISE EXCEPTION")
        ));
        assert!(
            msg.contains("entities excised from store \"ex_old\" before 1.19.0 were not looked at: current_long"),
            "{msg:?}"
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT count(*) FROM public.ex_ran")
                .unwrap()
                .unwrap(),
            0
        );
    }

    fn viewdef(view: &str) -> String {
        text(&format!("SELECT pg_get_viewdef('{view}'::regclass)"))
    }

    // A 1.17 store the 1.18.0 upgrade skipped (it WARNs and goes on, e.g.
    // when mentat_<name>_pre118 already exists) keeps the wide layout. The
    // 1.19.0 upgrade's CREATE OR REPLACE VIEW ... FROM mentat_<name>.current_ref
    // then failed with "relation does not exist" and aborted the whole
    // upgrade. Such a store is skipped with a WARNING; the others are done.
    #[pg_test]
    fn upgrade_skips_a_store_without_the_1_18_layout() {
        setup();
        make_1_18_store("lay_ok", false);
        Spi::run(
            "INSERT INTO mentat.stores (store_name, schema_name) VALUES ('lay_old', 'mentat_lay_old'); \
             CREATE SCHEMA mentat_lay_old; \
             CREATE TABLE mentat_lay_old.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
               value_type_tag SMALLINT NOT NULL, v_text TEXT, tx BIGINT NOT NULL, added BOOLEAN NOT NULL); \
             CREATE TABLE mentat_lay_old.schema (entid BIGINT, ident TEXT); \
             CREATE VIEW mentat_lay_old.facts AS SELECT e AS entity_id, v_text AS value \
               FROM mentat_lay_old.datoms WHERE added",
        )
        .expect("1.17 store");
        let old = viewdef("mentat_lay_old.facts");
        let block = upgrade_119_block("views");
        let msg = text(&format!(
            "SELECT pg_temp.capture($cap${}$cap$)",
            block.replace("RAISE WARNING", "RAISE EXCEPTION")
        ));
        assert!(
            msg.starts_with(
                "pg_mentat upgrade: views of named store \"lay_old\" not replaced: schema mentat_lay_old \
                 does not have the 1.18 layout (no current_boolean, current_bytes,"
            ),
            "{msg:?}"
        );
        assert!(msg.contains("idents, transactions)"), "{msg:?}");
        Spi::run(&block).expect("the upgrade goes on");
        assert_eq!(viewdef("mentat_lay_old.facts"), old);
        assert!(
            viewdef("mentat_lay_ok.facts").contains("current_text"),
            "the other store's views are replaced"
        );
    }

    // The upgrade caught SQLSTATE 55000 by its condition name, which is a
    // class code: it caught all of class 55, so a lock timeout (55P03) on a
    // store view became a WARNING, the extension reached 1.19.0 and that view
    // kept reading the log. A lock timeout must abort the upgrade. Two real
    // sessions (dblink): one creates a store and commits, the other holds a
    // lock on one of its views.
    #[pg_test]
    fn upgrade_aborts_on_a_lock_timeout() {
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
             CREATE OR REPLACE FUNCTION pg_temp.try_rollback(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RAISE EXCEPTION 'ran'; \
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
        Spi::run(&format!(
            "SELECT dblink_connect('lk_a', '{conn}'), dblink_connect('lk_b', '{conn}')"
        ))
        .expect("connect");
        let drop =
            "DO $d$ BEGIN PERFORM drop_store('lk_s'); EXCEPTION WHEN OTHERS THEN NULL; END $d$";
        on("lk_a", "SET search_path TO mentat, public");
        on("lk_a", drop);
        on("lk_a", "DO $d$ BEGIN PERFORM create_store('lk_s'); END $d$");
        on("lk_b", "BEGIN");
        on(
            "lk_b",
            "LOCK TABLE mentat_lk_s.\"references\" IN ACCESS SHARE MODE",
        );
        Spi::run("SET LOCAL lock_timeout = '200ms'").expect("lock_timeout");
        // Rolled back either way, so its locks don't block the cleanup.
        let msg = text(&format!(
            "SELECT pg_temp.try_rollback($cap${}$cap$)",
            upgrade_119_block("views")
        ));
        Spi::run("RESET lock_timeout").expect("reset");
        on("lk_b", "ROLLBACK");
        on("lk_a", drop);
        Spi::run("SELECT dblink_disconnect('lk_a'), dblink_disconnect('lk_b')").expect("close");
        assert!(
            msg.contains("lock timeout"),
            "the upgrade must fail: {msg:?}"
        );
    }

    // CREATE OR REPLACE resets a view's options and a function's attributes
    // its statement doesn't name. The 1.19.0 upgrade replaced a store's views
    // and functions that way, so a reader of a security_invoker view got the
    // view owner's privileges, and a SECURITY DEFINER function lost it. Each
    // setting the store's owner made is kept, on the new definition.
    #[pg_test]
    fn upgrade_keeps_the_owners_settings() {
        setup();
        make_1_18_store("own_s", false);
        Spi::run(
            "CREATE SCHEMA \"Own Path\"; \
             ALTER VIEW mentat_own_s.facts SET (security_invoker = true); \
             ALTER VIEW mentat_own_s.text_values SET (security_barrier = true); \
             ALTER FUNCTION mentat_own_s.entity_value(BIGINT, TEXT) SECURITY DEFINER \
               SET search_path = pg_catalog, \"Own Path\", pg_temp; \
             ALTER FUNCTION mentat_own_s.find_text(TEXT) COST 5000 ROWS 7 PARALLEL RESTRICTED STRICT; \
             ALTER FUNCTION mentat_own_s.lookup_entity(TEXT, TEXT) SET work_mem = '7MB' VOLATILE; \
             ALTER FUNCTION mentat_own_s.count_by_attribute() LEAKPROOF",
        )
        .expect("owner settings");
        let settings = || -> Vec<String> {
            let mut v: Vec<String> = Spi::connect(|client| {
                client
                    .select(
                        "SELECT c.relname || ' ' || c.reloptions::TEXT FROM pg_class c \
                          WHERE c.relnamespace = 'mentat_own_s'::regnamespace AND c.relkind = 'v' \
                            AND c.reloptions IS NOT NULL \
                         UNION ALL \
                         SELECT p.proname || ' ' || concat_ws(' ', p.prosecdef, p.proleakproof, p.proisstrict, \
                                p.provolatile, p.proparallel, p.procost, p.prorows, p.proconfig::TEXT) \
                           FROM pg_proc p WHERE p.pronamespace = 'mentat_own_s'::regnamespace",
                        None,
                        &[],
                    )
                    .unwrap()
                    .map(|r| r.get::<String>(1).unwrap().unwrap_or_default())
                    .collect()
            });
            v.sort();
            v
        };
        let before = settings();
        assert_eq!(
            before,
            vec![
                "count_by_attribute f t f s u 100 1000",
                "entities_with_attribute f f f s u 100 1000",
                "entity_value t f f s u 100 0 {\"search_path=pg_catalog, \\\"Own Path\\\", pg_temp\"}",
                "facts {security_invoker=true}",
                "find_text f f t s r 5000 7",
                "lookup_entity f f f v u 100 1000 {work_mem=7MB}",
                "text_values {security_barrier=true}",
            ]
        );
        let block = upgrade_119_block("views");
        Spi::run(&block).expect("upgrade");
        assert_eq!(settings(), before);
        for (f, sig) in [
            ("entity_value", "BIGINT, TEXT"),
            ("find_text", "TEXT"),
            ("lookup_entity", "TEXT, TEXT"),
        ] {
            assert!(
                text(&format!(
                    "SELECT prosrc FROM pg_proc WHERE oid = 'mentat_own_s.{f}({sig})'::regprocedure"
                ))
                .contains("current_"),
                "{f}: the new body"
            );
        }
        assert!(
            text("SELECT pg_get_viewdef('mentat_own_s.facts'::regclass)").contains("current_text")
        );
        Spi::run(&block).expect("twice");
        assert_eq!(settings(), before);
    }

    // A store function its owner replaced by a procedure with the same
    // arguments: ALTER EXTENSION ADD FUNCTION failed with 42809 "is not a
    // function", which aborted the upgrade. It is left alone, with a WARNING.
    #[pg_test]
    fn upgrade_skips_a_procedure_in_a_functions_place() {
        setup();
        make_1_18_store("proc_s", false);
        Spi::run(
            "DROP FUNCTION mentat_proc_s.find_text(TEXT); \
             CREATE PROCEDURE mentat_proc_s.find_text(q TEXT) LANGUAGE sql AS $$ SELECT 1 $$",
        )
        .expect("procedure");
        let block = upgrade_119_block("views");
        let msg = text(&format!(
            "SELECT pg_temp.capture($cap${}$cap$)",
            block.replace("RAISE WARNING", "RAISE EXCEPTION")
        ));
        assert_eq!(
            msg,
            "pg_mentat upgrade: mentat_proc_s.find_text(text) of store \"proc_s\" not replaced: \
             it is not a function."
        );
        Spi::run(&block).expect("upgrade");
        assert_eq!(
            text(
                "SELECT prokind::TEXT FROM pg_proc WHERE proname = 'find_text' \
                    AND pronamespace = 'mentat_proc_s'::regnamespace"
            ),
            "p"
        );
        assert!(text(
            "SELECT prosrc FROM pg_proc WHERE oid = 'mentat_proc_s.lookup_entity(TEXT, TEXT)'::regprocedure"
        )
        .contains("current_"));
    }

    // On 1.18 a store's find_text and searchable_text used the log's GIN
    // index on to_tsvector('english', v); current_text had none, so on the
    // projection they scanned every live string. The store layout has the
    // index now and the upgrade builds it for existing stores.
    #[pg_test]
    fn store_full_text_reads_use_an_index() {
        setup();
        Spi::run("SELECT create_store('fts_new')").expect("create_store");
        make_1_18_store("fts_old", false);
        let has = |store: &str| -> bool {
            Spi::get_one::<bool>(&format!(
                "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'mentat_{store}' \
                   AND indexname = 'idx_current_text_fts' AND indexdef LIKE '%USING gin (to_tsvector(%')"
            ))
            .unwrap()
            .unwrap()
        };
        assert!(has("fts_new"), "a new store");
        assert!(
            Spi::get_one::<bool>(
                "SELECT EXISTS (SELECT 1 FROM pg_indexes WHERE schemaname = 'mentat' \
                   AND indexname = 'idx_current_text_fts')"
            )
            .unwrap()
            .unwrap(),
            "the default store"
        );
        assert!(!has("fts_old"));
        Spi::run(&upgrade_119_block("views")).expect("upgrade");
        assert!(has("fts_old"), "an upgraded store");
        for store in ["fts_new", "fts_old"] {
            Spi::run(&format!(
                "INSERT INTO mentat_{store}.current_text (store_id, e, a, v, tx) \
                 SELECT (SELECT store_id FROM mentat.stores WHERE store_name = '{store}'), \
                        1000000 + i, 100, CASE WHEN i % 1000 = 0 THEN 'a fox' ELSE 'word ' || i END, 1 \
                   FROM generate_series(1, 5000) i; \
                 ANALYZE mentat_{store}.current_text"
            ))
            .expect("rows");
            let plan: String = Spi::connect(|client| {
                client
                    .select(
                        &format!(
                            "EXPLAIN SELECT * FROM mentat_{store}.searchable_text \
                              WHERE search_vector @@ plainto_tsquery('english', 'fox')"
                        ),
                        None,
                        &[],
                    )
                    .unwrap()
                    .map(|r| r.get::<String>(1).unwrap().unwrap_or_default())
                    .collect::<Vec<_>>()
                    .join("\n")
            });
            assert!(plan.contains("idx_current_text_fts"), "{store}: {plan}");
        }
    }

    // Stores the upgrade can't replace the views of were skipped with no
    // WARNING: a store row whose schema was renamed by hand, and one whose
    // schema is gone.
    #[pg_test]
    fn upgrade_names_the_stores_it_skips() {
        setup();
        make_1_18_store("sk_ren", false);
        make_1_18_store("sk_gone", false);
        Spi::run(
            "ALTER SCHEMA mentat_sk_ren RENAME TO mentat_sk_moved; \
             UPDATE mentat.stores SET schema_name = 'mentat_sk_moved' WHERE store_name = 'sk_ren'",
        )
        .expect("renamed");
        let block = upgrade_119_block("views");
        let first = || {
            text(&format!(
                "SELECT pg_temp.capture($cap${}$cap$)",
                block.replace("RAISE WARNING", "RAISE EXCEPTION")
            ))
        };
        assert_eq!(
            first(),
            "pg_mentat upgrade: views in schema mentat_sk_moved not replaced: its mentat.stores row \
             \"sk_ren\" would not have been made by create_store() (renamed by hand?). Rename it back, \
             or use rename_store, then run create_virtual_tables('sk_ren')."
        );
        Spi::run(
            "UPDATE mentat.stores SET schema_name = 'mentat_sk_ren' WHERE store_name = 'sk_ren'; \
             ALTER SCHEMA mentat_sk_moved RENAME TO mentat_sk_ren; \
             DROP SCHEMA mentat_sk_gone CASCADE",
        )
        .expect("gone");
        assert_eq!(
            first(),
            "pg_mentat upgrade: views of named store \"sk_gone\" not replaced: its schema \
             mentat_sk_gone does not exist (dropped by hand?). drop_store('sk_gone') removes the store."
        );
        Spi::run("DELETE FROM mentat.stores WHERE store_name = 'sk_gone'").expect("drop row");
        assert_eq!(first(), "", "the others: no WARNING");
        Spi::run(&block).expect("the upgrade goes on");
    }

    fn names_of(store: &str) -> Vec<String> {
        sorted(q(store, "[:find [?n ...] :where [?e :ex/name ?n]]", "{}")["result"].clone())
    }
}
