//! The EAVT store handle backing `mino.store`. Ports the in-memory slice of
//! `src/prim/store.c`: the `MINO_STORE` handle plus the C prims the Clojure
//! layer (`lib/mino/store.clj`) calls — `store-open*`, `store-commit*`,
//! `store-clock*`, `store-checkpoint*`, `store-close*`, `store?`,
//! `store-read-snapshot*`, `store-read-wal*` — and `deref` on a store.
//!
//! A store is an identity cell wrapping the current immutable db value (a
//! persistent map). `store-commit*` swaps the value and fires watches;
//! `deref`/`@` reads it. Durability uses snapshot + WAL (ADR 11): a durable
//! store carries a filesystem `path`; the snapshot lives at `<path>` (a 1-byte
//! `0x00` version header followed by the db value as EDN) and the WAL lives at
//! `<path>.wal` (line-delimited EDN, one tx-info map per line). `store-commit*`
//! appends to the WAL before publishing; `store-checkpoint*` writes the
//! snapshot atomically (temp + rename) and deletes the WAL; `store-close*`
//! checkpoints then releases. `store/open` (Clojure) reads them back via
//! `store-read-snapshot*` / `store-read-wal*`.

use crate::collections::map::PMap;
use crate::collections::vector::PVec;
use crate::env::Env;
use crate::error::{throw_classified, Throw};
use crate::eval::func::apply;
use crate::eval::Interp;
use crate::printer::print_str;
use crate::reader::read_one;
use crate::symbol::Symbol;
use crate::value::{Prim, Value};
use gc::{Finalize, Gc, GcCell, Trace};
use std::io::Write;

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

/// Refuse a disk-touching store op in a sandboxed interpreter.
fn deny_in_sandbox(it: &Interp, what: &str) -> Result<(), Throw> {
    if it.sandboxed {
        return Err(throw_classified(
            "eval/contract",
            "MCT001",
            &format!("{what}: durable (on-disk) stores are disabled in a sandboxed interpreter"),
        ));
    }
    Ok(())
}

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
    if path.is_some() {
        deny_in_sandbox(it, "store-open*")?;
    }
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
    // WAL append before publish (crash safety): if the store is durable and a
    // non-nil tx-info was supplied, append it as one EDN line + flush before
    // the in-memory value swap. Ports prim_store_commit's WAL branch.
    let tx_info = match args {
        [_, _, ti] if !matches!(ti, Value::Nil) => Some(ti),
        _ => None,
    };
    if let Some(ti) = tx_info {
        let path = cell.borrow().path.clone();
        if let Some(path) = path {
            deny_in_sandbox(it, "store-commit*")?;
            wal_append(&path, ti)?;
        }
    }
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
        apply(
            it,
            f,
            &[key.clone(), conn.clone(), old_val.clone(), new_db.clone()],
        )?;
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

/// `(store-checkpoint* conn)` — for a durable store, write the snapshot
/// atomically (temp file with `0x00` header + EDN, fsync, rename into place)
/// and delete the WAL. In-memory: no-op. Returns nil. Ports
/// `prim_store_checkpoint` + `mino_store_checkpoint`.
fn store_checkpoint_star(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [conn] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-checkpoint* requires one argument",
        ));
    };
    let Some(cell) = as_store(conn) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-checkpoint* requires a store connection",
        ));
    };
    let (path, val) = {
        let s = cell.borrow();
        (s.path.clone(), s.val.clone())
    };
    if let Some(path) = path {
        deny_in_sandbox(it, "store-checkpoint*")?;
        checkpoint_to_disk(&path, &val)?;
    }
    Ok(Value::Nil)
}

/// `(store-close* conn)` — for a durable store, checkpoint (write snapshot +
/// delete WAL) then release the path. In-memory: no-op. Idempotent (a second
/// close finds no path). Returns nil. Ports `prim_store_close` +
/// `mino_store_close` (which checkpoints before releasing the handle).
fn store_close_star(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [conn] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-close* requires one argument",
        ));
    };
    let Some(cell) = as_store(conn) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-close* requires a store connection",
        ));
    };
    let (path, val) = {
        let s = cell.borrow();
        (s.path.clone(), s.val.clone())
    };
    if let Some(path) = path {
        deny_in_sandbox(it, "store-close*")?;
        checkpoint_to_disk(&path, &val)?;
        // Release the path so a second close is a no-op (idempotent), matching
        // mino_store_close freeing the handle.
        cell.borrow_mut().path = None;
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

/// `(store-read-snapshot* path)` — read the snapshot at `<path>` if it exists.
/// The file may carry a 1-byte `0x00` version header (skip it) or be headerless
/// (v1: parse the whole file as EDN). Returns the parsed db value, or nil if
/// the file is absent / unparseable. Ports `prim_store_read_snapshot`.
fn store_read_snapshot_star(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    deny_in_sandbox(it, "store-read-snapshot*")?;
    let [path] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-read-snapshot* requires one argument",
        ));
    };
    let Value::Str(p) = path else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-read-snapshot*: path must be a string",
        ));
    };
    let Ok(bytes) = std::fs::read(&**p) else {
        return Ok(Value::Nil); // no file
    };
    // Skip the 1-byte version header (0x00 = EDN text); headerless v1 snapshots
    // parse the whole file.
    let body: &[u8] = match bytes.first() {
        Some(0x00) => &bytes[1..],
        _ => &bytes,
    };
    match std::str::from_utf8(body)
        .ok()
        .and_then(|s| read_one(s).ok())
    {
        Some((db, _)) => Ok(db),
        None => Ok(Value::Nil), // unparseable snapshot -> nil (start fresh)
    }
}

/// `(store-read-wal* path)` — read the WAL at `<path>.wal` if it exists, parse
/// each line as EDN, and return a vector of tx-info maps. A torn/unparseable
/// line stops the scan (the malformed trailing line is dropped — torn-write
/// recovery). Returns nil if the WAL file is absent. Ports
/// `prim_store_read_wal` / `store_wal_read`.
fn store_read_wal_star(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    deny_in_sandbox(it, "store-read-wal*")?;
    let [path] = args else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "store-read-wal* requires one argument",
        ));
    };
    let Value::Str(p) = path else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "store-read-wal*: path must be a string",
        ));
    };
    let wal_path = format!("{}.wal", &**p);
    let Ok(text) = std::fs::read_to_string(&wal_path) else {
        return Ok(Value::Nil); // no WAL file
    };
    let mut entries: Vec<Value> = Vec::new();
    for line in text.split('\n') {
        let trimmed = line.trim_start_matches([' ', '\t', '\r']);
        if trimmed.is_empty() {
            continue;
        }
        match read_one(trimmed) {
            // A well-formed line parses to exactly one form consuming the whole
            // (trimmed) line. If parsing leaves trailing non-whitespace, the
            // line is torn (e.g. `GARBAGE{not valid`) — stop and drop the tail,
            // matching store_wal_read's eval-the-whole-line semantics.
            Ok((entry, consumed)) if trimmed[consumed..].trim().is_empty() => entries.push(entry),
            _ => break,
        }
    }
    Ok(Value::Vector(Gc::new(PVec::from_vec(entries))))
}

/// Append `tx_info` as one EDN line + newline to `<path>.wal`, flushed to disk
/// (fsync) before returning. Ports `store_wal_append`.
fn wal_append(path: &str, tx_info: &Value) -> Result<(), Throw> {
    let wal_path = format!("{path}.wal");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&wal_path)
        .map_err(|_| throw_classified("io", "MIO001", "store: cannot open WAL for append"))?;
    let mut line = print_str(tx_info);
    line.push('\n');
    f.write_all(line.as_bytes())
        .and_then(|_| f.flush())
        .and_then(|_| f.sync_all())
        .map_err(|_| throw_classified("io", "MIO001", "store: WAL flush failed"))
}

/// Write the db `val` to `<path>` atomically: serialize to `<path>.tmp` (a
/// `0x00` version header + EDN), fsync, then rename into place; then delete
/// `<path>.wal`. A crash mid-write leaves a stale `.tmp` and the previous
/// snapshot intact (the next checkpoint overwrites `.tmp`). Ports
/// `mino_store_checkpoint`.
fn checkpoint_to_disk(path: &str, val: &Value) -> Result<(), Throw> {
    let tmp_path = format!("{path}.tmp");
    {
        let mut f = std::fs::File::create(&tmp_path).map_err(|_| {
            throw_classified(
                "io",
                "MIO001",
                "store-checkpoint: cannot open file for writing",
            )
        })?;
        let mut buf = Vec::with_capacity(256);
        buf.push(0x00u8); // STORE_SNAPSHOT_VERSION
        buf.extend_from_slice(print_str(val).as_bytes());
        f.write_all(&buf)
            .and_then(|_| f.flush())
            .and_then(|_| f.sync_all())
            .map_err(|_| {
                let _ = std::fs::remove_file(&tmp_path);
                throw_classified("io", "MIO001", "store-checkpoint: write failed")
            })?;
    }
    std::fs::rename(&tmp_path, path).map_err(|_| {
        let _ = std::fs::remove_file(&tmp_path);
        throw_classified(
            "io",
            "MIO001",
            "store-checkpoint: cannot rename snapshot into place",
        )
    })?;
    // Delete the WAL — the snapshot captures all state up to :tx.
    let _ = std::fs::remove_file(format!("{path}.wal"));
    Ok(())
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

/// Register the host filesystem prims. NOT installed in a sandboxed
/// interpreter, so there they are unbound symbols.
pub fn install_host_fs(root: &Env) {
    let reg = |name: &'static str, f: crate::value::PrimFn| {
        root.set(Symbol::plain(name), Value::Prim(Prim(f, name)));
    };
    // Filesystem prims the store test corpus drives durability with. These
    // port prim/fs.c (file-exists?, mkdir-p, rm-rf) + prim/io.c (spit, slurp).
    // Registered here because the store tests are their only consumer in the
    // port; a full fs/io module can lift them out later if other tests need it.
    reg("file-exists?", fs_file_exists_p);
    reg("mkdir-p", fs_mkdir_p);
    reg("rm-rf", fs_rm_rf);
    reg("spit", io_spit);
    reg("slurp", io_slurp);
}

/// `(file-exists? path)` -> bool. Ports `prim_file_exists_p`.
fn fs_file_exists_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [Value::Str(p)] = args else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "file-exists?: argument must be a string",
        ));
    };
    Ok(Value::Bool(std::path::Path::new(&**p).exists()))
}

/// `(mkdir-p path)` -> nil. Creates the directory and all parents. Ports
/// `prim_mkdir_p`.
fn fs_mkdir_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [Value::Str(p)] = args else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "mkdir-p: argument must be a string",
        ));
    };
    std::fs::create_dir_all(&**p)
        .map_err(|_| throw_classified("host", "MHO001", "mkdir-p: cannot create directory"))?;
    Ok(Value::Nil)
}

/// `(rm-rf path)` -> nil. Recursively removes a file or directory; a missing
/// path is not an error. Ports `prim_rm_rf`.
fn fs_rm_rf(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [Value::Str(p)] = args else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "rm-rf: argument must be a string",
        ));
    };
    let path = std::path::Path::new(&**p);
    let r = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match r {
        Ok(()) => Ok(Value::Nil),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Value::Nil),
        Err(_) => Err(throw_classified("host", "MHO001", "rm-rf: cannot remove")),
    }
}

/// `(spit path content & opts)` -> nil. Writes `content` to `path`; `:append`
/// truthy selects append mode. Strings are written verbatim; other values are
/// printed via pr-str. Ports `prim_spit`.
fn io_spit(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (path, content, opts) = match args {
        [Value::Str(p), content, opts @ ..] => (p, content, opts),
        [_, _, ..] => {
            return Err(throw_classified(
                "eval/type",
                "MTY001",
                "spit: first argument must be a string path",
            ))
        }
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "spit requires two arguments",
            ))
        }
    };
    // Scan trailing key/value option pairs for :append.
    let mut append = false;
    let mut i = 0;
    while i < opts.len() {
        let Some(v) = opts.get(i + 1) else {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "spit: options must be key/value pairs",
            ));
        };
        if let Value::Keyword(kw) = &opts[i] {
            if &*kw.name == "append" {
                append = v.is_truthy();
            }
        }
        i += 2;
    }
    let body = match content {
        Value::Str(s) => (**s).clone(),
        other => print_str(other),
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(&**path)
        .map_err(|_| throw_classified("host", "MHO001", "spit: cannot open file"))?;
    f.write_all(body.as_bytes())
        .map_err(|_| throw_classified("host", "MHO001", "spit: write failed"))?;
    Ok(Value::Nil)
}

/// `(slurp path)` -> string of the file contents. Ports `prim_slurp`.
fn io_slurp(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let [Value::Str(p)] = args else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "slurp: argument must be a string",
        ));
    };
    let s = std::fs::read_to_string(&**p)
        .map_err(|_| throw_classified("host", "MHO001", "slurp: cannot read file"))?;
    Ok(Value::Str(Gc::new(s)))
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;
    use std::io::Write;

    fn eval(src: &str) -> String {
        let mut it = Interp::new();
        print_str(&it.eval_str(src).unwrap())
    }

    // Each expected value confirmed against the reference mino interpreter (`mino -e`).

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

    // ---- durability (snapshot + WAL), Task 7.1 ---------------------------

    /// A unique temp path under the OS temp dir; dropping it removes the
    /// snapshot, its `.wal`, and any `.tmp` so no turds survive the test.
    struct TmpStore(std::path::PathBuf);
    impl TmpStore {
        fn new(tag: &str) -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static N: AtomicU64 = AtomicU64::new(0);
            let pid = std::process::id();
            let n = N.fetch_add(1, Ordering::Relaxed);
            let mut p = std::env::temp_dir();
            p.push(format!("mino-rs-store-{tag}-{pid}-{n}.db"));
            let _ = std::fs::remove_file(&p);
            let _ = std::fs::remove_file(format!("{}.wal", p.display()));
            let _ = std::fs::remove_file(format!("{}.tmp", p.display()));
            TmpStore(p)
        }
        fn path(&self) -> String {
            self.0.display().to_string()
        }
    }
    impl Drop for TmpStore {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
            let _ = std::fs::remove_file(format!("{}.wal", self.0.display()));
            let _ = std::fs::remove_file(format!("{}.tmp", self.0.display()));
        }
    }

    /// Run a script on a fresh interpreter (a fresh runtime = a fresh process
    /// for durability purposes) and return the pr-str of the last value.
    fn run(script: &str) -> String {
        let mut it = Interp::new();
        print_str(&it.eval_str(script).unwrap())
    }

    #[test]
    fn wal_survives_reopen_without_checkpoint() {
        // Transact without checkpoint, then reopen on a fresh runtime: WAL
        // replay recovers the data (simulates a crash before checkpoint).
        let t = TmpStore::new("wal-reopen");
        let p = t.path();
        run(&format!(
            "(require (quote mino.store)) \
             (def c (mino.store/open \"{p}\")) \
             (mino.store/transact c {{1 {{:name \"Alice\"}}}})"
        ));
        assert!(
            std::path::Path::new(&format!("{p}.wal")).exists(),
            "WAL written per-tx"
        );
        let got = run(&format!(
            "(require (quote mino.store)) \
             (def db (mino.store/db (mino.store/open \"{p}\"))) \
             [(mino.store/read db 1 :name) (:tx db)]"
        ));
        assert_eq!(got, "[\"Alice\" 1]");
    }

    #[test]
    fn checkpoint_writes_snapshot_and_deletes_wal() {
        let t = TmpStore::new("ckpt");
        let p = t.path();
        run(&format!(
            "(require (quote mino.store)) \
             (def c (mino.store/open \"{p}\")) \
             (mino.store/transact c [:db/add 1 :name \"Alice\"]) \
             (mino.store/checkpoint c)"
        ));
        // Snapshot exists with the 0x00 header; WAL is gone.
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes.first(), Some(&0x00), "snapshot has version header");
        assert!(
            !std::path::Path::new(&format!("{p}.wal")).exists(),
            "WAL deleted"
        );
        // Reopen sees the snapshot value.
        let got = run(&format!(
            "(require (quote mino.store)) \
             (mino.store/read (mino.store/db (mino.store/open \"{p}\")) 1 :name)"
        ));
        assert_eq!(got, "\"Alice\"");
    }

    #[test]
    fn checkpoint_then_transact_replays_wal_on_snapshot() {
        let t = TmpStore::new("ckpt-tx");
        let p = t.path();
        run(&format!(
            "(require (quote mino.store)) \
             (def c (mino.store/open \"{p}\")) \
             (mino.store/transact c [:db/add 1 :name \"Alice\"]) \
             (mino.store/checkpoint c) \
             (mino.store/transact c [:db/add 2 :name \"Bob\"])"
        ));
        let got = run(&format!(
            "(require (quote mino.store)) \
             (def db (mino.store/db (mino.store/open \"{p}\"))) \
             [(mino.store/read db 1 :name) (mino.store/read db 2 :name)]"
        ));
        assert_eq!(got, "[\"Alice\" \"Bob\"]");
    }

    #[test]
    fn torn_final_wal_line_is_skipped() {
        // A truncated/garbled trailing WAL line must be dropped, not crash the
        // replay, and the good entries before it must still apply.
        let t = TmpStore::new("torn");
        let p = t.path();
        run(&format!(
            "(require (quote mino.store)) \
             (def c (mino.store/open \"{p}\")) \
             (mino.store/transact c [:db/add 1 :name \"Alice\"])"
        ));
        // Append garbage (no newline) to simulate a torn write.
        {
            let mut f = std::fs::OpenOptions::new()
                .append(true)
                .open(format!("{p}.wal"))
                .unwrap();
            f.write_all(b"GARBAGE{not valid").unwrap();
        }
        let got = run(&format!(
            "(require (quote mino.store)) \
             (def db (mino.store/db (mino.store/open \"{p}\"))) \
             [(mino.store/read db 1 :name) (:tx db)]"
        ));
        assert_eq!(got, "[\"Alice\" 1]");
    }

    #[test]
    fn atomic_snapshot_leaves_no_partial_and_cleans_stale_tmp() {
        let t = TmpStore::new("atomic");
        let p = t.path();
        run(&format!(
            "(require (quote mino.store)) \
             (def c (mino.store/open \"{p}\")) \
             (mino.store/transact c [:db/add 1 :name \"Alice\"]) \
             (mino.store/checkpoint c)"
        ));
        // A stale .tmp left by a hypothetical crashed checkpoint attempt.
        std::fs::write(format!("{p}.tmp"), b"STALE GARBAGE").unwrap();
        run(&format!(
            "(require (quote mino.store)) \
             (def c (mino.store/open \"{p}\")) \
             (mino.store/transact c [:db/add 2 :name \"Bob\"]) \
             (mino.store/checkpoint c)"
        ));
        // The rename consumed the .tmp; the canonical snapshot is whole.
        assert!(
            !std::path::Path::new(&format!("{p}.tmp")).exists(),
            "stale .tmp gone"
        );
        let bytes = std::fs::read(&p).unwrap();
        assert_eq!(bytes.first(), Some(&0x00));
        let got = run(&format!(
            "(require (quote mino.store)) \
             (def db (mino.store/db (mino.store/open \"{p}\"))) \
             [(mino.store/read db 1 :name) (mino.store/read db 2 :name)]"
        ));
        assert_eq!(got, "[\"Alice\" \"Bob\"]");
    }

    #[test]
    fn reads_headerless_v1_snapshot() {
        // A legacy headerless snapshot (whole file is EDN, no 0x00 byte) still
        // reads, per store_read_snapshot's backward-compat branch.
        let t = TmpStore::new("v1");
        let p = t.path();
        std::fs::write(&p, b"{:entities {1 {:name \"Zed\"}} :log [] :tx 5}").unwrap();
        let got = run(&format!(
            "(require (quote mino.store)) \
             (def db (mino.store/db (mino.store/open \"{p}\"))) \
             [(mino.store/read db 1 :name) (:tx db)]"
        ));
        assert_eq!(got, "[\"Zed\" 5]");
    }
}
