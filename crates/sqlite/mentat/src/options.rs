//! The JSON query options (`{"inputs": [...], "asOf": T, "since": T}`) live in
//! `mentat_transaction::options`, shared with the DuckDB backend; re-exported
//! here as `mentat::options_from_json`.

pub use mentat_transaction::options::options_from_json;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IntoResult, Queryable, Store};
    use crate::{QueryInputs, TemporalBound};
    use public_traits::errors::Result;
    use serde_json::json;
    use serde_json::Value as Json;

    fn store() -> Store {
        let mut s = Store::open("").unwrap();
        s.transact(
            r#"[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :p/friend :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]"#,
        )
        .unwrap();
        s
    }

    fn opts(s: &Store, q: &str, o: Json) -> Result<(QueryInputs, Option<TemporalBound>)> {
        options_from_json(&s.conn().current_schema(), q, &o)
    }

    #[test]
    fn test_none_and_temporal() {
        let s = store();
        let q = "[:find ?e :where [?e :p/name _]]";
        for o in [Json::Null, json!({})] {
            let (i, t) = opts(&s, q, o).unwrap();
            assert_eq!(t, None);
            s.q_once(q, i).unwrap();
        }
        assert_eq!(
            opts(&s, q, json!({"asOf": 7})).unwrap().1,
            Some(TemporalBound::AsOf(7))
        );
        assert_eq!(
            opts(&s, q, json!({"since": 9})).unwrap().1,
            Some(TemporalBound::Since(9))
        );
    }

    #[test]
    fn test_errors() {
        let s = store();
        let q = "[:find ?e :in ?n :where [?e :p/name ?n]]";
        for (o, want) in [
            (json!([1]), "must be a JSON object"),
            (json!({"asOf": "x"}), "integer tx id"),
            (json!({"asOf": 1, "since": 2}), "mutually exclusive"),
            (json!({"bogus": 1}), "unknown option \"bogus\""),
            (json!({"inputs": 3}), "must be an array"),
            (
                json!({"inputs": []}),
                "1 :in binding(s) but \"inputs\" has 0",
            ),
            (json!({"inputs": [null]}), "unsupported input value"),
            (json!({"inputs": [":not a kw"]}), "invalid keyword"),
        ] {
            let e = opts(&s, q, o.clone()).err().unwrap().to_string();
            assert!(e.contains(want), "{o}: {e}");
        }
        let e = opts(
            &s,
            "[:find ?e :in [?e ...] :where [?e :p/name _]]",
            json!({"inputs": [1]}),
        )
        .err()
        .unwrap()
        .to_string();
        assert!(e.contains("must be a JSON array"), "{e}");
    }

    #[test]
    fn test_inputs_bind_and_run() {
        let mut s = store();
        let r = s
            .transact(r#"[{:db/id "a" :p/name "Alice" :p/friend "b"} {:db/id "b" :p/name "Bob"}]"#)
            .unwrap();
        let (a, b) = (r.tempids["a"], r.tempids["b"]);
        let run = |q: &str, o: Json| {
            let (i, _) = opts(&s, q, o).unwrap();
            s.q_once(q, i).into_rel_result().unwrap().row_count()
        };
        // Scalar string; an integer in entity position is a ref.
        assert_eq!(
            run(
                "[:find ?e :in ?n :where [?e :p/name ?n]]",
                json!({"inputs": ["Alice"]})
            ),
            1
        );
        assert_eq!(
            run(
                "[:find ?n :in ?e :where [?e :p/name ?n]]",
                json!({"inputs": [a]})
            ),
            1
        );
        // Ref-valued attribute's value place is a ref too.
        assert_eq!(
            run(
                "[:find ?e :in ?f :where [?e :p/friend ?f]]",
                json!({"inputs": [b]})
            ),
            1
        );
        // Collection, tuple, relation (with a placeholder), and mixed.
        assert_eq!(
            run(
                "[:find ?e :in [?n ...] :where [?e :p/name ?n]]",
                json!({"inputs": [["Alice", "Bob", "Zed"]]})
            ),
            2
        );
        assert_eq!(
            run(
                "[:find ?e :in [?e ?n] :where [?e :p/name ?n]]",
                json!({"inputs": [[a, "Alice"]]})
            ),
            1
        );
        assert_eq!(
            run(
                "[:find ?e :in [[?e _ ?n]] :where [?e :p/name ?n]]",
                json!({"inputs": [[[a, 0, "Alice"], [b, 0, "Nope"]]]})
            ),
            1
        );
        assert_eq!(
            run(
                "[:find ?e :in $ ?f [?n ...] :where [?e :p/name ?n] [?e :p/friend ?f]]",
                json!({"inputs": [b, ["Alice", "Bob"]]})
            ),
            1
        );
    }
}
