// Mozilla-era issues fixed in 1.18.0: pg_mentat agrees with the embedded
// engine on each (see docs/design/mozilla-issue-triage-2026-10-21.md).

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn setup() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema failed");
        Spi::run(
            "CREATE OR REPLACE FUNCTION mentat._test_error_msg(stmt TEXT) RETURNS TEXT
             LANGUAGE plpgsql AS $$
             BEGIN
                 EXECUTE stmt;
                 RETURN ''::TEXT;
             EXCEPTION WHEN OTHERS THEN
                 RETURN SQLERRM;
             END;
             $$",
        )
        .expect("create error_msg helper");
    }

    /// SQLERRM of `sql` run in a subtransaction ("" when it succeeds).
    fn error_message(sql: &str) -> String {
        let escaped = sql.replace('\'', "''");
        Spi::get_one::<String>(&format!("SELECT mentat._test_error_msg('{}')", escaped))
            .expect("error_msg call")
            .unwrap_or_default()
    }

    fn transact(edn: &str) -> String {
        error_message(&format!(
            "SELECT edn_t('{}'::TEXT)",
            edn.replace('\'', "''")
        ))
    }

    // #286: a new attribute needs :db/ident, :db/valueType and :db/cardinality.
    #[pg_test]
    fn new_attribute_needs_minimal_set() {
        setup();
        let e = transact(
            r#"[[:db/add "p" :db/ident :mi/prop] [:db/add "p" :db/valueType :db.type/string]]"#,
        );
        assert!(e.contains("missing :db/cardinality"), "{e}");
        let e = transact(
            r#"[[:db/add "p" :db/ident :mi/prop] [:db/add "p" :db/cardinality :db.cardinality/one]]"#,
        );
        assert!(e.contains("missing :db/valueType"), "{e}");
        let e = transact(
            r#"[[:db/add "p" :db/valueType :db.type/string] [:db/add "p" :db/cardinality :db.cardinality/one]]"#,
        );
        assert!(e.contains("missing :db/ident"), "{e}");

        // A bare ident is fine; so is the rest of the set in a later transaction.
        assert_eq!(transact(r#"[[:db/add "p" :db/ident :mi/prop]]"#), "");
        assert_eq!(
            transact("[[:db/add :mi/prop :db/valueType :db.type/string] [:db/add :mi/prop :db/cardinality :db.cardinality/one]]"),
            ""
        );
        // Altering an existing attribute needs only the changed piece.
        assert_eq!(transact("[[:db/add :mi/prop :db/index true]]"), "");
        assert_eq!(
            transact("[[:db/add :mi/prop :db/cardinality :db.cardinality/many]]"),
            ""
        );
        let row = Spi::get_one::<String>(
            "SELECT value_type::text || ' ' || cardinality::text || ' ' || indexed::text \
             FROM mentat.schema WHERE ident = ':mi/prop'",
        )
        .expect("schema row")
        .expect("non-NULL");
        assert_eq!(row, "string many true");
        // The type cannot change; re-asserting it can.
        let e = transact("[[:db/add :mi/prop :db/valueType :db.type/long]]");
        assert!(
            e.contains("Schema alteration for existing attribute :mi/prop is not valid"),
            "{e}"
        );
        assert_eq!(
            transact("[[:db/add :mi/prop :db/valueType :db.type/string]]"),
            ""
        );
    }

    // #286: an alteration the existing data violates is rejected, as on the embedded engine.
    #[pg_test]
    fn alteration_checks_existing_data() {
        setup();
        tx("[{:db/ident :mv/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
             {:db/ident :mv/code :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]");
        let a = tx(r#"[{:db/id "a" :mv/tags ["x" "y"]}]"#)["tempids"]["a"]
            .as_i64()
            .unwrap();
        tx(r#"[{:mv/code "same"} {:mv/code "same"}]"#);
        let row = |ident: &str| {
            Spi::get_one::<String>(&format!(
                "SELECT cardinality::text || ' ' || coalesce(unique_constraint::text, 'none') \
                 FROM mentat.schema WHERE ident = '{ident}'"
            ))
            .expect("schema row")
            .expect("non-NULL")
        };

        let e = transact("[[:db/add :mv/tags :db/cardinality :db.cardinality/one]]");
        assert!(
            e.contains("Cannot alter schema attribute :mv/tags to be :db.cardinality/one"),
            "{e}"
        );
        assert_eq!(row(":mv/tags"), "many none");
        let e = transact(
            "[[:db/add :mv/code :db/unique :db.unique/identity] [:db/add :mv/code :db/index true]]",
        );
        assert!(
            e.contains("Cannot alter schema attribute :mv/code to be :db.unique/identity"),
            "{e}"
        );
        let e = transact(
            "[[:db/add :mv/code :db/unique :db.unique/value] [:db/add :mv/code :db/index true]]",
        );
        assert!(
            e.contains("Cannot alter schema attribute :mv/code to be :db.unique/value"),
            "{e}"
        );
        assert_eq!(row(":mv/code"), "one none");

        // Without the violation, both alterations go through.
        assert_eq!(transact(&format!("[[:db/retract {a} :mv/tags \"x\"]]")), "");
        assert_eq!(
            transact("[[:db/add :mv/tags :db/cardinality :db.cardinality/one]]"),
            ""
        );
        assert_eq!(row(":mv/tags"), "one none");
        tx(r#"[{:mv/code "other"}]"#);
        let dup = Spi::get_one::<i64>(
            "SELECT min(e) FROM mentat.current_text t JOIN mentat.idents i ON i.entid = t.a \
             WHERE i.ident = ':mv/code' AND t.v = 'same'",
        )
        .expect("dup")
        .expect("non-NULL");
        assert_eq!(
            transact(&format!("[[:db/retract {dup} :mv/code \"same\"]]")),
            ""
        );
        assert_eq!(
            transact("[[:db/add :mv/code :db/unique :db.unique/identity] [:db/add :mv/code :db/index true]]"),
            ""
        );
        assert_eq!(row(":mv/code"), "one identity");
    }

    fn tx(edn: &str) -> serde_json::Value {
        let raw = Spi::get_one::<String>(&format!(
            "SELECT edn_t('{}'::TEXT)::TEXT",
            edn.replace('\'', "''")
        ))
        .expect("transact")
        .expect("non-NULL report");
        serde_json::from_str(&raw).expect("parse report")
    }

    fn q(query: &str) -> serde_json::Value {
        let raw = Spi::get_one::<String>(&format!(
            "SELECT edn_q('{}'::TEXT, '{{}}'::jsonb)::TEXT",
            query.replace('\'', "''")
        ))
        .expect("query ran")
        .expect("non-NULL result");
        serde_json::from_str(&raw).expect("parse result")
    }

    // #249: min/max over refs, including transaction ids (agrees with the embedded engine).
    #[pg_test]
    fn min_max_ref() {
        setup();
        tx(
            "[{:db/ident :mi/r :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
             {:db/ident :mi/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let first = tx(r#"[{:db/id "a" :mi/n 1}]"#);
        let a = first["tempids"]["a"].as_i64().unwrap();
        let last = tx(r#"[{:db/id "b" :mi/n 2}]"#);
        let b = last["tempids"]["b"].as_i64().unwrap();
        tx(&format!(
            "[[:db/add {a} :mi/r {a}] [:db/add {a} :mi/r {b}]]"
        ));
        let first_tx = first["db-after"]["basis-t"].as_i64().unwrap();
        let last_tx = last["db-after"]["basis-t"].as_i64().unwrap();

        assert_eq!(
            q("[:find (max ?tx) . :where [_ :mi/n _ ?tx]]")["result"],
            last_tx
        );
        assert_eq!(
            q("[:find (min ?tx) . :where [_ :mi/n _ ?tx]]")["result"],
            first_tx
        );
        assert_eq!(
            q("[:find (max ?v) . :where [_ :mi/r ?v]]")["result"],
            a.max(b)
        );
        assert_eq!(
            q("[:find (min ?v) . :where [_ :mi/r ?v]]")["result"],
            a.min(b)
        );
        assert_eq!(
            q("[:find ?e (max ?v) :where [?e :mi/r ?v]]")["results"],
            serde_json::json!([[a, a.max(b)]])
        );
    }

    fn coll(query: &str) -> Vec<String> {
        let mut v: Vec<String> = q(query)["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{query}: no result array"))
            .iter()
            .map(|x| {
                x.as_str()
                    .map(str::to_string)
                    .unwrap_or_else(|| x.to_string())
            })
            .collect();
        v.sort();
        v
    }

    // #267: like / ilike / starts-with? / ends-with? / includes?, as on the embedded engine.
    #[pg_test]
    fn string_predicates() {
        setup();
        tx("[{:db/ident :ms/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
             {:db/ident :ms/pre :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
             {:db/ident :ms/kw :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}]");
        tx(
            r#"[{:ms/name "Alice" :ms/pre "Al" :ms/kw :k/alpha} {:ms/name "alfred" :ms/pre "xx"}
               {:ms/name "50%" :ms/pre "5"} {:ms/name "500"} {:ms/name "a*b?[c]"} {:ms/name "café" :ms/pre ""}]"#,
        );
        let q = |pred: &str| coll(&format!("[:find [?s ...] :where [_ :ms/name ?s] [{pred}]]"));
        assert_eq!(q(r#"(like ?s "Al%")"#), vec!["Alice"]);
        assert_eq!(q(r#"(ilike ?s "AL%")"#), vec!["Alice", "alfred"]);
        assert_eq!(q(r#"(like ?s "50\\%")"#), vec!["50%"]);
        assert_eq!(q(r#"(like ?s "a*b?[c]")"#), vec!["a*b?[c]"]);
        assert_eq!(q(r#"(starts-with? ?s "Al")"#), vec!["Alice"]);
        assert_eq!(q(r#"(ends-with? ?s "é")"#), vec!["café"]);
        assert_eq!(q(r#"(includes? ?s "*b?")"#), vec!["a*b?[c]"]);
        assert_eq!(q(r#"(ends-with? ?s "")"#).len(), 6);
        assert_eq!(
            coll("[:find [?s ...] :where [?e :ms/name ?s] [?e :ms/pre ?p] [(starts-with? ?s ?p)]]"),
            vec!["50%", "Alice", "café"]
        );
        assert_eq!(
            coll(r#"[:find [?v ...] :where [_ :ms/kw ?v] [(like ?v "k/%")]]"#),
            vec![":k/alpha"]
        );
        let e = error_message(
            r#"SELECT edn_q('[:find ?s :where [_ :ms/name ?s] [(includes? ?s)]]'::TEXT, '{}'::jsonb)"#,
        );
        assert!(e.contains("requires exactly 2 arguments, got 1"), "{e}");

        // A value of unknown type matches only if it is a string, as on the embedded engine:
        // the long 17 and the keyword :k/17 don't match "1%" as text.
        tx(
            "[{:db/ident :ms/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
               :db/unique :db.unique/identity :db/index true}
              {:db/ident :ms/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        tx(r#"[{:ms/id "E" :ms/name "17a" :ms/n 17 :ms/kw :k/17}]"#);
        for pred in [
            r#"(starts-with? ?v "1")"#,
            r#"(like ?v "1%")"#,
            r#"(ilike ?v "%7%")"#,
            r#"(includes? ?v "7")"#,
            r#"(ends-with? ?v "7a")"#,
        ] {
            assert_eq!(
                coll(&format!(
                    r#"[:find [?v ...] :where [?e :ms/id "E"] [?e ?a ?v] [{pred}]]"#
                )),
                vec!["17a"],
                "{pred}"
            );
        }
    }

    // #281: the tx report counts asserted and retracted datoms, as on the embedded engine.
    #[pg_test]
    fn tx_report_counts() {
        setup();
        let counts = |r: &serde_json::Value| {
            (
                r["datoms_asserted"].as_i64().unwrap(),
                r["datoms_retracted"].as_i64().unwrap(),
            )
        };
        let r = tx("[{:db/ident :mc/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                     {:db/ident :mc/m :db/valueType :db.type/long :db/cardinality :db.cardinality/many}]");
        assert_eq!(counts(&r), (6, 0));
        let r = tx(r#"[[:db/add "a" :mc/n 1] [:db/add "a" :mc/m 1] [:db/add "a" :mc/m 2]]"#);
        assert_eq!(counts(&r), (3, 0));
        let a = r["tempids"]["a"].as_i64().unwrap();
        for (t, want) in [
            (
                format!("[[:db/add {a} :mc/n 1] [:db/add {a} :mc/m 2]]"),
                (0, 0),
            ),
            ("[]".to_string(), (0, 0)),
            (format!("[[:db/add {a} :mc/n 2]]"), (1, 1)),
            (
                format!("[[:db/add {a} :mc/m 3] [:db/retract {a} :mc/m 1]]"),
                (1, 1),
            ),
            (format!("[[:db/retractEntity {a}]]"), (0, 3)),
        ] {
            assert_eq!(counts(&tx(&t)), want, "{t}");
        }

        // tx-data lists what the counts count, as in Datomic: the retraction a
        // replacement implies, and no no-op re-assertion. A :db/noHistory replacement
        // retracts the old value too, though its history is not kept; the embedded
        // engine counts it the same way.
        tx(
            "[{:db/ident :mc/h :db/valueType :db.type/long :db/cardinality :db.cardinality/one
               :db/noHistory true}]",
        );
        let b = tx(r#"[{:db/id "b" :mc/n 1 :mc/h 1}]"#)["tempids"]["b"]
            .as_i64()
            .unwrap();
        let data = |r: &serde_json::Value| -> Vec<(i64, bool)> {
            let mut d: Vec<_> = r["tx-data"].as_array().unwrap()[1..]
                .iter()
                .map(|d| (d[2].as_i64().unwrap(), d[4].as_bool().unwrap()))
                .collect();
            d.sort();
            d
        };
        for t in [
            format!("[[:db/add {b} :mc/n 2]]"),
            format!("[[:db/add {b} :mc/h 2]]"),
        ] {
            let r = tx(&t);
            assert_eq!(counts(&r), (1, 1), "{t}");
            assert_eq!(data(&r), [(1, false), (2, true)], "{t}");
        }
        let r = tx(&format!("[[:db/add {b} :mc/n 2] [:db/add {b} :mc/h 3]]"));
        assert_eq!(counts(&r), (1, 1));
        assert_eq!(data(&r), [(2, false), (3, true)]);
        // The noHistory replacement's retraction, also written out (or by :db/cas), is
        // one retraction, as on the embedded engine.
        for (t, old, new) in [
            (
                format!("[[:db/retract {b} :mc/h 3] [:db/add {b} :mc/h 4]]"),
                3,
                4,
            ),
            (format!("[[:db/cas {b} :mc/h 4 5]]"), 4, 5),
        ] {
            let r = tx(&t);
            assert_eq!(counts(&r), (1, 1), "{t}");
            assert_eq!(data(&r), [(old, false), (new, true)], "{t}");
        }
        assert_eq!(
            coll(&format!("[:find [?v ...] :where [{b} :mc/h ?v]]")),
            vec!["5"]
        );
    }

    // #362: in one transaction, a retraction of a live value of each type is applied and one
    // of an absent value is dropped. (The liveness check is one query per value type.)
    #[pg_test]
    fn retractions_of_every_value_type() {
        setup();
        let types = [
            ("ref", "{r}", ":db/ident"),
            ("boolean", "true", "false"),
            ("long", "7", "8"),
            ("double", "0.1", "0.2"),
            (
                "instant",
                r#"#inst "2020-01-01T00:00:00.000001Z""#,
                r#"#inst "2021-01-01T00:00:00Z""#,
            ),
            ("string", r#""it's""#, r#""its""#),
            ("keyword", ":k/a", ":k/b"),
            (
                "uuid",
                r#"#uuid "550e8400-e29b-41d4-a716-446655440000""#,
                r#"#uuid "550e8400-e29b-41d4-a716-446655440001""#,
            ),
            ("bytes", "#bytes 0a0b", "#bytes 0c"),
        ];
        let schema: String = types
            .iter()
            .map(|(t, ..)| {
                format!("{{:db/ident :mt/{t} :db/valueType :db.type/{t} :db/cardinality :db.cardinality/many}}")
            })
            .collect();
        tx(&format!("[{schema}]"));
        let r = tx(r#"[{:db/id "r" :db/doc "r"}]"#)["tempids"]["r"]
            .as_i64()
            .unwrap();
        let vals = |i: usize| -> Vec<String> {
            types
                .iter()
                .map(|t| [t.1, t.2][i].replace("{r}", &r.to_string()))
                .collect()
        };
        let (live, absent) = (vals(0), vals(1));
        let adds: String = types
            .iter()
            .zip(&live)
            .map(|((t, ..), v)| format!(":mt/{t} {v} "))
            .collect();
        let e = tx(&format!(r#"[{{:db/id "e" {adds}}}]"#))["tempids"]["e"]
            .as_i64()
            .unwrap();
        let mut t = String::from("[");
        for (((ty, ..), l), a) in types.iter().zip(&live).zip(&absent) {
            t.push_str(&format!(
                "[:db/retract {e} :mt/{ty} {l}] [:db/retract {e} :mt/{ty} {a}]"
            ));
        }
        t.push(']');
        let rep = tx(&t);
        assert_eq!(
            rep["datoms_retracted"].as_i64(),
            Some(types.len() as i64),
            "{rep}"
        );
        assert_eq!(
            coll(&format!("[:find [?a ...] :where [{e} ?a _]]")),
            Vec::<String>::new()
        );
    }

    // #368, #369: asserting and retracting the same datom in one transaction is a conflict,
    // in either order, as on the embedded engine (AddRetractConflict) and in Datomic.
    #[pg_test]
    fn add_and_retract_of_one_datom_conflict() {
        setup();
        tx(
            "[{:db/ident :mk/m :db/valueType :db.type/long :db/cardinality :db.cardinality/many}
             {:db/ident :mk/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let a = tx(r#"[{:db/id "a" :mk/m [1 2] :mk/n 5}]"#)["tempids"]["a"]
            .as_i64()
            .unwrap();
        for t in [
            format!("[[:db/retract {a} :mk/m 1] [:db/add {a} :mk/m 1]]"),
            format!("[[:db/add {a} :mk/m 5] [:db/retract {a} :mk/m 5]]"),
            format!("[[:db/add {a} :mk/n 7] [:db/retract {a} :mk/n 7]]"),
            format!("[[:db/retractEntity {a}] [:db/add {a} :mk/m 2]]"),
        ] {
            let e = transact(&t);
            assert!(e.contains("AddRetractConflict"), "{t}: {e}");
        }
        assert_eq!(
            coll(&format!("[:find [?v ...] :where [{a} :mk/m ?v]]")),
            vec!["1", "2"]
        );
        assert_eq!(
            coll(&format!("[:find [?v ...] :where [{a} :mk/n ?v]]")),
            vec!["5"]
        );
    }

    // [:db/cas e a v v] with v current is a no-op, as in Datomic (its cas expands to the
    // assertion alone) and on the embedded engine. It was an AddRetractConflict, and before
    // that deleted v. A mismatch still fails.
    #[pg_test]
    fn cas_to_the_same_value_is_a_no_op() {
        setup();
        tx(
            "[{:db/ident :mq/m :db/valueType :db.type/long :db/cardinality :db.cardinality/many}
             {:db/ident :mq/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let a = tx(r#"[{:db/id "a" :mq/m 1 :mq/n 5}]"#)["tempids"]["a"]
            .as_i64()
            .unwrap();
        for t in [
            format!("[[:db/cas {a} :mq/n 5 5]]"),
            format!("[[:db/cas {a} :mq/m 1 1]]"),
        ] {
            let r = tx(&t);
            assert_eq!(
                (
                    r["datoms_asserted"].as_i64(),
                    r["datoms_retracted"].as_i64()
                ),
                (Some(0), Some(0)),
                "{t}: {r}"
            );
        }
        assert_eq!(
            coll(&format!("[:find [?v ...] :where [{a} :mq/n ?v]]")),
            vec!["5"]
        );
        assert_eq!(
            coll(&format!("[:find [?v ...] :where [{a} :mq/m ?v]]")),
            vec!["1"]
        );
        assert!(transact(&format!("[[:db/cas {a} :mq/n 6 6]]")).contains("cas failed"));
    }

    // #152: `[a v]` lookup refs in entity, :db/id and value position, and a list of them
    // for a cardinality-many ref; a vector of values for any cardinality-many attribute.
    #[pg_test]
    fn vector_lookup_refs() {
        setup();
        tx(
            r#"[{:db/ident :ml/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                :db/unique :db.unique/identity :db/index true}
               {:db/ident :ml/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
               {:db/ident :ml/ref :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
               {:db/ident :ml/refs :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
               {:db/ident :ml/kws :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}]"#,
        );
        tx(r#"[{:ml/id "e1"} {:ml/id "e2"} {:ml/id "e3"}]"#);
        for t in [
            r#"[[:db/add [:ml/id "e1"] :ml/n 5]]"#,
            r#"[{:db/id [:ml/id "e2"] :ml/n 6}]"#,
            r#"[{:ml/id "x" :ml/ref [:ml/id "e1"]}]"#,
            r#"[{:ml/id "y" :ml/refs [[:ml/id "e1"] [:ml/id "e2"]]}]"#,
            r#"[[:db/add [:ml/id "y"] :ml/refs [:ml/id "e3"]]]"#,
            r#"[{:ml/id "z" :ml/kws [:k/a :k/b]}]"#,
        ] {
            assert_eq!(transact(t), "", "{t}");
        }
        assert_eq!(
            coll(r#"[:find [?b ...] :where [?x :ml/id "y"] [?x :ml/refs ?r] [?r :ml/id ?b]]"#),
            vec!["e1", "e2", "e3"]
        );
        assert_eq!(
            coll(r#"[:find [?b ...] :where [?x :ml/id "x"] [?x :ml/ref ?r] [?r :ml/id ?b]]"#),
            vec!["e1"]
        );
        assert_eq!(
            coll(r#"[:find [?k ...] :where [?x :ml/id "z"] [?x :ml/kws ?k]]"#).len(),
            2
        );
        assert_eq!(
            coll(r#"[:find [?n ...] :where [_ :ml/n ?n]]"#),
            vec!["5", "6"]
        );
    }

    // #152: only a :db.unique/identity attribute makes `[:attr v]` in a cardinality-many ref's
    // value a lookup ref, as on the embedded engine (DataScript's rule).
    #[pg_test]
    fn vector_of_idents_on_a_ref_list() {
        setup();
        tx(
            r#"[{:db/ident :mf/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                :db/unique :db.unique/value :db/index true}
               {:db/ident :mf/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                :db/unique :db.unique/identity :db/index true}
               {:db/ident :mf/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
               {:db/ident :mf/fields :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]"#,
        );
        let fields = |t: &str| {
            let f = tx(t)["tempids"]["f"].as_i64().unwrap();
            coll(&format!(
                "[:find [?i ...] :where [{f} :mf/fields ?r] [?r :db/ident ?i]]"
            ))
        };
        assert_eq!(
            fields(r#"[{:db/id "f" :mf/fields [:mf/email :mf/name]}]"#),
            [":mf/email", ":mf/name"]
        );
        assert_eq!(
            fields(r#"[{:db/id "f" :mf/fields [:db/ident :mf/name]}]"#),
            [":mf/name"]
        );
        assert_eq!(
            fields(r#"[{:db/id "f" :mf/fields [[:db/ident :mf/id] :mf/name]}]"#),
            [":mf/id", ":mf/name"]
        );
    }

    // #152: outside the ambiguous place (the whole value of a cardinality-many ref), any
    // :db/unique attribute makes `[a v]` a lookup ref, as in Datomic, and each vector in a
    // ref's list of values is a lookup ref (it made empty tempid entities).
    #[pg_test]
    fn unique_value_lookup_refs() {
        setup();
        tx(
            r#"[{:db/ident :mu/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                :db/unique :db.unique/value :db/index true}
               {:db/ident :mu/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
               {:db/ident :mu/boss :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
               {:db/ident :mu/likes :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]"#,
        );
        tx(r#"[{:mu/id "a"} {:mu/id "b"}]"#);
        for t in [
            r#"[{:db/id [:mu/id "a"] :mu/n 1}]"#,
            r#"[[:db/add [:mu/id "b"] :mu/n 2]]"#,
            r#"[{:mu/id "x" :mu/boss [:mu/id "a"]}]"#,
        ] {
            assert_eq!(transact(t), "", "{t}");
        }
        let r = tx(r#"[{:db/id "z" :mu/id "z" :mu/likes [[:mu/id "a"] [:mu/id "b"]]}]"#);
        assert_eq!(r["tempids"].as_object().unwrap().len(), 1, "{r}");
        assert_eq!(
            coll(r#"[:find [?b ...] :where [?e :mu/id "z"] [?e :mu/likes ?r] [?r :mu/id ?b]]"#),
            vec!["a", "b"]
        );
        assert_eq!(
            coll(r#"[:find [?b ...] :where [?e :mu/id "x"] [?e :mu/boss ?r] [?r :mu/id ?b]]"#),
            vec!["a"]
        );
        assert_eq!(coll("[:find [?n ...] :where [_ :mu/n ?n]]"), vec!["1", "2"]);
        // An inner vector that isn't a lookup ref that resolves is an error.
        for t in [
            r#"[{:db/id "w" :mu/likes [[:mu/id "nope"]]}]"#,
            r#"[{:db/id "w" :mu/likes [[:mu/n 1]]}]"#,
            r#"[{:db/id "w" :mu/likes [[:mu/id "a" "b"]]}]"#,
        ] {
            assert_ne!(transact(t), "", "{t}");
        }
    }

    // #375: an instant with a fractional second reads back exactly, so replacing it leaves
    // one live value and re-asserting it is a no-op. Pull and entity return the stored
    // microseconds (they rounded to the second, then added the fraction).
    #[pg_test]
    fn fractional_second_instants() {
        setup();
        tx("[{:db/ident :mi/t :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}]");
        let i = tx(r#"[{:db/id "i" :mi/t #inst "2020-01-01T00:00:00.700001Z"}]"#)["tempids"]["i"]
            .as_i64()
            .unwrap();
        let old = 1_577_836_800_700_001_i64;
        let pulled = || {
            let raw = Spi::get_one::<String>(&format!("SELECT mentat_pull('[:mi/t]', {i})::TEXT"))
                .expect("pull")
                .expect("non-NULL");
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()[":mi/t"].clone()
        };
        let entity = || {
            let raw = Spi::get_one::<String>(&format!("SELECT mentat_entity({i})::TEXT"))
                .expect("entity")
                .expect("non-NULL");
            serde_json::from_str::<serde_json::Value>(&raw).unwrap()[":mi/t"].clone()
        };
        assert_eq!(pulled(), serde_json::json!(old));
        assert_eq!(entity(), serde_json::json!(old));

        let r = tx(&format!(
            r#"[[:db/add {i} :mi/t #inst "2020-01-01T00:00:00.700001Z"]]"#
        ));
        assert_eq!(
            (
                r["datoms_asserted"].as_i64(),
                r["datoms_retracted"].as_i64()
            ),
            (Some(0), Some(0)),
            "{r}"
        );
        let r = tx(&format!(
            r#"[[:db/add {i} :mi/t #inst "2021-01-01T00:00:00Z"]]"#
        ));
        assert_eq!(
            (
                r["datoms_asserted"].as_i64(),
                r["datoms_retracted"].as_i64()
            ),
            (Some(1), Some(1))
        );
        let retracted = r["tx-data"].as_array().unwrap()[1..]
            .iter()
            .find(|d| d[4] == false)
            .map(|d| d[2]["v"].clone());
        assert_eq!(retracted, Some(serde_json::json!(old)), "{r}");
        let n = Spi::get_one::<i64>(&format!(
            "SELECT count(*) FROM mentat.current_instant WHERE e = {i}"
        ))
        .unwrap()
        .unwrap();
        assert_eq!(n, 1);
        assert_eq!(pulled(), serde_json::json!(1_609_459_200_000_000_i64));
    }

    // #375: an instant in any year 1..9999 is stored and read back to the microsecond on
    // every pg version. to_timestamp(double) lost 1 us outside ~1932..2100, and on pg13
    // (EXTRACT returns double) the read was off by up to 5 us far from 1970, so re-asserting
    // such a card-one value failed with "ON CONFLICT DO UPDATE command cannot affect row a
    // second time" and replacing it left two values.
    #[pg_test]
    fn instants_are_exact_in_every_year() {
        setup();
        tx(
            "[{:db/ident :mx/t :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}
             {:db/ident :mx/k :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        let cases = [
            ("0001-01-01T00:00:00.000001Z", -62_135_596_799_999_999_i64),
            ("1583-07-14T12:34:56.123457Z", -12_195_746_703_876_543),
            ("1847-09-09T15:47:02.481870Z", -3_859_776_777_518_130),
            ("1914-06-18T00:50:07.049709Z", -1_752_707_392_950_291),
            ("1969-12-31T23:59:59.999999Z", -1),
            ("2262-04-11T23:47:16.854775Z", 9_223_372_036_854_775),
            ("4268-06-28T23:42:12.524912Z", 72_533_403_732_524_912),
            ("9999-12-31T23:59:59.999999Z", 253_402_300_799_999_999),
        ];
        let json_of = |sql: &str| -> serde_json::Value {
            let raw = Spi::get_one::<String>(sql).expect(sql).expect("non-NULL");
            serde_json::from_str(&raw).unwrap()
        };
        let counts = |r: &serde_json::Value| {
            (
                r["datoms_asserted"].as_i64(),
                r["datoms_retracted"].as_i64(),
            )
        };
        for (k, (text, micros)) in cases.iter().enumerate() {
            let inst = format!(r#"#inst "{text}""#);
            let i = tx(&format!(r#"[{{:db/id "i" :mx/k {k} :mx/t {inst}}}]"#))["tempids"]["i"]
                .as_i64()
                .unwrap();
            let stored = Spi::get_one::<String>(&format!(
                "SELECT to_char(v AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') \
                 FROM mentat.current_instant WHERE e = {i}"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(stored, *text, "written");
            let want = serde_json::json!(micros);
            let found = q(&format!("[:find ?t . :where [{i} :mx/t ?t]]"));
            assert_eq!(found["result"], serde_json::json!(text), "q {text}");
            let on = q(&format!("[:find ?e . :where [?e :mx/t {inst}]]"));
            assert_eq!(on["result"], serde_json::json!(i), "q by value {text}");
            let pulled = json_of(&format!("SELECT mentat_pull('[:mx/t]', {i})::TEXT"));
            assert_eq!(pulled[":mx/t"], want, "pull {text}");
            let ent = json_of(&format!("SELECT mentat_entity({i})::TEXT"));
            assert_eq!(ent[":mx/t"], want, "entity {text}");
            let export = Spi::get_one::<String>(&format!("SELECT mentat.export_edn(ARRAY[{i}])"))
                .unwrap()
                .unwrap();
            assert!(
                export.contains(&format!("#inst {micros}")),
                "export {text}: {export}"
            );

            // Re-asserting the same value is a no-op; replacing it leaves one value.
            let r = tx(&format!("[[:db/add {i} :mx/t {inst}]]"));
            assert_eq!(counts(&r), (Some(0), Some(0)), "re-assert {text}: {r}");
            let r = tx(&format!(
                r#"[[:db/add {i} :mx/t #inst "2021-01-01T00:00:00Z"]]"#
            ));
            assert_eq!(counts(&r), (Some(1), Some(1)), "replace {text}: {r}");
            let retracted = r["tx-data"].as_array().unwrap()[1..]
                .iter()
                .find(|d| d[4] == false)
                .map(|d| d[2]["v"].clone());
            assert_eq!(retracted, Some(want.clone()), "replace {text}: {r}");
            let n = Spi::get_one::<i64>(&format!(
                "SELECT count(*) FROM mentat.current_instant WHERE e = {i}"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(n, 1, "replace {text}");
        }
    }

    // #179: a comparison over a value of unknown type applies to comparable values only.
    #[pg_test]
    fn inequality_over_untyped_values() {
        setup();
        tx(
            r#"[{:db/ident :mu/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                :db/unique :db.unique/identity :db/index true}
               {:db/ident :mu/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
               {:db/ident :mu/d :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
               {:db/ident :mu/b :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
               {:db/ident :mu/r :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
               {:db/ident :mu/t :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}]"#,
        );
        tx(
            r#"[{:db/id "x" :mu/id "e1" :mu/n 5 :mu/d 2.5 :mu/b true :mu/r "x"
                :mu/t #inst "2020-01-01T00:00:00Z"}]"#,
        );
        let r = q(r#"[:find [?v ...] :where [?e :mu/id "e1"] [?e ?a ?v] [(> ?v 1)]]"#);
        let mut got: Vec<String> = r["result"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.to_string())
            .collect();
        got.sort();
        assert_eq!(got, vec!["2.5", "5"]);
        let r = q(r#"[:find [?v ...] :where [?e :mu/id "e1"] [?e ?a ?v] [(<= ?v 2.5)]]"#);
        assert_eq!(r["result"].as_array().unwrap().len(), 1, "{r}");
        let r = q(
            r#"[:find [?v ...] :where [?e :mu/id "e1"] [?e ?a ?v] [(< ?v #inst "2021-01-01T00:00:00Z")]]"#,
        );
        assert_eq!(r["result"].as_array().unwrap().len(), 1, "{r}");
        let e = error_message(
            "SELECT edn_q('[:find ?v :where [_ ?a ?v] [(> ?v 4)]]'::TEXT, '{}'::jsonb)",
        );
        assert_eq!(e, "");
        // A typed attribute is unchanged.
        assert_eq!(
            coll("[:find [?v ...] :where [_ :mu/n ?v] [(> ?v 1)]]"),
            vec!["5"]
        );
        // Against a string, an untyped value still compares as text (no type error).
        let r = q(r#"[:find [?v ...] :where [?e :mu/id "e1"] [?e ?a ?v] [(< ?v "e2")]]"#);
        assert!(
            r["result"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("e1")),
            "{r}"
        );
        let e = error_message(
            r#"SELECT edn_q('[:find ?v :where [?e :mu/id ?s] [?e ?a ?v] [(< ?v ?s)]]'::TEXT, '{}'::jsonb)"#,
        );
        assert_eq!(e, "");
    }

    // #179 + #375: an instant constant compared with a value of unknown type is exact in
    // every year. Built with to_timestamp(double), it was 1 us off far from 1970, so `=` and
    // `<=` missed the stored value.
    #[pg_test]
    fn untyped_instant_comparison_is_exact() {
        setup();
        tx(
            r#"[{:db/ident :mw/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
               {:db/ident :mw/t :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}]"#,
        );
        tx(r#"[{:db/id "y" :mw/id "y1k" :mw/t #inst "1000-01-01T00:00:00.000001Z"}]"#);
        let found = |op: &str| {
            coll(&format!(
                r#"[:find [?id ...] :where [?e :mw/id ?id] [?e ?a ?v]
                    [({op} ?v #inst "1000-01-01T00:00:00.000001Z")]]"#
            ))
        };
        for op in ["<=", "=", ">="] {
            assert_eq!(found(op), vec!["y1k"], "{op}");
        }
        for op in ["<", ">", "!="] {
            assert!(found(op).is_empty(), "{op}");
        }
    }

    // #12: renaming a unique identity value and retracting the old value in the same
    // transaction logs one retraction of the old value, and the report counts one.
    #[pg_test]
    fn rename_and_retract_logs_one_retraction_row() {
        setup();
        tx(
            r#"[{:db/ident :mr/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                :db/unique :db.unique/identity :db/index true}]"#,
        );
        tx(r#"[{:mr/id "One"}]"#);
        let r = tx(
            r#"[[:db/add [:mr/id "One"] :mr/id "Two"] [:db/retract [:mr/id "One"] :mr/id "One"]]"#,
        );
        assert_eq!(
            (
                r["datoms_asserted"].as_i64(),
                r["datoms_retracted"].as_i64()
            ),
            (Some(1), Some(1)),
            "{r}"
        );
        let rows = Spi::get_one::<i64>(
            "SELECT count(*) FROM mentat.datoms d JOIN mentat.idents i ON i.entid = d.a \
             WHERE i.ident = ':mr/id' AND d.v_text = 'One' AND NOT d.added",
        )
        .expect("count")
        .unwrap_or_default();
        assert_eq!(rows, 1);
    }

    // #237: an aggregate over no rows is nil, for every find spec, as on the embedded engine
    // and DuckDB; a count is 0. A grouped aggregate has no groups, so no rows.
    #[pg_test]
    fn aggregates_over_no_rows_are_nil() {
        setup();
        tx(
            "[{:db/ident :ma/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
             {:db/ident :ma/d :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
             {:db/ident :ma/t :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}
             {:db/ident :ma/k :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        );
        tx("[{:ma/k 1}]");
        let empty = serde_json::json!([]);
        for agg in ["sum", "avg", "min", "max", "count", "count-distinct"] {
            let count = agg.starts_with("count");
            for attr in [":ma/n", ":ma/d", ":ma/t"] {
                if matches!(agg, "sum" | "avg") && attr == ":ma/t" {
                    continue;
                }
                let r = q(&format!("[:find ({agg} ?v) . :where [_ {attr} ?v]]"));
                let want = if count {
                    serde_json::json!(0)
                } else {
                    serde_json::Value::Null
                };
                assert_eq!(r["result"], want, "{agg} {attr} scalar: {r}");
                // :with takes the de-duplicating path.
                let r = q(&format!(
                    "[:find ({agg} ?v) . :with ?e :where [?e {attr} ?v]]"
                ));
                assert_eq!(r["result"], want, "{agg} {attr} with: {r}");
                let r = q(&format!("[:find [({agg} ?v) ...] :where [_ {attr} ?v]]"));
                let want = if count {
                    serde_json::json!([0])
                } else {
                    empty.clone()
                };
                assert_eq!(r["result"], want, "{agg} {attr} coll: {r}");
                let r = q(&format!("[:find [({agg} ?v)] :where [_ {attr} ?v]]"));
                let want = if count {
                    serde_json::json!([0])
                } else {
                    serde_json::Value::Null
                };
                assert_eq!(r["result"], want, "{agg} {attr} tuple: {r}");
                let r = q(&format!("[:find ({agg} ?v) :where [_ {attr} ?v]]"));
                let want = if count {
                    serde_json::json!(0)
                } else {
                    empty.clone()
                };
                assert_eq!(r["result"], want, "{agg} {attr} rel: {r}");
                let r = q(&format!(
                    "[:find ?k ({agg} ?v) :where [?e :ma/k ?k] [?e {attr} ?v]]"
                ));
                assert_eq!(r["result"], empty, "{agg} {attr} grouped rel: {r}");
            }
        }
        // With a nil aggregate in the find there is no row, even beside a count.
        assert!(q("[:find [(sum ?v) (count ?v)] :where [_ :ma/n ?v]]")["result"].is_null());
        // Over some rows they still aggregate.
        tx("[{:ma/n 5}]");
        assert_eq!(
            q("[:find [(sum ?v) (count ?v)] :where [_ :ma/n ?v]]")["result"],
            serde_json::json!([5, 1])
        );
    }
}
