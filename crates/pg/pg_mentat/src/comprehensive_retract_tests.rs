// Comprehensive retract tests: exhaustive coverage of retraction
// operations across all value types, cardinalities, and scenarios.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
    }

    fn setup_cr_schema() {
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"n\" :db/ident :cr/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/id \"v\" :db/ident :cr/val :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/id \"d\" :db/ident :cr/dbl :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
                {:db/id \"b\" :db/ident :cr/flag :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
                {:db/id \"k\" :db/ident :cr/kw :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
                {:db/id \"t\" :db/ident :cr/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
                {:db/id \"lm\" :db/ident :cr/nums :db/valueType :db.type/long :db/cardinality :db.cardinality/many}
                {:db/id \"r\" :db/ident :cr/ref :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
                {:db/id \"rm\" :db/ident :cr/refs :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
            ]'::TEXT)",
        ).expect("cr schema");
    }

    // ========================================================================
    // Retract string (5 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_string() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/name \"gone\"]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/name \"gone\"]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/name ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    #[pg_test]
    fn test_cr_retract_empty_string() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/name \"\"]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/name \"\"]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/name ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    // ========================================================================
    // Retract long (5 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_long_positive() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/val 42]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/val 42]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    #[pg_test]
    fn test_cr_retract_long_negative() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/val -999]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/val -999]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    #[pg_test]
    fn test_cr_retract_long_zero() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/val 0]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/val 0]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    // ========================================================================
    // Retract boolean (4 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_bool_true() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/flag true]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/flag true]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/flag ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    #[pg_test]
    fn test_cr_retract_bool_false() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/flag false]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/flag false]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/flag ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    // ========================================================================
    // Retract double (3 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_double() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/dbl 3.14]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/dbl 3.14]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/dbl ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    // ========================================================================
    // Retract keyword (3 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_keyword() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/kw :active]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/kw :active]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/kw ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    // ========================================================================
    // RetractEntity (10 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_entity_with_string() {
        setup();
        setup_cr_schema();
        let r =
            Spi::get_one::<String>("SELECT edn_t('[{:db/id \"e\" :cr/name \"doomed\"}]'::TEXT)")
                .expect("tx")
                .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retractEntity {}]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/name ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert!(v["result"].is_null());
    }

    #[pg_test]
    fn test_cr_retract_entity_with_many_attrs() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[{:db/id \"e\" :cr/name \"full\" :cr/val 42 :cr/flag true :cr/kw :test}]'::TEXT)").expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retractEntity {}]]'::TEXT)",
            eid
        ))
        .expect("retract");
        for attr in &[":cr/name", ":cr/val", ":cr/flag", ":cr/kw"] {
            let q = Spi::get_one::<String>(&format!(
                "SELECT edn_q('[:find ?v . :where [{} {} ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
                eid, attr
            ))
            .expect("q")
            .expect("NULL");
            let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
            assert!(v["result"].is_null(), "{} should be null", attr);
        }
    }

    #[pg_test]
    fn test_cr_retract_entity_with_many_values() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[[:db/add \"e\" :cr/name \"tagged\"] [:db/add \"e\" :cr/tags \"a\"] [:db/add \"e\" :cr/tags \"b\"] [:db/add \"e\" :cr/tags \"c\"]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retractEntity {}]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?t ...] :where [{} :cr/tags ?t]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_array().expect("arr").len(), 0);
    }

    #[pg_test]
    fn test_cr_retract_entity_doesnt_affect_others() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[{:db/id \"a\" :cr/name \"keep\" :cr/val 1} {:db/id \"b\" :cr/name \"remove\" :cr/val 2}]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let a = j["tempids"]["a"].as_i64().expect("a");
        let b = j["tempids"]["b"].as_i64().expect("b");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retractEntity {}]]'::TEXT)",
            b
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!("SELECT edn_q('[:find ?n ?v :where [{e} :cr/name ?n] [{e} :cr/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT", e = a)).expect("q").expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["results"].as_array().expect("arr").len(), 1);
    }

    #[pg_test]
    fn test_cr_batch_retract_entity_10() {
        setup();
        setup_cr_schema();
        let mut ops = Vec::new();
        for i in 0..10 {
            ops.push(format!(
                "{{:db/id \"e{i}\" :cr/name \"doomed-{i}\" :cr/val {i}}}",
                i = i
            ));
        }
        let r = Spi::get_one::<String>(&format!("SELECT edn_t('[{}]'::TEXT)", ops.join("\n")))
            .expect("create")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let mut retracts = Vec::new();
        for i in 0..10 {
            let eid = j["tempids"][&format!("e{}", i)].as_i64().expect("eid");
            retracts.push(format!("[:db/retractEntity {}]", eid));
        }
        Spi::run(&format!("SELECT edn_t('[{}]'::TEXT)", retracts.join("\n")))
            .expect("batch retract");
        // Append-only model: retractEntity appends retraction datoms; the
        // original assertions remain in the immutable log. "Are these
        // entities gone?" is a current-state question, answered by the
        // projection (current_text), not by an `added=true` scan of the log.
        let count = Spi::get_one::<i64>(
            "SELECT COUNT(DISTINCT e) FROM mentat.current_text WHERE a = (SELECT entid FROM mentat.idents WHERE ident = ':cr/name') AND v LIKE 'doomed-%'",
        ).expect("q").expect("NULL");
        assert_eq!(count, 0);
    }

    // ========================================================================
    // Retract many-valued attributes (10 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_one_of_many_strings() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[[:db/add \"e\" :cr/name \"h\"] [:db/add \"e\" :cr/tags \"a\"] [:db/add \"e\" :cr/tags \"b\"] [:db/add \"e\" :cr/tags \"c\"]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/tags \"b\"]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?t ...] :where [{} :cr/tags ?t]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        let tags: Vec<&str> = v["result"]
            .as_array()
            .expect("arr")
            .iter()
            .map(|t| t.as_str().expect("s"))
            .collect();
        assert_eq!(tags.len(), 2);
        assert!(!tags.contains(&"b"));
    }

    #[pg_test]
    fn test_cr_retract_all_many_one_by_one() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[[:db/add \"e\" :cr/name \"h\"] [:db/add \"e\" :cr/tags \"x\"] [:db/add \"e\" :cr/tags \"y\"] [:db/add \"e\" :cr/tags \"z\"]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/tags \"x\"]]'::TEXT)",
            eid
        ))
        .expect("r1");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/tags \"y\"]]'::TEXT)",
            eid
        ))
        .expect("r2");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/tags \"z\"]]'::TEXT)",
            eid
        ))
        .expect("r3");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?t ...] :where [{} :cr/tags ?t]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_array().expect("arr").len(), 0);
    }

    #[pg_test]
    fn test_cr_retract_all_many_in_one_tx() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[[:db/add \"e\" :cr/name \"h\"] [:db/add \"e\" :cr/tags \"a\"] [:db/add \"e\" :cr/tags \"b\"]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!("SELECT edn_t('[[:db/retract {} :cr/tags \"a\"] [:db/retract {} :cr/tags \"b\"]]'::TEXT)", eid, eid)).expect("retract all");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?t ...] :where [{} :cr/tags ?t]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_array().expect("arr").len(), 0);
    }

    #[pg_test]
    fn test_cr_retract_many_nums() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[[:db/add \"e\" :cr/name \"h\"] [:db/add \"e\" :cr/nums 10] [:db/add \"e\" :cr/nums 20] [:db/add \"e\" :cr/nums 30]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/nums 20]]'::TEXT)",
            eid
        ))
        .expect("retract");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?v ...] :where [{} :cr/nums ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_array().expect("arr").len(), 2);
    }

    #[pg_test]
    fn test_cr_retract_many_refs_one() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[{:db/id \"hub\" :cr/name \"Hub\"} {:db/id \"s1\" :cr/name \"S1\"} {:db/id \"s2\" :cr/name \"S2\"} [:db/add \"hub\" :cr/refs \"s1\"] [:db/add \"hub\" :cr/refs \"s2\"]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let hub = j["tempids"]["hub"].as_i64().expect("hub");
        let s1 = j["tempids"]["s1"].as_i64().expect("s1");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/refs {}]]'::TEXT)",
            hub, s1
        ))
        .expect("retract ref");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?r ...] :where [{} :cr/refs ?r]]'::TEXT, '{{}}'::jsonb)::TEXT",
            hub
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_array().expect("arr").len(), 1);
    }

    // ========================================================================
    // Retract then re-add (5 tests)
    // ========================================================================

    #[pg_test]
    fn test_cr_retract_readd_string() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/name \"test\"]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/name \"test\"]]'::TEXT)",
            eid
        ))
        .expect("retract");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {} :cr/name \"test\"]]'::TEXT)",
            eid
        ))
        .expect("readd");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/name ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_str().expect("s"), "test");
    }

    #[pg_test]
    fn test_cr_retract_readd_different_value() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>("SELECT edn_t('[[:db/add \"e\" :cr/val 10]]'::TEXT)")
            .expect("tx")
            .expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/val 10]]'::TEXT)",
            eid
        ))
        .expect("retract");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {} :cr/val 99]]'::TEXT)",
            eid
        ))
        .expect("readd");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find ?v . :where [{} :cr/val ?v]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_i64().expect("v"), 99);
    }

    #[pg_test]
    fn test_cr_retract_readd_many() {
        setup();
        setup_cr_schema();
        let r = Spi::get_one::<String>(
            "SELECT edn_t('[[:db/add \"e\" :cr/name \"h\"] [:db/add \"e\" :cr/tags \"tag1\"]]'::TEXT)"
        ).expect("tx").expect("NULL");
        let j: serde_json::Value = serde_json::from_str(&r).expect("parse");
        let eid = j["tempids"]["e"].as_i64().expect("eid");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :cr/tags \"tag1\"]]'::TEXT)",
            eid
        ))
        .expect("retract");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {} :cr/tags \"tag1\"]]'::TEXT)",
            eid
        ))
        .expect("readd");
        let q = Spi::get_one::<String>(&format!(
            "SELECT edn_q('[:find [?t ...] :where [{} :cr/tags ?t]]'::TEXT, '{{}}'::jsonb)::TEXT",
            eid
        ))
        .expect("q")
        .expect("NULL");
        let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
        assert_eq!(v["result"].as_array().expect("arr").len(), 1);
    }

    // Issue #326: :db/retractEntity retracts INBOUND refs (datoms whose value
    // is a ref to the retracted entity), as Datomic does, so no dangling ref
    // survives. The referring entity itself is left intact.
    #[pg_test]
    fn test_retract_entity_retracts_inbound_refs() {
        setup();
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :ib/name :db/valueType :db.type/string
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                {:db/ident :ib/friend :db/valueType :db.type/ref
                 :db/cardinality :db.cardinality/many}
             ]'::TEXT)",
        )
        .expect("schema");
        // alice -> friend -> bob ; carol -> friend -> bob
        Spi::run(
            "SELECT edn_t('[
                {:db/id \"b\" :ib/name \"bob\"}
                {:ib/name \"alice\" :ib/friend \"b\"}
                {:ib/name \"carol\" :ib/friend \"b\"}
             ]'::TEXT)",
        )
        .expect("data");

        let friends_of_bob = || -> i64 {
            let q = Spi::get_one::<String>(
                "SELECT edn_q('[:find (count ?p) . :where [?b :ib/name \"bob\"] [?p :ib/friend ?b]]'::TEXT, '{}'::jsonb)::TEXT",
            )
            .expect("q")
            .expect("NULL");
            let v: serde_json::Value = serde_json::from_str(&q).expect("parse");
            v["result"].as_i64().unwrap_or(0)
        };
        assert_eq!(friends_of_bob(), 2, "two inbound refs before retract");

        Spi::run("SELECT edn_t('[[:db/retractEntity [:ib/name \"bob\"]]]'::TEXT)")
            .expect("retractEntity bob");

        // No live :ib/friend datom points at bob any more. Check the
        // current-state projection: the datom log is append-only (the old
        // assertion row stays, with a later retraction datom beside it), so
        // `mentat.datoms WHERE added` would still count it.
        let dangling = Spi::get_one::<i64>(
            "SELECT count(*) FROM mentat.current_ref c \
             JOIN mentat.schema s ON s.entid = c.a \
             WHERE s.ident = ':ib/friend' AND c.store_id = 0",
        )
        .expect("count")
        .unwrap_or(-1);
        assert_eq!(dangling, 0, "inbound :ib/friend refs must be retracted");
        // And the query engine agrees.
        let via_q = Spi::get_one::<String>(
            "SELECT edn_q('[:find (count ?p) . :where [?p :ib/friend _]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let vq: serde_json::Value = serde_json::from_str(&via_q).expect("parse");
        assert!(
            vq["result"].is_null() || vq["result"].as_i64() == Some(0),
            "no :ib/friend refs should remain, got {vq}"
        );

        // The referrers themselves survive (only their ref datom went away):
        // after retracting bob, the remaining :ib/name entities are alice and
        // carol.
        let alive = Spi::get_one::<String>(
            "SELECT edn_q('[:find (count ?p) . :where [?p :ib/name _]]'::TEXT, '{}'::jsonb)::TEXT",
        )
        .expect("q")
        .expect("NULL");
        let a: serde_json::Value = serde_json::from_str(&alive).expect("parse");
        assert_eq!(a["result"].as_i64(), Some(2), "alice and carol remain");
    }
}
