// Copyright 2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

#![allow(dead_code)]

//! A pull expression is a function.
//!
//! Its inputs are a store, a schema, and a set of bindings.
//!
//! Its output is a map whose keys are the input bindings and whose values are
//! appropriate structured values to represent the pull expression.
//!
//! For example, the pull expression:
//!
//! ```edn
//! (pull ?person [:person/name
//!                :person/tattoo
//!                {:person/friend [*]}])`
//! ```
//!
//! will return values shaped like:
//!
//! ```edn
//! {:person/name "Alice"                            ; Single-valued attribute
//!                                                  ; Absence: Alice has no tattoos.
//!  :person/friend [                                ; Multi-valued attribute.
//!    {:person/name "Bob"                           ; Nesting and wildcard.
//!     :person/pet ["Harrison", "Hoppy"]}]}
//! ```
//!
//! There will be one such value for each input binding.
//!
//! We fetch layers of a pull expression iteratively: all attributes at the same
//! 'level' can be fetched at the same time and accumulated into maps.
//!
//! Those maps are wrapped in `Rc` for two reasons:
//! - They might occur multiple times when projected from a `:find` query.
//! - They might refer to each other (consider recursion).
//!
//! A nested or recursive pull expression consumes values produced by earlier stages
//! (the recursion with a smaller recursion limit and a growing 'seen' list),
//! generating another layer of mappings.
//!
//! For example, you can imagine the nesting in the earlier pull expression being
//! decomposed into two chained expressions:
//!
//! ```edn
//! (pull
//!     (pull ?person [:person/friend])
//!      [*]))
//! ```

extern crate core_traits;
extern crate edn;
extern crate mentat_core;
extern crate mentat_db;
extern crate query_pull_traits;

use std::collections::{BTreeMap, BTreeSet};

use std::iter::once;

use core_traits::{Binding, Entid, StructuredMap, TypedValue};

use mentat_core::{Cloned, HasSchema, Keyword, Schema, ValueRc};

use db_traits::errors::DbError;
use mentat_db::cache;
use mentat_query_algebrizer::TemporalBound;
use mentat_sql::{Dialect, SqlConn, SqlConnExt, SqlValue};

use edn::query::{NamedPullAttribute, PullAttributeSpec, PullConcreteAttribute};

use query_pull_traits::errors::{PullError, Result};

type PullResults = BTreeMap<Entid, ValueRc<StructuredMap>>;

pub fn pull_attributes_for_entity<A>(
    schema: &Schema,
    db: &dyn SqlConn,
    entity: Entid,
    attributes: A,
) -> Result<StructuredMap>
where
    A: IntoIterator<Item = Entid>,
{
    let attrs = attributes
        .into_iter()
        .map(|e| PullAttributeSpec::Attribute(PullConcreteAttribute::Entid(e).into()))
        .collect();
    Puller::prepare(schema, attrs)?
        .pull(schema, db, once(entity))
        .map(|m| {
            m.into_iter()
                .next()
                .map(|(k, vs)| {
                    assert_eq!(k, entity);
                    vs.cloned()
                })
                .unwrap_or_default()
        })
}

pub fn pull_attributes_for_entities<E, A>(
    schema: &Schema,
    db: &dyn SqlConn,
    entities: E,
    attributes: A,
) -> Result<PullResults>
where
    E: IntoIterator<Item = Entid>,
    A: IntoIterator<Item = Entid>,
{
    let attrs = attributes
        .into_iter()
        .map(|e| PullAttributeSpec::Attribute(PullConcreteAttribute::Entid(e).into()))
        .collect();
    Puller::prepare(schema, attrs)?.pull(schema, db, entities)
}

/// Pull `pattern` (parsed pull specs) for one entity on `basis` (`None` =
/// current). An entity with nothing to show yields an empty map.
pub fn pull_pattern_for_entity(
    schema: &Schema,
    db: &dyn SqlConn,
    entity: Entid,
    pattern: Vec<PullAttributeSpec>,
    basis: Option<TemporalBound>,
) -> Result<StructuredMap> {
    let mut m =
        Puller::prepare(schema, pattern)?
            .with_basis(basis)
            .pull(schema, db, once(entity))?;
    Ok(m.remove(&entity).map(|v| v.cloned()).unwrap_or_default())
}

/// A `Puller` constructs on demand a map from a provided set of entity IDs to a set of structured maps.
pub struct Puller {
    // The domain of this map is the set of attributes to fetch.
    // The range is the set of aliases to use in the output.
    attributes: BTreeMap<Entid, ValueRc<Keyword>>,
    attribute_spec: cache::AttributeSpec,
    forward_attrs: BTreeSet<Entid>,

    // Reverse-reference attributes: `:ns/_attr`. Maps the *forward* attribute
    // entid to the output name (default `:ns/_attr`). For each pulled entity we
    // find, via the VAET index, all entities that refer to it through this
    // attribute, and emit them as a cardinality-many `Ref` list. Ported from
    // pg_mentat's reverse pull.
    reverse_attributes: BTreeMap<Entid, ValueRc<Keyword>>,

    // If this is set, each pulled entity is contributed to its own output map, labeled with this
    // keyword. This is a divergence from Datomic, which has no types by which to differentiate a
    // long from an entity ID, and thus represents all entities in pull as, _e.g._, `{:db/id 1234}`.
    //  Mentat can use `TypedValue::Ref(1234)`, but it's sometimes convenient to fetch the entity ID
    // itself as part of a pull expression: `{:person 1234, :person/name "Peter"}`.
    db_id_alias: Option<ValueRc<Keyword>>,

    // Map specs, `{:attr [sub-pattern]}`: each followed ref is pulled with the
    // sub-puller, batched per spec.
    nested: Vec<NestedPull>,

    // The basis to read: `None` is the live `datoms` table; `AsOf(t)` is the
    // state after tx `t` (asserted at or before `t`, not retracted at or before
    // `t`); `Since(t)` is the current datoms transacted after `t`.
    basis: Option<TemporalBound>,
}

/// One map spec: follow `attr` (forward, or reverse when `reverse`) from each
/// pulled entity and pull `puller` on what it reaches, under `name`.
struct NestedPull {
    attr: Entid,
    reverse: bool,
    many: bool,
    name: ValueRc<Keyword>,
    puller: Puller,
}

/// The ref targets in a pulled forward binding (one ref, or a vec of refs).
fn refs_in(b: Binding) -> Vec<Entid> {
    match b {
        Binding::Scalar(TypedValue::Ref(r)) => vec![r],
        Binding::Vec(vs) => vs
            .iter()
            .filter_map(|v| match v {
                Binding::Scalar(TypedValue::Ref(r)) => Some(*r),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// The datoms of a historical basis as a subquery of `a, e, v, value_type_tag`
/// rows. A fulltext value is still its `fulltext_values` rowid on SQLite.
fn basis_datoms_sql(basis: TemporalBound) -> String {
    match basis {
        // Datomic's `since`: the current datoms whose tx is after t.
        TemporalBound::Since(t) => {
            format!("(SELECT a, e, v, value_type_tag FROM datoms WHERE tx > {t})")
        }
        // Same rule as the algebrizer's as-of rewrite: an assertion at or
        // before t (and after `since`, if set) with no retraction of the same
        // (e, a, v) after it and at or before t. `added` is 0/1 on SQLite and
        // BOOLEAN on DuckDB; bare `t.added` / `NOT r.added` read the same on both.
        TemporalBound::AsOf(t) | TemporalBound::AsOfSince { as_of: t, .. } => {
            let lower = basis
                .since()
                .map(|s| format!(" AND t.tx > {s}"))
                .unwrap_or_default();
            format!(
                "(SELECT t.a AS a, t.e AS e, t.v AS v, t.value_type_tag AS value_type_tag \
                 FROM transactions t WHERE t.tx <= {t}{lower} AND t.added AND NOT EXISTS (\
                 SELECT 1 FROM transactions r WHERE r.a = t.a AND r.e = t.e \
                 AND r.value_type_tag = t.value_type_tag AND r.v = t.v \
                 AND NOT r.added AND r.tx > t.tx AND r.tx <= {t}))"
            )
        }
    }
}

fn csv(ids: &[Entid]) -> String {
    ids.iter()
        .map(|i| i.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fill a cache with the forward values of `attrs` for `entities` on a
/// historical basis (the counterpart of `make_cache_for_entities_and_attributes`).
fn historical_cache(
    schema: &Schema,
    db: &dyn SqlConn,
    attrs: &BTreeSet<Entid>,
    entities: &[Entid],
    basis: TemporalBound,
) -> Result<cache::AttributeCaches> {
    if attrs.is_empty() || entities.is_empty() {
        return Ok(cache::AttributeCaches::default());
    }
    let src = basis_datoms_sql(basis);
    let es = csv(entities);
    // SQLite keeps fulltext text in `fulltext_values` and the rowid in `v`;
    // DuckDB stores fulltext values inline (see crates/duckdb/store).
    let (fts, plain): (Vec<Entid>, Vec<Entid>) = attrs.iter().partition(|a| {
        db.dialect() == Dialect::Sqlite
            && schema.attribute_for_entid(**a).is_some_and(|x| x.fulltext)
    });
    let mut parts = Vec::new();
    if !plain.is_empty() {
        parts.push(format!(
            "SELECT b.a, b.e, b.v, b.value_type_tag FROM {src} b \
             WHERE b.e IN ({es}) AND b.a IN ({})",
            csv(&plain)
        ));
    }
    if !fts.is_empty() {
        parts.push(format!(
            "SELECT b.a, b.e, f.text, b.value_type_tag FROM {src} b \
             JOIN fulltext_values f ON f.rowid = b.v \
             WHERE b.e IN ({es}) AND b.a IN ({})",
            csv(&fts)
        ));
    }
    let sql = format!("{} ORDER BY 1 ASC, 2 ASC", parts.join(" UNION ALL "));
    Ok(cache::AttributeCaches::make_cache_from_aevt_sql(
        schema, db, attrs, &sql,
    )?)
}

impl Puller {
    pub fn prepare(schema: &Schema, attributes: Vec<PullAttributeSpec>) -> Result<Puller> {
        // TODO: eventually this entry point will handle aliasing and that kind of
        // thing. For now it's just a convenience.

        let lookup_name = |i: &Entid| {
            // In the unlikely event that we have an attribute with no name, we bail.
            schema
                .get_ident(*i)
                .map(|ident| ValueRc::new(ident.clone()))
                .ok_or_else(|| PullError::UnnamedAttribute(*i))
        };

        let mut names: BTreeMap<Entid, ValueRc<Keyword>> = Default::default();
        let mut attrs: BTreeSet<Entid> = Default::default();
        let mut reverse_attributes: BTreeMap<Entid, ValueRc<Keyword>> = Default::default();
        let mut nested: Vec<NestedPull> = Vec::new();
        let db_id = ::std::rc::Rc::new(Keyword::namespaced("db", "id"));
        let mut db_id_alias = None;

        for attr in attributes.iter() {
            match attr {
                PullAttributeSpec::Wildcard => {
                    let attribute_ids = schema.attribute_map.keys();
                    for id in attribute_ids {
                        names.insert(*id, lookup_name(id)?);
                        attrs.insert(*id);
                    }
                    // No `break`: items after `*` (`[* {:a [..]}]`) still apply.
                }
                PullAttributeSpec::Nested(
                    NamedPullAttribute {
                        ref attribute,
                        ref alias,
                        reverse,
                    },
                    ref sub,
                ) => {
                    let entid = match attribute {
                        PullConcreteAttribute::Ident(ref i) => schema.get_entid(i).map(|e| e.0),
                        PullConcreteAttribute::Entid(e) => Some(*e),
                    };
                    // An unknown attribute pulls nothing, as for a plain one.
                    let Some(entid) = entid else { continue };
                    let name = match alias {
                        Some(a) => a.to_value_rc(),
                        None => {
                            let fwd = schema
                                .get_ident(entid)
                                .ok_or(PullError::UnnamedAttribute(entid))?;
                            ValueRc::new(if *reverse {
                                fwd.to_reversed()
                            } else {
                                fwd.clone()
                            })
                        }
                    };
                    let many = *reverse
                        || schema
                            .attribute_for_entid(entid)
                            .is_some_and(|a| a.multival);
                    if !*reverse {
                        attrs.insert(entid); // fetched, emitted by the spec
                    }
                    nested.push(NestedPull {
                        attr: entid,
                        reverse: *reverse,
                        many,
                        name,
                        puller: Puller::prepare(schema, sub.clone())?,
                    });
                }
                PullAttributeSpec::Attribute(NamedPullAttribute {
                    ref attribute,
                    ref alias,
                    reverse,
                }) => {
                    let alias = alias.as_ref().map(|r| r.to_value_rc());

                    // Reverse pull (`:ns/_attr`). `attribute` holds the forward
                    // ident; find its entid, and default the output name to the
                    // reversed keyword unless an explicit `:as` alias is given.
                    if *reverse {
                        let entid = match attribute {
                            PullConcreteAttribute::Ident(ref i) => {
                                schema.get_entid(i).map(|e| e.into())
                            }
                            PullConcreteAttribute::Entid(ref e) => Some(*e),
                        };
                        if let Some(entid) = entid {
                            let name = match alias {
                                Some(a) => a,
                                None => {
                                    let fwd = schema
                                        .get_ident(entid)
                                        .ok_or(PullError::UnnamedAttribute(entid))?;
                                    ValueRc::new(fwd.to_reversed())
                                }
                            };
                            reverse_attributes.insert(entid, name);
                        }
                        continue;
                    }

                    match attribute {
                        // Handle :db/id.
                        PullConcreteAttribute::Ident(ref i) if i.as_ref() == db_id.as_ref() => {
                            // We only allow :db/id once.
                            if db_id_alias.is_some() {
                                return Err(PullError::RepeatedDbId);
                            }
                            db_id_alias = Some(alias.unwrap_or_else(|| db_id.to_value_rc()));
                        }
                        PullConcreteAttribute::Ident(ref i) => {
                            if let Some(entid) = schema.get_entid(i) {
                                let name = alias.unwrap_or_else(|| i.to_value_rc());
                                names.insert(entid.into(), name);
                                attrs.insert(entid.into());
                            }
                        }
                        PullConcreteAttribute::Entid(ref entid) => {
                            let name = alias.map(Ok).unwrap_or_else(|| lookup_name(entid))?;
                            names.insert(*entid, name);
                            attrs.insert(*entid);
                        }
                    }
                }
            }
        }

        Ok(Puller {
            attributes: names,
            attribute_spec: cache::AttributeSpec::specified(&attrs, schema),
            forward_attrs: attrs,
            reverse_attributes,
            db_id_alias,
            nested,
            basis: None,
        })
    }

    /// Read `basis` instead of the live datoms (`None` = current), in nested
    /// map specs too.
    pub fn with_basis(mut self, basis: Option<TemporalBound>) -> Puller {
        self.set_basis(basis);
        self
    }

    fn set_basis(&mut self, basis: Option<TemporalBound>) {
        self.basis = basis;
        for n in self.nested.iter_mut() {
            n.puller.set_basis(basis);
        }
    }

    /// The entities that refer to `e` through `attr` on this puller's basis.
    fn referrers(&self, db: &dyn SqlConn, attr: Entid, e: Entid) -> Result<Vec<Entid>> {
        // `IS DISTINCT FROM 0` is SQLite's `IS NOT 0`, so it matches the
        // partial idx_datoms_vaet (`... WHERE index_vaet IS NOT 0`); DuckDB
        // has it too. `<> 0` (1.11.0-1.17.x) missed the index and scanned
        // every datom of the attribute. Since S: the same live rows, tx > S.
        let live = "SELECT DISTINCT e FROM datoms \
                    WHERE a = ? AND v = ? AND value_type_tag = 0 AND index_vaet IS DISTINCT FROM 0";
        let sql = match self.basis {
            None => format!("{live} ORDER BY e ASC"),
            Some(TemporalBound::Since(s)) => format!("{live} AND tx > {s} ORDER BY e ASC"),
            Some(basis) => format!(
                "SELECT DISTINCT e FROM {} b \
                 WHERE a = ? AND v = ? AND value_type_tag = 0 ORDER BY e ASC",
                basis_datoms_sql(basis)
            ),
        };
        Ok(db
            .query_rows(
                &sql,
                &[SqlValue::Integer(attr), SqlValue::Integer(e)],
                |row| row.get_i64(0),
            )
            .map_err(DbError::from)?)
    }

    pub fn pull<E>(&self, schema: &Schema, db: &dyn SqlConn, entities: E) -> Result<PullResults>
    where
        E: IntoIterator<Item = Entid>,
    {
        // We implement pull by:
        // - Generating `AttributeCaches` for the provided attributes and entities.
        //   TODO: it would be nice to invert the cache as we build it, rather than have to invert it here.
        // - Recursing. (TODO: we'll need AttributeCaches to not overwrite in case of recursion! And
        //   ideally not do excess work when some entity/attribute pairs are known.)
        // - Building a structure by walking the pull expression with the caches.
        // TODO: limits.

        // Build a cache for these attributes and entities.
        // TODO: use the store's existing cache!
        let entities: Vec<Entid> = entities.into_iter().collect();
        let caches = match self.basis {
            None => cache::AttributeCaches::make_cache_for_entities_and_attributes(
                schema,
                db,
                self.attribute_spec.clone(),
                &entities,
            )?,
            Some(basis) => historical_cache(schema, db, &self.forward_attrs, &entities, basis)?,
        };

        // Now construct the appropriate result format.
        // TODO: should we walk `e` then `a`, or `a` then `e`? Possibly the right answer
        // is just to collect differently!
        let mut maps = BTreeMap::new();

        // Collect :db/id if requested.
        if let Some(ref alias) = self.db_id_alias {
            for e in entities.iter() {
                let r = maps
                    .entry(*e)
                    .or_insert_with(|| ValueRc::new(StructuredMap::default()));
                let m = ValueRc::get_mut(r).unwrap();
                m.insert(alias.clone(), Binding::Scalar(TypedValue::Ref(*e)));
            }
        }

        for (name, cache) in self.attributes.iter().filter_map(|(a, name)| {
            caches
                .forward_attribute_cache_for_attribute(schema, *a)
                .map(|cache| (name.clone(), cache))
        }) {
            for e in entities.iter() {
                if let Some(binding) = cache.binding_for_e(*e) {
                    let r = maps
                        .entry(*e)
                        .or_insert_with(|| ValueRc::new(StructuredMap::default()));

                    // Get into the inner map so we can accumulate a value.
                    // We can unwrap here because we created all of these maps…
                    let m = ValueRc::get_mut(r).unwrap();

                    m.insert(name.clone(), binding);
                }
            }
        }

        // Reverse-reference attributes (`:ns/_attr`). For each such attribute,
        // query the VAET index for every entity that refers to one of our
        // pulled entities via that attribute. Refs are stored as plain integers
        // with value_type_tag = 0 (ValueType::Ref).
        if !self.reverse_attributes.is_empty() && !entities.is_empty() {
            for (attr_entid, name) in self.reverse_attributes.iter() {
                for e in entities.iter() {
                    let referrers: Vec<Binding> = self
                        .referrers(db, *attr_entid, *e)?
                        .into_iter()
                        .map(|r| Binding::Scalar(TypedValue::Ref(r)))
                        .collect();
                    if !referrers.is_empty() {
                        let r = maps
                            .entry(*e)
                            .or_insert_with(|| ValueRc::new(StructuredMap::default()));
                        let m = ValueRc::get_mut(r).unwrap();
                        m.insert(name.clone(), Binding::Vec(ValueRc::new(referrers)));
                    }
                }
            }
        }

        // Map specs: follow each ref, pull the sub-pattern for every target in
        // one batch, and nest the results. Each nested map carries `:db/id`
        // (pg_mentat's edn_pull shape).
        let db_id = ValueRc::new(Keyword::namespaced("db", "id"));
        for n in self.nested.iter() {
            let mut targets: Vec<(Entid, Vec<Entid>)> = Vec::new();
            for e in entities.iter() {
                let ts = if n.reverse {
                    self.referrers(db, n.attr, *e)?
                } else {
                    caches
                        .forward_attribute_cache_for_attribute(schema, n.attr)
                        .and_then(|c| c.binding_for_e(*e))
                        .map(refs_in)
                        .unwrap_or_default()
                };
                if !ts.is_empty() {
                    targets.push((*e, ts));
                }
            }
            if targets.is_empty() {
                continue;
            }
            let all: BTreeSet<Entid> = targets.iter().flat_map(|(_, ts)| ts.clone()).collect();
            let sub = n.puller.pull(schema, db, all)?;
            for (e, ts) in targets {
                let mut nested_maps: Vec<Binding> = ts
                    .into_iter()
                    .map(|t| {
                        let mut m = StructuredMap::default();
                        m.insert(db_id.clone(), Binding::Scalar(TypedValue::Ref(t)));
                        if let Some(pulled) = sub.get(&t) {
                            for (k, v) in pulled.0.iter() {
                                m.insert(k.clone(), v.clone());
                            }
                        }
                        Binding::Map(ValueRc::new(m))
                    })
                    .collect();
                let value = if n.many {
                    Binding::Vec(ValueRc::new(nested_maps))
                } else {
                    nested_maps.swap_remove(0)
                };
                let r = maps
                    .entry(e)
                    .or_insert_with(|| ValueRc::new(StructuredMap::default()));
                ValueRc::get_mut(r).unwrap().insert(n.name.clone(), value);
            }
        }

        Ok(maps)
    }
}
