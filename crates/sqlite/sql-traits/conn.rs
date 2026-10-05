// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! The storage seam: the few SQL operations the mentat engine (transactor,
//! query runner, projector, pull) needs from a database, so the same engine can
//! run on SQLite (the CLI and library) and on DuckDB (the DuckDB extension).
//!
//! `mentat_sql::SqliteConn` implements [`SqlConn`] over a `rusqlite::Connection`,
//! keeping its statement cache, so the SQLite path runs the same SQL it always
//! has. Where another engine
//! needs different SQL, callers branch on [`SqlConn::dialect`].

use std::fmt;

/// A SQL value as the engine stores it: SQLite's five storage classes. DuckDB
/// maps these onto the members of its `v` column's `UNION` type.
#[derive(Clone, Debug, PartialEq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<i64> for SqlValue {
    fn from(v: i64) -> Self {
        SqlValue::Integer(v)
    }
}

impl From<i32> for SqlValue {
    fn from(v: i32) -> Self {
        SqlValue::Integer(v.into())
    }
}

impl From<bool> for SqlValue {
    fn from(v: bool) -> Self {
        SqlValue::Integer(v.into())
    }
}

impl From<&str> for SqlValue {
    fn from(v: &str) -> Self {
        SqlValue::Text(v.to_string())
    }
}

impl From<String> for SqlValue {
    fn from(v: String) -> Self {
        SqlValue::Text(v)
    }
}

impl SqlValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            SqlValue::Integer(i) => Some(*i),
            _ => None,
        }
    }
}

/// Which SQL dialect a connection speaks. The engine's SQL is SQLite's; a
/// handful of statements (DDL, temp tables, fulltext) differ on DuckDB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dialect {
    Sqlite,
    DuckDb,
}

/// An error from the underlying database, carried as text so the trait stays
/// engine-neutral. `sqlite_code` is set for SQLite errors that callers inspect
/// (e.g. a constraint violation).
#[derive(Clone, Debug, PartialEq)]
pub struct SqlError {
    pub message: String,
    pub sqlite_code: Option<i32>,
}

impl fmt::Display for SqlError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SqlError {}

impl SqlError {
    pub fn new(message: impl Into<String>) -> Self {
        SqlError {
            message: message.into(),
            sqlite_code: None,
        }
    }
}

/// One result row. Columns are 0-based.
pub trait SqlRow {
    fn get_value(&self, idx: usize) -> Result<SqlValue, SqlError>;

    fn column_count(&self) -> usize;

    fn get_i64(&self, idx: usize) -> Result<i64, SqlError> {
        match self.get_value(idx)? {
            SqlValue::Integer(i) => Ok(i),
            other => Err(SqlError::new(format!(
                "column {idx}: expected an integer, got {other:?}"
            ))),
        }
    }

    fn get_bool(&self, idx: usize) -> Result<bool, SqlError> {
        Ok(self.get_i64(idx)? != 0)
    }

    fn get_text(&self, idx: usize) -> Result<String, SqlError> {
        match self.get_value(idx)? {
            SqlValue::Text(s) => Ok(s),
            other => Err(SqlError::new(format!(
                "column {idx}: expected text, got {other:?}"
            ))),
        }
    }

    fn get_opt_i64(&self, idx: usize) -> Result<Option<i64>, SqlError> {
        match self.get_value(idx)? {
            SqlValue::Null => Ok(None),
            SqlValue::Integer(i) => Ok(Some(i)),
            other => Err(SqlError::new(format!(
                "column {idx}: expected an integer or NULL, got {other:?}"
            ))),
        }
    }
}

impl SqlRow for Vec<SqlValue> {
    fn get_value(&self, idx: usize) -> Result<SqlValue, SqlError> {
        self.get(idx)
            .cloned()
            .ok_or_else(|| SqlError::new(format!("column {idx} out of range")))
    }

    fn column_count(&self) -> usize {
        self.len()
    }
}

/// The SQL operations the engine needs. Positional parameters are `?`; named
/// parameters (`$name`, used by the query builder) go through
/// [`SqlConn::query_named`].
pub trait SqlConn {
    fn dialect(&self) -> Dialect;

    /// Run statements with no parameters and no results.
    fn execute_batch(&self, sql: &str) -> Result<(), SqlError>;

    /// Run one statement; returns the number of rows changed.
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<usize, SqlError>;

    /// Run a query, calling `f` for every row in order. `f` returning an error
    /// stops the scan and propagates it.
    fn query(
        &self,
        sql: &str,
        params: &[SqlValue],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError>;

    /// As [`SqlConn::query`], with named parameters (`$name` -> value).
    fn query_named(
        &self,
        sql: &str,
        params: &[(&str, SqlValue)],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError>;

    /// The most bound parameters one statement may take.
    fn max_variables(&self) -> usize;

    /// Number of result columns of `sql` (prepared, not run).
    fn column_count(&self, sql: &str) -> Result<usize, SqlError>;
}

/// Convenience helpers over any [`SqlConn`].
pub trait SqlConnExt: SqlConn {
    /// Collect every row (`f` maps one row).
    fn query_rows<T>(
        &self,
        sql: &str,
        params: &[SqlValue],
        mut f: impl FnMut(&dyn SqlRow) -> Result<T, SqlError>,
    ) -> Result<Vec<T>, SqlError> {
        let mut out = Vec::new();
        self.query(sql, params, &mut |row| {
            out.push(f(row)?);
            Ok(())
        })?;
        Ok(out)
    }

    /// The first row, if any.
    fn query_opt<T>(
        &self,
        sql: &str,
        params: &[SqlValue],
        mut f: impl FnMut(&dyn SqlRow) -> Result<T, SqlError>,
    ) -> Result<Option<T>, SqlError> {
        let mut out = None;
        self.query(sql, params, &mut |row| {
            if out.is_none() {
                out = Some(f(row)?);
            }
            Ok(())
        })?;
        Ok(out)
    }
}

impl<C: SqlConn + ?Sized> SqlConnExt for C {}
