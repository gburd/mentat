//! Datomic-in-Clojure-style scripting layer for Mentat, backed by the real
//! SQLite-backed [`mentat::Store`].
//!
//! This module wraps the embedded [`mino_rs`] interpreter and registers the
//! `mentat.store/*` primitives as *native host closures* over a table of live
//! [`Store`]s. The language never sees a `Store` (it is not `Clone` and holds a
//! `rusqlite::Connection`); instead `mentat.store/open` hands back an opaque
//! integer connection handle.
//!
//! # The Datomic-a-like model
//!
//! Rich Hickey's crux (point #1): a *database value* is an immutable basis, not
//! a mutable connection. Two tiers of handle exist here:
//!
//!   * **conn handle** — an opaque `Int` from `mentat.store/open`. Mutating and
//!     lifecycle ops (`transact`, `close`) take a conn.
//!   * **db value** — an immutable snapshot, represented as a mino map
//!     ```clojure
//!     {:mentat.store/db true,      ; tag
//!      :mentat.store/conn N,       ; which store
//!      :mentat.store/basis-tx T,   ; the basis tx id
//!      :mentat.store/as-of A,      ; nil, or an as-of point-in-time tx
//!      :mentat.store/since S}      ; nil, or a since floor tx
//!     ```
//!     `mentat.store/db` turns a conn into a db value at the current basis;
//!     `as-of`/`since`/`with` return *new* db values. All reads (`q`, `pull`,
//!     `entity`, `read`, `entities`, `datoms`) take a db value (a bare conn
//!     `Int` is accepted for ergonomics and treated as the current db).
//!
//! # Temporal honesty
//!
//! Mentat's algebrizer has NO as-of query rewriting: arbitrary Datalog cannot
//! be run against a historical point-in-time at the SQL level. So:
//!
//!   * `datoms`/`entity`/`read`/`pull` on the **current** basis use the live
//!     `datoms` table (`debug::datoms`, `pull_attributes_for_entity`, lookups).
//!   * `since T` uses `debug::datoms_after(.., T)` (datoms asserted after T).
//!   * `as-of T` is reconstructed for the *read* path by replaying
//!     `debug::transactions_after(.., TX0-1)` up to and including tx T,
//!     applying each datom's `added`/retracted flag, to rebuild the datom set
//!     as of T. `entity`/`read`/`pull` on an as-of/since db route through this
//!     reconstructed datom set.
//!   * `q` (arbitrary Datalog) is only supported on the current basis. On an
//!     as-of/since db it returns an **honest error**; it does not silently run
//!     against the current basis and pretend.
//!
//! # inst/uuid representation
//!
//! This mino-rs port has no `#uuid`/`read-string` and reads `#inst "..."` to a
//! plain calendar map. To keep tagged temporal/uuid values *round-tripping
//! through the reader*, a `TypedValue::Instant`/`Uuid` (and the matching
//! `edn::Value` variants) is emitted as the exact cons form that
//! `mino_rs::reader::read_one` produces from the literal:
//! `(clojure.instant/read-instant-date "RFC3339")` for an inst and
//! `(parse-uuid "uuid")` for a uuid. Hence `read_one(print_str(v)) == v` (the
//! printed call form re-reads to the identical form) and a source
//! `#inst "..."` / `#uuid "..."` literal reads to the same value.
//!
//! Gated behind the `mino` feature; nothing here is compiled for a default
//! (pure-Rust, mino-off) Mentat build.

use std::cell::Cell;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::rc::Rc;

use mino_rs::collections::map::{PMap, PSet};
use mino_rs::collections::vector::PVec;
use mino_rs::error::throw_str;
use mino_rs::printer::print_str;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Throw, Value};

use core_traits::{Binding, Entid, StructuredMap, TypedValue};
use edn::entities::EntidOrIdent;
use mentat_core::{HasSchema, Keyword};
use mentat_db::debug;
use mentat_transaction::query::{QueryOutput, QueryResults};
use mentat_transaction::{Pullable, Queryable};

use crate::store::Store;

/// Host-side table of open stores, keyed by the integer handle the language
/// holds. `Rc<RefCell<..>>` so every registered prim closure can share it;
/// `next_id` hands out fresh handles.
type Stores = Rc<std::cell::RefCell<HashMap<i64, Store>>>;

/// Per-eval resource limits for scripts. `depth` 1000 is safe on an 8 MB
/// (main-thread) stack even in a debug build: the worst frame mix measured
/// (destructuring + let + try + macro per call) overflows 8 MB at ~2600 depth
/// units in debug, so 1000 leaves >2x headroom (see `mino_rs::Limits`).
const SCRIPT_LIMITS: mino_rs::Limits = mino_rs::Limits {
    steps: Some(10_000_000),
    heap_bytes: Some(64 * 1024 * 1024),
    depth: Some(1000),
};

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
        // Sandboxed: scripts get no host filesystem and captured print
        // output; every top-level eval is bounded in steps, heap, and depth
        // (see `SCRIPT_LIMITS`).
        let mut inner = mino_rs::Interpreter::sandboxed();
        inner.set_limits(SCRIPT_LIMITS);
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

// ---------------------------------------------------------------------------
// Prim registration
// ---------------------------------------------------------------------------

/// Register `mentat.store/*` prims, each closing over the shared store table.
fn register_store_prims(
    inner: &mut mino_rs::Interpreter,
    stores: &Stores,
    next_id: &Rc<Cell<i64>>,
) {
    // open: (open) -> conn handle (in-memory), or (open "path") -> file-backed.
    {
        let stores = stores.clone();
        let next_id = next_id.clone();
        inner.register_prim_fn("mentat.store/open", move |_it, args| {
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
            let store =
                Store::open(&path).map_err(|e| throw_str(&format!("mentat.store/open: {e}")))?;
            let id = next_id.get();
            next_id.set(id + 1);
            stores.borrow_mut().insert(id, store);
            Ok(Value::Int(id))
        });
    }

    // transact: (transact conn tx-data) -> {:mentat.store/tx-id N ...}. Commits.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/transact", move |_it, args| {
            let id = conn_handle("mentat.store/transact", args.first())?;
            let tx = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/transact: expected (conn tx-data)"))?;
            let edn = print_str(tx);
            let mut map = stores.borrow_mut();
            let store = store_for("mentat.store/transact", &mut map, id)?;
            let report = store
                .transact(&edn)
                .map_err(|e| throw_str(&format!("mentat.store/transact: {e}")))?;
            Ok(tx_report_value(&report, None))
        });
    }

    // db: (db conn) -> db value at the current basis.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/db", move |_it, args| {
            let id = conn_handle("mentat.store/db", args.first())?;
            let map = stores.borrow();
            let store = map.get(&id).ok_or_else(|| {
                throw_str(&format!("mentat.store/db: no open store for handle {id}"))
            })?;
            Ok(db_value(id, store.last_tx_id(), None, None))
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

    // q / q-once: (q db query). Only the CURRENT basis supports arbitrary
    // Datalog; an as-of/since db errors honestly.
    for name in ["mentat.store/q", "mentat.store/q-once"] {
        let stores = stores.clone();
        inner.register_prim_fn(name, move |_it, args| {
            let db = destructure_db("mentat.store/q", args.first())?;
            let query = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/q: expected (db query)"))?;
            if db.as_of.is_some() || db.since.is_some() {
                return Err(throw_str(
                    "mentat.store/q: arbitrary Datalog against an as-of/since basis is not \
                     supported (Mentat has no as-of query rewriting). Use the current-basis q, \
                     or datoms/entity/read/pull on this db.",
                ));
            }
            let edn = print_str(query);
            let map = stores.borrow();
            let store = map.get(&db.conn).ok_or_else(|| {
                throw_str(&format!(
                    "mentat.store/q: no open store for handle {}",
                    db.conn
                ))
            })?;
            let out = store
                .q_once(&edn, None)
                .map_err(|e| throw_str(&format!("mentat.store/q: {e}")))?;
            Ok(query_output_value(out))
        });
    }

    // pull: (pull db eid pattern) -> a map, or (pull db [eids] pattern) -> vec.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/pull", move |_it, args| {
            let db = destructure_db("mentat.store/pull", args.first())?;
            let eid_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/pull: expected (db eid pattern)"))?;
            let pattern = args
                .get(2)
                .ok_or_else(|| throw_str("mentat.store/pull: expected (db eid pattern)"))?;
            let map = stores.borrow();
            let store = map.get(&db.conn).ok_or_else(|| {
                throw_str(&format!(
                    "mentat.store/pull: no open store for handle {}",
                    db.conn
                ))
            })?;
            let schema = store.conn().current_schema();
            let attr_entids = pull_attr_entids(&schema, store, pattern)?;

            match eid_arg {
                Value::Vector(v) => {
                    let mut out = Vec::with_capacity(v.len());
                    for e in v.iter() {
                        let entid = resolve_eid("mentat.store/pull", store, e)?;
                        out.push(pull_one(&db, store, entid, &attr_entids)?);
                    }
                    Ok(Value::Vector(Gc::new(PVec::from_vec(out))))
                }
                other => {
                    let entid = resolve_eid("mentat.store/pull", store, other)?;
                    pull_one(&db, store, entid, &attr_entids)
                }
            }
        });
    }

    // entity: (entity db eid) -> {:db/id eid, :attr v, ...}.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/entity", move |_it, args| {
            let db = destructure_db("mentat.store/entity", args.first())?;
            let eid_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/entity: expected (db eid)"))?;
            let map = stores.borrow();
            let store = map.get(&db.conn).ok_or_else(|| {
                throw_str(&format!(
                    "mentat.store/entity: no open store for handle {}",
                    db.conn
                ))
            })?;
            let entid = resolve_eid("mentat.store/entity", store, eid_arg)?;
            entity_map(&db, store, entid)
        });
    }

    // read: (read db eid attr) -> value. Cardinality-one -> scalar,
    // cardinality-many -> a set of values.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/read", move |_it, args| {
            let db = destructure_db("mentat.store/read", args.first())?;
            let eid_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/read: expected (db eid attr)"))?;
            let attr_arg = args
                .get(2)
                .ok_or_else(|| throw_str("mentat.store/read: expected (db eid attr)"))?;
            let kw = keyword_arg("mentat.store/read", attr_arg)?;
            let map = stores.borrow();
            let store = map.get(&db.conn).ok_or_else(|| {
                throw_str(&format!(
                    "mentat.store/read: no open store for handle {}",
                    db.conn
                ))
            })?;
            let entid = resolve_eid("mentat.store/read", store, eid_arg)?;
            read_attr(&db, store, entid, &kw)
        });
    }

    // entities: (entities db attr) -> a set of eids having that attr.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/entities", move |_it, args| {
            let db = destructure_db("mentat.store/entities", args.first())?;
            let attr_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/entities: expected (db attr)"))?;
            let kw = keyword_arg("mentat.store/entities", attr_arg)?;
            if db.as_of.is_some() || db.since.is_some() {
                return Err(throw_str(
                    "mentat.store/entities: not supported on an as-of/since basis; use datoms.",
                ));
            }
            let map = stores.borrow();
            let store = map.get(&db.conn).ok_or_else(|| {
                throw_str(&format!(
                    "mentat.store/entities: no open store for handle {}",
                    db.conn
                ))
            })?;
            let query = format!("[:find ?e :where [?e {} _]]", kw);
            let out = store
                .q_once(&query, None)
                .map_err(|e| throw_str(&format!("mentat.store/entities: {e}")))?;
            Ok(query_output_value(out))
        });
    }

    // datoms: (datoms db) -> a vector of [e a v tx added] tuples for the basis.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/datoms", move |_it, args| {
            let db = destructure_db("mentat.store/datoms", args.first())?;
            let map = stores.borrow();
            let store = map.get(&db.conn).ok_or_else(|| {
                throw_str(&format!(
                    "mentat.store/datoms: no open store for handle {}",
                    db.conn
                ))
            })?;
            datoms_value(&db, store)
        });
    }

    // with: (with db tx-data) -> {:mentat.store/db-after db', :mentat.store/tx-report {...}}.
    // Speculative: transacts, returns a new db value at the resulting basis,
    // and does NOT commit.
    {
        let stores = stores.clone();
        inner.register_prim_fn("mentat.store/with", move |_it, args| {
            let db = destructure_db("mentat.store/with", args.first())?;
            let tx = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/with: expected (db tx-data)"))?;
            if db.as_of.is_some() || db.since.is_some() {
                return Err(throw_str(
                    "mentat.store/with: speculative transact is only defined against the current \
                     basis, not an as-of/since db.",
                ));
            }
            let edn = print_str(tx);
            let mut map = stores.borrow_mut();
            let store = store_for("mentat.store/with", &mut map, db.conn)?;
            let report = store
                .with_speculative(&edn)
                .map_err(|e| throw_str(&format!("mentat.store/with: {e}")))?;
            // db-after is at the speculative report's basis; the store itself
            // rolled back, so its current basis is unchanged. Callers that read
            // db-after through this layer get the current (unchanged) datoms —
            // the speculative datoms are NOT queryable after rollback. We
            // return the tx id so callers can inspect the report; a full
            // in-memory speculative db is beyond this layer.
            let db_after = db_value(db.conn, report.tx_id, None, None);
            let report_val = tx_report_value(&report, Some(db_after));
            let m = PMap::empty()
                .assoc(
                    kw_ns("mentat.store", "db-after"),
                    db_value(db.conn, report.tx_id, None, None),
                )
                .assoc(kw_ns("mentat.store", "tx-report"), report_val);
            Ok(Value::Map(Gc::new(m)))
        });
    }

    // as-of: (as-of db T) -> db' with :as-of T, basis-tx clamped to T.
    {
        inner.register_prim_fn("mentat.store/as-of", move |_it, args| {
            let db = destructure_db("mentat.store/as-of", args.first())?;
            let t = int_arg("mentat.store/as-of", args.get(1))?;
            Ok(db_value(db.conn, t, Some(t), db.since))
        });
    }

    // since: (since db T) -> db' with :since T.
    {
        inner.register_prim_fn("mentat.store/since", move |_it, args| {
            let db = destructure_db("mentat.store/since", args.first())?;
            let t = int_arg("mentat.store/since", args.get(1))?;
            Ok(db_value(db.conn, db.basis_tx, db.as_of, Some(t)))
        });
    }
}

// ---------------------------------------------------------------------------
// db value construct / destructure
// ---------------------------------------------------------------------------

/// A destructured db value.
struct DbRef {
    conn: i64,
    basis_tx: i64,
    as_of: Option<i64>,
    since: Option<i64>,
}

/// Build a db-value mino map.
fn db_value(conn: i64, basis_tx: i64, as_of: Option<i64>, since: Option<i64>) -> Value {
    let opt = |o: Option<i64>| o.map(Value::Int).unwrap_or(Value::Nil);
    let m = PMap::empty()
        .assoc(kw_ns("mentat.store", "db"), Value::Bool(true))
        .assoc(kw_ns("mentat.store", "conn"), Value::Int(conn))
        .assoc(kw_ns("mentat.store", "basis-tx"), Value::Int(basis_tx))
        .assoc(kw_ns("mentat.store", "as-of"), opt(as_of))
        .assoc(kw_ns("mentat.store", "since"), opt(since));
    Value::Map(Gc::new(m))
}

/// Destructure a db value, accepting a bare conn `Int` (treated as the current
/// db — basis unknown here, so `basis_tx` is left 0 and callers relying on the
/// current basis re-read `last_tx_id`; as-of/since are nil).
fn destructure_db(prim: &str, arg: Option<&Value>) -> Result<DbRef, Throw> {
    match arg {
        Some(Value::Int(n)) => Ok(DbRef {
            conn: *n,
            basis_tx: 0,
            as_of: None,
            since: None,
        }),
        Some(Value::Map(m)) => {
            let conn = match m.get(&kw_ns("mentat.store", "conn")) {
                Some(Value::Int(n)) => *n,
                _ => {
                    return Err(throw_str(&format!(
                        "{prim}: db value missing :mentat.store/conn"
                    )))
                }
            };
            let basis_tx = match m.get(&kw_ns("mentat.store", "basis-tx")) {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            let opt = |k: &str| match m.get(&kw_ns("mentat.store", k)) {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            };
            Ok(DbRef {
                conn,
                basis_tx,
                as_of: opt("as-of"),
                since: opt("since"),
            })
        }
        _ => Err(throw_str(&format!(
            "{prim}: first arg must be a db value (from mentat.store/db) or a conn handle"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Argument helpers
// ---------------------------------------------------------------------------

/// Extract an integer connection handle from a prim argument.
fn conn_handle(prim: &str, arg: Option<&Value>) -> Result<i64, Throw> {
    match arg {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(throw_str(&format!(
            "{prim}: first arg must be a connection handle (from mentat.store/open)"
        ))),
    }
}

fn int_arg(prim: &str, arg: Option<&Value>) -> Result<i64, Throw> {
    match arg {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(throw_str(&format!("{prim}: expected an integer tx id"))),
    }
}

/// A mino `Keyword` argument as a Mentat `Keyword`.
fn keyword_arg(prim: &str, arg: &Value) -> Result<Keyword, Throw> {
    match arg {
        Value::Keyword(sym) => Ok(sym_to_keyword(sym)),
        _ => Err(throw_str(&format!(
            "{prim}: expected an attribute keyword, got {}",
            print_str(arg)
        ))),
    }
}

fn sym_to_keyword(sym: &Symbol) -> Keyword {
    match sym.ns.as_deref() {
        Some(ns) => Keyword::namespaced(ns, &*sym.name),
        None => Keyword::plain(&*sym.name),
    }
}

/// Resolve an eid argument to a numeric entid. Accepts an integer, an ident
/// keyword (resolved via schema), or a lookup-ref `[:attr val]` (resolved by
/// querying the unique attribute).
fn resolve_eid(prim: &str, store: &Store, arg: &Value) -> Result<Entid, Throw> {
    match arg {
        Value::Int(n) => Ok(*n),
        Value::Keyword(sym) => {
            let kw = sym_to_keyword(sym);
            store
                .conn()
                .current_schema()
                .get_entid(&kw)
                .map(|k| k.0)
                .ok_or_else(|| throw_str(&format!("{prim}: unknown ident {kw}")))
        }
        Value::Vector(v) if v.len() == 2 => {
            // Lookup ref [:attr val]: query `[:find ?e . :where [?e :attr val]]`.
            let attr = v.nth(0).unwrap();
            let val = v.nth(1).unwrap();
            let attr_kw = match attr {
                Value::Keyword(sym) => sym_to_keyword(sym),
                _ => {
                    return Err(throw_str(&format!(
                        "{prim}: lookup-ref attr must be a keyword"
                    )))
                }
            };
            let query = format!("[:find ?e . :where [?e {} {}]]", attr_kw, print_str(val));
            let out = store
                .q_once(&query, None)
                .map_err(|e| throw_str(&format!("{prim}: lookup-ref query: {e}")))?;
            match out.results {
                QueryResults::Scalar(Some(Binding::Scalar(TypedValue::Ref(e)))) => Ok(e),
                QueryResults::Scalar(Some(Binding::Scalar(TypedValue::Long(e)))) => Ok(e),
                _ => Err(throw_str(&format!(
                    "{prim}: lookup-ref {} resolved to no entity",
                    print_str(arg)
                ))),
            }
        }
        _ => Err(throw_str(&format!(
            "{prim}: eid must be an integer, ident keyword, or [:attr val] lookup-ref, got {}",
            print_str(arg)
        ))),
    }
}

// ---------------------------------------------------------------------------
// Reads: datoms / entity / read / pull, current + as-of + since
// ---------------------------------------------------------------------------

/// The datom set backing a db value's reads, as `(e, keyword-a, edn-v)` rows.
/// Current basis -> `debug::datoms`; since -> `debug::datoms_after`; as-of ->
/// reconstruction by replaying `transactions_after(.., TX0-1)` up to T.
fn basis_datoms(db: &DbRef, store: &Store) -> Result<Vec<(i64, Keyword, edn::Value)>, Throw> {
    let sqlite = store_sqlite(store);
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
        // Reconstruct: replay every transaction (tx > TX0-1) up to and
        // including T, applying added/retracted flags to a live map keyed by
        // (e, a, v). Retract removes; assert inserts.
        const TX0: i64 = 0x1000_0000;
        let txns = debug::transactions_after(sqlite, &*schema, TX0 - 1)
            .map_err(|e| throw_str(&format!("mentat.store: transactions_after: {e}")))?;
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
        let ds = debug::datoms_after(sqlite, &*schema, s)
            .map_err(|e| throw_str(&format!("mentat.store: datoms_after: {e}")))?;
        Ok(ds
            .0
            .into_iter()
            .map(|d| (eid(&d.e), to_kw(&d.a), d.v))
            .collect())
    } else {
        let ds = debug::datoms(sqlite, &*schema)
            .map_err(|e| throw_str(&format!("mentat.store: datoms: {e}")))?;
        Ok(ds
            .0
            .into_iter()
            .map(|d| (eid(&d.e), to_kw(&d.a), d.v))
            .collect())
    }
}

/// `(datoms db)` -> a vector of `[e a v]` tuple vectors for the basis. (`tx`
/// and `added` are not carried by the reconstructed set; the raw current-basis
/// `datoms` also carry `added = None`, so we expose the stable `[e a v]` shape.)
fn datoms_value(db: &DbRef, store: &Store) -> Result<Value, Throw> {
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

/// `(entity db eid)` -> `{:db/id eid, :attr v, ...}` built from the basis
/// datoms of that eid. Cardinality-many attrs collect into a set.
fn entity_map(db: &DbRef, store: &Store, entid: Entid) -> Result<Value, Throw> {
    let rows = basis_datoms(db, store)?;
    // Group values by attribute for this eid, preserving first-seen order.
    let mut by_attr: Vec<(Keyword, Vec<edn::Value>)> = Vec::new();
    for (e, a, v) in rows {
        if e != entid {
            continue;
        }
        match by_attr.iter_mut().find(|(k, _)| *k == a) {
            Some((_, vals)) => vals.push(v),
            None => by_attr.push((a, vec![v])),
        }
    }
    let schema = store.conn().current_schema();
    let mut m = PMap::empty().assoc(kw_ns("db", "id"), Value::Int(entid));
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

/// `(read db eid attr)` -> the value(s) for that eid/attr at the basis.
/// Cardinality-one -> the scalar (nil if absent); many -> a set.
fn read_attr(db: &DbRef, store: &Store, entid: Entid, kw: &Keyword) -> Result<Value, Throw> {
    let schema = store.conn().current_schema();
    let many = schema
        .attribute_for_ident(kw)
        .map(|(attr, _)| attr.multival)
        .unwrap_or(false);

    // Current basis: use the live lookup fns. Otherwise: filter basis datoms.
    if db.as_of.is_none() && db.since.is_none() {
        if many {
            let vals = store
                .lookup_values_for_attribute(entid, kw)
                .map_err(|e| throw_str(&format!("mentat.store/read: {e}")))?;
            let mut set = PSet::empty();
            for tv in vals {
                set = set.conj(typed_value(tv));
            }
            Ok(Value::Set(Gc::new(set)))
        } else {
            let v = store
                .lookup_value_for_attribute(entid, kw)
                .map_err(|e| throw_str(&format!("mentat.store/read: {e}")))?;
            Ok(v.map(typed_value).unwrap_or(Value::Nil))
        }
    } else {
        let rows = basis_datoms(db, store)?;
        let target = kw.to_string();
        let vals: Vec<edn::Value> = rows
            .into_iter()
            .filter(|(e, a, _)| *e == entid && a.to_string() == target)
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

/// Parse a minimal pull pattern into a list of attribute entids. Supports a
/// vector of attr keywords and the `*` wildcard (all attributes in the schema).
fn pull_attr_entids(
    schema: &mentat_core::Schema,
    _store: &Store,
    pattern: &Value,
) -> Result<Vec<Entid>, Throw> {
    let items = match pattern {
        Value::Vector(v) => v.iter().cloned().collect::<Vec<_>>(),
        _ => {
            return Err(throw_str(
                "mentat.store/pull: pattern must be a vector of attribute keywords (or [*])",
            ))
        }
    };
    let mut out = Vec::new();
    for item in items {
        match item {
            // `*` wildcard: all attributes defined in the schema.
            Value::Sym(ref s) if &*s.name == "*" && s.ns.is_none() => {
                for entid in schema.attribute_map.keys() {
                    out.push(*entid);
                }
            }
            Value::Keyword(ref sym) => {
                let kw = sym_to_keyword(sym);
                match schema.get_entid(&kw) {
                    Some(k) => out.push(k.0),
                    None => {
                        return Err(throw_str(&format!(
                            "mentat.store/pull: unknown attribute {kw}"
                        )))
                    }
                }
            }
            other => {
                return Err(throw_str(&format!(
                    "mentat.store/pull: unsupported pull item {} (only attr keywords and * are \
                     supported)",
                    print_str(&other)
                )))
            }
        }
    }
    Ok(out)
}

/// Pull a single entity's attributes into a mino map.
fn pull_one(
    db: &DbRef,
    store: &Store,
    entid: Entid,
    attr_entids: &[Entid],
) -> Result<Value, Throw> {
    if db.as_of.is_some() || db.since.is_some() {
        return Err(throw_str(
            "mentat.store/pull: pull against an as-of/since basis is not supported; use \
             entity/read/datoms on this db (which reconstruct the basis).",
        ));
    }
    let sm = store
        .pull_attributes_for_entity(entid, attr_entids.iter().copied())
        .map_err(|e| throw_str(&format!("mentat.store/pull: {e}")))?;
    Ok(structured_map_value(&sm))
}

// ---------------------------------------------------------------------------
// Value conversions
// ---------------------------------------------------------------------------

/// A `TxReport` as a mino map `{:mentat.store/tx-id N, :mentat.store/tempids {...}}`,
/// optionally carrying a `:mentat.store/db-after`.
fn tx_report_value(report: &crate::TxReport, db_after: Option<Value>) -> Value {
    let mut tempids = PMap::empty();
    for (k, v) in &report.tempids {
        tempids = tempids.assoc(str_val(k), Value::Int(*v));
    }
    let mut m = PMap::empty()
        .assoc(kw_ns("mentat.store", "tx-id"), Value::Int(report.tx_id))
        .assoc(
            kw_ns("mentat.store", "tempids"),
            Value::Map(Gc::new(tempids)),
        );
    if let Some(db) = db_after {
        m = m.assoc(kw_ns("mentat.store", "db-after"), db);
    }
    Value::Map(Gc::new(m))
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

/// A single query `Binding` as a mino `Value`, RECURSIVELY: `Scalar` maps its
/// `TypedValue`; `Vec` -> a mino vector; `Map` (a pull `StructuredMap`) -> a
/// mino map of keyword -> value.
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

/// A Mentat `TypedValue` as a mino `Value`. Instant/Uuid become the tagged
/// cons form the reader produces from `#inst`/`#uuid` (see module docs), so
/// they round-trip through the reader.
fn typed_value(tv: TypedValue) -> Value {
    match tv {
        TypedValue::Ref(e) => Value::Int(e),
        TypedValue::Long(n) => Value::Int(n),
        TypedValue::Boolean(b) => Value::Bool(b),
        TypedValue::Double(d) => Value::Float(d.into_inner()),
        TypedValue::String(s) => str_val(s.as_str()),
        TypedValue::Keyword(k) => Value::Keyword(keyword_to_sym(&k)),
        TypedValue::Uuid(u) => uuid_value(&u.to_string()),
        TypedValue::Instant(t) => inst_value(&t.to_rfc3339()),
        TypedValue::Bytes(b) => str_val(&format!("{b:?}")),
    }
}

/// An `edn::Value` (as carried by a `Datom`) as a mino `Value`. Datom `v`s that
/// map ident refs to keywords arrive as `Keyword`; instants/uuids become the
/// tagged reader forms.
fn edn_value(v: &edn::Value) -> Value {
    use edn::Value as E;
    match v {
        E::Nil => Value::Nil,
        E::Boolean(b) => Value::Bool(*b),
        E::Integer(n) => Value::Int(*n),
        E::BigInteger(_) => str_val(&print_edn(v)),
        E::Float(f) => Value::Float(f.into_inner()),
        E::Text(s) => str_val(s),
        E::Instant(t) => inst_value(&t.to_rfc3339()),
        E::Uuid(u) => uuid_value(&u.to_string()),
        E::Keyword(k) => Value::Keyword(keyword_to_sym(k)),
        E::PlainSymbol(s) => Value::Sym(Symbol::plain(s.to_string().trim_start_matches('\''))),
        E::NamespacedSymbol(_) => str_val(&print_edn(v)),
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
        E::Bytes(b) => str_val(&format!("{b:?}")),
    }
}

/// The tagged cons form `(clojure.instant/read-instant-date "RFC3339")` — the
/// exact value `read_one("#inst \"RFC3339\"")` yields, so it round-trips.
fn inst_value(rfc3339: &str) -> Value {
    cons_call(
        Symbol::namespaced("clojure.instant", "read-instant-date"),
        str_val(rfc3339),
    )
}

/// The tagged cons form `(parse-uuid "uuid")` — the exact value
/// `read_one("#uuid \"uuid\"")` yields, so it round-trips.
fn uuid_value(uuid: &str) -> Value {
    cons_call(Symbol::plain("parse-uuid"), str_val(uuid))
}

/// Build `(sym arg)` as a proper one-arg cons list.
fn cons_call(sym: Symbol, arg: Value) -> Value {
    Value::Cons(Gc::new((
        Value::Sym(sym),
        Value::Cons(Gc::new((arg, Value::EmptyList))),
    )))
}

fn keyword_to_sym(k: &Keyword) -> Symbol {
    match k.namespace() {
        Some(ns) => Symbol::namespaced(ns, k.name()),
        None => Symbol::plain(k.name()),
    }
}
fn str_val(s: &str) -> Value {
    Value::Str(Gc::new(s.to_string()))
}

fn kw_ns(ns: &str, name: &str) -> Value {
    Value::Keyword(Symbol::namespaced(ns, name))
}

/// `edn::Value` -> its pretty EDN text (for bigints/symbols and datom keys).
fn print_edn(v: &edn::Value) -> String {
    v.to_pretty(200).unwrap_or_else(|_| format!("{v:?}"))
}

// ---------------------------------------------------------------------------
// Store internals access
// ---------------------------------------------------------------------------

/// The store's `rusqlite::Connection` as `&`, for read-only `debug::*` fns.
fn store_sqlite(store: &Store) -> &rusqlite::Connection {
    store.sqlite_ref()
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
