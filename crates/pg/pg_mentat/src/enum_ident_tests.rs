// A keyword in a ref attribute's value place is an ident naming an entity
// (#416): Datomic's enum idiom, `[?e :c/color :color/red]`. pg_mentat compiled
// it to a keyword value (v_keyword), which a ref datom never has, so the
// pattern silently matched nothing. The embedded engine's tests/enum_idents.rs
// runs the same queries.

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
        tx(
            store,
            "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
              {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
              {:db/ident :c/kw :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}]",
        );
        tx(store, "[{:db/ident :color/red} {:db/ident :color/blue}]");
        tx(
            store,
            r#"[{:c/name "car" :c/color :color/red}
                {:c/name "sky" :c/color :color/blue}
                {:db/id "t" :c/kw :color/red}]"#,
        );
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

    fn q(store: &str, query: &str, opts: &str) -> serde_json::Value {
        Spi::get_one::<pgrx::JsonB>(&format!(
            "SELECT q('{store}', '{}', '{opts}'::jsonb)",
            query.replace('\'', "''")
        ))
        .unwrap_or_else(|e| panic!("{query}: {e}"))
        .map(|j| j.0)
        .unwrap_or(serde_json::Value::Null)
    }

    /// The sorted strings of a `[:find [?n ...]]` result.
    fn names(store: &str, query: &str, opts: &str) -> Vec<String> {
        let r = q(store, query, opts);
        let mut v: Vec<String> = r["result"]
            .as_array()
            .unwrap_or_else(|| panic!("{query}: {r}"))
            .iter()
            .map(|x| x.as_str().unwrap_or_default().to_string())
            .collect();
        v.sort();
        v
    }

    fn red_names(store: &str) {
        let q1 = "[:find [?n ...] :where [?e :c/color :color/red] [?e :c/name ?n]]";
        assert_eq!(names(store, q1, "{}"), ["car"]);
        // An unknown ident matches nothing, not an error.
        let q2 = "[:find [?n ...] :where [?e :c/color :color/green] [?e :c/name ?n]]";
        assert!(names(store, q2, "{}").is_empty());
        // The same keyword on a keyword attribute is a keyword value.
        let q3 = "[:find [?e ...] :where [?e :c/kw :color/red]]";
        assert_eq!(
            q(store, q3, "{}")["result"].as_array().map(Vec::len),
            Some(1)
        );
        // not / or / rule bodies.
        let q4 = "[:find [?n ...] :where [?e :c/name ?n] (not [?e :c/color :color/red])]";
        assert_eq!(names(store, q4, "{}"), ["sky"]);
        let q5 = "[:find [?n ...] :where [?e :c/name ?n] \
                  (or [?e :c/color :color/red] [?e :c/color :color/blue])]";
        assert_eq!(names(store, q5, "{}"), ["car", "sky"]);
        let q6 = "[:find [?n ...] :rules [[(red ?e) [?e :c/color :color/red]]] \
                  :where (red ?e) [?e :c/name ?n]]";
        assert_eq!(names(store, q6, "{}"), ["car"]);
        let q7 =
            "[:find [?n ...] :where [?e :c/name ?n] (not-join [?e] [?e :c/color :color/blue])]";
        assert_eq!(names(store, q7, "{}"), ["car"]);
    }

    #[pg_test]
    fn enum_ident_in_ref_value_place() {
        setup("default");
        red_names("default");
    }

    #[pg_test]
    fn enum_ident_in_ref_value_place_named_store() {
        setup("enum416");
        red_names("enum416");
    }

    /// `:in` inputs into a ref value place: a keyword is an ident there too.
    #[pg_test]
    fn enum_ident_as_input() {
        setup("default");
        let s = "default";
        let q1 = "[:find [?n ...] :in ?c :where [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(names(s, q1, r#"{"inputs": [":color/red"]}"#), ["car"]);
        assert!(names(s, q1, r#"{"inputs": [":color/green"]}"#).is_empty());
        let q2 = "[:find [?n ...] :in [?c ...] :where [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(
            names(s, q2, r#"{"inputs": [[":color/red", ":color/blue"]]}"#),
            ["car", "sky"]
        );
        assert_eq!(
            names(s, q2, r#"{"inputs": [[":color/green", ":color/blue"]]}"#),
            ["sky"]
        );
        let q3 = "[:find [?n ...] :in [?c ?n] :where [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(
            names(s, q3, r#"{"inputs": [[":color/red", "car"]]}"#),
            ["car"]
        );
        let q4 = "[:find ?n ?c :in [[?c ?n]] :where [?e :c/color ?c] [?e :c/name ?n]]";
        let r = q(
            s,
            q4,
            r#"{"inputs": [[[":color/red", "car"], [":color/red", "sky"]]]}"#,
        );
        assert_eq!(r["results"].as_array().map(Vec::len), Some(1), "{r}");
        // A keyword input into a keyword attribute is still a keyword value.
        let q5 = "[:find [?e ...] :in ?k :where [?e :c/kw ?k]]";
        let r = q(s, q5, r#"{"inputs": [":color/red"]}"#);
        assert_eq!(r["result"].as_array().map(Vec::len), Some(1), "{r}");
        // An :in input bound only inside a not.
        let q6 = "[:find [?n ...] :in ?c :where [?e :c/name ?n] (not [?e :c/color ?c])]";
        assert_eq!(names(s, q6, r#"{"inputs": [":color/red"]}"#), ["sky"]);
        // ground binds like an :in input.
        let q7 =
            "[:find [?n ...] :where [(ground :color/red) ?c] [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(names(s, q7, "{}"), ["car"]);
        let q8 = "[:find [?n ...] :where [(ground [:color/red :color/blue]) [?c ...]] \
                  [?e :c/color ?c] [?e :c/name ?n]]";
        assert_eq!(names(s, q8, "{}"), ["car", "sky"]);
        // ...and into a keyword attribute it is a keyword value.
        let q9 = "[:find [?e ...] :where [(ground :color/red) ?k] [?e :c/kw ?k]]";
        assert_eq!(q(s, q9, "{}")["result"].as_array().map(Vec::len), Some(1));
        let r = q(
            s,
            "[:find ?k . :where [?e :c/kw ?k] [(ground :color/red) ?k]]",
            "{}",
        );
        assert_eq!(r["result"], ":color/red", "{r}");
        let r = q(
            s,
            "[:find ?k . :where [?e :c/kw _] [(ground :color/red) ?k]]",
            "{}",
        );
        assert_eq!(r["result"], ":color/red", "{r}");
    }

    /// as-of / since / history read the ident the same way.
    #[pg_test]
    fn enum_ident_temporal() {
        setup("default");
        let s = "default";
        let before = q(s, "[:find (max ?tx) . :where [_ :c/name _ ?tx]]", "{}")["result"]
            .as_i64()
            .expect("tx");
        tx(s, r#"[{:c/name "rose" :c/color :color/red}]"#);
        let q1 = "[:find [?n ...] :where [?e :c/color :color/red] [?e :c/name ?n]]";
        assert_eq!(names(s, q1, &format!(r#"{{"asOf": {before}}}"#)), ["car"]);
        assert_eq!(names(s, q1, &format!(r#"{{"since": {before}}}"#)), ["rose"]);
        assert_eq!(names(s, q1, r#"{"history": true}"#), ["car", "rose"]);
        let q2 = "[:find [?n ...] :where [?e :c/color :color/red _ _] [?e :c/name ?n]]";
        assert_eq!(names(s, q2, "{}"), ["car", "rose"]);
    }

    /// Untyped value place (`?a` unknown): as on the embedded engine, the
    /// keyword is a keyword value only, not a ref to the ident's entity.
    #[pg_test]
    fn enum_ident_untyped_value_place_is_keyword() {
        setup("default");
        let r = q("default", "[:find ?e ?a :where [?e ?a :color/red]]", "{}");
        let rows = r["results"].as_array().cloned().unwrap_or_default();
        // The :db/ident datom of :color/red and the :c/kw value, not the car.
        assert_eq!(rows.len(), 2, "{r}");
        let car = q("default", "[:find ?e . :where [?e :c/name \"car\"]]", "{}")["result"].clone();
        assert!(rows.iter().all(|row| row[0] != car), "{r}");
    }
}
