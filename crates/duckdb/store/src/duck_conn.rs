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
    params: &[&dyn duckdb::ToSql],
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
pub struct DuckConn<'c>(pub &'c duckdb::Connection);

impl SqlConn for DuckConn<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::DuckDb
    }

    fn execute_batch(&self, sql: &str) -> Result<(), SqlError> {
        self.0.execute_batch(sql).map_err(err)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<usize, SqlError> {
        let ps: Vec<Param> = params.iter().map(Param).collect();
        let refs: Vec<&dyn duckdb::ToSql> = ps.iter().map(|p| p as &dyn duckdb::ToSql).collect();
        self.0.execute(sql, refs.as_slice()).map_err(err)
    }

    fn query(
        &self,
        sql: &str,
        params: &[SqlValue],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let ps: Vec<Param> = params.iter().map(Param).collect();
        let refs: Vec<&dyn duckdb::ToSql> = ps.iter().map(|p| p as &dyn duckdb::ToSql).collect();
        let mut stmt = self.0.prepare(sql).map_err(err)?;
        each_row(&mut stmt, &refs, f)
    }

    fn query_named(
        &self,
        sql: &str,
        params: &[(&str, SqlValue)],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let mut stmt = self.0.prepare(sql).map_err(err)?;
        if params.is_empty() {
            return each_row(&mut stmt, &[], f);
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
        let mut rows = stmt.query(named.as_slice()).map_err(err)?;
        let n = rows.as_ref().map(|s| s.column_count()).unwrap_or(0);
        while let Some(row) = rows.next().map_err(err)? {
            f(&DuckRow(row, n))?;
        }
        Ok(())
    }

    fn max_variables(&self) -> usize {
        // DuckDB has no fixed parameter limit; keep statements a sane size.
        32766
    }

    fn column_count(&self, sql: &str) -> Result<usize, SqlError> {
        let mut stmt = self.0.prepare(sql).map_err(err)?;
        stmt.execute([]).map_err(err)?;
        Ok(stmt.column_count())
    }
}
