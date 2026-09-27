// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! The JSON query options every front end shares (pg_mentat's `edn_q`, the
//! SQLite and DuckDB extensions, the CLI's `.q`):
//!
//! ```json
//! {"inputs": [v1, v2, ...], "asOf": T, "since": T}
//! ```
//!
//! `inputs` has one element per `:in` binding form (a source var like `$` is
//! not one). A scalar `?x` takes a JSON value; `[?x ...]` an array;
//! `[?a ?b]` an array (one row); `[[?a ?b]]` an array of arrays. A JSON
//! integer bound to an entity variable becomes a ref, else a long; `":kw"` is
//! a keyword; any other string a string. `asOf` and `since` are exclusive.

use serde_json::Value as Json;

use core_traits::{TypedValue, ValueType};
use edn::query::{
    Binding as InBinding, OrWhereClause, PatternNonValuePlace, PatternValuePlace, Variable,
    VariableOrPlaceholder, WhereClause,
};
use mentat_core::{HasSchema, Schema};
use public_traits::errors::{MentatError, Result};

use crate::{QueryInputs, TemporalBound};

fn err<T>(msg: String) -> Result<T> {
    Err(MentatError::BadQueryOptions(msg))
}

/// Parse `options` for `query`: the `:in` values and an optional temporal
/// bound. `null`, `{}` and a missing `inputs` mean none.
pub fn options_from_json(
    schema: &Schema,
    query: &str,
    options: &Json,
) -> Result<(QueryInputs, Option<TemporalBound>)> {
    let obj = match options {
        Json::Null => return Ok((QueryInputs::default(), None)),
        Json::Object(o) => o,
        _ => return err("options must be a JSON object".into()),
    };
    let tx = |k: &str, v: &Json| match v.as_i64() {
        Some(t) => Ok(t),
        None => err(format!("\"{k}\" must be an integer tx id, got {v}")),
    };
    let (mut inputs, mut as_of, mut since) = (None, None, None);
    for (k, v) in obj {
        match k.as_str() {
            "inputs" => match v {
                Json::Array(a) => inputs = Some(a.clone()),
                other => return err(format!("\"inputs\" must be an array, got {other}")),
            },
            "asOf" => as_of = Some(tx(k, v)?),
            "since" => since = Some(tx(k, v)?),
            other => {
                return err(format!(
                    "unknown option \"{other}\" (expected inputs, asOf, since)"
                ))
            }
        }
    }
    let temporal = match (as_of, since) {
        (Some(_), Some(_)) => return err("\"asOf\" and \"since\" are mutually exclusive".into()),
        (Some(t), None) => Some(TemporalBound::AsOf(t)),
        (None, Some(t)) => Some(TemporalBound::Since(t)),
        (None, None) => None,
    };
    let inputs = match inputs {
        Some(vals) => inputs_from_json(schema, query, vals)?,
        None => QueryInputs::default(),
    };
    Ok((inputs, temporal))
}

/// Variables that stand for entities: the entity/tx place of a pattern, or the
/// value place of a `:db.type/ref` attribute. A JSON integer bound to one
/// becomes `TypedValue::Ref` (a `Long` in entity position is a type mismatch,
/// i.e. a silently empty result).
// ponytail: walks patterns/or/not only, not rule bodies; add rules if needed.
fn ref_vars(clauses: &[WhereClause], schema: &Schema, out: &mut Vec<Variable>) {
    for c in clauses {
        match c {
            WhereClause::Pattern(p) => {
                for place in [&p.entity, &p.tx] {
                    if let PatternNonValuePlace::Variable(v) = place {
                        out.push(v.clone());
                    }
                }
                if let (PatternNonValuePlace::Ident(a), PatternValuePlace::Variable(v)) =
                    (&p.attribute, &p.value)
                {
                    if schema
                        .attribute_for_ident(a)
                        .is_some_and(|(attr, _)| attr.value_type == ValueType::Ref)
                    {
                        out.push(v.clone());
                    }
                }
            }
            WhereClause::NotJoin(n) => ref_vars(&n.clauses, schema, out),
            WhereClause::OrJoin(o) => {
                for oc in &o.clauses {
                    match oc {
                        OrWhereClause::Clause(c) => ref_vars(std::slice::from_ref(c), schema, out),
                        OrWhereClause::And(cs) => ref_vars(cs, schema, out),
                    }
                }
            }
            _ => {}
        }
    }
}

/// JSON -> TypedValue, mirroring pg_mentat's `bind_input_value`: integer ->
/// Long (Ref for an entity var), float -> Double, bool -> Boolean, `":kw"` ->
/// Keyword, other string -> String.
fn json_to_typed(j: &Json, is_ref: bool) -> Result<TypedValue> {
    Ok(match j {
        Json::Bool(b) => TypedValue::Boolean(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if is_ref => TypedValue::Ref(i),
            (Some(i), _) => TypedValue::Long(i),
            (None, Some(f)) => TypedValue::from(f),
            _ => return err(format!("unrepresentable number {n}")),
        },
        Json::String(s) if s.starts_with(':') => {
            match edn::parse::value(s).map(|v| v.without_spans()) {
                Ok(edn::Value::Keyword(k)) => TypedValue::from(k),
                _ => return err(format!("invalid keyword input {s}")),
            }
        }
        Json::String(s) => TypedValue::typed_string(s),
        other => return err(format!("unsupported input value {other}")),
    })
}

/// `QueryInputs` from the positional `inputs` array.
fn inputs_from_json(schema: &Schema, query: &str, vals: Vec<Json>) -> Result<QueryInputs> {
    let parsed = edn::parse::parse_query(query)?;
    if vals.len() != parsed.in_bindings.len() {
        return err(format!(
            "query has {} :in binding(s) but \"inputs\" has {} value(s)",
            parsed.in_bindings.len(),
            vals.len()
        ));
    }
    let mut refs = Vec::new();
    ref_vars(&parsed.where_clauses, schema, &mut refs);
    let tv = |v: &Variable, j: &Json| json_to_typed(j, refs.contains(v));
    let array = |j: Json, what: &str| match j {
        Json::Array(a) => Ok(a),
        other => err(format!("{what} input must be a JSON array, got {other}")),
    };
    // One tuple row; `_` placeholder columns are dropped.
    let row = |vps: &[VariableOrPlaceholder], vals: Vec<Json>| {
        if vals.len() != vps.len() {
            return err(format!(
                "tuple needs {} value(s), got {}",
                vps.len(),
                vals.len()
            ));
        }
        let mut vars = Vec::new();
        let mut out = Vec::new();
        for (vp, j) in vps.iter().zip(vals) {
            if let VariableOrPlaceholder::Variable(v) = vp {
                out.push(tv(v, &j)?);
                vars.push(v.clone());
            }
        }
        Ok((vars, out))
    };

    let mut scalars = Vec::new();
    let mut non_scalar = Vec::new();
    for (b, j) in parsed.in_bindings.iter().zip(vals) {
        match b {
            InBinding::BindScalar(v) => scalars.push((v.clone(), tv(v, &j)?)),
            InBinding::BindColl(v) => {
                let xs = array(j, "collection [?x ...]")?;
                let xs = xs.iter().map(|x| tv(v, x)).collect::<Result<_>>()?;
                non_scalar.push(QueryInputs::with_collection(v.clone(), xs));
            }
            InBinding::BindTuple(vps) => {
                let (vars, xs) = row(vps, array(j, "tuple [?a ?b]")?)?;
                non_scalar.push(QueryInputs::with_tuple(vars, xs));
            }
            InBinding::BindRel(vps) => {
                let mut vars: Vec<Variable> =
                    vps.iter().filter_map(|vp| vp.clone().into_var()).collect();
                let mut rows = Vec::new();
                for r in array(j, "relation [[?a ?b]]")? {
                    let (vs, xs) = row(vps, array(r, "relation row")?)?;
                    vars = vs;
                    rows.push(xs);
                }
                non_scalar.push(QueryInputs::with_relation(vars, rows));
            }
        }
    }
    // Scalars plus any number of collection/tuple/relation bindings, merged.
    let mut out = QueryInputs::with_value_sequence(scalars);
    for ns in non_scalar {
        out = match out.merge(ns) {
            Ok(o) => o,
            Err(e) => return err(format!("inputs: {e}")),
        };
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IntoResult, Queryable, Store};
    use serde_json::json;

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
