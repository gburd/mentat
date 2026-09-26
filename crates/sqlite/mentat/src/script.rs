//! Datomic-in-Clojure-style scripting layer for Mentat, backed by the real
//! SQLite-backed [`mentat::Store`].
//!
//! This module implements the shared [`mentat_script::ScriptBackend`] trait
//! over live SQLite-backed [`Store`]s and wraps the embedded [`mino_rs`]
//! interpreter. The backend-independent `mentat.store/*` surface — the
//! db-value shape, argument parsing, the tx-report / inst / uuid value
//! builders, and the prim registration — lives in the `mentat_script` crate
//! (plan § 1.19). What stays here is (a) the [`Store`] storage impl and (b) the
//! `Binding`/`TypedValue`/`edn::Value` → mino `Value` conversion.
//!
//! # The Datomic-a-like model
//!
//! A *database value* is an immutable basis, not a mutable connection. A conn
//! handle is an opaque `Int` from `mentat.store/open`; a db value is an
//! immutable snapshot map carrying its conn, basis-tx, and any as-of/since
//! bound (see `mentat_script::db_value`). Reads take a db value.
//!
//! # Temporal honesty
//!
//! Mentat's algebrizer has no as-of query rewriting, so:
//!
//!   * `datoms`/`entity`/`read` on the **current** basis use the live `datoms`
//!     table; `since T` uses `debug::datoms_after`; `as-of T` is reconstructed
//!     by replaying `transactions_after` up to and including T.
//!   * `q` (arbitrary Datalog) is only supported on the current basis; on an
//!     as-of/since db it returns an honest error.
//!
//! # inst/uuid representation
//!
//! Since the mino refresh (Task 8), `#uuid` reads to a real `Value::Uuid` and
//! `#inst` to the `clojure.instant/read-instant-date` constructor form; the
//! shared `inst_value`/`uuid_value` builders emit exactly those, so a
//! `TypedValue::Instant`/`Uuid` round-trips through the reader.
//!
//! Gated behind the `mino` feature.

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use mino_rs::collections::map::{PMap, PSet};
use mino_rs::collections::vector::PVec;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Value};

use core_traits::{Binding, Entid, StructuredMap, TypedValue};
use edn::entities::EntidOrIdent;
use mentat_core::{HasSchema, Keyword};
use mentat_db::debug;
use mentat_script::{DbRef, ScriptBackend, TxReport};
use mentat_transaction::query::{QueryOutput, QueryResults};
use mentat_transaction::{Pullable, Queryable};

use crate::store::Store;

/// Per-eval resource limits for scripts. `depth` 1000 is safe on an 8 MB
/// (main-thread) stack even in a debug build.
const SCRIPT_LIMITS: mino_rs::Limits = mino_rs::Limits {
    steps: Some(10_000_000),
    heap_bytes: Some(64 * 1024 * 1024),
    depth: Some(1000),
};

/// The Mentat storage backend: a private table of live SQLite [`Store`]s keyed
/// by the integer handle the language holds. Interior mutability lets the
/// `&self` read methods and the mutating open/close/transact/with share it.
struct MentatBackend {
    stores: RefCell<HashMap<i64, Store>>,
    next_id: Cell<i64>,
    /// What a no-arg `(mentat.store/open)` opens; `None` = in-memory (`""`).
    default_path: Option<String>,
}

impl MentatBackend {
    fn new(default_path: Option<String>) -> Self {
        MentatBackend {
            stores: RefCell::new(HashMap::new()),
            next_id: Cell::new(1),
            default_path,
        }
    }
}

/// A Mentat scripting interpreter: an embedded mino-rs interpreter whose
/// `mentat.store/*` namespace is bound (via [`mentat_script::install`]) to the
/// [`MentatBackend`] over real SQLite-backed [`Store`]s.
pub struct Interpreter {
    inner: mino_rs::Interpreter,
    // Kept alive for the interpreter's lifetime; the installed prims each hold
    // their own clone of this `Rc`.
    _backend: Rc<RefCell<MentatBackend>>,
}

impl Interpreter {
    /// A fresh scripting interpreter with the `mentat.store/*` prims registered
    /// over a private table of live SQLite stores.
    pub fn new() -> Self {
        Self::build(None)
    }

    /// Like [`Interpreter::new`], but a no-arg `(mentat.store/open)` opens the
    /// store at `path` instead of a fresh in-memory one. Hosts that own a
    /// database (e.g. the DuckDB extension's `edn_eval(db_path, script)`) use
    /// this so scripts act on that database without naming a path themselves.
    pub fn with_default_path(path: &str) -> Self {
        Self::build(Some(path.to_string()))
    }

    fn build(default_path: Option<String>) -> Self {
        // Sandboxed: scripts get no host filesystem; every top-level eval is
        // bounded in steps, heap, and depth (see `SCRIPT_LIMITS`).
        let mut inner = mino_rs::Interpreter::sandboxed();
        inner.set_limits(SCRIPT_LIMITS);
        let backend = Rc::new(RefCell::new(MentatBackend::new(default_path)));
        mentat_script::install(&mut inner, backend.clone());
        Interpreter {
            inner,
            _backend: backend,
        }
    }

    /// Eval a source string; the error is the printed exception.
    pub fn eval(&mut self, src: &str) -> Result<Value, String> {
        self.inner.eval(src)
    }

    /// Eval a source string and return `pr-str` of the result (EDN text).
    pub fn eval_to_string(&mut self, src: &str) -> Result<String, String> {
        self.inner.eval_to_string(src)
    }

    /// The underlying mino-rs interpreter, for host extensions.
    pub fn inner(&mut self) -> &mut mino_rs::Interpreter {
        &mut self.inner
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ScriptBackend over SQLite Store
// ---------------------------------------------------------------------------

impl ScriptBackend for MentatBackend {
    fn open(&mut self, path: Option<&str>) -> Result<i64, String> {
        let path = path.or(self.default_path.as_deref()).unwrap_or("");
        let store = Store::open(path).map_err(|e| e.to_string())?;
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.stores.borrow_mut().insert(id, store);
        Ok(id)
    }

    fn close(&mut self, conn: i64) -> Result<(), String> {
        self.stores.borrow_mut().remove(&conn);
        Ok(())
    }

    fn basis_tx(&self, conn: i64) -> Result<i64, String> {
        let map = self.stores.borrow();
        let store = map
            .get(&conn)
            .ok_or_else(|| format!("no open store for handle {conn}"))?;
        Ok(store.last_tx_id())
    }

    fn transact(&mut self, conn: i64, edn: &str) -> Result<TxReport, String> {
        let mut map = self.stores.borrow_mut();
        let store = map
            .get_mut(&conn)
            .ok_or_else(|| format!("no open store for handle {conn}"))?;
        let report = store.transact(edn).map_err(|e| e.to_string())?;
        Ok(to_tx_report(&report))
    }

    fn with(&mut self, db: &DbRef, edn: &str) -> Result<(i64, TxReport), String> {
        let mut map = self.stores.borrow_mut();
        let store = map
            .get_mut(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let report = store.with_speculative(edn).map_err(|e| e.to_string())?;
        Ok((report.tx_id, to_tx_report(&report)))
    }

    fn q(&self, db: &DbRef, query_edn: &str) -> Result<Value, String> {
        if db.as_of.is_some() || db.since.is_some() {
            return Err(
                "arbitrary Datalog against an as-of/since basis is not supported (Mentat has no \
                 as-of query rewriting). Use the current-basis q, or datoms/entity/read/pull on \
                 this db."
                    .into(),
            );
        }
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let out = store.q_once(query_edn, None).map_err(|e| e.to_string())?;
        Ok(query_output_value(out))
    }

    fn pull(&self, db: &DbRef, eid: i64, pattern_edn: &str) -> Result<Value, String> {
        if db.as_of.is_some() || db.since.is_some() {
            return Err(
                "pull against an as-of/since basis is not supported; use entity/read/datoms on \
                 this db (which reconstruct the basis)."
                    .into(),
            );
        }
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let schema = store.conn().current_schema();
        let attr_entids = pull_attr_entids(&schema, pattern_edn)?;
        let sm = store
            .pull_attributes_for_entity(eid, attr_entids.iter().copied())
            .map_err(|e| e.to_string())?;
        Ok(structured_map_value(&sm))
    }

    fn datoms(&self, db: &DbRef) -> Result<Value, String> {
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let rows = basis_datoms(db, store)?;
        let tuples: Vec<Value> = rows
            .into_iter()
            .map(|(e, a, v)| {
                Value::Vector(Gc::new(PVec::from_vec(vec![
                    Value::Int(e),
                    Value::Keyword(keyword_to_sym(&a)),
                    edn_value(&v),
                ])))
            })
            .collect();
        Ok(Value::Vector(Gc::new(PVec::from_vec(tuples))))
    }

    fn entity(&self, db: &DbRef, eid: i64) -> Result<Value, String> {
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let rows = basis_datoms(db, store)?;
        // Group values by attribute for this eid, preserving first-seen order.
        let mut by_attr: Vec<(Keyword, Vec<edn::Value>)> = Vec::new();
        for (e, a, v) in rows {
            if e != eid {
                continue;
            }
            match by_attr.iter_mut().find(|(k, _)| *k == a) {
                Some((_, vals)) => vals.push(v),
                None => by_attr.push((a, vec![v])),
            }
        }
        let schema = store.conn().current_schema();
        let mut m = PMap::empty().assoc(mentat_script::kw_ns("db", "id"), Value::Int(eid));
        for (a, vals) in by_attr {
            let many = schema
                .attribute_for_ident(&a)
                .map(|(attr, _)| attr.multival)
                .unwrap_or(false);
            let key = Value::Keyword(keyword_to_sym(&a));
            if many {
                let mut set = PSet::empty();
                for v in &vals {
                    set = set.conj(edn_value(v));
                }
                m = m.assoc(key, Value::Set(Gc::new(set)));
            } else if let Some(v) = vals.first() {
                m = m.assoc(key, edn_value(v));
            }
        }
        Ok(Value::Map(Gc::new(m)))
    }

    fn read(&self, db: &DbRef, eid: i64, attr: &str) -> Result<Value, String> {
        let kw = parse_kw(attr);
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let schema = store.conn().current_schema();
        let many = schema
            .attribute_for_ident(&kw)
            .map(|(a, _)| a.multival)
            .unwrap_or(false);

        // Current basis: live lookup fns. Otherwise: filter basis datoms.
        if db.as_of.is_none() && db.since.is_none() {
            if many {
                let vals = store
                    .lookup_values_for_attribute(eid, &kw)
                    .map_err(|e| e.to_string())?;
                let mut set = PSet::empty();
                for tv in vals {
                    set = set.conj(typed_value(tv));
                }
                Ok(Value::Set(Gc::new(set)))
            } else {
                let v = store
                    .lookup_value_for_attribute(eid, &kw)
                    .map_err(|e| e.to_string())?;
                Ok(v.map(typed_value).unwrap_or(Value::Nil))
            }
        } else {
            let rows = basis_datoms(db, store)?;
            let target = kw.to_string();
            let vals: Vec<edn::Value> = rows
                .into_iter()
                .filter(|(e, a, _)| *e == eid && a.to_string() == target)
                .map(|(_, _, v)| v)
                .collect();
            if many {
                let mut set = PSet::empty();
                for v in &vals {
                    set = set.conj(edn_value(v));
                }
                Ok(Value::Set(Gc::new(set)))
            } else {
                Ok(vals.first().map(edn_value).unwrap_or(Value::Nil))
            }
        }
    }

    fn entities(&self, db: &DbRef, attr: &str) -> Result<Value, String> {
        if db.as_of.is_some() || db.since.is_some() {
            return Err("not supported on an as-of/since basis; use datoms.".into());
        }
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        let query = format!("[:find ?e :where [?e {attr} _]]");
        let out = store.q_once(&query, None).map_err(|e| e.to_string())?;
        // Project the relation of eids to a set of the scalar eids.
        match out.results {
            QueryResults::Rel(rel) => {
                let mut set = PSet::empty();
                for row in rel.rows() {
                    if let Some(b) = row.iter().next() {
                        set = set.conj(binding_value(b.clone()));
                    }
                }
                Ok(Value::Set(Gc::new(set)))
            }
            other => Ok(query_output_value(QueryOutput {
                spec: out.spec,
                results: other,
            })),
        }
    }

    fn resolve_eid(&self, db: &DbRef, arg: &Value) -> Result<i64, String> {
        let map = self.stores.borrow();
        let store = map
            .get(&db.conn)
            .ok_or_else(|| format!("no open store for handle {}", db.conn))?;
        match arg {
            Value::Int(n) => Ok(*n),
            Value::Keyword(sym) => {
                let kw = sym_to_keyword(sym);
                store
                    .conn()
                    .current_schema()
                    .get_entid(&kw)
                    .map(|k| k.0)
                    .ok_or_else(|| format!("unknown ident {kw}"))
            }
            Value::Vector(v) if v.len() == 2 => {
                let attr = v.nth(0).unwrap();
                let val = v.nth(1).unwrap();
                let attr_kw = match attr {
                    Value::Keyword(sym) => sym_to_keyword(sym),
                    _ => return Err("lookup-ref attr must be a keyword".into()),
                };
                let query = format!(
                    "[:find ?e . :where [?e {} {}]]",
                    attr_kw,
                    mino_rs::printer::print_str(val)
                );
                let out = store.q_once(&query, None).map_err(|e| e.to_string())?;
                match out.results {
                    QueryResults::Scalar(Some(Binding::Scalar(TypedValue::Ref(e)))) => Ok(e),
                    QueryResults::Scalar(Some(Binding::Scalar(TypedValue::Long(e)))) => Ok(e),
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

// ---------------------------------------------------------------------------
// Basis reconstruction (current / as-of / since)
// ---------------------------------------------------------------------------

/// The datom set backing a db value's reads, as `(e, keyword-a, edn-v)` rows.
fn basis_datoms(db: &DbRef, store: &Store) -> Result<Vec<(i64, Keyword, edn::Value)>, String> {
    let sqlite = store.sqlite_ref();
    let schema = store.conn().current_schema();

    let to_kw = |a: &EntidOrIdent| -> Keyword {
        match a {
            EntidOrIdent::Ident(k) => k.clone(),
            EntidOrIdent::Entid(e) => Keyword::plain(format!("db/entid-{e}")),
        }
    };
    let eid = |e: &EntidOrIdent| -> i64 {
        match e {
            EntidOrIdent::Entid(n) => *n,
            EntidOrIdent::Ident(_) => 0,
        }
    };

    if let Some(t) = db.as_of {
        const TX0: i64 = 0x1000_0000;
        let txns = debug::transactions_after(sqlite, &*schema, TX0 - 1)
            .map_err(|e| format!("transactions_after: {e}"))?;
        let mut live: BTreeMap<(i64, String, String), (i64, Keyword, edn::Value)> = BTreeMap::new();
        for group in &txns.0 {
            for d in &group.0 {
                if d.tx > t {
                    continue;
                }
                let e = eid(&d.e);
                let a = to_kw(&d.a);
                let key = (e, a.to_string(), print_edn(&d.v));
                match d.added {
                    Some(false) => {
                        live.remove(&key);
                    }
                    _ => {
                        live.insert(key, (e, a, d.v.clone()));
                    }
                }
            }
        }
        Ok(live.into_values().collect())
    } else if let Some(s) = db.since {
        let ds =
            debug::datoms_after(sqlite, &*schema, s).map_err(|e| format!("datoms_after: {e}"))?;
        Ok(ds
            .0
            .into_iter()
            .map(|d| (eid(&d.e), to_kw(&d.a), d.v))
            .collect())
    } else {
        let ds = debug::datoms(sqlite, &*schema).map_err(|e| format!("datoms: {e}"))?;
        Ok(ds
            .0
            .into_iter()
            .map(|d| (eid(&d.e), to_kw(&d.a), d.v))
            .collect())
    }
}

/// Parse a minimal pull pattern (EDN text) into a list of attribute entids.
/// Supports a vector of attr keywords and the `*` wildcard.
fn pull_attr_entids(schema: &mentat_core::Schema, pattern_edn: &str) -> Result<Vec<Entid>, String> {
    let (pattern, _) =
        mino_rs::reader::read_one(pattern_edn).map_err(|e| format!("bad pull pattern: {e}"))?;
    let items = match &pattern {
        Value::Vector(v) => v.iter().cloned().collect::<Vec<_>>(),
        _ => return Err("pattern must be a vector of attribute keywords (or [*])".into()),
    };
    let mut out = Vec::new();
    for item in items {
        match item {
            Value::Sym(ref s) if &*s.name == "*" && s.ns.is_none() => {
                for entid in schema.attribute_map.keys() {
                    out.push(*entid);
                }
            }
            Value::Keyword(ref sym) => {
                let kw = sym_to_keyword(sym);
                match schema.get_entid(&kw) {
                    Some(k) => out.push(k.0),
                    None => return Err(format!("unknown attribute {kw}")),
                }
            }
            other => {
                return Err(format!(
                    "unsupported pull item {} (only attr keywords and * are supported)",
                    mino_rs::printer::print_str(&other)
                ))
            }
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Result conversion: Binding / TypedValue / edn::Value -> mino Value
// ---------------------------------------------------------------------------

/// Convert a mentat `TxReport` to the backend-independent shared shape.
fn to_tx_report(report: &crate::TxReport) -> TxReport {
    TxReport {
        tx_id: report.tx_id,
        tempids: report
            .tempids
            .iter()
            .map(|(k, v)| (k.clone(), *v))
            .collect(),
    }
}

/// A `QueryOutput` as a mino value: Scalar -> the value (nil if empty),
/// Coll -> a vector, Tuple -> a vector, Rel -> a set of tuple-vectors.
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

/// A single query `Binding` as a mino `Value`, recursively.
fn binding_value(b: Binding) -> Value {
    match b {
        Binding::Scalar(tv) => typed_value(tv),
        Binding::Vec(v) => Value::Vector(Gc::new(PVec::from_vec(
            v.iter().cloned().map(binding_value).collect(),
        ))),
        Binding::Map(sm) => structured_map_value(&sm),
    }
}

/// A pull `StructuredMap` as a mino map `{:attr value, ...}` (recursive).
fn structured_map_value(sm: &StructuredMap) -> Value {
    let mut m = PMap::empty();
    for (k, v) in sm.0.iter() {
        m = m.assoc(Value::Keyword(keyword_to_sym(k)), binding_value(v.clone()));
    }
    Value::Map(Gc::new(m))
}

/// A Mentat `TypedValue` as a mino `Value`. Instant/Uuid use the shared
/// builders (which emit the reader-round-tripping forms).
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

/// An `edn::Value` (as carried by a `Datom`) as a mino `Value`.
fn edn_value(v: &edn::Value) -> Value {
    use edn::Value as E;
    match v {
        E::Nil => Value::Nil,
        E::Boolean(b) => Value::Bool(*b),
        E::Integer(n) => Value::Int(*n),
        E::BigInteger(_) => mentat_script::str_val(&print_edn(v)),
        E::Float(f) => Value::Float(f.into_inner()),
        E::Text(s) => mentat_script::str_val(s),
        E::Instant(t) => mentat_script::inst_value(&t.to_rfc3339()),
        E::Uuid(u) => mentat_script::values::uuid_value(&u.to_string()),
        E::Keyword(k) => Value::Keyword(keyword_to_sym(k)),
        E::PlainSymbol(s) => Value::Sym(Symbol::plain(s.to_string().trim_start_matches('\''))),
        E::NamespacedSymbol(_) => mentat_script::str_val(&print_edn(v)),
        E::Vector(items) => Value::Vector(Gc::new(PVec::from_vec(
            items.iter().map(edn_value).collect(),
        ))),
        E::List(items) => Value::Vector(Gc::new(PVec::from_vec(
            items.iter().map(edn_value).collect(),
        ))),
        E::Set(items) => {
            let mut set = PSet::empty();
            for i in items {
                set = set.conj(edn_value(i));
            }
            Value::Set(Gc::new(set))
        }
        E::Map(entries) => {
            let mut m = PMap::empty();
            for (k, val) in entries {
                m = m.assoc(edn_value(k), edn_value(val));
            }
            Value::Map(Gc::new(m))
        }
        E::Bytes(b) => mentat_script::str_val(&format!("{b:?}")),
    }
}

// ---------------------------------------------------------------------------
// Keyword helpers
// ---------------------------------------------------------------------------

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

/// Parse `:ns/name` / `:name` text (the keyword ABI the shared layer feeds to
/// `read`/`entities`) back to a Mentat `Keyword`.
fn parse_kw(text: &str) -> Keyword {
    let rest = text.strip_prefix(':').unwrap_or(text);
    match rest.split_once('/') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => Keyword::namespaced(ns, name),
        _ => Keyword::plain(rest),
    }
}

/// `edn::Value` -> its pretty EDN text (for bigints/symbols and datom keys).
fn print_edn(v: &edn::Value) -> String {
    v.to_pretty(200).unwrap_or_else(|_| format!("{v:?}"))
}
