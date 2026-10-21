// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! `edn_eval`'s `mentat.store/*` surface over DuckDB-backed stores.
//!
//! The shared layer (`mentat_script`) owns argument parsing and the db-value
//! shape; this backend owns storage. Every handle names a store in the DuckDB
//! database the extension was loaded into. Queries against an as-of/since db
//! value run as temporal Datalog (`TemporalBound`), so `q` works on any basis.
//! `with` (speculative transact) applies the tx-data in a DuckDB transaction
//! on this call's connection and rolls it back (`DuckStore::with_speculative`).

use std::cell::{Cell, RefCell};
use std::collections::HashMap;

use mino_rs::collections::map::{PMap, PSet};
use mino_rs::collections::vector::PVec;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Value};

use crate::{DuckStore, SqlConn};
use core_traits::{Binding, StructuredMap, TypedValue};
use mentat_core::{HasSchema, Keyword};
use mentat_query_projector::{QueryOutput, QueryResults};
use mentat_script::{DbRef, ScriptBackend, TxReport};

const SCRIPT_LIMITS: mino_rs::Limits = mino_rs::Limits {
    steps: Some(10_000_000),
    heap_bytes: Some(64 * 1024 * 1024),
    depth: Some(1000),
};

struct DuckBackend {
    conn: &'static dyn SqlConn,
    default_store: String,
    stores: RefCell<HashMap<i64, String>>,
    next_id: Cell<i64>,
}

impl DuckBackend {
    fn store(&self, conn: i64) -> Result<DuckStore<'static>, String> {
        let map = self.stores.borrow();
        let name = map
            .get(&conn)
            .ok_or_else(|| format!("no open store for handle {conn}"))?;
        Ok(DuckStore::new(self.conn, name))
    }

    fn query(
        &self,
        db: &DbRef,
        query: &str,
        inputs: &[serde_json::Value],
    ) -> Result<QueryOutput, String> {
        // Both bounds when both are set: `(since (as-of db T) S)`.
        let mut opts = serde_json::json!({ "inputs": inputs });
        if let Some(t) = db.as_of {
            opts["asOf"] = t.into();
        }
        if let Some(s) = db.since {
            opts["since"] = s.into();
        }
        self.store(db.conn)?
            .q_json(query, &opts)
            .map_err(|e| e.to_string())
    }
}

impl ScriptBackend for DuckBackend {
    fn open(&mut self, path: Option<&str>) -> Result<i64, String> {
        let name = path.unwrap_or(&self.default_store).to_string();
        DuckStore::new(self.conn, &name)
            .open()
            .map_err(|e| e.to_string())?;
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.stores.borrow_mut().insert(id, name);
        Ok(id)
    }

    fn close(&mut self, conn: i64) -> Result<(), String> {
        self.stores.borrow_mut().remove(&conn);
        Ok(())
    }

    fn basis_tx(&self, conn: i64) -> Result<i64, String> {
        self.store(conn)?.last_tx().map_err(|e| e.to_string())
    }

    fn transact(&mut self, conn: i64, edn: &str) -> Result<TxReport, String> {
        let name = {
            let map = self.stores.borrow();
            map.get(&conn)
                .cloned()
                .ok_or_else(|| format!("no open store for handle {conn}"))?
        };
        let r = transact(self.conn, &name, edn).map_err(|e| e.to_string())?;
        Ok(TxReport {
            tx_id: r.tx_id,
            tempids: r.tempids.into_iter().collect(),
            datoms_asserted: r.datoms_asserted as i64,
            datoms_retracted: r.datoms_retracted as i64,
        })
    }

    fn with(&mut self, db: &DbRef, edn: &str) -> Result<(i64, TxReport), String> {
        let name = {
            let map = self.stores.borrow();
            map.get(&db.conn)
                .cloned()
                .ok_or_else(|| format!("no open store for handle {}", db.conn))?
        };
        let r = with(self.conn, &name, edn).map_err(|e| e.to_string())?;
        Ok((
            r.tx_id,
            TxReport {
                tx_id: r.tx_id,
                tempids: r.tempids.into_iter().collect(),
                datoms_asserted: r.datoms_asserted as i64,
                datoms_retracted: r.datoms_retracted as i64,
            },
        ))
    }

    fn q(&self, db: &DbRef, query_edn: &str) -> Result<Value, String> {
        Ok(query_output_value(self.query(db, query_edn, &[])?))
    }

    fn q_with_inputs(
        &self,
        db: &DbRef,
        query_edn: &str,
        inputs: &[serde_json::Value],
    ) -> Result<Value, String> {
        Ok(query_output_value(self.query(db, query_edn, inputs)?))
    }

    fn pull(&self, db: &DbRef, eid: i64, pattern_edn: &str) -> Result<Value, String> {
        let (pattern, _) =
            mino_rs::reader::read_one(pattern_edn).map_err(|e| format!("bad pull pattern: {e}"))?;
        if !matches!(pattern, Value::Vector(_)) {
            return Err("pattern must be a vector of attribute keywords (or [*])".into());
        }
        let query = format!("[:find (pull ?e {pattern_edn}) . :in ?e :where [?e _ _]]");
        let out = self.query(db, &query, &[serde_json::json!(eid)])?;
        Ok(match out.results {
            QueryResults::Scalar(Some(Binding::Map(sm))) => structured_map_value(&sm),
            _ => Value::Map(Gc::new(PMap::empty())),
        })
    }

    fn datoms(&self, db: &DbRef) -> Result<Value, String> {
        let out = self.query(db, "[:find ?e ?a ?v :where [?e ?a ?v]]", &[])?;
        let store = self.store(db.conn)?;
        let schema = store.current_schema().map_err(|e| e.to_string())?;
        let mut tuples = Vec::new();
        if let QueryResults::Rel(rel) = out.results {
            for row in rel.rows() {
                let (Binding::Scalar(TypedValue::Ref(e)), Binding::Scalar(TypedValue::Ref(a))) =
                    (&row[0], &row[1])
                else {
                    continue;
                };
                let a = schema
                    .get_ident(*a)
                    .map(|k| Value::Keyword(keyword_to_sym(k)))
                    .unwrap_or(Value::Int(*a));
                tuples.push(Value::Vector(Gc::new(PVec::from_vec(vec![
                    Value::Int(*e),
                    a,
                    binding_value(row[2].clone()),
                ]))));
            }
        }
        Ok(Value::Vector(Gc::new(PVec::from_vec(tuples))))
    }

    fn resolve_eid(&self, db: &DbRef, arg: &Value) -> Result<i64, String> {
        match arg {
            Value::Int(n) => Ok(*n),
            Value::Keyword(sym) => {
                let kw = sym_to_keyword(sym);
                let schema = self
                    .store(db.conn)?
                    .current_schema()
                    .map_err(|e| e.to_string())?;
                schema
                    .get_entid(&kw)
                    .map(|k| k.0)
                    .ok_or_else(|| format!("unknown ident {kw}"))
            }
            Value::Vector(v) if v.len() == 2 => {
                let attr_kw = match v.nth(0).unwrap() {
                    Value::Keyword(sym) => sym_to_keyword(sym),
                    _ => return Err("lookup-ref attr must be a keyword".into()),
                };
                let query = format!(
                    "[:find ?e . :where [?e {attr_kw} {}]]",
                    mino_rs::printer::print_str(v.nth(1).unwrap())
                );
                match self.query(db, &query, &[])?.results {
                    QueryResults::Scalar(Some(Binding::Scalar(TypedValue::Ref(e)))) => Ok(e),
                    _ => Err(format!(
                        "lookup-ref {} resolved to no entity",
                        mino_rs::printer::print_str(arg)
                    )),
                }
            }
            _ => Err(format!(
                "eid must be an integer, ident keyword, or [:attr val] lookup-ref, got {}",
                mino_rs::printer::print_str(arg)
            )),
        }
    }
}

/// Transact `edn` into `store`, first expanding user-defined transaction
/// functions (`[:my/fn arg ...]` on an ident carrying `:db/fn`; issue #333)
/// and defining the `:db/fn` attribute the first time a function is installed.
/// Functions run in a sandboxed interpreter over the same connection, before
/// the store's transaction begins, so they see the database as of the start of
/// the transaction. Nesting is bounded by `MENTAT_TXFN_MAX_DEPTH` (default 16).
/// Every scripted and SQL-level `edn_t` goes through here.
pub fn transact(
    conn: &'static dyn SqlConn,
    store: &str,
    edn: &str,
) -> public_traits::errors::Result<mentat_core::TxReport> {
    let s = DuckStore::new(conn, store);
    let (define, expanded) = expand_tx_fns(conn, &s, store, edn)?;
    if define {
        s.transact(DB_FN_ATTRIBUTE)?;
    }
    s.transact(&expanded)
}

/// `mentat.store/with`: [`transact`]'s expansion, then the tx-data applied
/// and rolled back (`DuckStore::with_speculative`). A first function's
/// `:db/fn` attribute is defined inside the rolled-back transaction too, so
/// `with` commits nothing.
pub fn with(
    conn: &'static dyn SqlConn,
    store: &str,
    edn: &str,
) -> public_traits::errors::Result<mentat_core::TxReport> {
    let s = DuckStore::new(conn, store);
    let (define, expanded) = expand_tx_fns(conn, &s, store, edn)?;
    let edns: Vec<&str> = define
        .then_some(DB_FN_ATTRIBUTE)
        .into_iter()
        .chain([expanded.as_str()])
        .collect();
    s.with_speculative(&edns)
}

const DB_FN_ATTRIBUTE: &str =
    "[{:db/ident :db/fn :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]";

/// `edn` with its tx-fn calls expanded, and whether the store first needs
/// the `:db/fn` attribute (`edn` installs its first function). A store
/// without `:db/fn` has no functions, so nothing to expand.
fn expand_tx_fns(
    conn: &'static dyn SqlConn,
    s: &DuckStore<'_>,
    store: &str,
    edn: &str,
) -> public_traits::errors::Result<(bool, String)> {
    let has_db_fn = s
        .current_schema()?
        .get_entid(&Keyword::namespaced("db", "fn"))
        .is_some();
    if !has_db_fn {
        return Ok((edn.contains(":db/fn"), edn.to_string()));
    }
    let max_depth = std::env::var("MENTAT_TXFN_MAX_DEPTH")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(mentat_script::txfn::DEFAULT_MAX_DEPTH);
    let mut it = interpreter(conn, store);
    let expanded = mentat_script::txfn::expand_str(
        edn,
        max_depth,
        |ident| db_fn_source(s, ident),
        |form| it.eval_to_string(form),
    )
    .map_err(public_traits::errors::MentatError::TransactionFunction)?;
    Ok((false, expanded))
}

/// The live `:db/fn` source on the entity whose `:db/ident` is `ident`, if any.
fn db_fn_source(s: &DuckStore<'_>, ident: &str) -> Result<Option<String>, String> {
    let q = "[:find ?src . :in ?id :where [?e :db/ident ?id] [?e :db/fn ?src]]";
    let kw = match edn::parse::value(ident).map(|v| v.without_spans()) {
        Ok(edn::Value::Keyword(kw)) => kw,
        _ => return Ok(None),
    };
    let inputs = mentat_query_algebrizer::QueryInputs::with_value_sequence(vec![(
        edn::query::Variable::from_valid_name("?id"),
        TypedValue::Keyword(kw.into()),
    )]);
    let out = s.q(q, Some(inputs), None).map_err(|e| e.to_string())?;
    Ok(match out.results {
        QueryResults::Scalar(Some(Binding::Scalar(TypedValue::String(src)))) => {
            Some(src.as_ref().clone())
        }
        _ => None,
    })
}

/// A sandboxed, limited interpreter with the `mentat.store/*` prims over
/// DuckDB stores; a no-arg `(mentat.store/open)` opens `store`.
pub fn interpreter(conn: &'static dyn SqlConn, store: &str) -> mino_rs::Interpreter {
    let mut it = mino_rs::Interpreter::sandboxed();
    it.set_limits(SCRIPT_LIMITS);
    let backend = std::rc::Rc::new(RefCell::new(DuckBackend {
        conn,
        default_store: store.to_string(),
        stores: RefCell::new(HashMap::new()),
        next_id: Cell::new(1),
    }));
    mentat_script::install(&mut it, backend);
    it
}

/// Evaluate `src` in a fresh [`interpreter`]; returns `pr-str` of the result.
pub fn eval(conn: &'static dyn SqlConn, store: &str, src: &str) -> Result<String, String> {
    interpreter(conn, store).eval_to_string(src)
}

// ---------------------------------------------------------------------------
// Result conversion (as mentat's SQLite backend does it).
// ---------------------------------------------------------------------------

fn query_output_value(out: QueryOutput) -> Value {
    match out.results {
        QueryResults::Scalar(opt) => opt.map(binding_value).unwrap_or(Value::Nil),
        QueryResults::Coll(bs) => Value::Vector(Gc::new(PVec::from_vec(
            bs.into_iter().map(binding_value).collect(),
        ))),
        QueryResults::Tuple(opt) => match opt {
            Some(bs) => Value::Vector(Gc::new(PVec::from_vec(
                bs.into_iter().map(binding_value).collect(),
            ))),
            None => Value::Nil,
        },
        QueryResults::Rel(rel) => {
            let mut set = PSet::empty();
            for row in rel.rows() {
                let tuple: Vec<Value> = row.iter().cloned().map(binding_value).collect();
                set = set.conj(Value::Vector(Gc::new(PVec::from_vec(tuple))));
            }
            Value::Set(Gc::new(set))
        }
    }
}

fn binding_value(b: Binding) -> Value {
    match b {
        Binding::Scalar(tv) => typed_value(tv),
        Binding::Vec(v) => Value::Vector(Gc::new(PVec::from_vec(
            v.iter().cloned().map(binding_value).collect(),
        ))),
        Binding::Map(sm) => structured_map_value(&sm),
    }
}

fn structured_map_value(sm: &StructuredMap) -> Value {
    let mut m = PMap::empty();
    for (k, v) in sm.0.iter() {
        m = m.assoc(Value::Keyword(keyword_to_sym(k)), binding_value(v.clone()));
    }
    Value::Map(Gc::new(m))
}

fn typed_value(tv: TypedValue) -> Value {
    match tv {
        TypedValue::Ref(e) => Value::Int(e),
        TypedValue::Long(n) => Value::Int(n),
        TypedValue::Boolean(b) => Value::Bool(b),
        TypedValue::Double(d) => Value::Float(d.into_inner()),
        TypedValue::String(s) => mentat_script::str_val(s.as_str()),
        TypedValue::Keyword(k) => Value::Keyword(keyword_to_sym(&k)),
        TypedValue::Uuid(u) => mentat_script::values::uuid_value(&u.to_string()),
        TypedValue::Instant(t) => mentat_script::inst_value(&t.to_rfc3339()),
        TypedValue::Bytes(b) => mentat_script::str_val(&format!("{b:?}")),
    }
}

fn sym_to_keyword(sym: &Symbol) -> Keyword {
    match sym.ns.as_deref() {
        Some(ns) => Keyword::namespaced(ns, &*sym.name),
        None => Keyword::plain(&*sym.name),
    }
}

fn keyword_to_sym(k: &Keyword) -> Symbol {
    match k.namespace() {
        Some(ns) => Symbol::namespaced(ns, k.name()),
        None => Symbol::plain(k.name()),
    }
}
