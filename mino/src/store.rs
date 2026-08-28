//! The EAVT store handle backing `mino.store`. Ports the in-memory slice of
//! `src/prim/store.c`: the `MINO_STORE` handle plus the C prims the Clojure
//! layer (`lib/mino/store.clj`) calls — `store-open*`, `store-commit*`,
//! `store-clock*`, `store-checkpoint*`, `store-close*`, `store?`,
//! `store-read-snapshot*`, `store-read-wal*` — and `deref` on a store.
//!
//! A store is an identity cell wrapping the current immutable db value (a
//! persistent map). `store-commit*` swaps the value and fires watches;
//! `deref`/`@` reads it. Durability (path/WAL/snapshot/checkpoint) is Phase 7:
//! here the path handling is stubbed so in-memory stores work and the snapshot
//! /WAL reads return nil (`store/open` only calls them `(when path ...)`).

use crate::collections::map::PMap;
use crate::env::Env;
use crate::error::{throw_classified, Throw};
use crate::eval::func::apply;
use crate::eval::Interp;
use crate::symbol::Symbol;
use crate::value::{Prim, Value};
use gc::{Finalize, Gc, GcCell, Trace};

/// The mutable state behind a store handle: the current db value, the optional
/// durable path (None = in-memory), a monotonic print id, and the watches map
/// (`key -> fn`). Mirrors `mino_store_handle` + the `as.store` cell fields.
#[derive(Trace, Finalize)]
pub struct StoreState {
    /// The current immutable db value (a persistent map). `store-commit*`
    /// replaces it; `deref` reads it.
    pub val: Value,
    /// Durable path. None for in-memory stores. Phase 7 uses it for WAL/snapshot.
    pub path: Option<String>,
    /// Per-state monotonic id, printed as `0xN` hex in `#store[0xN VAL]`.
    pub id: u64,
    /// Watches registered via `add-watch` (`key -> fn`). Fired by `store-commit*`.
    /// (store.clj's public `listen`/`unlisten` use a separate atom registry;
    /// this mirrors the C `store->watches` slot that `store-commit*` notifies.)
    pub watches: PMap,
}

/// A `Value::Store`-shaped handle. Since `Value` is a closed enum and adding a
/// variant would ripple through every match, the store is instead a boxed
/// state reachable only through the prims below; the public surface treats it
/// as an opaque handle. We keep it as a distinct `Value` variant so `store?`,
/// `deref`, `=` (identity), `type`, and printing all dispatch correctly.
pub type StoreCell = Gc<GcCell<StoreState>>;

fn as_store(v: &Value) -> Option<&StoreCell> {
    match v {
        Value::Store(cell) => Some(cell),
        _ => None,
    }
}

/// `(store-open* db path)` — wrap a db value in a fresh store identity. `path`
/// is a string (durable) or nil (in-memory). Pure constructor: snapshot read +
/// WAL replay happen in the Clojure `store/open` (Phase 7 fills the durable
/// path). Ports `prim_store_open`.
fn store_open_star(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [db, path_val] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-open* requires two arguments",
        ));
    };
    let path = match path_val {
        Value::Nil => None,
        Value::Str(s) => Some((**s).clone()),
        _ => {
            return Err(throw_classified(
                "eval/type",
                "MTY001",
                "store-open*: path must be a string or nil",
            ))
        }
    };
    it.next_store_id += 1;
    let id = it.next_store_id;
    Ok(Value::Store(Gc::new(GcCell::new(StoreState {
        val: db.clone(),
        path,
        id,
        watches: PMap::empty(),
    }))))
}

/// `(store-commit* conn new-db [tx-info])` — publish a new db value and fire
/// watches; returns new-db. In-memory: no WAL append (Phase 7). Ports
/// `prim_store_commit` + `mino_store_publish` + `store_notify_watches`.
fn store_commit_star(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (conn, new_db) = match args {
        [conn, new_db] | [conn, new_db, _] => (conn, new_db),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "store-commit* requires two or three arguments",
            ))
        }
    };
    let Some(cell) = as_store(conn) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-commit* requires a store connection",
        ));
    };
    // ponytail: in-memory only; WAL append (the tx-info 3rd arg) is Phase 7.
    let old_val = cell.borrow().val.clone();
    let watches: Vec<(Value, Value)> = cell
        .borrow()
        .watches
        .entries()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    cell.borrow_mut().val = new_db.clone();
    // Fire watches (fn key store old new), mirroring store_notify_watches.
    for (key, f) in &watches {
        apply(it, f, &[key.clone(), conn.clone(), old_val.clone(), new_db.clone()])?;
    }
    Ok(new_db.clone())
}

/// `(store-clock* conn)` — current instant in wall-clock epoch-ms. `conn` may
/// be a store or nil (the pure `store/with` path passes nil). Ports
/// `prim_store_clock`. The port has no injectable clock hook yet, so it always
/// uses the wall clock (matching the C default `clock == NULL` branch).
fn store_clock_star(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [conn] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-clock* requires one argument",
        ));
    };
    match conn {
        Value::Nil | Value::Store(_) => Ok(Value::Int(wall_clock_ms())),
        _ => Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-clock* requires a store connection",
        )),
    }
}

/// `(store-checkpoint* conn)` — no-op for in-memory (documented: see
/// `store-in-memory-checkpoint-noop`). Phase 7 writes the snapshot for durable
/// stores. Returns nil. Ports `prim_store_checkpoint`.
fn store_checkpoint_star(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [conn] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-checkpoint* requires one argument",
        ));
    };
    if as_store(conn).is_none() {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-checkpoint* requires a store connection",
        ));
    }
    Ok(Value::Nil)
}

/// `(store-close* conn)` — no-op close for in-memory (Phase 7 flushes durable).
/// Returns nil. Idempotent. Ports `prim_store_close`.
fn store_close_star(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [conn] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-close* requires one argument",
        ));
    };
    if as_store(conn).is_none() {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-close* requires a store connection",
        ));
    }
    Ok(Value::Nil)
}

/// `(store? x)` — true iff x is a store connection. Ports `prim_store_p`.
fn store_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [x] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store? requires one argument",
        ));
    };
    Ok(Value::Bool(matches!(x, Value::Store(_))))
}

/// `(store-read-snapshot* path)` — in-memory stub: no snapshot file, return
/// nil. Phase 7 reads + parses the on-disk snapshot. Ports
/// `prim_store_read_snapshot` (durable read is Phase 7).
fn store_read_snapshot_star(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [path] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-read-snapshot* requires one argument",
        ));
    };
    if !matches!(path, Value::Str(_)) {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-read-snapshot*: path must be a string",
        ));
    }
    // ponytail: in-memory stub returns nil (no file); Phase 7 reads EDN snapshot.
    Ok(Value::Nil)
}

/// `(store-read-wal* path)` — in-memory stub: no WAL file, return nil. Phase 7
/// reads + replays the WAL. Ports `prim_store_read_wal`.
fn store_read_wal_star(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [path] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-read-wal* requires one argument",
        ));
    };
    if !matches!(path, Value::Str(_)) {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-read-wal*: path must be a string",
        ));
    }
    // ponytail: in-memory stub returns nil (no file); Phase 7 replays the WAL.
    Ok(Value::Nil)
}

/// Portable wall-clock epoch-ms. Matches `store_wall_clock_ms` (CLOCK_REALTIME).
pub fn wall_clock_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Deref a store to its current db value (`@conn` / `(deref conn)`). Called by
/// the `deref` prim's store branch. Ports `mino_store_deref`.
pub fn store_deref(cell: &StoreCell) -> Value {
    cell.borrow().val.clone()
}

/// Register the 8 store C prims into `root`. Called from `Interp::new` before
/// `store.clj` loads (store.clj captures `store?` at load time). Ports
/// `k_prims_store` / `mino_install_store`.
pub fn install(root: &Env) {
    let reg = |name: &'static str, f: crate::value::PrimFn| {
        root.set(Symbol::plain(name), Value::Prim(Prim(f, name)));
    };
    reg("store-open*", store_open_star);
    reg("store-commit*", store_commit_star);
    reg("store-clock*", store_clock_star);
    reg("store-checkpoint*", store_checkpoint_star);
    reg("store-close*", store_close_star);
    reg("store?", store_p);
    reg("store-read-snapshot*", store_read_snapshot_star);
    reg("store-read-wal*", store_read_wal_star);
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    fn eval(src: &str) -> String {
        let mut it = Interp::new();
        print_str(&it.eval_str(src).unwrap())
    }

    // Each expected value confirmed against `~/src/mino/mino -e`.

    #[test]
    fn open_transact_read_map_sugar() {
        // (require 'mino.store) (transact {:alice {:name "Alice" :age 30}}) read -> 30
        assert_eq!(
            eval(
                "(def c (mino.store/open)) \
                 (mino.store/transact c {:alice {:name \"Alice\" :age 30}}) \
                 (mino.store/read (mino.store/db c) :alice :age)"
            ),
            "30"
        );
    }

    #[test]
    fn store_predicate_and_db_deref_agree() {
        assert_eq!(eval("(mino.store/store? (mino.store/open))"), "true");
        assert_eq!(eval("(mino.store/store? 42)"), "false");
        assert_eq!(
            eval("(let [c (mino.store/open)] (= (mino.store/db c) @c))"),
            "true"
        );
    }

    #[test]
    fn transact_eavt_and_entity() {
        // [:db/add 1 :name "Alice"] -> read back "Alice"; entity tags :db/id.
        assert_eq!(
            eval(
                "(def c (mino.store/open)) \
                 (mino.store/transact c [:db/add 1 :name \"Alice\"]) \
                 (mino.store/read (mino.store/db c) 1 :name)"
            ),
            "\"Alice\""
        );
        assert_eq!(
            eval(
                "(def c (mino.store/open)) \
                 (mino.store/transact c {1 {:name \"Alice\"}}) \
                 (:db/id (mino.store/entity (mino.store/db c) 1))"
            ),
            "1"
        );
    }

    #[test]
    fn retract_drops_value() {
        // add then retract the attribute -> nil.
        assert_eq!(
            eval(
                "(def c (mino.store/open)) \
                 (mino.store/transact c [:db/add 1 :name \"Alice\"]) \
                 (mino.store/transact c [:db/retract 1 :name]) \
                 (mino.store/read (mino.store/db c) 1 :name)"
            ),
            "nil"
        );
    }

    #[test]
    fn simple_datalog_query() {
        // q :find ?n :where [?e :name ?n] over two entities -> set of names.
        assert_eq!(
            eval(
                "(def c (mino.store/open)) \
                 (mino.store/transact c {1 {:name \"Alice\"} 2 {:name \"Bob\"}}) \
                 (mino.store/q (mino.store/db c) '[:find ?n :where [?e :name ?n]])"
            ),
            "#{[\"Alice\"] [\"Bob\"]}"
        );
    }

    #[test]
    fn store_ids_are_distinct_small_counters() {
        // #store[0xN ...]: two stores print distinct short ids.
        let mut it = Interp::new();
        let a = print_str(&it.eval_str("(mino.store/open)").unwrap());
        let b = print_str(&it.eval_str("(mino.store/open)").unwrap());
        assert!(a.starts_with("#store[0x"));
        assert_ne!(a, b);
    }
}
