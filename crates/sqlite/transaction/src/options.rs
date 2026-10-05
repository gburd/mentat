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

use mentat_query_algebrizer::{QueryInputs, TemporalBound};

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
