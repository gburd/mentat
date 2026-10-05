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
//! SQL runs on a connection cloned from the one DuckDB hands the entrypoint,
//! one per DuckDB thread. Each `edn_t` commits its own DuckDB transaction (it
//! is not part of the caller's).
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
use std::{error::Error, sync::Mutex, sync::OnceLock};

use core_traits::{Binding, StructuredMap, TypedValue};
use mentat_core::TxReport;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_query_projector::QueryResults;

type BoxErr = Box<dyn Error>;

#[path = "../store/src/duck_conn.rs"]
mod duck_conn;

/// The extension's connection to the host database, made in the entrypoint.
/// It has to be made there: the database handle DuckDB passes the entrypoint
/// is only valid during that call (a later `duckdb_connect` on it fails), so
/// one connection is opened up front and every call shares it.
struct Shared(duck_conn::DuckConn<'static>);

// SAFETY: a `duckdb::Connection` is `Send` but not `Sync` (statement-cache
// RefCell). Every use goes through `conn()`, which holds `CALL` for the whole
// call, so the connection is never touched by two threads at once.
unsafe impl Sync for Shared {}
unsafe impl Send for Shared {}

static SHARED: OnceLock<Shared> = OnceLock::new();

/// Serializes mentat calls on the shared connection.
// ponytail: one connection, so mentat calls run one at a time (DuckDB's own
// work inside each call is still parallel). Upgrade: a pool of connections
// opened in the entrypoint.
static CALL: Mutex<()> = Mutex::new(());

/// The shared connection, locked for the caller's scope.
fn conn() -> Result<(&'static dyn SqlConn, std::sync::MutexGuard<'static, ()>), BoxErr> {
    let guard = CALL.lock().unwrap_or_else(|e| e.into_inner());
    let shared = SHARED.get().ok_or("mentat: extension not initialized")?;
    Ok((&shared.0 as &'static dyn SqlConn, guard))
}

/// A store on the shared connection, plus the lock that guards it.
fn store(name: &str) -> Result<(DuckStore<'static>, std::sync::MutexGuard<'static, ()>), BoxErr> {
    let (c, guard) = conn()?;
    Ok((DuckStore::new(c, name), guard))
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
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(edn)) = (str_arg(input, 0, i), str_arg(input, 1, i)) else {
                return Ok(None);
            };
            let (store, _lock) = store(&db)?;
            let report = store.transact(&edn)?;
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

fn pull_json(db: &str, pattern: &str, eid: i64) -> Result<String, BoxErr> {
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
    let (store, _lock) = store(db)?;
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
    type State = ();

    fn invoke(
        _: &(),
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
            pull_json(&db, &pat, e).map(Some)
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
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(src)) = (str_arg(input, 0, i), str_arg(input, 1, i)) else {
                return Ok(None);
            };
            // Sandboxed (no host fs prims) + step/heap/depth limits; a no-arg
            // `(mentat.store/open)` opens store `db`.
            let (c, _lock) = conn()?;
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
        let (store, _lock) = store(&db_path)?;
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
    // Keep a clone of the host connection: the functions run SQL against the
    // database the extension was loaded into.
    let mine: &'static duckdb::Connection = Box::leak(Box::new(con.try_clone()?));
    let _ = SHARED.set(Shared(duck_conn::DuckConn(mine)));
    con.register_scalar_function::<EdnTransact>("edn_t")?;
    con.register_table_function::<EdnQuery>("edn_q")?;
    con.register_scalar_function::<EdnPull>("edn_pull")?;
    #[cfg(feature = "script")]
    con.register_scalar_function::<EdnEval>("edn_eval")?;
    Ok(())
}
