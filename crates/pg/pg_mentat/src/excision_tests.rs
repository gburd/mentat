// mentat_excise (#396): excision removed an entity's rows from the datom log
// only. The current-state projection, which every current-time read serves
// from, kept them, so edn_q, pull and entity() still returned the excised
// values. The fulltext copy table kept its text.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use pgrx::prelude::*;

    fn text(sql: &str) -> String {
        Spi::get_one::<String>(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .unwrap_or_default()
    }

    fn json(sql: &str) -> serde_json::Value {
        Spi::get_one::<pgrx::JsonB>(sql)
            .unwrap_or_else(|e| panic!("{sql}: {e}"))
            .map(|j| j.0)
            .unwrap_or_default()
    }

    /// After mentat_excise(X), nothing reads X's values: q (current, as-of
    /// and a [?e ?a ?v ?tx ?added] history pattern), pull, entity, export,
    /// fulltext search, the projection and the fulltext copy table. Another
    /// entity's data stays. On the default store and on a named store.
    #[pg_test]
    fn excised_entity_is_gone_everywhere() {
        crate::ensure_extension_loaded();
        Spi::run("SELECT bootstrap_schema()").expect("bootstrap_schema");
        Spi::run("UPDATE mentat.partitions SET allow_excision = true").expect("allow");
        Spi::run("SELECT create_store('exc_s')").expect("create_store");
        Spi::run("UPDATE mentat_exc_s.partitions SET allow_excision = true").expect("allow");
        for store in ["default", "exc_s"] {
            let sch = if store == "default" {
                "mentat".to_string()
            } else {
                format!("mentat_{store}")
            };
            let t = |edn: &str| -> serde_json::Value {
                let out = text(&format!(
                    "SELECT t('{store}', '{}')",
                    edn.replace('\'', "''")
                ));
                serde_json::from_str(&out).expect("tx report")
            };
            let q = |query: &str, inputs: &str| -> serde_json::Value {
                json(&format!(
                    "SELECT q('{store}', '{}', '{inputs}'::jsonb)",
                    query.replace('\'', "''")
                ))["result"]
                    .clone()
            };
            t("[{:db/ident :x/name :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one :db/unique :db.unique/identity} \
                {:db/ident :x/bio :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one :db/fulltext true} \
                {:db/ident :x/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one} \
                {:db/ident :x/tag :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}]");
            let r = t(
                "[{:db/id \"x\" :x/name \"Xavier\" :x/bio \"secret zebra memoir\" \
                         :x/n 1 :x/tag [:k/a :k/b]} \
                        {:db/id \"y\" :x/name \"Yolanda\" :x/bio \"public giraffe notes\" :x/n 2}]",
            );
            let (x, y) = (
                r["tempids"]["x"].as_i64().expect("x"),
                r["tempids"]["y"].as_i64().expect("y"),
            );
            let t_x = r["db-after"]["basis-t"].as_i64().expect("basis-t");
            t(&format!(
                "[[:db/add {x} :x/n 3] [:db/retract {x} :x/tag :k/b]]"
            ));

            let out = text(&format!(
                "SELECT mentat_excise(ARRAY[{x}]::BIGINT[], '{store}')"
            ));
            assert!(out.contains("\"excised\":1"), "{store}: {out}");

            let none = serde_json::json!([]);
            let by_e = format!("[:find ?a ?v :where [{x} ?a ?v]]");
            assert_eq!(q(&by_e, "{}"), none, "{store}: q");
            assert_eq!(
                q(&by_e, &format!("{{\"asOf\": {t_x}}}")),
                none,
                "{store}: as-of"
            );
            let hist = format!("[:find ?a ?v ?tx ?op :where [{x} ?a ?v ?tx ?op]]");
            assert_eq!(q(&hist, "{}"), none, "{store}: history pattern");
            assert_eq!(
                q("[:find ?e :where [?e :x/name \"Xavier\"]]", "{}"),
                none,
                "{store}: by value"
            );
            assert_eq!(
                q(
                    "[:find ?e :where [(fulltext $ :x/bio \"zebra\") [[?e ?v]]]]",
                    "{}"
                ),
                none,
                "{store}: fulltext"
            );
            let p = json(&format!("SELECT pull('{store}', '[*]', {x})"));
            assert!(
                p.get(":x/name").is_none() && p.get(":x/n").is_none(),
                "{store}: {p}"
            );
            let ent = json(&format!("SELECT entity('{store}', {x})"));
            assert_eq!(ent, serde_json::json!({":db/id": x}), "{store}: {ent}");
            if store == "default" {
                let export = text(&format!("SELECT mentat.export_edn(ARRAY[{x}])"));
                assert!(!export.contains("Xavier"), "{export}");
                assert!(!text("SELECT mentat.export_all_edn()").contains("zebra"));
            }
            let leftover = Spi::get_one::<i64>(&format!(
                "SELECT (SELECT count(*) FROM {sch}.current_datoms WHERE e = {x}) \
                      + (SELECT count(*) FROM {sch}.datoms WHERE e = {x}) \
                      + (SELECT count(*) FROM {sch}.fulltext WHERE text_value LIKE '%zebra%')"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(leftover, 0, "{store}: rows left for the excised entity");

            // The other entity is untouched.
            assert_eq!(
                q(
                    "[:find ?e . :where [(fulltext $ :x/bio \"giraffe\") [[?e ?v]]]]",
                    "{}"
                ),
                serde_json::json!(y),
                "{store}"
            );
            let p = json(&format!("SELECT pull('{store}', '[:x/name :x/n]', {y})"));
            assert_eq!(p[":x/name"], "Yolanda", "{store}: {p}");

            // An enum entity's ident goes with it.
            let r = t("[{:db/id \"k\" :db/ident :color/mauve}]");
            let k = r["tempids"]["k"].as_i64().expect("k");
            text(&format!(
                "SELECT mentat_excise(ARRAY[{k}]::BIGINT[], '{store}')"
            ));
            assert_eq!(
                q("[:find ?e . :where [?e :db/ident :color/mauve]]", "{}"),
                serde_json::Value::Null,
                "{store}: ident"
            );
            let idents = Spi::get_one::<i64>(&format!(
                "SELECT count(*) FROM {sch}.idents WHERE ident = ':color/mauve'"
            ))
            .unwrap()
            .unwrap();
            assert_eq!(idents, 0, "{store}");
        }
        assert_eq!(
            Spi::get_one::<i64>("SELECT mentat.verify_current_projection(0)")
                .unwrap()
                .unwrap(),
            0
        );
        Spi::run("SELECT drop_store('exc_s')").expect("drop_store");
    }
}
