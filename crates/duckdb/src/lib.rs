//! `mentat` as a DuckDB loadable extension.
//!
//! Exposes the embedded `mentat` SQLite store (plan §3.1 option (a)) through
//! DuckDB:
//!   - `mentat_hello()`                        table fn, M0 smoke test.
//!   - `mentat_transact(db_path, edn)`         scalar -> VARCHAR (JSON tx-report).
//!   - `mentat_query(db_path, query, inputs)`  table fn -> rows, all VARCHAR.
//!
//! Storage is per-call `Store::open` (plan §3.1): open -> use -> drop within one
//! function invocation. `Store` is `!Sync`; never shared across DuckDB threads.
//!
//! This crate is EXTENSION-ONLY. The `loadable-extension` feature replaces the
//! DuckDB C API functions; opening a normal `Connection` here panics with
//! "API not initialized" (plan §9 risk 5). Do not add client `Connection` use.

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    ffi::duckdb_string_t,
    types::DuckString,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::{arrow::WritableVector, BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use std::{
    error::Error,
    ffi::CString,
    sync::atomic::{AtomicBool, Ordering},
    sync::Mutex,
};

use mentat::{
    Binding, QueryResults, Queryable, Store, StructuredMap, TxReport, TypedValue,
};

// ---------------------------------------------------------------------------
// Value rendering (plan §2.4 v1: all-VARCHAR, stringified like the SQLite CLI's
// binding_as_string / value_as_string in crates/sqlite/cli/.../repl.rs).
// ---------------------------------------------------------------------------

fn value_as_string(value: &TypedValue) -> String {
    use TypedValue::*;
    match value {
        Boolean(b) => {
            if *b {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
        Double(d) => format!("{}", d),
        Instant(i) => format!("{}", i),
        Keyword(k) => format!("{}", k),
        Long(l) => format!("{}", l),
        Ref(r) => format!("{}", r),
        String(s) => format!("{:?}", s.to_string()),
        Uuid(u) => format!("{}", u),
        Bytes(b) => format!("#bytes {:?}", b.to_vec()),
    }
}

fn vec_as_string(value: &[Binding]) -> String {
    let vals: Vec<String> = value.iter().map(binding_as_string).collect();
    format!("[{}]", vals.join(", "))
}

fn map_as_string(value: &StructuredMap) -> String {
    let mut out = std::string::String::from("{");
    let mut first = true;
    for (k, v) in value.0.iter() {
        if !first {
            out.push_str(", ");
        }
        first = false;
        out.push_str(&k.to_string());
        out.push(' ');
        out.push_str(&binding_as_string(v));
    }
    out.push('}');
    out
}

fn binding_as_string(value: &Binding) -> String {
    match value {
        Binding::Scalar(v) => value_as_string(v),
        Binding::Map(v) => map_as_string(v),
        Binding::Vec(v) => vec_as_string(v),
    }
}

// ---------------------------------------------------------------------------
// mentat_hello() — M0 smoke test, one constant row.
// ---------------------------------------------------------------------------

#[repr(C)]
struct HelloInitData {
    done: AtomicBool,
}

struct HelloVTab;

impl VTab for HelloVTab {
    type InitData = HelloInitData;
    type BindData = ();

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        bind.add_result_column("greeting", LogicalTypeHandle::from(LogicalTypeId::Varchar));
        Ok(())
    }

    fn init(_: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        Ok(HelloInitData {
            done: AtomicBool::new(false),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        if func.get_init_data().done.swap(true, Ordering::Relaxed) {
            output.set_len(0);
        } else {
            let vector = output.flat_vector(0);
            vector.insert(0, CString::new("mentat duckdb extension loaded")?);
            output.set_len(1);
        }
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![])
    }
}

// ---------------------------------------------------------------------------
// mentat_transact(db_path, edn) -> VARCHAR (JSON tx-report). Plan §2, §6, §7.
// ---------------------------------------------------------------------------

fn tx_report_json(report: &TxReport) -> String {
    let tempids: serde_json::Map<std::string::String, serde_json::Value> = report
        .tempids
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::Value::from(*v)))
        .collect();
    serde_json::json!({
        "tx_id": report.tx_id,
        "tx_instant": report.tx_instant.to_rfc3339(),
        "tempids": tempids,
    })
    .to_string()
}

struct MentatTransactScalar;

impl VScalar for MentatTransactScalar {
    type State = ();

    fn invoke(
        _state: &Self::State,
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), Box<dyn Error>> {
        let path_vec = input.flat_vector(0);
        let edn_vec = input.flat_vector(1);
        let paths = unsafe { path_vec.as_slice_with_len::<duckdb_string_t>(input.len()) };
        let edns = unsafe { edn_vec.as_slice_with_len::<duckdb_string_t>(input.len()) };
        let mut out = output.flat_vector();

        for i in 0..input.len() {
            if path_vec.row_is_null(i as u64) || edn_vec.row_is_null(i as u64) {
                out.set_null(i);
                continue;
            }
            let mut p = paths[i];
            let mut e = edns[i];
            let db_path = DuckString::new(&mut p).as_str().to_string();
            let edn = DuckString::new(&mut e).as_str().to_string();

            // Per-call open (plan §3.1). !Sync store, never shared.
            let mut store = Store::open(&db_path)?;
            let report = store.transact(&edn)?;
            out.insert(i, tx_report_json(&report).as_str());
        }
        Ok(())
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![
                LogicalTypeId::Varchar.into(),
                LogicalTypeId::Varchar.into(),
            ],
            LogicalTypeId::Varchar.into(),
        )]
    }

    /// mentat_transact mutates the store, so it is NOT pure. Mark it volatile
    /// so DuckDB evaluates it exactly once per row (a non-volatile scalar can be
    /// constant-folded or re-evaluated, which would apply the transaction more
    /// than once).
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// mentat_query(db_path, query, inputs) -> table (all VARCHAR). Plan §2.2.
// ---------------------------------------------------------------------------

/// Materialized rows-of-columns from a QueryResults (plan §2.2 normalization).
struct MentatQueryBindData {
    rows: Vec<Vec<std::string::String>>,
    ncols: usize,
}

struct MentatQueryInitData {
    /// Next row index to emit; guarded so `func` can be re-entered safely.
    cursor: Mutex<usize>,
}

/// Normalize the four QueryResults arms into rows-of-columns of strings.
fn results_to_rows(results: QueryResults) -> Vec<Vec<std::string::String>> {
    match results {
        QueryResults::Scalar(v) => v
            .into_iter()
            .map(|b| vec![binding_as_string(&b)])
            .collect(),
        QueryResults::Coll(vs) => vs
            .into_iter()
            .map(|b| vec![binding_as_string(&b)])
            .collect(),
        QueryResults::Tuple(vv) => vv
            .into_iter()
            .map(|row| row.iter().map(binding_as_string).collect())
            .collect(),
        QueryResults::Rel(rel) => rel
            .rows()
            .map(|row| row.iter().map(binding_as_string).collect())
            .collect(),
    }
}

struct MentatQueryVTab;

impl VTab for MentatQueryVTab {
    type InitData = MentatQueryInitData;
    type BindData = MentatQueryBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, Box<dyn Error>> {
        let db_path = bind.get_parameter(0).to_string();
        let query = bind.get_parameter(1).to_string();
        // inputs: `{}` / `null` / empty == no inputs. M1 only supports
        // no-input queries; `:in` binding is a follow-up (README).
        let _inputs = bind.get_parameter(2).to_string();

        // Per-call open (plan §3.1); run q_once once to learn the FindSpec
        // columns AND materialize the results (smallest correct diff).
        let store = Store::open(&db_path)?;
        let output = store.q_once(&query, None)?;

        // Declare one VARCHAR column per FindSpec element, named like the CLI.
        let mut ncols = 0;
        for element in output.spec.columns() {
            bind.add_result_column(
                &element.to_string(),
                LogicalTypeHandle::from(LogicalTypeId::Varchar),
            );
            ncols += 1;
        }

        let rows = results_to_rows(output.results);
        Ok(MentatQueryBindData { rows, ncols })
    }

    fn init(info: &InitInfo) -> Result<Self::InitData, Box<dyn Error>> {
        // The whole result set is materialized in one BindData shared across
        // scan threads; a single cursor streams it. Force single-threaded scan
        // so each row is emitted exactly once (parallel scans would each
        // re-emit the full set). ponytail: fine for a materialized result;
        // partition across threads only if huge result sets need it.
        info.set_max_threads(1);
        Ok(MentatQueryInitData {
            cursor: Mutex::new(0),
        })
    }

    fn func(
        func: &TableFunctionInfo<Self>,
        output: &mut DataChunkHandle,
    ) -> Result<(), Box<dyn Error>> {
        let bind_data = func.get_bind_data();
        let init_data = func.get_init_data();
        let mut cursor = init_data.cursor.lock().unwrap();

        // DuckDB's STANDARD_VECTOR_SIZE; the output chunk is pre-sized to this.
        // ponytail: hardcoded 2048 (the crate exposes no capacity accessor at
        // v1.5.5); revisit if a future crate version adds one.
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
        Some(vec![
            LogicalTypeHandle::from(LogicalTypeId::Varchar), // db_path
            LogicalTypeHandle::from(LogicalTypeId::Varchar), // query
            LogicalTypeHandle::from(LogicalTypeId::Varchar), // inputs
        ])
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<HelloVTab>("mentat_hello")?;
    con.register_scalar_function::<MentatTransactScalar>("mentat_transact")?;
    con.register_table_function::<MentatQueryVTab>("mentat_query")?;
    Ok(())
}
