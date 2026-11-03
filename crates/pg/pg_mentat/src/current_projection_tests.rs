// Tests for the current-state projection (item 3 of the 1.5.0 append-only
// work): the nine mentat.current_<type> tables, their incremental
// maintenance by the transact path, and the rebuild / verify helpers.
//
// The central invariant under test: after ANY sequence of transactions,
// mentat.verify_current_projection(0) returns 0 -- the incrementally
// maintained projection equals a fresh latest-tx-wins resolution of the
// append-only log, with no rebuild required. This is the safety gate that
// must hold before item (1) removes the in-place `added` flip.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
    }

    fn schema() {
        Spi::run(
            "SELECT edn_t('[
                {:db/ident :p/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity}
                {:db/ident :p/name  :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :p/age   :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
                {:db/ident :p/tag   :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
                {:db/ident :p/score :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
                {:db/ident :p/seen  :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}
                {:db/ident :p/admin :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
                {:db/ident :p/friend :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
            ]'::TEXT)",
        )
        .expect("schema tx");
    }

    fn verify_clean() {
        let m = Spi::get_one::<i64>("SELECT mentat.verify_current_projection(0)")
            .expect("verify")
            .expect("NULL");
        assert_eq!(m, 0, "projection drifted from log: {} mismatches", m);
    }

    fn email_eid(email: &str) -> i64 {
        Spi::get_one::<i64>(&format!(
            "SELECT e FROM mentat.current_text WHERE v = '{}' AND a = mentat.attr_id(':p/email')",
            email
        ))
        .expect("q")
        .expect("NULL")
    }

    /// Fresh insert populates the projection and stays consistent.
    #[pg_test]
    fn pg_test_proj_fresh_insert_consistent() {
        setup();
        schema();
        Spi::run(
            "SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/name \"Alice\" :p/age 30 :p/score 9.5 :p/admin true :p/tag \"x\"}]'::TEXT)",
        )
        .expect("tx");
        verify_clean();
        let e = email_eid("a@x.io");
        // Each cardinality-one value present exactly once.
        assert_eq!(
            Spi::get_one::<String>(&format!(
                "SELECT v FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/name')",
                e
            ))
            .expect("q")
            .expect("NULL"),
            "Alice"
        );
        assert_eq!(
            Spi::get_one::<i64>(&format!(
                "SELECT v FROM mentat.current_long WHERE e={} AND a=mentat.attr_id(':p/age')",
                e
            ))
            .expect("q")
            .expect("NULL"),
            30
        );
        assert!(Spi::get_one::<bool>(&format!(
            "SELECT v FROM mentat.current_boolean WHERE e={} AND a=mentat.attr_id(':p/admin')",
            e
        ))
        .expect("q")
        .expect("NULL"));
    }

    /// Cardinality-one replace leaves exactly one current value.
    #[pg_test]
    fn pg_test_proj_cardinality_one_replace() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/name \"Alice\"}]'::TEXT)")
            .expect("tx");
        let e = email_eid("a@x.io");
        Spi::run(&format!(
            "SELECT edn_t('[{{:db/id {} :p/name \"Alyce\"}}]'::TEXT)",
            e
        ))
        .expect("r1");
        Spi::run(&format!(
            "SELECT edn_t('[{{:db/id {} :p/name \"Alicia\"}}]'::TEXT)",
            e
        ))
        .expect("r2");

        let cnt = Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/name')",
            e
        ))
        .expect("q")
        .expect("NULL");
        assert_eq!(
            cnt, 1,
            "cardinality-one must have exactly one current value"
        );
        let v = Spi::get_one::<String>(&format!(
            "SELECT v FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/name')",
            e
        ))
        .expect("q")
        .expect("NULL");
        assert_eq!(v, "Alicia");
        verify_clean();
    }

    /// Cardinality-many: multiple values coexist; retract removes exactly one.
    #[pg_test]
    fn pg_test_proj_cardinality_many_add_retract() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/tag \"x\"}]'::TEXT)")
            .expect("tx");
        let e = email_eid("a@x.io");
        Spi::run(&format!(
            "SELECT edn_t('[{{:db/id {} :p/tag \"y\"}} {{:db/id {} :p/tag \"z\"}}]'::TEXT)",
            e, e
        ))
        .expect("add");
        // Three tags now.
        let cnt = Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/tag')",
            e
        ))
        .expect("q")
        .expect("NULL");
        assert_eq!(cnt, 3);

        // Retract one.
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :p/tag \"y\"]]'::TEXT)",
            e
        ))
        .expect("retract");
        let tags: Vec<String> = {
            let raw = Spi::get_one::<String>(&format!(
                "SELECT string_agg(v, ',' ORDER BY v) FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/tag')", e
            )).expect("q").expect("NULL");
            raw.split(',').map(|s| s.to_string()).collect()
        };
        assert_eq!(
            tags,
            vec!["x".to_string(), "z".to_string()],
            "y retracted, x and z remain"
        );
        verify_clean();
    }

    /// Retract-then-reassert within the projection ends with the value live.
    #[pg_test]
    fn pg_test_proj_retract_then_reassert() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/tag \"x\"}]'::TEXT)")
            .expect("tx");
        let e = email_eid("a@x.io");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {} :p/tag \"x\"]]'::TEXT)",
            e
        ))
        .expect("retract");
        // After retract, gone from projection.
        let after_retract = Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/tag') AND v='x'", e
        )).expect("q").expect("NULL");
        assert_eq!(after_retract, 0, "retracted tag must leave the projection");
        verify_clean();

        // Re-assert.
        Spi::run(&format!(
            "SELECT edn_t('[{{:db/id {} :p/tag \"x\"}}]'::TEXT)",
            e
        ))
        .expect("reassert");
        let after_reassert = Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM mentat.current_text WHERE e={} AND a=mentat.attr_id(':p/tag') AND v='x'", e
        )).expect("q").expect("NULL");
        assert_eq!(
            after_reassert, 1,
            "re-asserted tag must return to the projection"
        );
        verify_clean();
    }

    /// retractEntity clears all of an entity's projection rows.
    #[pg_test]
    fn pg_test_proj_retract_entity() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/name \"Alice\" :p/age 30 :p/tag \"x\"}]'::TEXT)").expect("tx");
        let e = email_eid("a@x.io");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retractEntity {}]]'::TEXT)",
            e
        ))
        .expect("retractEntity");

        for tbl in ["current_text", "current_long"] {
            let cnt = Spi::get_one::<i64>(&format!(
                "SELECT count(*) FROM mentat.{} WHERE e={}",
                tbl, e
            ))
            .expect("q")
            .expect("NULL");
            assert_eq!(cnt, 0, "{} must be empty after retractEntity", tbl);
        }
        verify_clean();
    }

    /// txInstant is mirrored into current_instant and is queryable.
    #[pg_test]
    fn pg_test_proj_txinstant_present() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\"}]'::TEXT)").expect("tx");
        // Every transaction's txInstant datom should be in current_instant.
        let txinstant_rows =
            Spi::get_one::<i64>("SELECT count(*) FROM mentat.current_instant WHERE a = 50")
                .expect("q")
                .expect("NULL");
        assert!(txinstant_rows >= 1, "txInstant datoms must be projected");
        verify_clean();
    }

    /// rebuild_current_projection reproduces the incrementally-maintained
    /// state exactly (verify stays 0 after a rebuild).
    #[pg_test]
    fn pg_test_proj_rebuild_idempotent() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/name \"Alice\" :p/tag \"x\" :p/tag \"y\"}]'::TEXT)").expect("tx");
        let e = email_eid("a@x.io");
        Spi::run(&format!(
            "SELECT edn_t('[{{:db/id {} :p/name \"Alyce\"}}]'::TEXT)",
            e
        ))
        .expect("replace");

        let before = Spi::get_one::<i64>("SELECT count(*) FROM mentat.current_text")
            .expect("q")
            .expect("NULL");
        Spi::get_one::<i64>("SELECT mentat.rebuild_current_projection(0)").expect("rebuild");
        let after = Spi::get_one::<i64>("SELECT count(*) FROM mentat.current_text")
            .expect("q")
            .expect("NULL");
        assert_eq!(before, after, "rebuild must reproduce the same row count");
        verify_clean();
    }

    /// A deliberately corrupted projection is detected by verify and healed
    /// by rebuild.
    #[pg_test]
    fn pg_test_proj_verify_detects_drift() {
        setup();
        schema();
        Spi::run("SELECT edn_t('[{:db/id \"p\" :p/email \"a@x.io\" :p/name \"Alice\"}]'::TEXT)")
            .expect("tx");
        verify_clean();

        // Corrupt: delete a projection row that the log says is live.
        Spi::run("DELETE FROM mentat.current_text WHERE v = 'Alice'").expect("corrupt");
        let m = Spi::get_one::<i64>("SELECT mentat.verify_current_projection(0)")
            .expect("verify")
            .expect("NULL");
        assert!(m >= 1, "verify must detect the deleted row");

        // Heal.
        Spi::get_one::<i64>("SELECT mentat.rebuild_current_projection(0)").expect("rebuild");
        verify_clean();
    }

    /// 1.10.0: a value-bound lookup ([?e :p/email "x"], [?e :p/age 42]) is an
    /// index probe on the AVET index, not an AEV scan + `Filter: v = ...`.
    #[pg_test]
    fn pg_test_proj_value_lookup_uses_avet() {
        setup();
        schema();
        Spi::run(
            "SELECT edn_t('[' || string_agg(format('{:db/id \"p%s\" :p/email \"u%s@x.io\" :p/age %s}', g, g, g), ' ') || ']')
             FROM generate_series(1, 300) g",
        )
        .expect("tx");
        Spi::run("ANALYZE mentat.current_text").expect("analyze");
        Spi::run("ANALYZE mentat.current_long").expect("analyze");
        Spi::run("SET LOCAL enable_seqscan = off").expect("set");
        for (q, idx) in [
            (
                "[:find ?e :where [?e :p/email \"u7@x.io\"]]",
                "idx_current_text_avet",
            ),
            ("[:find ?e :where [?e :p/age 42]]", "idx_current_long_avet"),
        ] {
            let plan = Spi::get_one::<String>(&format!(
                "SELECT mentat_explain($q${q}$q$, '{{}}'::jsonb)->>'explain_plan'"
            ))
            .expect("explain")
            .expect("NULL");
            assert!(plan.contains(idx), "{q} should use {idx}, got:\n{plan}");
        }
        let n = Spi::get_one::<pgrx::JsonB>(
            "SELECT edn_q('[:find ?e :where [?e :p/email \"u7@x.io\"]]', '{}')",
        )
        .expect("q")
        .expect("NULL");
        assert_eq!(
            n.0["results"].as_array().map(|a| a.len()),
            Some(1),
            "one match: {:?}",
            n.0
        );
    }

    /// 1.18.0: since S reads the projection rows with tx > S; with few rows
    /// after S that is a probe on the TX index, not a scan of every live
    /// datom of the attribute (issue #364 follow-up).
    #[pg_test]
    fn pg_test_proj_since_uses_the_tx_index() {
        setup();
        schema();
        Spi::run(
            "SELECT edn_t('[' || string_agg(format('{:db/id \"p%s\" :p/age %s :p/name \"n%s\"}', g, g, g), ' ') || ']')
             FROM generate_series(1, 2000) g",
        )
        .expect("tx");
        let s = Spi::get_one::<i64>("SELECT max(tx) FROM mentat.transactions")
            .expect("max tx")
            .expect("NULL");
        Spi::run("SELECT edn_t('[{:db/id \"q\" :p/age 7 :p/name \"late\"}]')").expect("tx2");
        Spi::run("ANALYZE mentat.current_long").expect("analyze");
        Spi::run("ANALYZE mentat.current_text").expect("analyze");
        for (q, idx) in [
            ("[:find ?e ?v :where [?e :p/age ?v]]", "idx_current_long_tx"),
            (
                "[:find ?e ?a :where [?e ?a \"late\"]]",
                "idx_current_text_tx",
            ),
        ] {
            let plan = Spi::get_one::<String>(&format!(
                "SELECT mentat_explain($q${q}$q$, '{{\"since\": {s}}}'::jsonb)->>'explain_plan'"
            ))
            .expect("explain")
            .expect("NULL");
            assert!(plan.contains(idx), "{q} should use {idx}, got:\n{plan}");
        }
        let n = Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT edn_q('[:find ?v :where [_ :p/age ?v]]', '{{\"since\": {s}}}')"
        ))
        .expect("q")
        .expect("NULL");
        assert_eq!(n.0["results"], serde_json::json!([[7]]), "{:?}", n.0);
    }

    /// The 1.17.2 -> 1.18.0 upgrade creates the same TX indexes as a fresh
    /// install.
    #[test]
    fn upgrade_creates_the_projection_tx_indexes() {
        let fresh = include_str!("../sql/24_current_projection.sql");
        let upgrade = include_str!("../sql/pg_mentat--1.17.2--1.18.0.sql");
        let tx_lines = |sql: &'static str| -> Vec<&'static str> {
            sql.lines()
                .filter(|l| l.starts_with("CREATE INDEX") && l.contains("_tx "))
                .collect()
        };
        assert_eq!(tx_lines(fresh).len(), 9);
        assert_eq!(tx_lines(fresh), tx_lines(upgrade));
    }

    /// The SQL helpers that answer for the current state read the projection,
    /// not the append-only log with `added = true`, which keeps replaced and
    /// retracted values (Refs #374): export_edn / export_all_edn exported
    /// them, retract_entity re-retracted them (and failed), attribute_values,
    /// entity_attrs and lookup_by_ident listed them, mentat_query_stats
    /// counted them, mentat_excise refused an entity a retracted reference
    /// had pointed at, and an [fts:lang=...] tag removed from :db/doc still
    /// set the fulltext language.
    #[pg_test]
    fn pg_test_proj_sql_helpers_read_current_state() {
        setup();
        schema();
        Spi::run(
            "SELECT edn_t('[{:db/id \"a\" :p/email \"a@x.io\" :p/name \"Ann\" :p/age 1 :p/tag [\"t1\" \"t2\"]}
                            {:db/id \"b\" :p/email \"b@x.io\" :p/name \"Bob\"}]'::TEXT)",
        )
        .expect("tx");
        let (a, b) = (email_eid("a@x.io"), email_eid("b@x.io"));
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {a} :p/name \"Anna\"] [:db/add {a} :p/age 2] \
                            [:db/retract {a} :p/tag \"t1\"] [:db/add {b} :p/friend {a}]]'::TEXT)"
        ))
        .expect("replace");
        Spi::run(&format!(
            "SELECT edn_t('[[:db/retract {b} :p/friend {a}] [:db/retract {b} :p/name \"Bob\"]]'::TEXT)"
        ))
        .expect("retract");
        let text = |sql: &str| -> String {
            Spi::get_one::<String>(sql)
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                .unwrap_or_default()
        };
        let json = |sql: &str| -> serde_json::Value {
            Spi::get_one::<pgrx::JsonB>(sql)
                .unwrap_or_else(|e| panic!("{sql}: {e}"))
                .map(|j| j.0)
                .unwrap_or_default()
        };

        let export = text(&format!("SELECT mentat.export_edn(ARRAY[{a}, {b}])"));
        for gone in ["\"Ann\"", ":p/age 1", "\"t1\"", "\"Bob\"", ":p/friend"] {
            assert!(!export.contains(gone), "export has {gone}: {export}");
        }
        for live in ["\"Anna\"", ":p/age 2", "\"t2\""] {
            assert!(export.contains(live), "export lacks {live}: {export}");
        }
        let all = text("SELECT mentat.export_all_edn()");
        assert!(
            !all.contains("\"Ann\"") && !all.contains("\"Bob\""),
            "{all}"
        );

        let mut names: Vec<String> = json("SELECT mentat.attribute_values(':p/name')")
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap_or("").to_string())
            .collect();
        names.sort();
        assert_eq!(names, vec!["Anna"]);
        let attrs = json(&format!("SELECT mentat.entity_attrs({b})"));
        assert_eq!(attrs, serde_json::json!([":p/email"]), "{attrs}");
        assert_eq!(
            Spi::get_one::<i64>("SELECT mentat.lookup_by_ident(':p/name', 'Ann')").unwrap(),
            None
        );
        assert_eq!(
            Spi::get_one::<i64>("SELECT mentat.lookup_by_ident(':p/name', 'Anna')").unwrap(),
            Some(a)
        );
        let live = Spi::get_one::<i64>("SELECT count(*) FROM mentat.current_datoms")
            .unwrap()
            .unwrap();
        let stats = json("SELECT mentat_query_stats()");
        assert_eq!(stats["database_stats"]["total_datoms"], live, "{stats}");

        // retract_entity retracts b's live datoms, once each.
        let n = Spi::get_one::<i64>(&format!("SELECT mentat.retract_entity({b})"))
            .unwrap()
            .unwrap();
        assert_eq!(n, 1, "only :p/email is live on b");

        // A removed [fts:lang=...] tag no longer sets the language.
        Spi::run(
            "SELECT edn_t('[{:db/ident :p/body :db/valueType :db.type/string \
               :db/cardinality :db.cardinality/one :db/fulltext true \
               :db/doc \"Body [fts:lang=german]\"}]'::TEXT)",
        )
        .expect("body");
        Spi::run("SELECT edn_t('[[:db/add :p/body :db/doc \"Body\"]]'::TEXT)").expect("doc");
        let sql = json(
            "SELECT mentat_query_sql('[:find ?e :where [(fulltext $ :p/body \"haus\") [[?e ?v]]]]', '{}'::jsonb)",
        )["sql"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            sql.contains("'english'") && !sql.contains("'german'"),
            "{sql}"
        );
        verify_clean();

        // b no longer refers to a: excising a is not refused.
        Spi::run("UPDATE mentat.partitions SET allow_excision = true").expect("allow");
        let out = text(&format!("SELECT mentat_excise(ARRAY[{a}]::BIGINT[])"));
        assert!(out.contains("\"excised\":1"), "{out}");
    }

    /// The default store's SQL helpers that answer for the current state
    /// (datom_text_like, datom_long_between, datom_ref_in, datom_text_values,
    /// datom_ref_values, current(e, a), attribute_health's live_datoms and the
    /// store_overview view) read the projection (#395): the log keeps a
    /// replaced or retracted value's added = true row. After a card-one
    /// replace and a retraction each returns only the live value, on the
    /// install source and on the 1.18.0 -> 1.19.0 upgrade's definitions.
    #[pg_test]
    fn pg_test_proj_datom_helpers_and_current_read_live_values() {
        setup();
        schema();
        Spi::run(
            "SELECT edn_t('[{:db/id \"a\" :p/email \"a@x.io\" :p/name \"Ann\" :p/age 1 \
                            :p/tag [\"t1\" \"t2\"]}
                            {:db/id \"b\" :p/email \"b@x.io\" :p/name \"Bob\" :p/age 5 :p/friend \"a\"}
                            {:db/id \"c\" :p/email \"c@x.io\"}]'::TEXT)",
        )
        .expect("tx");
        let (a, b, c) = (
            email_eid("a@x.io"),
            email_eid("b@x.io"),
            email_eid("c@x.io"),
        );
        // a: card-one replace of name and age, a card-many value retracted.
        // b: name and age retracted, friend a replaced by c.
        Spi::run(&format!(
            "SELECT edn_t('[[:db/add {a} :p/name \"Anna\"] [:db/add {a} :p/age 2] \
                            [:db/retract {a} :p/tag \"t1\"] [:db/retract {b} :p/name \"Bob\"] \
                            [:db/retract {b} :p/age 5] [:db/retract {b} :p/friend {a}] \
                            [:db/add {b} :p/friend {c}]]'::TEXT)"
        ))
        .expect("replace and retract");
        let col = |sql: &str| -> Vec<String> {
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
        let upgrade = include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql");
        let upgrade = &upgrade[upgrade.find("-- === pg-views ===").expect("section")..];
        let upgrade = &upgrade[..upgrade
            .find("CREATE FUNCTION mentat.\"_store_virtual_tables\"")
            .unwrap()];
        for round in ["install", "upgrade"] {
            if round == "upgrade" {
                // Outside an extension script, CREATE OR REPLACE of a member
                // is allowed.
                Spi::run(upgrade).expect("the upgrade's definitions");
            }
            let n = |sql: &str| -> Vec<String> { col(&format!("SELECT x::TEXT FROM {sql}")) };
            assert_eq!(
                n("mentat.datom_text_like(':p/name', '%') AS t(e, x, tx)"),
                vec!["Anna"],
                "{round}"
            );
            assert_eq!(
                n("mentat.datom_long_between(':p/age', 0, 10) AS t(e, x, tx)"),
                vec!["2"],
                "{round}"
            );
            assert_eq!(
                n(&format!(
                    "mentat.datom_ref_in(':p/friend', ARRAY[{a}, {c}]) AS t(x, v, tx)"
                )),
                vec![b.to_string()],
                "{round}"
            );
            assert_eq!(
                n(&format!(
                    "mentat.datom_ref_in(':p/friend', ARRAY[{a}, {c}]) AS t(e, x, tx)"
                )),
                vec![c.to_string()],
                "{round}"
            );
            assert_eq!(
                n(&format!(
                    "mentat.datom_text_values({a}, ':p/tag') AS t(x, tx)"
                )),
                vec!["t2"],
                "{round}"
            );
            assert_eq!(
                n(&format!(
                    "mentat.datom_ref_values({b}, ':p/friend') AS t(x, tx)"
                )),
                vec![c.to_string()],
                "{round}"
            );
            for (e, attr, want) in [
                (a, ":p/name", Some("Anna".to_string())),
                (a, ":p/age", Some("2".to_string())),
                (b, ":p/name", None),
                (b, ":p/age", None),
                (b, ":p/friend", Some(c.to_string())),
            ] {
                let by_ident =
                    Spi::get_one::<String>(&format!("SELECT mentat.current({e}, '{attr}')"))
                        .unwrap();
                assert_eq!(by_ident, want, "{round}: current({e}, {attr})");
            }
            // An attribute the schema doesn't type: current() probes every
            // table. A value only the log has (as a replaced one is) is not
            // current.
            Spi::run(&format!(
                "INSERT INTO mentat.datoms_text_new (store_id, e, a, v, tx, added) \
                 VALUES (0, {a}, 999999, 'logged', 1, true) ON CONFLICT DO NOTHING"
            ))
            .expect("log row");
            assert_eq!(
                Spi::get_one::<String>(&format!("SELECT mentat.current({a}, 999999::BIGINT)"))
                    .unwrap(),
                None,
                "{round}"
            );
            Spi::run("DELETE FROM mentat.datoms_text_new WHERE a = 999999").expect("cleanup");
            let live = |attr: &str| -> i64 {
                Spi::get_one::<i64>(&format!(
                    "SELECT live_datoms FROM mentat.attribute_health() WHERE attr_ident = '{attr}'"
                ))
                .unwrap()
                .unwrap()
            };
            assert_eq!(live(":p/name"), 1, "{round}");
            assert_eq!(live(":p/tag"), 1, "{round}");
            let total = Spi::get_one::<i64>(
                "SELECT total_datoms FROM mentat.store_overview WHERE store_name = 'default'",
            )
            .unwrap()
            .unwrap();
            let current = Spi::get_one::<i64>("SELECT count(*) FROM mentat.current_datoms")
                .unwrap()
                .unwrap();
            assert_eq!(total, current, "{round}: store_overview counts live datoms");
            let text = Spi::get_one::<i64>(
                "SELECT text_datoms FROM mentat.store_overview WHERE store_name = 'default'",
            )
            .unwrap()
            .unwrap();
            let current_text =
                Spi::get_one::<i64>("SELECT count(*) FROM mentat.current_text WHERE store_id = 0")
                    .unwrap()
                    .unwrap();
            assert_eq!(text, current_text, "{round}");
        }
        verify_clean();
    }

    // With enable_multi_tenant_rls(true) the log tables were filtered by
    // store_id but the current_<type> projections were not, and current-time
    // reads (edn_q, current(), datom_text_like) read the projections: a
    // tenant saw another store's current values. Each read now shows a
    // tenant what the log shows it. Also the 1.19.0 upgrade's block, which
    // arms the projections where the log is armed.
    //
    // In a database of its own (dblink): enabling RLS takes ACCESS EXCLUSIVE
    // on the shared tables, which deadlocks the tests running beside it.
    #[pg_test]
    fn pg_test_proj_rls_covers_the_projection() {
        setup();
        let available: i64 =
            Spi::get_one("SELECT count(*) FROM pg_available_extensions WHERE name = 'dblink'")
                .unwrap()
                .unwrap();
        if available == 0 {
            return;
        }
        Spi::run("CREATE EXTENSION IF NOT EXISTS dblink").expect("dblink");
        let conn = |db: &str| -> String {
            Spi::get_one::<String>(&format!(
                "SELECT pg_catalog.format('dbname=%s port=%s host=%s', '{db}', current_setting('port'), \
                   split_part(current_setting('unix_socket_directories'), ',', 1))"
            ))
            .unwrap()
            .unwrap()
        };
        let on = |c: &str, sql: &str| {
            Spi::run_with_args("SELECT dblink_exec($1, $2)", &[c.into(), sql.into()])
                .unwrap_or_else(|e| panic!("{c}: {sql}: {e}"))
        };
        let get = |sql: &str| -> String {
            Spi::get_one_with_args::<String>(
                "SELECT r FROM dblink('pr_db', $1) AS t(r TEXT)",
                &[format!("SELECT ({sql})::TEXT").into()],
            )
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_default()
        };
        Spi::run(&format!(
            "SELECT dblink_connect('pr_admin', '{}')",
            conn("postgres")
        ))
        .expect("connect");
        on("pr_admin", "DROP DATABASE IF EXISTS mentat_rls_proj");
        on("pr_admin", "DROP ROLE IF EXISTS proj_tenant");
        on("pr_admin", "CREATE DATABASE mentat_rls_proj");
        on(
            "pr_admin",
            "CREATE ROLE proj_tenant NOSUPERUSER NOBYPASSRLS",
        );
        Spi::run(&format!(
            "SELECT dblink_connect('pr_db', '{}')",
            conn("mentat_rls_proj")
        ))
        .expect("connect");
        on("pr_db", "CREATE EXTENSION pg_mentat");
        on("pr_db", "SET search_path TO mentat, public");
        get("bootstrap_schema()");
        get("edn_t('[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]')");
        let z = get("(edn_t('[{:db/id \"z\" :p/name \"Zed\"}]')::jsonb)->'tempids'->>'z'");
        on(
            "pr_db",
            "GRANT USAGE ON SCHEMA mentat TO proj_tenant; \
             GRANT SELECT ON ALL TABLES IN SCHEMA mentat TO proj_tenant",
        );
        let upgrade = include_str!("../sql/pg_mentat--1.18.0--1.19.0.sql");
        let block = &upgrade[upgrade.find("DO $rls$").unwrap()..];
        let block = &block[..block.find("$rls$;").unwrap() + "$rls$;".len()];
        let mut seen = Vec::new();
        for round in ["install", "upgrade"] {
            if round == "upgrade" {
                // What 1.18.0 had: the log armed, the projections without a policy.
                get("mentat.enable_multi_tenant_rls(false)");
                on(
                    "pr_db",
                    "DO $d$ DECLARE t TEXT; BEGIN FOREACH t IN ARRAY ARRAY['ref', 'long', 'text', 'double', \
                       'instant', 'keyword', 'uuid', 'bytes', 'boolean'] LOOP \
                       EXECUTE format('DROP POLICY store_isolation_current_%s ON mentat.current_%s', t, t); \
                       EXECUTE format('ALTER TABLE mentat.datoms_%s_new ENABLE ROW LEVEL SECURITY', t); \
                     END LOOP; END $d$",
                );
                on("pr_db", block);
            } else {
                assert_eq!(get("mentat.enable_multi_tenant_rls(true)"), "18");
            }
            for tenant in ["0", "1"] {
                on("pr_db", "SET ROLE proj_tenant");
                on(
                    "pr_db",
                    &format!("SET mentat.current_store_id = '{tenant}'"),
                );
                let row = [
                    get(&format!("(SELECT count(*) FROM mentat.datoms_text_new WHERE e = {z}) > 0")),
                    get(&format!("mentat.current({z}, ':p/name') IS NOT NULL")),
                    get(&format!("(SELECT count(*) FROM mentat.current_text WHERE e = {z}) > 0")),
                    get("(SELECT count(*) FROM mentat.datom_text_like(':p/name', 'Zed')) > 0"),
                    get("edn_q('[:find ?n :where [?e :p/name ?n]]', '{}'::jsonb)::TEXT LIKE '%Zed%'"),
                ];
                on("pr_db", "RESET ROLE");
                on("pr_db", "RESET mentat.current_store_id");
                seen.push(format!("{round} {tenant}: {}", row.join(" ")));
            }
        }
        let off = get("mentat.enable_multi_tenant_rls(false)");
        // The block takes ACCESS EXCLUSIVE locks held to commit, so it comes
        // after the upgrade's index builds, and a table already in the state
        // it wants is left alone: a second run (here, with RLS off on all
        // eighteen) locks nothing.
        let after = |tag: &str| upgrade.find(tag).unwrap() < upgrade.find("DO $rls$").unwrap();
        assert!(
            after("DO $ftsidx$")
                && after("DO $views$")
                && after("CREATE INDEX IF NOT EXISTS idx_current_text_fts"),
            "$rls$ comes after the index builds"
        );
        on("pr_db", "BEGIN");
        on("pr_db", block);
        let locked = get(
            "SELECT count(*) FROM pg_locks WHERE pid = pg_backend_pid() AND mode = 'AccessExclusiveLock' \
               AND relation IN (SELECT oid FROM pg_class WHERE relnamespace = 'mentat'::regnamespace \
                                   AND relname LIKE 'current\\_%')",
        );
        on("pr_db", "COMMIT");
        Spi::run("SELECT dblink_disconnect('pr_db')").expect("close");
        on("pr_admin", "DROP DATABASE mentat_rls_proj");
        on("pr_admin", "DROP ROLE proj_tenant");
        Spi::run("SELECT dblink_disconnect('pr_admin')").expect("close");
        // log, current(), current_text, datom_text_like, edn_q
        assert_eq!(
            seen,
            vec![
                "install 0: true true true true true",
                "install 1: false false false false false",
                "upgrade 0: true true true true true",
                "upgrade 1: false false false false false",
            ]
        );
        assert_eq!(off, "18");
        assert_eq!(locked, "0", "a second run of $rls$ locks nothing");
    }
}
