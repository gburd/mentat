//! Test-only: the DuckDB `SqlConn`, built against a bundled (client) duckdb.
#[path = "../../src/duck_conn.rs"]
pub mod duck_conn;
pub use duck_conn::DuckConn;
