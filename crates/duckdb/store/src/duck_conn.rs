// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! `SqlConn` over a `duckdb::Connection`.
//!
//! This one file is compiled into two crates that build duckdb differently:
//! the extension cdylib (`loadable-extension`: the C API comes from the host
//! DuckDB) and the store's test harness (`bundled`). Cargo unifies features per
//! build, so the two can't share a library crate; they `#[path]`-include this.
//!
//! The DuckDB `v` column is `UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)`;
//! a value read from it arrives as `Value::Union(member)`, unwrapped here into
//! the matching `SqlValue`.

use std::cell::RefCell;

use duckdb::types::{ToSqlOutput, Value as DValue};
use mentat_duckdb_store::{Dialect, SqlConn, SqlError, SqlRow, SqlValue};

fn err(e: duckdb::Error) -> SqlError {
    SqlError::new(e.to_string())
}

fn to_duck(v: &SqlValue) -> DValue {
    match v {
        SqlValue::Null => DValue::Null,
        SqlValue::Integer(i) => DValue::BigInt(*i),
        SqlValue::Real(r) => DValue::Double(*r),
        SqlValue::Text(s) => DValue::Text(s.clone()),
        SqlValue::Blob(b) => DValue::Blob(b.clone()),
    }
}

fn from_duck(v: DValue) -> Result<SqlValue, SqlError> {
    Ok(match v {
        DValue::Null => SqlValue::Null,
        DValue::Boolean(b) => SqlValue::Integer(b.into()),
        DValue::TinyInt(i) => SqlValue::Integer(i.into()),
        DValue::SmallInt(i) => SqlValue::Integer(i.into()),
        DValue::Int(i) => SqlValue::Integer(i.into()),
        DValue::BigInt(i) => SqlValue::Integer(i),
        // sum(BIGINT) is HUGEINT in DuckDB.
        DValue::HugeInt(i) => SqlValue::Integer(
            i64::try_from(i).map_err(|_| SqlError::new(format!("integer out of range: {i}")))?,
        ),
        DValue::UTinyInt(i) => SqlValue::Integer(i.into()),
        DValue::USmallInt(i) => SqlValue::Integer(i.into()),
        DValue::UInt(i) => SqlValue::Integer(i.into()),
        DValue::UBigInt(i) => SqlValue::Integer(
            i64::try_from(i).map_err(|_| SqlError::new(format!("integer out of range: {i}")))?,
        ),
        DValue::Float(f) => SqlValue::Real(f.into()),
        DValue::Double(f) => SqlValue::Real(f),
        // avg() and friends can return DECIMAL.
        DValue::Decimal(d) => SqlValue::Real(
            d.to_string()
                .parse()
                .map_err(|_| SqlError::new(format!("bad decimal: {d}")))?,
        ),
        DValue::Text(s) | DValue::Enum(s) => SqlValue::Text(s),
        DValue::Blob(b) => SqlValue::Blob(b),
        DValue::Union(inner) => from_duck(*inner)?,
        other => {
            return Err(SqlError::new(format!(
                "unsupported DuckDB value: {other:?}"
            )))
        }
    })
}

struct Param<'a>(&'a SqlValue);

impl duckdb::ToSql for Param<'_> {
    fn to_sql(&self) -> duckdb::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::Owned(to_duck(self.0)))
    }
}

struct DuckRow<'a, 'stmt>(&'a duckdb::Row<'stmt>, usize);

impl SqlRow for DuckRow<'_, '_> {
    fn get_value(&self, idx: usize) -> Result<SqlValue, SqlError> {
        let v: DValue = self.0.get_ref(idx).map_err(err)?.to_owned();
        from_duck(v)
    }

    fn column_count(&self) -> usize {
        self.1
    }
}

fn each_row(
    stmt: &mut duckdb::Statement<'_>,
    params: impl duckdb::Params,
    f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
) -> Result<(), SqlError> {
    let mut rows = stmt.query(params).map_err(err)?;
    let n = rows.as_ref().map(|s| s.column_count()).unwrap_or(0);
    while let Some(row) = rows.next().map_err(err)? {
        f(&DuckRow(row, n))?;
    }
    Ok(())
}

/// `SqlConn` over a borrowed DuckDB connection.
///
/// `query_cached` / `execute_cached` statements stay prepared in the
/// connection's statement cache (a fresh prepare is 0.1-0.2 ms; the
/// generation read itself runs in 0.05). Everything else, including every
/// engine query, is prepared fresh; see `SqlConn::query_cached` for why. A
/// cached statement keeps the tables it resolved, so its key carries the store
/// schema that `use_schema` set.
pub struct DuckConn<'c> {
    conn: &'c duckdb::Connection,
    schema: RefCell<String>,
}

impl<'c> DuckConn<'c> {
    pub fn new(conn: &'c duckdb::Connection) -> Self {
        // A few fixed statements per store.
        conn.set_prepared_statement_cache_capacity(32);
        DuckConn {
            conn,
            schema: RefCell::new(String::new()),
        }
    }

    fn cached(&self, sql: &str) -> Result<duckdb::CachedStatement<'c>, SqlError> {
        let key = format!("/* {} */ {sql}", self.schema.borrow());
        self.conn.prepare_cached(&key).map_err(err)
    }
}

fn params<'p>(ps: &'p [Param<'p>]) -> Vec<&'p dyn duckdb::ToSql> {
    ps.iter().map(|p| p as &dyn duckdb::ToSql).collect()
}

impl SqlConn for DuckConn<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::DuckDb
    }

    fn execute_batch(&self, sql: &str) -> Result<(), SqlError> {
        self.conn.execute_batch(sql).map_err(err)
    }

    fn execute(&self, sql: &str, ps: &[SqlValue]) -> Result<usize, SqlError> {
        let ps: Vec<Param> = ps.iter().map(Param).collect();
        self.conn.execute(sql, params(&ps).as_slice()).map_err(err)
    }

    fn query(
        &self,
        sql: &str,
        ps: &[SqlValue],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let ps: Vec<Param> = ps.iter().map(Param).collect();
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        each_row(&mut stmt, params(&ps).as_slice(), f)
    }

    fn query_cached(
        &self,
        sql: &str,
        ps: &[SqlValue],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let ps: Vec<Param> = ps.iter().map(Param).collect();
        let mut stmt = self.cached(sql)?;
        each_row(&mut stmt, params(&ps).as_slice(), f)
    }

    fn execute_cached(&self, sql: &str, ps: &[SqlValue]) -> Result<usize, SqlError> {
        let ps: Vec<Param> = ps.iter().map(Param).collect();
        self.cached(sql)?
            .execute(params(&ps).as_slice())
            .map_err(err)
    }

    fn query_named(
        &self,
        sql: &str,
        params: &[(&str, SqlValue)],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        if params.is_empty() {
            return each_row(&mut stmt, [], f);
        }
        // duckdb-rs binds named parameters by name without the `$`.
        let ps: Vec<(String, Param)> = params
            .iter()
            .map(|(k, v)| (k.trim_start_matches('$').to_string(), Param(v)))
            .collect();
        let named: Vec<(&str, &dyn duckdb::ToSql)> = ps
            .iter()
            .map(|(k, p)| (k.as_str(), p as &dyn duckdb::ToSql))
            .collect();
        each_row(&mut stmt, named.as_slice(), f)
    }

    fn max_variables(&self) -> usize {
        // DuckDB has no fixed parameter limit; keep statements a sane size.
        32766
    }

    fn column_count(&self, sql: &str) -> Result<usize, SqlError> {
        let mut stmt = self.conn.prepare(sql).map_err(err)?;
        stmt.execute([]).map_err(err)?;
        Ok(stmt.column_count())
    }

    fn use_schema(&self, schema: &str) -> Result<(), SqlError> {
        // Always set: another `DuckConn` on the same connection may have
        // moved it.
        self.conn
            .execute_batch(&format!("SET search_path = '{schema}'"))
            .map_err(err)?;
        *self.schema.borrow_mut() = schema.to_string();
        Ok(())
    }
}
