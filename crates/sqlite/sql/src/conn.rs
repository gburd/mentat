// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! The rusqlite implementation of the storage seam (`sql_traits::conn`).

use sql_traits::conn::{Dialect, SqlConn, SqlError, SqlRow, SqlValue};

// ---------------------------------------------------------------------------
// rusqlite
// ---------------------------------------------------------------------------

/// A rusqlite error as the seam's engine-neutral error.
pub fn sql_error(e: rusqlite::Error) -> SqlError {
    let sqlite_code = match &e {
        rusqlite::Error::SqliteFailure(f, _) => Some(f.extended_code),
        _ => None,
    };
    SqlError {
        message: e.to_string(),
        sqlite_code,
    }
}

pub fn from_rusqlite(v: rusqlite::types::Value) -> SqlValue {
    use rusqlite::types::Value as V;
    match v {
        V::Null => SqlValue::Null,
        V::Integer(i) => SqlValue::Integer(i),
        V::Real(r) => SqlValue::Real(r),
        V::Text(s) => SqlValue::Text(s),
        V::Blob(b) => SqlValue::Blob(b),
    }
}

pub fn to_rusqlite(v: SqlValue) -> rusqlite::types::Value {
    use rusqlite::types::Value as V;
    match v {
        SqlValue::Null => V::Null,
        SqlValue::Integer(i) => V::Integer(i),
        SqlValue::Real(r) => V::Real(r),
        SqlValue::Text(s) => V::Text(s),
        SqlValue::Blob(b) => V::Blob(b),
    }
}

/// A borrowed `SqlValue` as a rusqlite parameter (a local newtype: both the
/// trait and `SqlValue` are foreign to this crate).
pub struct Param<'a>(pub &'a SqlValue);

impl rusqlite::types::ToSql for Param<'_> {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self.0 {
            SqlValue::Null => ValueRef::Null,
            SqlValue::Integer(i) => ValueRef::Integer(*i),
            SqlValue::Real(r) => ValueRef::Real(*r),
            SqlValue::Text(s) => ValueRef::Text(s.as_bytes()),
            SqlValue::Blob(b) => ValueRef::Blob(b),
        }))
    }
}

/// `SqlRow` over a rusqlite row (a local newtype, for the orphan rule).
pub struct RusqliteRow<'a, 'stmt>(pub &'a rusqlite::Row<'stmt>);

impl SqlRow for RusqliteRow<'_, '_> {
    fn get_value(&self, idx: usize) -> Result<SqlValue, SqlError> {
        let v: rusqlite::types::Value = self.0.get(idx).map_err(sql_error)?;
        Ok(from_rusqlite(v))
    }
}

/// Run a prepared statement, calling `f` per row.
fn each_row(
    rows: &mut rusqlite::Rows<'_>,
    f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
) -> Result<(), SqlError> {
    while let Some(row) = rows.next().map_err(sql_error)? {
        f(&RusqliteRow(row))?;
    }
    Ok(())
}

/// `SqlConn` for a rusqlite connection. `rusqlite::Connection` is foreign and
/// so is the trait, so the impl is on this local wrapper; callers pass
/// `&SqliteConn(&conn)`, or use [`sqlite`] to get one.
pub struct SqliteConn<'c>(pub &'c rusqlite::Connection);

/// Wrap a rusqlite connection as a `&dyn SqlConn`-able value.
pub fn sqlite(conn: &rusqlite::Connection) -> SqliteConn<'_> {
    SqliteConn(conn)
}

impl SqlConn for SqliteConn<'_> {
    fn dialect(&self) -> Dialect {
        Dialect::Sqlite
    }

    fn execute_batch(&self, sql: &str) -> Result<(), SqlError> {
        self.0.execute_batch(sql).map_err(sql_error)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<usize, SqlError> {
        let mut stmt = self.0.prepare_cached(sql).map_err(sql_error)?;
        stmt.execute(rusqlite::params_from_iter(params.iter().map(Param)))
            .map_err(sql_error)
    }

    fn query(
        &self,
        sql: &str,
        params: &[SqlValue],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let mut stmt = self.0.prepare_cached(sql).map_err(sql_error)?;
        let mut rows = stmt
            .query(rusqlite::params_from_iter(params.iter().map(Param)))
            .map_err(sql_error)?;
        each_row(&mut rows, f)
    }

    fn query_named(
        &self,
        sql: &str,
        params: &[(&str, SqlValue)],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        let mut stmt = self.0.prepare_cached(sql).map_err(sql_error)?;
        let owned: Vec<Param> = params.iter().map(|(_, v)| Param(v)).collect();
        let named: Vec<(&str, &dyn rusqlite::types::ToSql)> = params
            .iter()
            .zip(owned.iter())
            .map(|((k, _), p)| (*k, p as &dyn rusqlite::types::ToSql))
            .collect();
        let mut rows = stmt.query(named.as_slice()).map_err(sql_error)?;
        each_row(&mut rows, f)
    }

    fn max_variables(&self) -> usize {
        self.0
            .limit(rusqlite::limits::Limit::SQLITE_LIMIT_VARIABLE_NUMBER)
            .map(|n| n as usize)
            .unwrap_or(999)
    }

    fn column_count(&self, sql: &str) -> Result<usize, SqlError> {
        Ok(self
            .0
            .prepare_cached(sql)
            .map_err(sql_error)?
            .column_count())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sql_traits::conn::SqlConnExt;

    #[test]
    fn rusqlite_conn_round_trips_through_the_seam() {
        let c = rusqlite::Connection::open_in_memory().unwrap();
        let w = SqliteConn(&c);
        let conn: &dyn SqlConn = &w;
        conn.execute_batch("CREATE TABLE t (a INTEGER, b)").unwrap();
        for v in [
            SqlValue::Integer(7),
            SqlValue::Real(1.5),
            SqlValue::Text("x".into()),
            SqlValue::Blob(vec![1, 2]),
            SqlValue::Null,
        ] {
            assert_eq!(
                conn.execute("INSERT INTO t VALUES (?, ?)", &[1.into(), v])
                    .unwrap(),
                1
            );
        }
        let got = conn
            .query_rows("SELECT b FROM t ORDER BY rowid", &[], |r| r.get_value(0))
            .unwrap();
        assert_eq!(
            got,
            vec![
                SqlValue::Integer(7),
                SqlValue::Real(1.5),
                SqlValue::Text("x".into()),
                SqlValue::Blob(vec![1, 2]),
                SqlValue::Null
            ]
        );
        let named = {
            let mut n = 0;
            conn.query_named(
                "SELECT count(*) FROM t WHERE a = $one",
                &[("$one", 1.into())],
                &mut |r| {
                    n = r.get_i64(0)?;
                    Ok(())
                },
            )
            .unwrap();
            n
        };
        assert_eq!(named, 5);
        assert_eq!(conn.column_count("SELECT a, b FROM t").unwrap(), 2);
        assert!(conn.max_variables() >= 999);
        let e = conn
            .execute("INSERT INTO nope VALUES (1)", &[])
            .unwrap_err();
        assert!(e.message.contains("nope"), "{e}");
    }
}
