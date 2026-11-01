// Exhaustive upsert tests: unique/identity resolution, merge behavior,
// conflict detection, and edge cases.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._test_raises_error(stmt TEXT) RETURNS BOOLEAN
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN false;
             EXCEPTION WHEN OTHERS THEN
                 RETURN true;
             END;
             $$",
        )
        .expect("create helper");
    }

    fn raises_error(sql: &str) -> bool {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<bool>(&format!("SELECT mentat._test_raises_error('{}')", escaped))
            .expect("raises_error call")
            .unwrap_or(false)
    }

    fn setup_upsert_schema() {
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"ui\" :db/ident :up/uid :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                {:db/id \"uv\" :db/ident :up/code :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/value}
                {:db/id \"n\"  :db/ident :up/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"v\"  :db/ident :up/val :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/id \"t\"  :db/ident :up/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
                {:db/id \"r\"  :db/ident :up/ref :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
                {:db/id \"f\"  :db/ident :up/flag :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
            ]'::TEXT)",
        ).expect("upsert schema");
    }

    // ========================================================================
    // Basic upsert via unique/identity
    // ========================================================================

    #[pg_test]
    fn test_up_basic_upsert() {
        setup();
        setup_upsert_schema();
        Spi::run(
            "SELECT edn_t('[{:db/id \"e\" :up/uid \"U1\" :up/name \"Alice\" :up/val 10}]'::TEXT)",
        )
        .expect("create");
        Spi::run("SELECT edn_t('[{:db/id \"e\" :up/uid \"U1\" :up/val 20}]'::TEXT)")
            .expect("upsert");
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :up/uid \"U1\"] [?e :up/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_i64().expect("v"), 20);
    }

    #[pg_test]
    fn test_up_upsert_preserves_unmentioned_attrs() {
        setup();
        setup_upsert_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e\" :up/uid \"U2\" :up/name \"Bob\" :up/val 10 :up/flag true}]'::TEXT)").expect("create");
        Spi::run("SELECT edn_t('[{:db/id \"e\" :up/uid \"U2\" :up/val 20}]'::TEXT)")
            .expect("upsert");

        // Name and flag should still be there
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?n ?f :where [?e :up/uid \"U2\"] [?e :up/name ?n] [?e :up/flag ?f]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let r = j["results"].as_array().expect("arr");
        assert_eq!(r.len(), 1);
        assert_eq!(r[0][0].as_str().expect("n"), "Bob");
        assert_eq!(r[0][1].as_bool().expect("f"), true);
    }

    #[pg_test]
    fn test_up_upsert_entity_count_stable() {
        setup();
        setup_upsert_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e\" :up/uid \"U3\" :up/name \"Carol\"}]'::TEXT)")
            .expect("create");

        for i in 0..10 {
            Spi::run(&format!(
                "SELECT edn_t('[{{:db/id \"u\" :up/uid \"U3\" :up/val {}}}]'::TEXT)",
                i
            ))
            .expect("upsert");
        }

        let count = Spi::get_one::<i64>(
            "SELECT COUNT(DISTINCT e) FROM mentat.datoms
             WHERE a = (SELECT entid FROM mentat.idents WHERE ident = ':up/uid')
             AND v_text = 'U3' AND added = true",
        )
        .expect("q")
        .expect("NULL");
        assert_eq!(count, 1, "10 upserts should not create additional entities");
    }

    // ========================================================================
    // Multiple upserts in same tx
    // ========================================================================

    #[pg_test]
    fn test_up_two_upserts_same_tx() {
        setup();
        setup_upsert_schema();
        Spi::run(
            "SELECT edn_t('[
            {:db/id \"e1\" :up/uid \"MA\" :up/name \"Alice\"}
            {:db/id \"e2\" :up/uid \"MB\" :up/name \"Bob\"}
        ]'::TEXT)",
        )
        .expect("create");

        Spi::run(
            "SELECT edn_t('[
            {:db/id \"u1\" :up/uid \"MA\" :up/val 100}
            {:db/id \"u2\" :up/uid \"MB\" :up/val 200}
        ]'::TEXT)",
        )
        .expect("upsert both");

        let qa = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :up/uid \"MA\"] [?e :up/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let ja: serde_json::Value = serde_json::from_str(&qa).expect("parse");
        assert_eq!(ja["result"].as_i64().expect("v"), 100);

        let qb = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :up/uid \"MB\"] [?e :up/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let jb: serde_json::Value = serde_json::from_str(&qb).expect("parse");
        assert_eq!(jb["result"].as_i64().expect("v"), 200);
    }

    // ========================================================================
    // Upsert with cardinality-many
    // ========================================================================

    #[pg_test]
    fn test_up_upsert_adds_to_many() {
        setup();
        setup_upsert_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e\" :up/uid \"UT1\" :up/name \"Tagged\"}]'::TEXT)")
            .expect("create");
        Spi::run("SELECT edn_t('[{:db/id \"e\" :up/uid \"UT1\"} [:db/add \"e\" :up/tags \"tag1\"] [:db/add \"e\" :up/tags \"tag2\"]]'::TEXT)").expect("upsert tags");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find [?t ...] :where [?e :up/uid \"UT1\"] [?e :up/tags ?t]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_array().expect("arr").len(), 2);
    }

    // ========================================================================
    // Unique/value vs unique/identity
    // ========================================================================

    #[pg_test]
    fn test_up_unique_value_rejects_duplicate() {
        setup();
        setup_upsert_schema();
        Spi::run("SELECT edn_t('[[:db/add \"e1\" :up/code \"C1\"]]'::TEXT)").expect("first");
        assert!(
            raises_error("SELECT edn_t('[[:db/add \"e2\" :up/code \"C1\"]]'::TEXT)"),
            "unique/value should reject duplicate"
        );
    }

    #[pg_test]
    fn test_up_unique_identity_upserts() {
        setup();
        setup_upsert_schema();
        Spi::run("SELECT edn_t('[{:db/id \"e1\" :up/uid \"I1\" :up/name \"First\"}]'::TEXT)")
            .expect("first");
        Spi::run("SELECT edn_t('[{:db/id \"e2\" :up/uid \"I1\" :up/name \"Second\"}]'::TEXT)")
            .expect("upsert");

        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?n . :where [?e :up/uid \"I1\"] [?e :up/name ?n]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_str().expect("n"), "Second");
    }

    // ========================================================================
    // Upsert with new entity creation
    // ========================================================================

    #[pg_test]
    fn test_up_new_uid_creates_entity() {
        setup();
        setup_upsert_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[{:db/id \"e\" :up/uid \"NEW1\" :up/name \"New Entity\"}]'::TEXT)",
        )
        .expect("tx")
        .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        assert!(
            j["tempids"]["e"].as_i64().is_some(),
            "New UID should create new entity"
        );
    }

    // ========================================================================
    // Batch upsert
    // ========================================================================

    #[pg_test]
    fn test_up_batch_upsert_10() {
        setup();
        setup_upsert_schema();

        // Create 10 entities
        let mut create_ops = Vec::new();
        for i in 0..10 {
            create_ops.push(format!(
                "{{:db/id \"e{i}\" :up/uid \"BATCH-{i}\" :up/name \"Entity-{i}\" :up/val {i}}}",
                i = i
            ));
        }
        Spi::run(&format!(
            "SELECT edn_t('[{}]'::TEXT)",
            create_ops.join("\n")
        ))
        .expect("create");

        // Upsert all 10
        let mut upsert_ops = Vec::new();
        for i in 0..10 {
            upsert_ops.push(format!(
                "{{:db/id \"u{i}\" :up/uid \"BATCH-{i}\" :up/val {}}}",
                100 + i
            ));
        }
        Spi::run(&format!(
            "SELECT edn_t('[{}]'::TEXT)",
            upsert_ops.join("\n")
        ))
        .expect("upsert");

        // Verify entity count
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(DISTINCT e) FROM mentat.datoms
             WHERE a = (SELECT entid FROM mentat.idents WHERE ident = ':up/uid')
             AND added = true",
        )
        .expect("q")
        .expect("NULL");
        assert_eq!(count, 10, "Batch upsert should not create duplicates");

        // Spot check one value
        let q = Spi::get_one::<String>(
            "SELECT edn_q('[:find ?v . :where [?e :up/uid \"BATCH-5\"] [?e :up/val ?v]]'::TEXT, '{}'::jsonb)::TEXT",
        ).expect("q").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(j["result"].as_i64().expect("v"), 105);
    }

    // ========================================================================
    // A unique value an entity gave up (#393)
    // ========================================================================

    /// The transactor's unique-value lookup read the append-only log with
    /// `added = true`, so a value an entity had moved off (or retracted)
    /// still named it: upserting by that value merged into the former
    /// holder, a :db.unique/value assert of it was refused, and a lookup ref
    /// to a value nobody held resolved. It reads the store's live datoms
    /// now, on the default store and on a named store.
    #[pg_test]
    fn test_upsert_unique_value_given_up() {
        setup();
        Spi::run(
            "CREATE OR REPLACE FUNCTION pg_temp.capture(stmt TEXT) RETURNS TEXT \
             LANGUAGE plpgsql AS $$ BEGIN EXECUTE stmt; RETURN ''; \
             EXCEPTION WHEN OTHERS THEN RETURN SQLERRM; END $$",
        )
        .expect("capture helper");
        Spi::run("SELECT create_store('up_freed_s')").expect("create_store");
        for store in ["default", "up_freed_s"] {
            let t = |edn: &str| -> serde_json::Value {
                let out = Spi::get_one::<String>(&format!(
                    "SELECT t('{store}', '{}')",
                    edn.replace('\'', "''")
                ))
                .unwrap_or_else(|e| panic!("{store} {edn}: {e}"))
                .expect("NULL");
                serde_json::from_str(&out).expect("tx report")
            };
            let err = |edn: &str| -> String {
                let stmt = format!("SELECT t('{store}', '{}')", edn.replace('\'', "''"));
                Spi::get_one::<String>(&format!(
                    "SELECT pg_temp.capture('{}')",
                    stmt.replace('\'', "''")
                ))
                .expect("capture")
                .unwrap_or_default()
            };
            let n_of = |e: i64| -> serde_json::Value {
                Spi::get_one::<pgrx::JsonB>(&format!(
                    "SELECT q('{store}', '[:find ?n . :in $ ?e :where [?e :u/n ?n]]', \
                     '{{\"inputs\": [{e}]}}'::jsonb)"
                ))
                .expect("q")
                .expect("NULL")
                .0["result"]
                    .clone()
            };
            t("[{:db/ident :u/email :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/identity} \
                {:db/ident :u/code :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/value} \
                {:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]");
            let r = t("[{:db/id \"u\" :u/email \"old@x\" :u/code \"c1\" :u/n 1}]");
            let u = r["tempids"]["u"].as_i64().expect("u");
            t(&format!(
                "[[:db/add {u} :u/email \"new@x\"] [:db/retract {u} :u/code \"c1\"]]"
            ));

            // Upsert by the old identity value: a new entity, u untouched.
            let r = t("[{:db/id \"w\" :u/email \"old@x\" :u/n 50}]");
            let w = r["tempids"]["w"]
                .as_i64()
                .unwrap_or_else(|| panic!("{store}: {r}"));
            assert_ne!(w, u, "{store}: upserted into u, which gave old@x up");
            assert_eq!(n_of(u), serde_json::json!(1), "{store}");
            assert_eq!(n_of(w), serde_json::json!(50), "{store}");
            // Upsert by the current value still finds u.
            let r = t("[{:db/id \"x\" :u/email \"new@x\" :u/n 2}]");
            assert_eq!(r["tempids"]["x"].as_i64(), Some(u), "{store}: {r}");
            assert_eq!(n_of(u), serde_json::json!(2), "{store}");

            // A lookup ref to a value nobody holds now: the usual error.
            // (old@x is w's now; z@x was u's for one transaction.)
            t(&format!("[[:db/add {u} :u/email \"z@x\"]]"));
            t(&format!("[[:db/add {u} :u/email \"new@x\"]]"));
            let e = err("[[:db/add [:u/email \"z@x\"] :u/n 9]]");
            assert!(
                e.contains(":db.error/lookup-ref-not-found"),
                "{store}: {e:?}"
            );
            assert_eq!(n_of(u), serde_json::json!(2), "{store}");
            t("[[:db/add [:u/email \"old@x\"] :u/n 51]]");
            assert_eq!(n_of(w), serde_json::json!(51), "{store}");

            // A :db.unique/value value u retracted is free for another entity.
            let r = t("[{:db/id \"v\" :u/code \"c1\"}]");
            let v = r["tempids"]["v"]
                .as_i64()
                .unwrap_or_else(|| panic!("{store}: {r}"));
            assert_ne!(v, u, "{store}");
            // And held now, it is refused for anyone else.
            let e = err(&format!("[[:db/add {u} :u/code \"c1\"]]"));
            assert!(e.contains(":db.error/unique-conflict"), "{store}: {e:?}");
        }
        Spi::run("SELECT drop_store('up_freed_s')").expect("drop_store");
    }
}
