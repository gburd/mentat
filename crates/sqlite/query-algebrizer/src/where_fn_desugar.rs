// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! `get-else` and `missing?` (issue #335), desugared into clauses the
//! algebrizer already knows, so the SQLite and DuckDB backends support them
//! without new SQL forms:
//!
//! - `[(missing? $ ?e :a)]` is `(not [?e :a _])`.
//! - `[(get-else $ ?e :a d) ?v]` is
//!   `(or-join [?e ?v] [?e :a ?v] (and D (not [?e :a _]) [(ground d) ?v]))`,
//!   where `D` is a copy of a sibling pattern that binds `?e`, with its other
//!   variables blanked. The second arm needs `D` because an or-join arm is
//!   algebrized on its own and must bind `?e` itself. `D` can match more
//!   entities than the full query does; the enclosing query joins the or-join
//!   on `?e`, so the result is the same.
//!
//! As in Datomic and pg_mentat, get-else on a cardinality-many attribute is
//! an error.

use std::collections::BTreeSet;

use edn::query::{
    Binding, FnArg, NotJoin, OrJoin, OrWhereClause, Pattern, PatternNonValuePlace,
    PatternValuePlace, PlainSymbol, Predicate, UnifyVars, Variable, WhereClause, WhereFn,
};
use mentat_core::HasSchema;
use query_algebrizer_traits::errors::{AlgebrizerError, Result};

use crate::Known;

pub(crate) fn desugar(
    clauses: Vec<WhereClause>,
    known: Known,
    in_vars: &BTreeSet<Variable>,
) -> Result<Vec<WhereClause>> {
    let siblings = clauses.clone();
    clauses
        .into_iter()
        .map(|c| desugar_clause(c, &siblings, known, in_vars))
        .collect()
}

fn desugar_clause(
    clause: WhereClause,
    siblings: &[WhereClause],
    known: Known,
    in_vars: &BTreeSet<Variable>,
) -> Result<WhereClause> {
    match clause {
        WhereClause::Pred(p) if p.operator.0.as_str() == "missing?" => missing(p),
        WhereClause::WhereFn(f) if f.operator.0.as_str() == "get-else" => {
            get_else(f, siblings, known, in_vars)
        }
        WhereClause::NotJoin(n) => {
            let body = desugar(n.clauses, known, in_vars)?;
            Ok(WhereClause::NotJoin(NotJoin::new(n.unify_vars, body)))
        }
        WhereClause::OrJoin(o) => {
            let OrJoin {
                unify_vars,
                clauses,
                ..
            } = o;
            let arms = clauses
                .into_iter()
                .map(|arm| match arm {
                    OrWhereClause::Clause(c) => {
                        let one = vec![c.clone()];
                        Ok(OrWhereClause::Clause(desugar_clause(
                            c, &one, known, in_vars,
                        )?))
                    }
                    OrWhereClause::And(cs) => Ok(OrWhereClause::And(desugar(cs, known, in_vars)?)),
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(WhereClause::OrJoin(OrJoin::new(unify_vars, arms)))
        }
        other => Ok(other),
    }
}

/// `[(missing? $ ?e :attr)]` -> `(not [?e :attr _])`.
fn missing(p: Predicate) -> Result<WhereClause> {
    let name = p.operator.clone();
    let (e, attr) = entity_and_attr(&name, &p.args)?;
    if p.args.len() != 3 {
        bail!(AlgebrizerError::InvalidNumberOfArguments(
            name,
            p.args.len(),
            3
        ));
    }
    Ok(not_has(&e, &attr))
}

fn get_else(
    f: WhereFn,
    siblings: &[WhereClause],
    known: Known,
    in_vars: &BTreeSet<Variable>,
) -> Result<WhereClause> {
    let name = f.operator.clone();
    if f.args.len() != 4 {
        bail!(AlgebrizerError::InvalidNumberOfArguments(
            name,
            f.args.len(),
            4
        ));
    }
    let (e, attr) = entity_and_attr(&name, &f.args)?;
    let v = match &f.binding {
        Binding::BindScalar(v) => v.clone(),
        _ => bail!(AlgebrizerError::InvalidArgument(
            name,
            "a scalar binding",
            4
        )),
    };
    let (attribute, _) = known
        .schema
        .attribute_for_ident(&attr)
        .ok_or_else(|| AlgebrizerError::UnrecognizedIdent(attr.to_string()))?;
    if attribute.multival {
        bail!(AlgebrizerError::InvalidArgument(
            name,
            "a cardinality-one attribute (get-else is not supported for cardinality-many)",
            2
        ));
    }

    // Arm 1: the entity has the attribute.
    let has = WhereClause::Pattern(pattern(
        PatternNonValuePlace::Variable(e.clone()),
        &attr,
        PatternValuePlace::Variable(v.clone()),
    ));
    // Arm 2: it doesn't; bind the default. The arm must bind ?e itself (from
    // a copy of a pattern that binds it), unless ?e is an :in scalar.
    let mut lacks = Vec::new();
    match domain_for(&e, siblings) {
        Some(d) => lacks.push(WhereClause::Pattern(d)),
        None if in_vars.contains(&e) => {}
        None => bail!(AlgebrizerError::UnboundVariable(e.name())),
    }
    lacks.push(not_has(&e, &attr));
    lacks.push(WhereClause::WhereFn(WhereFn {
        operator: PlainSymbol::plain("ground"),
        args: vec![f.args[3].clone()],
        binding: Binding::BindScalar(v.clone()),
    }));
    Ok(WhereClause::OrJoin(OrJoin::new(
        UnifyVars::Explicit([e, v].into_iter().collect()),
        vec![OrWhereClause::Clause(has), OrWhereClause::And(lacks)],
    )))
}

fn entity_and_attr(name: &PlainSymbol, args: &[FnArg]) -> Result<(Variable, edn::Keyword)> {
    if args.len() < 3 {
        bail!(AlgebrizerError::InvalidNumberOfArguments(
            name.clone(),
            args.len(),
            3
        ));
    }
    if !matches!(args[0], FnArg::SrcVar(_)) {
        bail!(AlgebrizerError::InvalidArgument(
            name.clone(),
            "source variable ($)",
            0
        ));
    }
    let e = match &args[1] {
        FnArg::Variable(v) => v.clone(),
        _ => bail!(AlgebrizerError::InvalidArgument(
            name.clone(),
            "an entity variable",
            1
        )),
    };
    let attr = match &args[2] {
        FnArg::IdentOrKeyword(k) => k.clone(),
        _ => bail!(AlgebrizerError::InvalidArgument(
            name.clone(),
            "an attribute keyword",
            2
        )),
    };
    Ok((e, attr))
}

fn pattern(e: PatternNonValuePlace, attr: &edn::Keyword, v: PatternValuePlace) -> Pattern {
    Pattern {
        source: None,
        entity: e,
        attribute: PatternNonValuePlace::Ident(attr.clone().into()),
        value: v,
        tx: PatternNonValuePlace::Placeholder,
        added: PatternNonValuePlace::Placeholder,
    }
}

fn not_has(e: &Variable, attr: &edn::Keyword) -> WhereClause {
    WhereClause::NotJoin(NotJoin::new(
        UnifyVars::Implicit,
        vec![WhereClause::Pattern(pattern(
            PatternNonValuePlace::Variable(e.clone()),
            attr,
            PatternValuePlace::Placeholder,
        ))],
    ))
}

/// A sibling pattern that binds `e`, with every other variable blanked so the
/// copy binds only `e`.
fn domain_for(e: &Variable, siblings: &[WhereClause]) -> Option<Pattern> {
    let nv = |p: &PatternNonValuePlace| match p {
        PatternNonValuePlace::Variable(x) if x != e => PatternNonValuePlace::Placeholder,
        other => other.clone(),
    };
    siblings.iter().find_map(|c| match c {
        WhereClause::Pattern(p) => {
            let binds = matches!(&p.entity, PatternNonValuePlace::Variable(x) if x == e)
                || matches!(&p.value, PatternValuePlace::Variable(x) if x == e)
                || matches!(&p.tx, PatternNonValuePlace::Variable(x) if x == e);
            binds.then(|| Pattern {
                source: p.source.clone(),
                entity: nv(&p.entity),
                attribute: nv(&p.attribute),
                value: match &p.value {
                    PatternValuePlace::Variable(x) if x != e => PatternValuePlace::Placeholder,
                    other => other.clone(),
                },
                tx: nv(&p.tx),
                added: nv(&p.added),
            })
        }
        _ => None,
    })
}
