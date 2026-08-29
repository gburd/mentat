//! Datomic-in-Clojure-style scripting layer for Mentat, backed by the real
//! SQLite-backed [`mentat::Store`].
//!
//! This module wraps the embedded [`mino_rs`] interpreter and registers the
//! `mentat.store/*` primitives as *native host closures* over a table of live
//! [`Store`]s. The language never sees a `Store` (it is not `Clone` and holds a
//! `rusqlite::Connection`); instead `mentat.store/open` hands back an opaque
//! integer connection handle, and every other prim looks the `Store` back up
//! host-side. EDN text is the bridge in both directions:
//!
//!   * **`Value` -> EDN string**: tx-data and queries are ordinary mino
//!     Clojure values; [`mino_rs::printer::print_str`] renders them to the EDN
//!     text that [`Store::transact`] / [`Queryable::q_once`] already accept.
//!   * **query result -> `Value`**: each [`Binding::Scalar`] carries a
//!     [`TypedValue`], which we map straight to the corresponding mino `Value`.
//!
//! Backed prims (operate on real SQLite): `open`, `transact`, `q` (and its
//! alias `q-once`), `db`, `close`. Everything mino's schemaless in-process
//! store offered but Mentat's schema-driven model does not map onto
//! (`read`/`entity`/`entities`/`datoms`/`pull`) is stubbed with an honest
//! error pointing at `mentat.store/q`.
//!
//! Gated behind the `mino` feature; nothing here is compiled for a default
//! (pure-Rust, mino-off) Mentat build.

use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;

use mino_rs::error::throw_str;
use mino_rs::printer::print_str;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Throw, Value};

use core_traits::{Binding, TypedValue};
use mentat_transaction::query::{QueryOutput, QueryResults};

use crate::store::Store;
use mentat_transaction::Queryable;

/// Host-side table of open stores, keyed by the integer handle the language
/// holds. `Rc<RefCell<..>>` so every registered prim closure can share it;
/// `next_id` hands out fresh handles.
type Stores = Rc<std::cell::RefCell<HashMap<i64, Store>>>;

/// A Mentat scripting interpreter: an embedded mino-rs interpreter whose
/// `mentat.store/*` namespace is bound to native prims over real
/// SQLite-backed [`Store`]s.
pub struct Interpreter {
    inner: mino_rs::Interpreter,
    // Kept alive for the interpreter's lifetime; the registered prims each hold
    // their own clone of this `Rc`, so this field is what closes them out when
    // the interpreter drops. Not read directly.
    _stores: Stores,
}

impl Interpreter {
    /// A fresh scripting interpreter with the `mentat.store/*` prims registered
    /// over a private table of live SQLite stores.
    pub fn new() -> Self {
        let mut inner = mino_rs::Interpreter::new();
        let stores: Stores = Rc::new(std::cell::RefCell::new(HashMap::new()));
        let next_id = Rc::new(Cell::new(1i64));

        register_store_prims(&mut inner, &stores, &next_id);

        Interpreter {
            inner,
            _stores: stores,
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

/// Register `mentat.store/*` prims, each closing over the shared store table.
///
/// `register_prim_fn` with a namespaced name binds a namespaced key that
/// `Env::get` resolves directly, so these win with no namespace alias needed
/// (and the old `mentat.store -> mino.store` alias is deliberately gone).
fn register_store_prims(inner: &mut mino_rs::Interpreter, stores: &Stores, next_id: &Rc<Cell<i64>>) {
    // open: (open) -> in-memory db, or (open "path") -> file-backed db.
    {
        let stores = stores.clone();
        let next_id = next_id.clone();
        inner.register_prim_fn("mentat.store/open", move |_it, args| {
            // Empty path -> SQLite in-memory (mentat_db::make_connection opens
            // in-memory for a zero-length path).
            let path = match args.first() {
                None | Some(Value::Nil) => String::new(),
                Some(Value::Str(s)) => s.as_str().to_string(),
                Some(other) => {
                    return Err(throw_str(&format!(
                        "mentat.store/open: path must be a string, got {}",
                        print_str(other)
                    )))
                }
            };
            let store = Store::open(&path)
                .map_err(|e| throw_str(&format!("mentat.store/open: {e}")))?;
            let id = next_id.get();
            next_id.set(id + 1);
            stores.borrow_mut().insert(id, store);
            Ok(Value::Int(id))
        });
    }

    // transact: (transact conn tx-data) -> {:tx-id N :tempids {...}}
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/transact", move |_it, args| {
            let id = conn_handle("mentat.store/transact", args.first())?;
            let tx = args.get(1).ok_or_else(|| {
                throw_str("mentat.store/transact: expected (conn tx-data)")
            })?;
            let edn = print_str(tx);
            let mut map = stores.borrow_mut();
            let store = store_for("mentat.store/transact", &mut map, id)?;
            let report = store
                .transact(&edn)
                .map_err(|e| throw_str(&format!("mentat.store/transact: {e}")))?;
            Ok(tx_report_value(&report))
        });
    }

    // q / q-once: (q conn query) -> result value (scalar, vector, or set of tuples)
    for name in ["mentat.store/q", "mentat.store/q-once"] {
        let stores = stores.clone();
        inner.register_prim_fn(name, move |_it, args| {
            let id = conn_handle("mentat.store/q", args.first())?;
            let query = args.get(1).ok_or_else(|| {
                throw_str("mentat.store/q: expected (conn query)")
            })?;
            let edn = print_str(query);
            let map = stores.borrow();
            let store = map.get(&id).ok_or_else(|| {
                throw_str(&format!("mentat.store/q: no open store for handle {id}"))
            })?;
            let out = store
                .q_once(&edn, None)
                .map_err(|e| throw_str(&format!("mentat.store/q: {e}")))?;
            Ok(query_output_value(out))
        });
    }

    // db: identity-on-conn. Mentat queries run against the live Conn, not an
    // immutable db value, so `(mentat.store/db conn)` just yields the handle.
    {
        inner.register_prim_fn("mentat.store/db", move |_it, args| {
            let id = conn_handle("mentat.store/db", args.first())?;
            Ok(Value::Int(id))
        });
    }

    // close: (close conn) -> nil. Drops the Store, closing its SQLite handle.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/close", move |_it, args| {
            let id = conn_handle("mentat.store/close", args.first())?;
            stores.borrow_mut().remove(&id);
            Ok(Value::Nil)
        });
    }

    // Unsupported by the schema-driven SQLite backing: honest errors, not fakes.
    for name in [
        "mentat.store/read",
        "mentat.store/entity",
        "mentat.store/entities",
        "mentat.store/datoms",
        "mentat.store/pull",
    ] {
        let short = name.rsplit('/').next().unwrap_or(name).to_string();
        inner.register_prim_fn(name, move |_it, _args| {
            Err(throw_str(&format!(
                "mentat.store/{short}: not supported by the SQLite backing; \
                 use mentat.store/q with a [:find ... :where ...] query"
            )))
        });
    }
}

/// Extract an integer connection handle from a prim argument.
fn conn_handle(prim: &str, arg: Option<&Value>) -> Result<i64, Throw> {
    match arg {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(throw_str(&format!(
            "{prim}: first arg must be a connection handle (from mentat.store/open)"
        ))),
    }
}

/// Look up a mutable store by handle, erroring if it is closed/unknown.
fn store_for<'a>(
    prim: &str,
    map: &'a mut HashMap<i64, Store>,
    id: i64,
) -> Result<&'a mut Store, Throw> {
    map.get_mut(&id)
        .ok_or_else(|| throw_str(&format!("{prim}: no open store for handle {id}")))
}

/// A `TxReport` as a mino map `{:tx-id N :tempids {"t" E ...}}`.
fn tx_report_value(report: &crate::TxReport) -> Value {
    use mino_rs::collections::map::PMap;
    let mut tempids = PMap::empty();
    for (k, v) in &report.tempids {
        tempids = tempids.assoc(str_val(k), Value::Int(*v));
    }
    let m = PMap::empty()
        .assoc(kw_ns("mentat.store", "tx-id"), Value::Int(report.tx_id))
        .assoc(kw_ns("mentat.store", "tempids"), Value::Map(Gc::new(tempids)));
    Value::Map(Gc::new(m))
}

/// A `QueryOutput` as a mino value: Scalar -> the value (nil if empty),
/// Coll -> a vector, Tuple -> a vector, Rel -> a set of tuple-vectors (a
/// Datomic relation is a set; a mino set of vectors is the natural mapping).
fn query_output_value(out: QueryOutput) -> Value {
    use mino_rs::collections::map::PSet;
    use mino_rs::collections::vector::PVec;
    match out.results {
        QueryResults::Scalar(opt) => opt.map(binding_value).unwrap_or(Value::Nil),
        QueryResults::Coll(bs) => {
            Value::Vector(Gc::new(PVec::from_vec(bs.into_iter().map(binding_value).collect())))
        }
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

/// A single query `Binding` as a mino `Value`. Only scalar bindings arise from
/// the `[:find ... :where ...]` queries this layer runs; nested Vec/Map
/// bindings (from pull expressions, which we do not expose) become nil.
fn binding_value(b: Binding) -> Value {
    match b {
        Binding::Scalar(tv) => typed_value(tv),
        // Pull results are not exposed by this layer.
        Binding::Vec(_) | Binding::Map(_) => Value::Nil,
    }
}

/// A Mentat `TypedValue` as a mino `Value`. Ref/Long -> Int, Double -> Float,
/// Boolean -> Bool, String -> Str, Keyword -> Keyword. Uuid/Instant have no
/// distinct mino tier, so they render to their string form; Bytes likewise.
fn typed_value(tv: TypedValue) -> Value {
    match tv {
        TypedValue::Ref(e) => Value::Int(e),
        TypedValue::Long(n) => Value::Int(n),
        TypedValue::Boolean(b) => Value::Bool(b),
        TypedValue::Double(d) => Value::Float(d.into_inner()),
        TypedValue::String(s) => str_val(s.as_str()),
        TypedValue::Keyword(k) => match k.namespace() {
            Some(ns) => Value::Keyword(Symbol::namespaced(ns, k.name())),
            None => Value::Keyword(Symbol::plain(k.name())),
        },
        TypedValue::Uuid(u) => str_val(&u.to_string()),
        TypedValue::Instant(t) => str_val(&t.to_rfc3339()),
        TypedValue::Bytes(b) => str_val(&format!("{b:?}")),
    }
}

fn str_val(s: &str) -> Value {
    Value::Str(Gc::new(s.to_string()))
}

fn kw_ns(ns: &str, name: &str) -> Value {
    Value::Keyword(Symbol::namespaced(ns, name))
}
