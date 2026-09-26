//! `install`: register all 14 `mentat.store/*` prims onto a mino interpreter,
//! each driving through a shared [`ScriptBackend`]. Argument parsing, the
//! db-value shape, and result wrapping live here; storage and result
//! conversion live in the backend.

use std::cell::RefCell;
use std::rc::Rc;

use mino_rs::collections::vector::PVec;
use mino_rs::error::throw_str;
use mino_rs::printer::print_str;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Throw, Value};

use crate::values::{db_value, destructure_db, kw_ns, tx_report_value};
use crate::ScriptBackend;

type Backend = Rc<RefCell<dyn ScriptBackend>>;

/// Turn a backend `Result<_, String>` error into a mino throw, prefixed by the
/// prim name.
fn thr<T>(prim: &str, r: Result<T, String>) -> Result<T, Throw> {
    r.map_err(|e| throw_str(&format!("{prim}: {e}")))
}

/// Register the `mentat.store/*` prims on `it`, driving each through `backend`.
///
/// The interpreter should already be sandboxed/limited by the host (pg_mentat
/// wraps this with `sandboxed()` + GUC limits + a check hook; Mentat with
/// `sandboxed()` + fixed limits). `install` only adds the store surface.
pub fn install(it: &mut mino_rs::Interpreter, backend: Backend) {
    // open: (open) -> in-memory conn handle; (open "path") -> file-backed.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/open", move |_it, args| {
            let path = match args.first() {
                None | Some(Value::Nil) => None,
                Some(Value::Str(s)) => Some(s.as_str().to_string()),
                Some(other) => {
                    return Err(throw_str(&format!(
                        "mentat.store/open: path must be a string, got {}",
                        print_str(other)
                    )))
                }
            };
            let id = thr("mentat.store/open", b.borrow_mut().open(path.as_deref()))?;
            Ok(Value::Int(id))
        });
    }

    // close: (close conn) -> nil.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/close", move |_it, args| {
            let id = conn_handle("mentat.store/close", args.first())?;
            thr("mentat.store/close", b.borrow_mut().close(id))?;
            Ok(Value::Nil)
        });
    }

    // db: (db conn) -> a db value at the current basis.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/db", move |_it, args| {
            let id = conn_handle("mentat.store/db", args.first())?;
            let basis = thr("mentat.store/db", b.borrow().basis_tx(id))?;
            Ok(db_value(id, basis, None, None))
        });
    }

    // transact: (transact conn tx-data) -> tx report map. Commits.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/transact", move |_it, args| {
            let id = conn_handle("mentat.store/transact", args.first())?;
            let tx = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/transact: expected (conn tx-data)"))?;
            let edn = print_str(tx);
            let report = thr("mentat.store/transact", b.borrow_mut().transact(id, &edn))?;
            // A tx-report carries its resulting basis as :mentat.store/db-after
            // (Datomic shape), so a committed transact and a speculative `with`
            // report the same way.
            let db_after = db_value(id, report.tx_id, None, None);
            Ok(tx_report_value(&report, Some(db_after)))
        });
    }

    // with: (with db tx-data) -> {:mentat.store/db-after db', :mentat.store/tx-report {...}}.
    // Speculative: does NOT commit.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/with", move |_it, args| {
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
            let (basis_after, report) = thr("mentat.store/with", b.borrow_mut().with(&db, &edn))?;
            let db_after = db_value(db.conn, basis_after, None, None);
            let report_val = tx_report_value(&report, Some(db_after.clone()));
            let m = mino_rs::collections::map::PMap::empty()
                .assoc(kw_ns("mentat.store", "db-after"), db_after)
                .assoc(kw_ns("mentat.store", "tx-report"), report_val);
            Ok(Value::Map(Gc::new(m)))
        });
    }

    // q / q-once: (q db query) -> Datomic-shaped result. as-of/since handling
    // is the backend's (SQLite errors; pg forwards the temporal bound).
    for name in ["mentat.store/q", "mentat.store/q-once"] {
        let b = backend.clone();
        it.register_prim_fn(name, move |_it, args| {
            let db = destructure_db("mentat.store/q", args.first())?;
            let query = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/q: expected (db query)"))?;
            let edn = print_str(query);
            thr("mentat.store/q", b.borrow().q(&db, &edn))
        });
    }

    // pull: (pull db eid pattern) -> a map, or (pull db [eids] pattern) -> vec.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/pull", move |_it, args| {
            let db = destructure_db("mentat.store/pull", args.first())?;
            let eid_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/pull: expected (db eid pattern)"))?;
            let pattern = args
                .get(2)
                .ok_or_else(|| throw_str("mentat.store/pull: expected (db eid pattern)"))?;
            let pat_edn = print_str(pattern);
            let be = b.borrow();
            match eid_arg {
                Value::Vector(v) => {
                    let mut out = Vec::with_capacity(v.len());
                    for e in v.iter() {
                        let eid = thr("mentat.store/pull", be.resolve_eid(&db, e))?;
                        out.push(thr("mentat.store/pull", be.pull(&db, eid, &pat_edn))?);
                    }
                    Ok(Value::Vector(Gc::new(PVec::from_vec(out))))
                }
                other => {
                    let eid = thr("mentat.store/pull", be.resolve_eid(&db, other))?;
                    thr("mentat.store/pull", be.pull(&db, eid, &pat_edn))
                }
            }
        });
    }

    // entity: (entity db eid) -> {:db/id eid, :attr v, ...}.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/entity", move |_it, args| {
            let db = destructure_db("mentat.store/entity", args.first())?;
            let eid_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/entity: expected (db eid)"))?;
            let be = b.borrow();
            let eid = thr("mentat.store/entity", be.resolve_eid(&db, eid_arg))?;
            thr("mentat.store/entity", be.entity(&db, eid))
        });
    }

    // read: (read db eid attr) -> the value(s) for that eid/attr.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/read", move |_it, args| {
            let db = destructure_db("mentat.store/read", args.first())?;
            let eid_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/read: expected (db eid attr)"))?;
            let attr_arg = args
                .get(2)
                .ok_or_else(|| throw_str("mentat.store/read: expected (db eid attr)"))?;
            let attr = keyword_arg("mentat.store/read", attr_arg)?;
            let be = b.borrow();
            let eid = thr("mentat.store/read", be.resolve_eid(&db, eid_arg))?;
            thr("mentat.store/read", be.read(&db, eid, &attr))
        });
    }

    // entities: (entities db attr) -> a set of eids having that attr.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/entities", move |_it, args| {
            let db = destructure_db("mentat.store/entities", args.first())?;
            let attr_arg = args
                .get(1)
                .ok_or_else(|| throw_str("mentat.store/entities: expected (db attr)"))?;
            let attr = keyword_arg("mentat.store/entities", attr_arg)?;
            thr("mentat.store/entities", b.borrow().entities(&db, &attr))
        });
    }

    // datoms: (datoms db) -> a vector of [e a v ...] tuples for the basis.
    {
        let b = backend.clone();
        it.register_prim_fn("mentat.store/datoms", move |_it, args| {
            let db = destructure_db("mentat.store/datoms", args.first())?;
            thr("mentat.store/datoms", b.borrow().datoms(&db))
        });
    }

    // as-of: (as-of db T) -> db' with :as-of T, basis clamped to T.
    it.register_prim_fn("mentat.store/as-of", move |_it, args| {
        let db = destructure_db("mentat.store/as-of", args.first())?;
        let t = int_arg("mentat.store/as-of", args.get(1))?;
        Ok(db_value(db.conn, t, Some(t), db.since))
    });

    // since: (since db T) -> db' with :since T.
    it.register_prim_fn("mentat.store/since", move |_it, args| {
        let db = destructure_db("mentat.store/since", args.first())?;
        let t = int_arg("mentat.store/since", args.get(1))?;
        Ok(db_value(db.conn, db.basis_tx, db.as_of, Some(t)))
    });
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

/// An integer tx-id argument.
fn int_arg(prim: &str, arg: Option<&Value>) -> Result<i64, Throw> {
    match arg {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err(throw_str(&format!("{prim}: expected an integer tx id"))),
    }
}

/// A mino keyword argument as `:ns/name` / `:name` text (the ABI both backends
/// feed to their query/lookup layers).
fn keyword_arg(prim: &str, arg: &Value) -> Result<String, Throw> {
    match arg {
        Value::Keyword(sym) => Ok(sym_to_kw_str(sym)),
        _ => Err(throw_str(&format!(
            "{prim}: expected an attribute keyword, got {}",
            print_str(arg)
        ))),
    }
}

/// `:ns/name` / `:name` text for a mino keyword symbol.
pub fn sym_to_kw_str(sym: &Symbol) -> String {
    match sym.ns.as_deref() {
        Some(ns) => format!(":{ns}/{}", sym.name),
        None => format!(":{}", sym.name),
    }
}
