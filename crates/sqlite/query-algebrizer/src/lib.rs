// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

/// Return early with an error, converting via `From`. Replaces `failure::bail!`.
macro_rules! bail {
    ($e:expr) => {
        return ::std::result::Result::Err(::std::convert::From::from($e))
    };
}

extern crate core_traits;
extern crate edn;
extern crate mentat_core;
extern crate query_algebrizer_traits;

use std::collections::BTreeSet;
use std::ops::Sub;
use std::rc::Rc;

mod clauses;
mod types;
mod validate;

use core_traits::{Entid, TypedValue, ValueType};

use mentat_core::{parse_query, CachedAttributes, Schema};

use mentat_core::counter::RcCounter;

use edn::query::{
    Element, FindSpec, FnArg, Limit, NotJoin, Offset, OrJoin, OrWhereClause, Order, ParsedQuery,
    Pattern, PatternNonValuePlace, PatternValuePlace, PlainSymbol, Predicate, Rule, RuleInvocation,
    SrcVar, Variable, WhereClause, WhereFn,
};

use query_algebrizer_traits::errors::{AlgebrizerError, Result};

pub use crate::clauses::{QueryInputs, VariableBindings};

pub use crate::types::{EmptyBecause, FindQuery};

/// A convenience wrapper around things known in memory: the schema and caches.
/// We use a trait object here to avoid making dozens of functions generic over the type
/// of the cache. If performance becomes a concern, we should hard-code specific kinds of
/// cache right here, and/or eliminate the Option.
#[derive(Clone, Copy)]
pub struct Known<'s, 'c> {
    pub schema: &'s Schema,
    pub cache: Option<&'c dyn CachedAttributes>,
}

impl<'s, 'c> Known<'s, 'c> {
    pub fn for_schema(s: &'s Schema) -> Known<'s, 'static> {
        Known {
            schema: s,
            cache: None,
        }
    }

    pub fn new(s: &'s Schema, c: Option<&'c dyn CachedAttributes>) -> Known<'s, 'c> {
        Known {
            schema: s,
            cache: c,
        }
    }
}

/// This is `CachedAttributes`, but with handy generic parameters.
/// Why not make the trait generic? Because then we can't use it as a trait object in `Known`.
impl Known<'_, '_> {
    pub fn is_attribute_cached_reverse<U>(&self, entid: U) -> bool
    where
        U: Into<Entid>,
    {
        self.cache
            .map(|cache| cache.is_attribute_cached_reverse(entid.into()))
            .unwrap_or(false)
    }

    pub fn is_attribute_cached_forward<U>(&self, entid: U) -> bool
    where
        U: Into<Entid>,
    {
        self.cache
            .map(|cache| cache.is_attribute_cached_forward(entid.into()))
            .unwrap_or(false)
    }

    pub fn get_values_for_entid<U, V>(
        &self,
        schema: &Schema,
        attribute: U,
        entid: V,
    ) -> Option<&Vec<TypedValue>>
    where
        U: Into<Entid>,
        V: Into<Entid>,
    {
        self.cache
            .and_then(|cache| cache.get_values_for_entid(schema, attribute.into(), entid.into()))
    }

    pub fn get_value_for_entid<U, V>(
        &self,
        schema: &Schema,
        attribute: U,
        entid: V,
    ) -> Option<&TypedValue>
    where
        U: Into<Entid>,
        V: Into<Entid>,
    {
        self.cache
            .and_then(|cache| cache.get_value_for_entid(schema, attribute.into(), entid.into()))
    }

    pub fn get_entid_for_value<U>(&self, attribute: U, value: &TypedValue) -> Option<Entid>
    where
        U: Into<Entid>,
    {
        self.cache
            .and_then(|cache| cache.get_entid_for_value(attribute.into(), value))
    }

    pub fn get_entids_for_value<U>(
        &self,
        attribute: U,
        value: &TypedValue,
    ) -> Option<&BTreeSet<Entid>>
    where
        U: Into<Entid>,
    {
        self.cache
            .and_then(|cache| cache.get_entids_for_value(attribute.into(), value))
    }
}

#[derive(Debug)]
pub struct AlgebraicQuery {
    #[allow(dead_code)]
    default_source: SrcVar,
    pub find_spec: Rc<FindSpec>,
    #[allow(dead_code)]
    has_aggregates: bool,

    /// The set of variables that the caller wishes to be used for grouping when aggregating.
    /// These are specified in the query input, as `:with`, and are then chewed up during projection.
    /// If no variables are supplied, then no additional grouping is necessary beyond the
    /// non-aggregated projection list.
    pub with: BTreeSet<Variable>,

    /// Some query features, such as ordering, are implemented by implicit reference to SQL columns.
    /// In order for these references to be 'live', those columns must be projected.
    /// This is the set of variables that must be so projected.
    /// This is not necessarily every variable that will be so required -- some variables
    /// will already be in the projection list.
    pub named_projection: BTreeSet<Variable>,
    pub order: Option<Vec<OrderBy>>,
    pub limit: Limit,
    pub cc: clauses::ConjoiningClauses,
}

impl AlgebraicQuery {
    #[inline]
    pub fn is_known_empty(&self) -> bool {
        self.cc.is_known_empty()
    }

    /// Return true if every variable in the find spec is fully bound to a single value.
    pub fn is_fully_bound(&self) -> bool {
        self.find_spec.columns().all(|e| match e {
            // Pull expressions are never fully bound.
            // TODO: but the 'inside' of a pull expression certainly can be.
            &Element::Pull(_) => false,

            &Element::Variable(ref var) | &Element::Corresponding(ref var) => {
                self.cc.is_value_bound(var)
            }

            // For now, we pretend that aggregate functions are never fully bound:
            // we don't statically compute them, even if we know the value of the var.
            Element::Aggregate(_fn) => false,
        })
    }

    /// Return true if every variable in the find spec is fully bound to a single value,
    /// and evaluating the query doesn't require running SQL.
    pub fn is_fully_unit_bound(&self) -> bool {
        self.cc.wheres.is_empty() && self.is_fully_bound()
    }

    /// Return a set of the input variables mentioned in the `:in` clause that have not yet been
    /// bound. We do this by looking at the CC.
    pub fn unbound_variables(&self) -> BTreeSet<Variable> {
        self.cc
            .input_variables
            .sub(&self.cc.value_bound_variable_set())
    }
}

pub fn algebrize_with_counter(
    known: Known,
    parsed: FindQuery,
    counter: usize,
) -> Result<AlgebraicQuery> {
    algebrize_with_inputs(known, parsed, counter, QueryInputs::default())
}

pub fn algebrize(known: Known, parsed: FindQuery) -> Result<AlgebraicQuery> {
    algebrize_with_inputs(known, parsed, 0, QueryInputs::default())
}

/// Take an ordering list. Any variables that aren't fixed by the query are used to produce
/// a vector of `OrderBy` instances, including type comparisons if necessary. This function also
/// returns a set of variables that should be added to the `with` clause to make the ordering
/// clauses possible.
fn validate_and_simplify_order(
    cc: &ConjoiningClauses,
    order: Option<Vec<Order>>,
) -> Result<(Option<Vec<OrderBy>>, BTreeSet<Variable>)> {
    match order {
        None => Ok((None, BTreeSet::default())),
        Some(order) => {
            let mut order_bys: Vec<OrderBy> = Vec::with_capacity(order.len() * 2); // Space for tags.
            let mut vars: BTreeSet<Variable> = BTreeSet::default();

            for Order(direction, var) in order.into_iter() {
                // Eliminate any ordering clauses that are bound to fixed values.
                if cc.bound_value(&var).is_some() {
                    continue;
                }

                // Fail if the var isn't bound by the query.
                if !cc.column_bindings.contains_key(&var) {
                    bail!(AlgebrizerError::UnboundVariable(var.name()))
                }

                // Otherwise, determine if we also need to order by type…
                if cc.known_type(&var).is_none() {
                    order_bys.push(OrderBy(
                        direction.clone(),
                        VariableColumn::VariableTypeTag(var.clone()),
                    ));
                }
                order_bys.push(OrderBy(direction, VariableColumn::Variable(var.clone())));
                vars.insert(var.clone());
            }

            Ok((
                if order_bys.is_empty() {
                    None
                } else {
                    Some(order_bys)
                },
                vars,
            ))
        }
    }
}

fn simplify_limit(mut query: AlgebraicQuery) -> Result<AlgebraicQuery> {
    // Unpack any limit variables in place.
    let refined_limit = match query.limit {
        Limit::Variable(ref v) => {
            match query.cc.bound_value(v) {
                Some(TypedValue::Long(n)) => {
                    if n <= 0 {
                        // User-specified limits should always be natural numbers (> 0).
                        bail!(AlgebrizerError::InvalidLimit(
                            n.to_string(),
                            ValueType::Long
                        ))
                    } else {
                        Some(Limit::Fixed(n as u64))
                    }
                }
                Some(val) => {
                    // Same.
                    bail!(AlgebrizerError::InvalidLimit(
                        format!("{:?}", val),
                        val.value_type()
                    ))
                }
                None => {
                    // We know that the limit variable is mentioned in `:in`.
                    // That it's not bound here implies that we haven't got all the variables
                    // we'll need to run the query yet.
                    // (We should never hit this in `q_once`.)
                    // Simply pass the `Limit` through to `SelectQuery` untouched.
                    None
                }
            }
        }
        Limit::Unlimited => None,
        Limit::Fixed(_) => None,
    };

    if let Some(lim) = refined_limit {
        query.limit = lim;
    }
    Ok(query)
}

pub fn algebrize_with_inputs(
    known: Known,
    parsed: FindQuery,
    counter: usize,
    inputs: QueryInputs,
) -> Result<AlgebraicQuery> {
    algebrize_with_inputs_and_temporal(known, parsed, counter, inputs, None)
}

/// Like `algebrize_with_inputs`, but applies a whole-query temporal bound
/// (`as-of T` / `since T`) so `q` can run against a historical basis.
pub fn algebrize_with_inputs_and_temporal(
    known: Known,
    parsed: FindQuery,
    counter: usize,
    mut inputs: QueryInputs,
    temporal: Option<TemporalBound>,
) -> Result<AlgebraicQuery> {
    // Non-scalar `:in` bindings (collection/tuple/relation) come in as VALUES
    // tables; pull them out before `inputs` is moved into the CC (which only
    // consumes scalar `values`).
    let collections = std::mem::take(&mut inputs.collections);
    let alias_counter = RcCounter::with_initial(counter);
    let mut cc =
        ConjoiningClauses::with_inputs_and_alias_counter(parsed.in_vars, inputs, alias_counter);
    cc.temporal = temporal;

    // Materialize non-scalar `:in` inputs as VALUES joins *before* applying the
    // where-clauses, so pattern variables that reference them are already bound
    // and typed.
    if !collections.is_empty() {
        cc.apply_input_bindings(known.schema, collections)?;
    }

    // This is so the rest of the query knows that `?x` is a ref if `(pull ?x …)` appears in `:find`.
    cc.derive_types_from_find_spec(&parsed.find_spec);

    // Do we have a variable limit? If so, tell the CC that the var must be numeric.
    if let Limit::Variable(ref var) = parsed.limit {
        cc.constrain_var_to_long(var.clone());
    }

    // TODO: integrate default source into pattern processing.
    // TODO: flesh out the rest of find-into-context.
    let where_clauses = expand_rules(parsed.where_clauses, &parsed.rules)?;
    cc.apply_clauses(known, where_clauses)?;

    cc.expand_column_bindings();
    cc.prune_extracted_types();
    cc.process_required_types()?;

    let (order, extra_vars) = validate_and_simplify_order(&cc, parsed.order)?;

    // This might leave us with an unused `:in` variable.
    let limit = if parsed.find_spec.is_unit_limited() {
        Limit::Fixed(1)
    } else {
        parsed.limit
    };
    let q = AlgebraicQuery {
        default_source: parsed.default_source,
        find_spec: Rc::new(parsed.find_spec),
        has_aggregates: false, // TODO: we don't parse them yet.
        with: parsed.with,
        named_projection: extra_vars,
        order,
        limit,
        cc,
    };

    // Substitute in any fixed values and fail if they're out of range.
    simplify_limit(q)
}

pub use crate::clauses::ConjoiningClauses;
pub use crate::clauses::TemporalBound;

pub use crate::types::{
    Column, ColumnAlternation, ColumnConstraint, ColumnConstraintOrAlternation, ColumnIntersection,
    ColumnName, ComputedTable, DatomsColumn, DatomsTable, FulltextColumn, OrderBy, QualifiedAlias,
    QueryValue, SourceAlias, TableAlias, VariableColumn,
};

impl FindQuery {
    pub fn simple(spec: FindSpec, where_clauses: Vec<WhereClause>) -> FindQuery {
        FindQuery {
            find_spec: spec,
            default_source: SrcVar::DefaultSrc,
            with: BTreeSet::default(),
            in_vars: BTreeSet::default(),
            in_bindings: Vec::default(),
            in_sources: BTreeSet::default(),
            limit: Limit::Unlimited,
            offset: Offset::Unlimited,
            where_clauses,
            order: None,
            distinct: false,
            rules: Vec::new(),
        }
    }

    pub fn from_parsed_query(parsed: ParsedQuery) -> Result<FindQuery> {
        let in_vars = {
            let mut set: BTreeSet<Variable> = BTreeSet::default();

            for var in parsed.in_vars.into_iter() {
                if !set.insert(var.clone()) {
                    bail!(AlgebrizerError::DuplicateVariableError(var.name(), ":in"));
                }
            }

            set
        };

        let with = {
            let mut set: BTreeSet<Variable> = BTreeSet::default();

            for var in parsed.with.into_iter() {
                if !set.insert(var.clone()) {
                    bail!(AlgebrizerError::DuplicateVariableError(var.name(), ":with"));
                }
            }

            set
        };

        // Make sure that if we have `:limit ?x`, `?x` appears in `:in`.
        if let Limit::Variable(ref v) = parsed.limit {
            if !in_vars.contains(v) {
                bail!(AlgebrizerError::UnknownLimitVar(v.name()));
            }
        }

        Ok(FindQuery {
            find_spec: parsed.find_spec,
            default_source: parsed.default_source,
            with,
            in_vars,
            in_bindings: parsed.in_bindings,
            in_sources: parsed.in_sources,
            limit: parsed.limit,
            offset: parsed.offset,
            where_clauses: parsed.where_clauses,
            order: parsed.order,
            distinct: parsed.distinct,
            rules: parsed.rules,
        })
    }
}

pub fn parse_find_string(string: &str) -> Result<FindQuery> {
    parse_query(string)
        .map_err(|e| e.into())
        .and_then(FindQuery::from_parsed_query)
}

// ---------------------------------------------------------------------------
// Rule expansion (ported from pg_mentat's named rules).
//
// A pre-pass over the where-clauses that inlines `RuleExpr` invocations by
// substituting the invocation's arguments for the rule head's parameters and
// splicing in the rule body. Body-local variables are renamed to fresh gensyms
// so distinct invocations of the same rule don't collide.
//
// Scope of this slice (deliberately bounded; see AlgebrizerError variants):
//   * single-clause, non-recursive rules only.
//   * variable arguments only (a rule invoked with a constant errs).
// Multi-clause rules (OR alternatives) and recursive rules (self reference,
// needing WITH RECURSIVE in the SQL IR) are rejected with a clear error and
// remain future work.
// ---------------------------------------------------------------------------

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};

static RULE_GENSYM: AtomicUsize = AtomicUsize::new(0);

fn gensym(base: &str) -> Variable {
    let n = RULE_GENSYM.fetch_add(1, Ordering::Relaxed);
    // Leading "?__rule" keeps these out of the user's namespace.
    Variable::from_valid_name(&format!("?__rule_{}_{}", n, &base[1..]))
}

/// Public entry point: expand every rule invocation in `clauses` using `rules`.
fn expand_rules(clauses: Vec<WhereClause>, rules: &[Rule]) -> Result<Vec<WhereClause>> {
    if rules.is_empty() {
        // Fast path: no rules defined. Any `RuleExpr` present is an error, but
        // we only surface that if one is actually used (matching prior
        // behavior where undefined rule invocations were unimplemented!()).
        let mut out = Vec::with_capacity(clauses.len());
        for c in clauses {
            out.extend(expand_clause(c, rules, 0)?);
        }
        return Ok(out);
    }
    let mut out = Vec::with_capacity(clauses.len());
    for c in clauses {
        out.extend(expand_clause(c, rules, 0)?);
    }
    Ok(out)
}

const MAX_RULE_DEPTH: usize = 32;

fn find_rule<'r>(rules: &'r [Rule], name: &PlainSymbol) -> Option<&'r Rule> {
    rules.iter().find(|r| &r.name == name)
}

/// Expand a single clause into zero or more clauses (a rule invocation expands
/// into its body; everything else passes through, recursing into or/not).
fn expand_clause(clause: WhereClause, rules: &[Rule], depth: usize) -> Result<Vec<WhereClause>> {
    match clause {
        WhereClause::RuleExpr(inv) => expand_invocation(&inv, rules, depth),
        WhereClause::OrJoin(o) => {
            let OrJoin {
                unify_vars,
                clauses,
                ..
            } = o;
            let mut new_clauses = Vec::with_capacity(clauses.len());
            for owc in clauses {
                new_clauses.push(expand_or_where_clause(owc, rules, depth)?);
            }
            Ok(vec![WhereClause::OrJoin(OrJoin::new(
                unify_vars,
                new_clauses,
            ))])
        }
        WhereClause::NotJoin(n) => {
            let mut body = Vec::with_capacity(n.clauses.len());
            for c in n.clauses {
                body.extend(expand_clause(c, rules, depth)?);
            }
            Ok(vec![WhereClause::NotJoin(NotJoin::new(n.unify_vars, body))])
        }
        other => Ok(vec![other]),
    }
}

fn expand_or_where_clause(
    owc: OrWhereClause,
    rules: &[Rule],
    depth: usize,
) -> Result<OrWhereClause> {
    match owc {
        OrWhereClause::Clause(c) => {
            let mut expanded = expand_clause(c, rules, depth)?;
            if expanded.len() == 1 {
                Ok(OrWhereClause::Clause(expanded.pop().unwrap()))
            } else {
                Ok(OrWhereClause::And(expanded))
            }
        }
        OrWhereClause::And(cs) => {
            let mut body = Vec::with_capacity(cs.len());
            for c in cs {
                body.extend(expand_clause(c, rules, depth)?);
            }
            Ok(OrWhereClause::And(body))
        }
    }
}

fn expand_invocation(
    inv: &RuleInvocation,
    rules: &[Rule],
    depth: usize,
) -> Result<Vec<WhereClause>> {
    if depth >= MAX_RULE_DEPTH {
        bail!(AlgebrizerError::RecursiveRuleUnsupported(inv.name.clone()));
    }
    let rule = find_rule(rules, &inv.name)
        .ok_or_else(|| AlgebrizerError::UnknownRule(inv.name.clone()))?;

    if rule.clauses.len() != 1 {
        bail!(AlgebrizerError::MultiClauseRuleUnsupported(
            inv.name.clone()
        ));
    }
    let clause = &rule.clauses[0];

    // Reject recursive rules (self reference anywhere in the body).
    if body_references_rule(&clause.body, &inv.name) {
        bail!(AlgebrizerError::RecursiveRuleUnsupported(inv.name.clone()));
    }

    // Head params must be variables; invocation args must be variables (safe slice).
    let params = head_param_vars(&clause.head, &inv.name)?;
    let args = invocation_arg_vars(inv)?;
    if params.len() != args.len() {
        bail!(AlgebrizerError::RuleArgumentMismatch(
            inv.name.clone(),
            args.len(),
            params.len()
        ));
    }

    // Build substitution: every variable in the body maps to a fresh gensym,
    // except head params, which map to the corresponding invocation arg.
    let mut subst: BTreeMap<Variable, Variable> = BTreeMap::new();
    for (p, a) in params.iter().zip(args.iter()) {
        subst.insert(p.clone(), a.clone());
    }
    // Rename any remaining body-local variable to a gensym.
    let mut body_vars: BTreeSet<Variable> = BTreeSet::new();
    for c in &clause.body {
        collect_clause_vars(c, &mut body_vars);
    }
    for v in body_vars {
        subst.entry(v.clone()).or_insert_with(|| gensym(v.as_str()));
    }

    // Apply substitution and recursively expand (in case the body itself
    // invokes other, non-recursive rules).
    let mut out = Vec::with_capacity(clause.body.len());
    for c in &clause.body {
        let renamed = subst_clause(c.clone(), &subst);
        out.extend(expand_clause(renamed, rules, depth + 1)?);
    }
    Ok(out)
}

fn head_param_vars(head: &RuleInvocation, name: &PlainSymbol) -> Result<Vec<Variable>> {
    let mut vars = Vec::with_capacity(head.args.len());
    for a in &head.args {
        match a {
            FnArg::Variable(v) => vars.push(v.clone()),
            _ => bail!(AlgebrizerError::MultiClauseRuleUnsupported(name.clone())),
        }
    }
    Ok(vars)
}

fn invocation_arg_vars(inv: &RuleInvocation) -> Result<Vec<Variable>> {
    let mut vars = Vec::with_capacity(inv.args.len());
    for a in &inv.args {
        match a {
            FnArg::Variable(v) => vars.push(v.clone()),
            // Non-variable args (constants) aren't handled by this slice.
            _ => bail!(AlgebrizerError::RuleArgumentMismatch(
                inv.name.clone(),
                inv.args.len(),
                inv.args.len()
            )),
        }
    }
    Ok(vars)
}

fn body_references_rule(body: &[WhereClause], name: &PlainSymbol) -> bool {
    body.iter().any(|c| clause_references_rule(c, name))
}

fn clause_references_rule(c: &WhereClause, name: &PlainSymbol) -> bool {
    match c {
        WhereClause::RuleExpr(inv) => &inv.name == name,
        WhereClause::OrJoin(o) => o.clauses.iter().any(|owc| match owc {
            OrWhereClause::Clause(c) => clause_references_rule(c, name),
            OrWhereClause::And(cs) => cs.iter().any(|c| clause_references_rule(c, name)),
        }),
        WhereClause::NotJoin(n) => n.clauses.iter().any(|c| clause_references_rule(c, name)),
        _ => false,
    }
}

// --- variable collection + substitution over the clause AST ---

fn collect_clause_vars(c: &WhereClause, acc: &mut BTreeSet<Variable>) {
    match c {
        WhereClause::Pattern(p) => {
            collect_nv(&p.entity, acc);
            collect_nv(&p.attribute, acc);
            collect_v(&p.value, acc);
            collect_nv(&p.tx, acc);
        }
        WhereClause::Pred(p) => {
            for a in &p.args {
                if let FnArg::Variable(v) = a {
                    acc.insert(v.clone());
                }
            }
        }
        WhereClause::WhereFn(f) => {
            for a in &f.args {
                if let FnArg::Variable(v) = a {
                    acc.insert(v.clone());
                }
            }
        }
        WhereClause::RuleExpr(inv) => {
            for a in &inv.args {
                if let FnArg::Variable(v) = a {
                    acc.insert(v.clone());
                }
            }
        }
        WhereClause::OrJoin(o) => {
            for owc in &o.clauses {
                match owc {
                    OrWhereClause::Clause(c) => collect_clause_vars(c, acc),
                    OrWhereClause::And(cs) => cs.iter().for_each(|c| collect_clause_vars(c, acc)),
                }
            }
        }
        WhereClause::NotJoin(n) => n.clauses.iter().for_each(|c| collect_clause_vars(c, acc)),
        WhereClause::TypeAnnotation(_) => {}
    }
}

fn collect_nv(p: &PatternNonValuePlace, acc: &mut BTreeSet<Variable>) {
    if let PatternNonValuePlace::Variable(v) = p {
        acc.insert(v.clone());
    }
}

fn collect_v(p: &PatternValuePlace, acc: &mut BTreeSet<Variable>) {
    if let PatternValuePlace::Variable(v) = p {
        acc.insert(v.clone());
    }
}

fn sv(v: &Variable, subst: &BTreeMap<Variable, Variable>) -> Variable {
    subst.get(v).cloned().unwrap_or_else(|| v.clone())
}

fn subst_nv(p: PatternNonValuePlace, subst: &BTreeMap<Variable, Variable>) -> PatternNonValuePlace {
    match p {
        PatternNonValuePlace::Variable(v) => PatternNonValuePlace::Variable(sv(&v, subst)),
        other => other,
    }
}

fn subst_v(p: PatternValuePlace, subst: &BTreeMap<Variable, Variable>) -> PatternValuePlace {
    match p {
        PatternValuePlace::Variable(v) => PatternValuePlace::Variable(sv(&v, subst)),
        other => other,
    }
}

fn subst_fnarg(a: FnArg, subst: &BTreeMap<Variable, Variable>) -> FnArg {
    match a {
        FnArg::Variable(v) => FnArg::Variable(sv(&v, subst)),
        FnArg::Vector(xs) => FnArg::Vector(xs.into_iter().map(|x| subst_fnarg(x, subst)).collect()),
        other => other,
    }
}

fn subst_clause(c: WhereClause, subst: &BTreeMap<Variable, Variable>) -> WhereClause {
    match c {
        WhereClause::Pattern(p) => WhereClause::Pattern(Pattern {
            source: p.source,
            entity: subst_nv(p.entity, subst),
            attribute: subst_nv(p.attribute, subst),
            value: subst_v(p.value, subst),
            tx: subst_nv(p.tx, subst),
            added: subst_nv(p.added, subst),
        }),
        WhereClause::Pred(p) => WhereClause::Pred(Predicate {
            operator: p.operator,
            args: p.args.into_iter().map(|a| subst_fnarg(a, subst)).collect(),
        }),
        WhereClause::WhereFn(f) => WhereClause::WhereFn(WhereFn {
            operator: f.operator,
            args: f.args.into_iter().map(|a| subst_fnarg(a, subst)).collect(),
            binding: f.binding,
        }),
        WhereClause::RuleExpr(inv) => WhereClause::RuleExpr(RuleInvocation {
            name: inv.name,
            args: inv
                .args
                .into_iter()
                .map(|a| subst_fnarg(a, subst))
                .collect(),
        }),
        WhereClause::OrJoin(o) => {
            let clauses = o
                .clauses
                .into_iter()
                .map(|owc| match owc {
                    OrWhereClause::Clause(c) => OrWhereClause::Clause(subst_clause(c, subst)),
                    OrWhereClause::And(cs) => {
                        OrWhereClause::And(cs.into_iter().map(|c| subst_clause(c, subst)).collect())
                    }
                })
                .collect();
            WhereClause::OrJoin(OrJoin::new(o.unify_vars, clauses))
        }
        WhereClause::NotJoin(n) => WhereClause::NotJoin(NotJoin::new(
            n.unify_vars,
            n.clauses
                .into_iter()
                .map(|c| subst_clause(c, subst))
                .collect(),
        )),
        WhereClause::TypeAnnotation(a) => WhereClause::TypeAnnotation(a),
    }
}
