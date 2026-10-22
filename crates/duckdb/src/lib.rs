//! `mentat` as a DuckDB loadable extension, storing its datoms in DuckDB.
//!
//!   - `edn_t(store, edn)`                  scalar -> VARCHAR (JSON tx-report).
//!   - `edn_q(store, query, options)`       table fn -> rows, all VARCHAR.
//!   - `edn_pull(store, pattern, entity)`   scalar -> VARCHAR (JSON map).
//!   - `edn_eval(store, script)`            scalar -> VARCHAR (EDN), feature `script`.
//!
//! `store` names a mentat store in the DuckDB database the extension was
//! loaded into: a DuckDB schema holding its tables (`mentat` for `'default'`,
//! `mentat_<name>_<hash>` otherwise; see `mentat_duckdb_store`). Until 1.11 the
//! argument was a SQLite file path; such a value still works, as a name.
//! See docs/duckdb-native-storage-plan.md.
//!
//! SQL runs on a pool of connections cloned from the one DuckDB hands the
//! entrypoint (`MENTAT_DUCKDB_POOL`, default one per core up to 8); each call
//! leases one. Each `edn_t` commits its own DuckDB transaction (it is not part
//! of the caller's), and writes to one store take turns.
//!
//! This crate is EXTENSION-ONLY. The `loadable-extension` feature replaces the
//! DuckDB C API functions; opening a normal client `Connection` here panics
//! with "API not initialized". Connections come only from the entrypoint's.

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    ffi::duckdb_string_t,
    types::DuckString,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::{arrow::WritableVector, BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use serde_json::{json, Value as Json};
use std::{
    error::Error,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Condvar, Mutex,
    },
    time::Duration,
};

use core_traits::{Binding, StructuredMap, TypedValue};
use mentat_core::TxReport;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_query_projector::QueryResults;

type BoxErr = Box<dyn Error>;

#[path = "../store/src/duck_conn.rs"]
mod duck_conn;

/// One of the extension's connections to the host database: a `DuckConn` over
/// a `duckdb::Connection`, both on the heap, freed only by `Conn::close`.
#[derive(Clone, Copy)]
struct Conn {
    duck: *mut duck_conn::DuckConn<'static>,
    con: *mut duckdb::Connection,
}

// SAFETY: a `duckdb::Connection` is `Send` but not `Sync` (statement-cache
// RefCell), and so is `DuckConn`. A `Conn` is in the pool or leased to exactly
// one call (`Pool::conn`), so no two threads use one connection at once.
unsafe impl Send for Conn {}

impl Conn {
    fn new(c: duckdb::Connection) -> Conn {
        let con = Box::into_raw(Box::new(c));
        // SAFETY: `con` stays allocated until `close`, which frees `duck` first.
        let duck = Box::into_raw(Box::new(duck_conn::DuckConn::new(unsafe { &*con })));
        Conn { duck, con }
    }

    /// Valid until `close`; a lease returns it to the pool before that.
    fn sql(&self) -> &'static dyn SqlConn {
        unsafe { &*self.duck }
    }

    /// Disconnect. SAFETY: no reference from `sql` may still be in use.
    unsafe fn close(self) {
        unsafe {
            drop(Box::from_raw(self.duck));
            let con = Box::from_raw(self.con);
            con.flush_prepared_statement_cache();
            drop(con);
        }
    }
}

/// The extension's connections to ONE database, made in the entrypoint. They
/// have to be made there: the database handle DuckDB passes the entrypoint is
/// only valid during that call (a later `duckdb_connect` on it fails). Each
/// `LOAD` makes its own pool and hands it to the functions it registers (their
/// state), so a call always runs on the database it was made in. A mentat call
/// leases one connection for its duration, so up to `MENTAT_DUCKDB_POOL` calls
/// run at once. Writes to one store still take turns (`DuckStore::transact`).
///
/// The pool's connections keep the database open, so once they are the only
/// connections left the pool closes them (`watch`) and the database shuts
/// down.
struct Pool {
    free: Mutex<Free>,
    returned: Condvar,
    size: usize,
}

struct Free {
    conns: Vec<Conn>,
    released: bool,
    /// The last check found the database idle (see `release_if_idle`).
    idle: bool,
}

const RELEASED: &str = "mentat: this database's mentat connections were closed when it had \
    no other connections left (MENTAT_DUCKDB_RELEASE_IDLE); close and reopen the database to \
    use mentat in it again";

/// Connections to open: `MENTAT_DUCKDB_POOL`, else one per core up to 8.
fn pool_size() -> usize {
    std::env::var("MENTAT_DUCKDB_POOL")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get().min(8)))
}

/// `MENTAT_DUCKDB_RELEASE_IDLE=0` keeps the pool open for the database's
/// lifetime (and so the database too), for clients that hold a database with
/// no connection open and connect to it again later.
fn release_idle() -> bool {
    std::env::var("MENTAT_DUCKDB_RELEASE_IDLE").map_or(true, |v| v.trim() != "0")
}

/// A leased connection; dropping it returns the connection to its pool.
struct Lease<'p>(&'p Pool, Conn);

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut free = self.0.free.lock().unwrap_or_else(|e| e.into_inner());
        free.conns.push(self.1);
        drop(free);
        self.0.returned.notify_one();
    }
}

impl Pool {
    /// A free connection, waiting for one if all are leased. The reference is
    /// valid only while the `Lease` lives.
    fn conn(&self) -> Result<(&'static dyn SqlConn, Lease<'_>), BoxErr> {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if free.released {
                return Err(RELEASED.into());
            }
            if let Some(c) = free.conns.pop() {
                return Ok((c.sql(), Lease(self, c)));
            }
            free = self.returned.wait(free).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// A store on a leased connection, plus the lease.
    fn store(&self, name: &str) -> Result<(DuckStore<'static>, Lease<'_>), BoxErr> {
        let (c, lease) = self.conn()?;
        Ok((DuckStore::new(c, name), lease))
    }

    /// Close the pool's connections if nothing else is connected to the
    /// database, at two checks in a row; true once closed (or the process is
    /// exiting). Two, so a client that closes a file and opens it again at
    /// once (see `sole_opener`) is seen again first. Decided and done under
    /// the pool lock with every connection free, so no call is running, and a
    /// call that starts meanwhile waits, then gets `RELEASED`.
    fn release_if_idle(&self) -> bool {
        let mut free = self.free.lock().unwrap_or_else(|e| e.into_inner());
        if free.released {
            return true;
        }
        let _check = CHECK.lock().unwrap_or_else(|e| e.into_inner());
        if EXITING.load(Ordering::SeqCst) {
            return true; // leave everything to the host's exit
        }
        let idle = free.conns.len() == self.size && {
            // SAFETY: all free, so none is leased; this thread has the pool lock.
            let con = unsafe { &*free.conns[0].con };
            let others = con
                .prepare_cached("SELECT count FROM duckdb_connection_count()")
                .and_then(|mut s| s.query_row([], |r| r.get::<_, u64>(0)))
                .map_or(1, |n| n.saturating_sub(self.size as u64));
            others == 0 && sole_opener(&files(con))
        };
        let was_idle = std::mem::replace(&mut free.idle, idle);
        if !(idle && was_idle) {
            return false;
        }
        // SAFETY: as above.
        let con = unsafe { &*free.conns[0].con };
        // The database shuts down on this thread once these close, after its
        // client closed it, so a client may already be opening the same file
        // again: the closing instance must not write to it. No checkpoint (the
        // WAL stays and the next open replays it, as 1.11-1.17.1 left it).
        let _ = con.execute_batch("PRAGMA disable_checkpoint_on_shutdown");
        free.released = true;
        for c in free.conns.drain(..) {
            // SAFETY: every connection is free (checked above), so none is in use.
            unsafe { c.close() };
        }
        drop(free);
        self.returned.notify_all();
        true
    }
}

/// The files of the database's attached databases. Prepared fresh: this runs
/// only while the database looks idle, and a reused plan can be stale.
fn files(con: &duckdb::Connection) -> Vec<String> {
    con.prepare("SELECT path FROM duckdb_databases() WHERE NOT internal AND path IS NOT NULL")
        .and_then(|mut s| {
            s.query_map([], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_else(|_| vec![String::new()]) // unknown: never release
}

/// Whether this database instance is the only opener of each file in this
/// process. A client that doesn't go through DuckDB's instance cache (the
/// CLI's `.open`, the C API's `duckdb_open`) can open the same file again
/// while this instance lives; POSIX file locks belong to the process, so
/// closing this instance's descriptor would drop that other instance's lock,
/// and another process could then open the file for writing. One descriptor
/// per file is this instance's own. (`stat` of `/dev/fd/N`: never opens and
/// closes a descriptor on the file, which would drop the lock itself.)
/// shortcut: a reopen that lands between this check and the file's close
/// (the release takes 5-10 ms) still loses its lock until it closes; only a
/// reopen 100-200 ms after the close can (`release_if_idle` checks twice).
/// Closing that window needs a DuckDB database-close hook.
#[cfg(unix)]
fn sole_opener(files: &[String]) -> bool {
    use std::os::unix::fs::MetadataExt;
    let id = |m: std::fs::Metadata| (m.dev(), m.ino());
    let Ok(ids) = files
        .iter()
        .map(|f| std::fs::metadata(f).map(id))
        .collect::<Result<Vec<_>, _>>()
    else {
        return false;
    };
    if ids.is_empty() {
        return true; // in-memory: no file
    }
    // Lists every descriptor on Linux and macOS; elsewhere (FreeBSD without
    // fdescfs) only 0-2, so this instance's own isn't found: never release.
    let Ok(fds) = std::fs::read_dir("/dev/fd") else {
        return false;
    };
    let mut n = vec![0; ids.len()];
    for m in fds
        .flatten()
        .filter_map(|e| std::fs::metadata(e.path()).ok())
    {
        if let Some(i) = ids.iter().position(|&x| x == id(m.clone())) {
            n[i] += 1;
        }
    }
    n.iter().all(|&k| k == 1)
}

/// Windows file locks belong to a handle, not the process: closing this
/// instance's handle can't unlock another instance's.
#[cfg(not(unix))]
fn sole_opener(_: &[String]) -> bool {
    cfg!(windows)
}

/// How often the watcher checks each pool. A pool is released at the second
/// idle check, so a reopen of the same file in this process waits 100-200 ms.
const WATCH: Duration = Duration::from_millis(100);

/// The pools not yet released, all checked by one watcher thread.
static POOLS: Mutex<Vec<Arc<Pool>>> = Mutex::new(Vec::new());

/// Have the watcher close `pool` once its database has no other connections
/// (see `Pool`). If the thread can't start, pools stay open, as with
/// `MENTAT_DUCKDB_RELEASE_IDLE=0`.
fn watch(pool: Arc<Pool>) {
    static STARTED: std::sync::Once = std::sync::Once::new();
    POOLS.lock().unwrap_or_else(|e| e.into_inner()).push(pool);
    STARTED.call_once(|| {
        unsafe { atexit(on_exit) };
        let _ = std::thread::Builder::new()
            .name("mentat-pool".into())
            .spawn(|| {
                while !EXITING.load(Ordering::SeqCst) {
                    std::thread::sleep(WATCH);
                    // Checked outside the list lock: a release shuts a database
                    // down, and a LOAD meanwhile must not wait for that.
                    let pools = POOLS.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let done: Vec<_> = pools.iter().filter(|p| p.release_if_idle()).collect();
                    if !done.is_empty() {
                        POOLS
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .retain(|p| !done.iter().any(|d| Arc::ptr_eq(p, d)));
                    }
                }
            });
    });
}

/// Set at process exit: from then on no pool checks or closes anything, so
/// none races the host's own teardown (static destructors, a language
/// runtime). `CHECK` is held for each check, so `on_exit` can wait (briefly,
/// bounded) for one under way. DuckDB never unloads an extension, so the
/// handler stays mapped.
static EXITING: AtomicBool = AtomicBool::new(false);
static CHECK: Mutex<()> = Mutex::new(());

extern "C" fn on_exit() {
    EXITING.store(true, Ordering::SeqCst);
    // A check takes well under a millisecond; never hold up exit for long.
    for _ in 0..1000 {
        if CHECK.try_lock().is_ok() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

extern "C" {
    fn atexit(f: extern "C" fn()) -> std::os::raw::c_int;
}

// ---------------------------------------------------------------------------
// Value rendering (plan §2.4 v1: all-VARCHAR).
// ---------------------------------------------------------------------------

fn value_as_string(value: &TypedValue) -> String {
    use TypedValue::*;
    match value {
        Boolean(b) => b.to_string(),
        Double(d) => d.to_string(),
        Instant(i) => i.to_rfc3339(),
        Keyword(k) => k.to_string(), // `:ns/name`
        Long(l) => l.to_string(),
        Ref(r) => r.to_string(),
        // Inside a vec/map a string is EDN-quoted; a top-level cell is raw (`cell`).
        String(s) => format!("{:?}", s.as_str()),
        Uuid(u) => u.to_string(), // hyphenated
        Bytes(b) => format!("#bytes {:?}", b.to_vec()),
    }
}

fn binding_as_string(value: &Binding) -> String {
    match value {
        Binding::Scalar(v) => value_as_string(v),
        Binding::Vec(v) => {
            let vals: Vec<String> = v.iter().map(binding_as_string).collect();
            format!("[{}]", vals.join(", "))
        }
        Binding::Map(m) => {
            let kvs: Vec<String> =
                m.0.iter()
                    .map(|(k, v)| format!("{} {}", k, binding_as_string(v)))
                    .collect();
            format!("{{{}}}", kvs.join(", "))
        }
    }
}

/// One top-level result cell. A string is returned RAW (`Alice`, not
/// `"Alice"`) so it compares/joins equal to a native DuckDB VARCHAR.
fn cell(b: &Binding) -> String {
    match b {
        Binding::Scalar(TypedValue::String(s)) => s.to_string(),
        other => binding_as_string(other),
    }
}

// ---------------------------------------------------------------------------
// Scalar plumbing shared by edn_t / edn_pull / edn_eval.
// ---------------------------------------------------------------------------

/// Row `i` of VARCHAR column `col`; `None` if NULL.
fn str_arg(input: &DataChunkHandle, col: usize, i: usize) -> Option<String> {
    let v = input.flat_vector(col);
    if v.row_is_null(i as u64) {
        return None;
    }
    let mut s = unsafe { v.as_slice_with_len::<duckdb_string_t>(input.len()) }[i];
    Some(DuckString::new(&mut s).as_str().to_string())
}

/// Row `i` of BIGINT column `col`; `None` if NULL.
fn i64_arg(input: &DataChunkHandle, col: usize, i: usize) -> Option<i64> {
    let v = input.flat_vector(col);
    if v.row_is_null(i as u64) {
        return None;
    }
    Some(unsafe { v.as_slice_with_len::<i64>(input.len()) }[i])
}

/// Fill the VARCHAR output with `f(row)`; `None` -> NULL (any NULL arg).
fn map_rows(
    input: &DataChunkHandle,
    output: &mut dyn WritableVector,
    f: impl Fn(usize) -> Result<Option<String>, BoxErr>,
) -> Result<(), BoxErr> {
    let mut out = output.flat_vector();
    for i in 0..input.len() {
        match f(i)? {
            Some(s) => out.insert(i, s.as_str()),
            None => out.set_null(i),
        }
    }
    Ok(())
}

fn varchar() -> LogicalTypeHandle {
    LogicalTypeId::Varchar.into()
}

// ---------------------------------------------------------------------------
// edn_t(db_path, edn) -> VARCHAR (JSON tx-report). Plan §2, §6, §7.
// ---------------------------------------------------------------------------

fn tx_report_json(report: &TxReport) -> String {
    let tempids: serde_json::Map<String, Json> = report
        .tempids
        .iter()
        .map(|(k, v)| (k.clone(), Json::from(*v)))
        .collect();
    json!({
        "tx_id": report.tx_id,
        "tx_instant": report.tx_instant.to_rfc3339(),
        "tempids": tempids,
    })
    .to_string()
}

struct EdnTransact;

impl VScalar for EdnTransact {
    type State = Arc<Pool>;

    fn invoke(
        pool: &Arc<Pool>,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(edn)) = (str_arg(input, 0, i), str_arg(input, 1, i)) else {
                return Ok(None);
            };
            // With the `script` feature, user transaction functions (`:db/fn`)
            // expand first (issue #333).
            #[cfg(feature = "script")]
            let report = {
                let (c, _lease) = pool.conn()?;
                mentat_duckdb_store::script::transact(c, &db, &edn)?
            };
            #[cfg(not(feature = "script"))]
            let report = {
                let (store, _lease) = pool.store(&db)?;
                store.transact(&edn)?
            };
            Ok(Some(tx_report_json(&report)))
        })
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![varchar(), varchar()],
            varchar(),
        )]
    }

    /// Mutates the store: volatile so DuckDB evaluates it exactly once per row
    /// (never constant-folded or re-evaluated -> no double-applied tx).
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// edn_pull(db_path, pattern, entity) -> VARCHAR (JSON, pg_mentat's shape).
// ---------------------------------------------------------------------------

/// pg_mentat's pull value encoding (`decode_row_typed_value`): refs are
/// `{":db/id": n}`, keywords `":ns/name"`, instants epoch micros, bytes hex.
fn typed_json(tv: &TypedValue) -> Json {
    use TypedValue::*;
    match tv {
        Ref(r) => json!({ ":db/id": r }),
        Boolean(b) => json!(b),
        Long(l) => json!(l),
        Double(d) => json!(d.into_inner()),
        String(s) => json!(s.as_str()),
        Keyword(k) => json!(k.to_string()),
        Instant(t) => json!(t.timestamp_micros()),
        Uuid(u) => json!(u.to_string()),
        Bytes(b) => json!(b
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<std::string::String>()),
    }
}

fn binding_json(b: &Binding) -> Json {
    match b {
        Binding::Scalar(tv) => typed_json(tv),
        Binding::Vec(vs) => Json::Array(vs.iter().map(binding_json).collect()),
        Binding::Map(m) => Json::Object(map_json(m)),
    }
}

fn map_json(m: &StructuredMap) -> serde_json::Map<String, Json> {
    m.0.iter()
        .map(|(k, v)| (k.to_string(), binding_json(v)))
        .collect()
}

fn pull_json(pool: &Pool, db: &str, pattern: &str, eid: i64) -> Result<String, BoxErr> {
    // The pattern is spliced into a `(pull ?e …)` query (reusing mentat's pull
    // grammar), so require it to be exactly one EDN vector: it cannot close the
    // form early and inject clauses.
    if !matches!(
        edn::parse::value(pattern).map(|v| v.without_spans()),
        Ok(edn::Value::Vector(_))
    ) {
        return Err(format!(
            "edn_pull: pattern must be an EDN vector like [*] or [:person/name], got {pattern}"
        )
        .into());
    }
    let query = format!("[:find (pull ?e {pattern}) . :in ?e :where [?e _ _]]");
    let inputs = mentat_query_algebrizer::QueryInputs::with_value_sequence(vec![(
        edn::query::Variable::from_valid_name("?e"),
        TypedValue::Ref(eid),
    )]);
    let (store, _lease) = pool.store(db)?;
    let mut m = match store.q(&query, Some(inputs), None)?.results {
        QueryResults::Scalar(Some(Binding::Map(sm))) => map_json(&sm),
        _ => serde_json::Map::new(),
    };
    // Always present, as a plain integer (pg_mentat does the same).
    m.insert(":db/id".into(), json!(eid));
    Ok(Json::Object(m).to_string())
}

struct EdnPull;

impl VScalar for EdnPull {
    type State = Arc<Pool>;

    fn invoke(
        pool: &Arc<Pool>,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(pat), Some(e)) = (
                str_arg(input, 0, i),
                str_arg(input, 1, i),
                i64_arg(input, 2, i),
            ) else {
                return Ok(None);
            };
            pull_json(pool, &db, &pat, e).map(Some)
        })
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![varchar(), varchar(), LogicalTypeId::Bigint.into()],
            varchar(),
        )]
    }

    /// Reads external, mutable state (the store file): never constant-fold.
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// edn_eval(db_path, script) -> VARCHAR (EDN). Feature `script`.
// ---------------------------------------------------------------------------

#[cfg(feature = "script")]
struct EdnEval;

#[cfg(feature = "script")]
impl VScalar for EdnEval {
    type State = Arc<Pool>;

    fn invoke(
        pool: &Arc<Pool>,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(src)) = (str_arg(input, 0, i), str_arg(input, 1, i)) else {
                return Ok(None);
            };
            // Sandboxed (no host fs prims) + step/heap/depth limits; a no-arg
            // `(mentat.store/open)` opens store `db`.
            let (c, _lease) = pool.conn()?;
            let out = mentat_duckdb_store::script::eval(c, &db, &src)
                .map_err(|e| format!("edn_eval: {e}"))?;
            Ok(Some(out))
        })
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![varchar(), varchar()],
            varchar(),
        )]
    }

    /// Scripts can transact: evaluate exactly once per row.
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// edn_q options: {"inputs": [...], "asOf": T, "since": T} (pg_mentat's shape).
// Parsed by mentat_transaction::options::options_from_json, shared with the SQLite extension and CLI.
// ---------------------------------------------------------------------------

fn parse_opts(text: Option<&str>) -> Result<Json, String> {
    let text = text.map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Ok(Json::Null);
    }
    serde_json::from_str(text).map_err(|e| format!("edn_q: options are not valid JSON: {e}"))
}

// ---------------------------------------------------------------------------
// edn_q(db_path, query, options) -> table (all VARCHAR). Plan §2.2.
// ---------------------------------------------------------------------------

/// Materialized rows-of-columns from a QueryResults (plan §2.2 normalization).
struct EdnQueryBindData {
    rows: Vec<Vec<String>>,
    ncols: usize,
}

struct EdnQueryInitData {
    /// Next row index to emit; guarded so `func` can be re-entered safely.
    cursor: Mutex<usize>,
}

/// Normalize the four QueryResults arms into rows-of-columns of strings.
fn results_to_rows(results: QueryResults) -> Vec<Vec<String>> {
    match results {
        QueryResults::Scalar(v) => v.iter().map(|b| vec![cell(b)]).collect(),
        QueryResults::Coll(vs) => vs.iter().map(|b| vec![cell(b)]).collect(),
        QueryResults::Tuple(vv) => vv
            .iter()
            .map(|row| row.iter().map(cell).collect())
            .collect(),
        QueryResults::Rel(rel) => rel
            .rows()
            .map(|row| row.iter().map(cell).collect())
            .collect(),
    }
}

/// A VARCHAR bind parameter; `None` if SQL NULL.
fn param(bind: &BindInfo, i: u64) -> Option<String> {
    let v = bind.get_parameter(i);
    (!v.is_null()).then(|| v.to_string())
}

struct EdnQuery;

impl VTab for EdnQuery {
    type InitData = EdnQueryInitData;
    type BindData = EdnQueryBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, BoxErr> {
        let db_path = param(bind, 0).ok_or("edn_q: db_path must not be NULL")?;
        let query = param(bind, 1).ok_or("edn_q: query must not be NULL")?;
        let opts = parse_opts(param(bind, 2).as_deref())?;

        // Run the query once to learn the FindSpec columns AND materialize
        // the results (smallest correct diff).
        // SAFETY: the extra info is the `Arc<Pool>` the entrypoint registered
        // `edn_q` with; DuckDB keeps it alive as long as the function.
        let pool = unsafe { &*bind.get_extra_info::<Arc<Pool>>() };
        let (store, _lease) = pool.store(&db_path)?;
        let output = store.q_json(&query, &opts).map_err(|e| match e {
            public_traits::errors::MentatError::BadQueryOptions(m) => format!("edn_q: {m}"),
            other => other.to_string(),
        })?;

        // Declare one VARCHAR column per FindSpec element, named like the CLI.
        let mut ncols = 0;
        for element in output.spec.columns() {
            bind.add_result_column(&element.to_string(), varchar());
            ncols += 1;
        }

        let rows = results_to_rows(output.results);
        Ok(EdnQueryBindData { rows, ncols })
    }

    fn init(info: &InitInfo) -> Result<Self::InitData, BoxErr> {
        // The whole result set is materialized in one BindData shared across
        // scan threads; a single cursor streams it. Force single-threaded scan
        // so each row is emitted exactly once (parallel scans would each
        // re-emit the full set). ponytail: fine for a materialized result;
        // partition across threads only if huge result sets need it.
        info.set_max_threads(1);
        Ok(EdnQueryInitData {
            cursor: Mutex::new(0),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), BoxErr> {
        let bind_data = func.get_bind_data();
        let init_data = func.get_init_data();
        let mut cursor = init_data.cursor.lock().unwrap();

        // DuckDB's STANDARD_VECTOR_SIZE; the output chunk is pre-sized to this.
        // ponytail: hardcoded 2048 (the crate exposes no capacity accessor at
        // v1.5.6); revisit if a future crate version adds one.
        const VECTOR_SIZE: usize = 2048;
        let remaining = bind_data.rows.len().saturating_sub(*cursor);
        let n = remaining.min(VECTOR_SIZE);

        for col in 0..bind_data.ncols {
            let vec = output.flat_vector(col);
            for r in 0..n {
                vec.insert(r, bind_data.rows[*cursor + r][col].as_str());
            }
        }
        *cursor += n;
        output.set_len(n);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![varchar(), varchar(), varchar()]) // db_path, query, options
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    // The functions run SQL against the database the extension is being
    // loaded into, on clones of the host connection made now (see `Pool`).
    // Each load gets its own pool, carried in the functions' state, so a
    // second database in the process never shares the first one's.
    let size = pool_size();
    let mut conns = Vec::with_capacity(size);
    for _ in 0..size {
        conns.push(Conn::new(con.try_clone()?));
    }
    let pool = Arc::new(Pool {
        free: Mutex::new(Free {
            conns,
            released: false,
            idle: false,
        }),
        returned: Condvar::new(),
        size,
    });
    con.register_scalar_function_with_state::<EdnTransact>("edn_t", &pool)?;
    con.register_table_function_with_extra_info::<EdnQuery, _>("edn_q", &pool)?;
    con.register_scalar_function_with_state::<EdnPull>("edn_pull", &pool)?;
    #[cfg(feature = "script")]
    con.register_scalar_function_with_state::<EdnEval>("edn_eval", &pool)?;
    if release_idle() {
        watch(pool);
    }
    Ok(())
}
