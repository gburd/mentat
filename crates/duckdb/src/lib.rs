//! `mentat` as a DuckDB loadable extension.
//!
//! M0 scaffold: a constant-row `mentat_hello()` table function proving the
//! loadable-extension build + `LOAD` path works on this host. M1 adds
//! `mentat_transact` / `mentat_query` over the embedded SQLite store.
//!
//! This crate is EXTENSION-ONLY. The `loadable-extension` feature replaces the
//! DuckDB C API functions; opening a normal `Connection` here panics with
//! "API not initialized" (plan §9 risk 5). Do not add client `Connection` use.

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    vtab::{BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use std::{
    error::Error,
    ffi::CString,
    sync::atomic::{AtomicBool, Ordering},
};

#[repr(C)]
struct HelloInitData {
    done: AtomicBool,
}

/// `mentat_hello() -> table(column0 VARCHAR)` — one constant row. M0 smoke test.
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

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_table_function::<HelloVTab>("mentat_hello")?;
    Ok(())
}
