// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Import a SQLite-format mentat store (what the DuckDB extension wrote
//! before 1.11.0, and what the embedded engine and the SQLite extension
//! write) into a DuckDB store, row for row.
//!
//! No SQLite is linked here: the file is read through DuckDB's own `sqlite`
//! extension (`ATTACH ... (TYPE sqlite)` + `sqlite_query`). Every column it
//! hands back is text, so each value crosses as text and is rebuilt exactly:
//!
//! - integers (refs, longs, booleans, instants) and the bookkeeping columns:
//!   decimal text;
//! - strings and keywords: the text itself;
//! - uuids and bytes: `hex(v)`;
//! - doubles: SQLite prints at most 15-17 significant digits, which doesn't
//!   round-trip every double. [`double_parts_sql`] splits each one in SQLite
//!   into an integer mantissa and a binary exponent with exact power-of-two
//!   arithmetic, and DuckDB multiplies them back: bit for bit, subnormals and
//!   +-inf included.
//! - fulltext values: SQLite stores a rowid into its FTS4 table; they are
//!   replaced by the text (`fulltext_values.text`), which is what a DuckDB
//!   store holds.
//!
//! Imported: `datoms`, the whole `timelined_transactions` log (so as-of and
//! since work), `idents`, `schema` and `known_parts` (partition high-water
//! marks), with every entid, tx id and tx instant unchanged. The tables are
//! the same as the engine's on both sides (see `schema.rs`), so the imported
//! store is what transacting the same data on DuckDB would have produced.

use mentat_db::entids;
use mentat_sql::{SqlConn, SqlConnExt};
use public_traits::errors::{MentatError, Result};

use crate::schema::{typed_values, VALUE_TYPE};
use crate::store::{write_lock, OnDrop};
use crate::DuckStore;

/// An import error that displays as just `msg` (`BadQueryOptions` would
/// prefix "query options:").
fn err(msg: impl Into<String>) -> MentatError {
    MentatError::IoError(std::io::Error::other(msg.into()))
}

/// What one import copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub datoms: i64,
    pub transactions: i64,
    /// The last transaction id in the imported log.
    pub last_tx: i64,
}

/// Exact `2^j` as a SQLite REAL expression (`|j|` <= 1074).
fn pow2(j: i64) -> String {
    if j < 0 {
        return format!("(1.0/{})", pow2(-j));
    }
    let (q, r) = (j / 62, j % 62);
    let mut fs = vec!["CAST(4611686018427387904 AS REAL)".to_string(); q as usize];
    fs.push(format!("CAST({} AS REAL)", 1i64 << r));
    format!("({})", fs.join("*"))
}

/// SQLite SQL: `then` (a SELECT) preceded by a CTE `parts(k, m, x)` over
/// `src` (a query with columns `k` and REAL `v`), where
/// `v = m * 2^(x - 52)` exactly and `m` is an integer with `|m| < 2^53`. `x`
/// is NULL for +-inf, where `m` is `inf` / `-inf`.
///
/// `|v|` is brought into `[1, 2)` by multiplying by exact powers of two, one
/// step per `j` in 512, 256, ..., 1 (a recursive CTE, so the SQL stays small
/// and SQLite's parser shallow). Every step is exact: a power-of-two scaling
/// only moves the exponent, and subnormals are first scaled up by 2^600 so
/// no step leaves the normal range. Then `|v| * 2^52` is an integer below
/// 2^53, so the CAST is exact. (SQLite has no ldexp/log2 without its math
/// extension, which DuckDB's sqlite scanner doesn't build.)
///
/// shortcut: `-0.0` comes back as `0.0`: nothing in SQLite core exposes the
/// sign of a zero (every text form drops it). Both engines compare them
/// equal, so only the printed sign differs; fix if a store ever relies on it.
pub fn double_parts_sql(src: &str, then: &str) -> String {
    const STEPS: [i64; 10] = [512, 256, 128, 64, 32, 16, 8, 4, 2, 1];
    let steps: Vec<String> = STEPS
        .iter()
        .enumerate()
        .map(|(s, &j)| {
            format!(
                "SELECT {s}, {j}, {}, {}, {}",
                pow2(j),
                pow2(1 - j),
                pow2(-j)
            )
        })
        .collect();
    let tiny = pow2(-1000);
    format!(
        "WITH RECURSIVE pw(s, j, hi, lo, inv) AS ({}), \
         n(k, v, x, e, s) AS ( \
           SELECT k, v, CASE WHEN abs(v) < {tiny} THEN abs(v) * {} ELSE abs(v) END, \
                  CASE WHEN abs(v) < {tiny} THEN -600 ELSE 0 END, 0 FROM ({src}) \
           UNION ALL \
           SELECT n.k, n.v, \
                  CASE WHEN n.x >= pw.hi THEN n.x * pw.inv WHEN n.x < pw.lo THEN n.x * pw.hi ELSE n.x END, \
                  CASE WHEN n.x >= pw.hi THEN n.e + pw.j WHEN n.x < pw.lo THEN n.e - pw.j ELSE n.e END, \
                  n.s + 1 \
           FROM n JOIN pw ON pw.s = n.s), \
         parts(k, m, x) AS ( \
           SELECT k, CASE WHEN x >= 2.0 THEN CASE WHEN v > 0 THEN 'inf' ELSE '-inf' END \
                  ELSE CAST(CASE WHEN v < 0 THEN -1 ELSE 1 END * CAST(x * {} AS INTEGER) AS TEXT) END, \
                  CASE WHEN x < 2.0 THEN e END \
           FROM n WHERE s = {}) \
         {then}",
        steps.join(" UNION ALL "),
        pow2(600),
        pow2(52),
        STEPS.len()
    )
}

/// DuckDB SQL rebuilding the double from [`double_parts_sql`]'s `m`, `x`.
fn double_from_parts(m: &str, x: &str) -> String {
    format!(
        "CASE WHEN {x} IS NULL THEN {m}::DOUBLE \
         ELSE ({m}::BIGINT::DOUBLE * pow(2.0, -52)) * pow(2.0, {x}::BIGINT) END"
    )
}

/// Quote `s` as a SQL string literal (the same quoting in SQLite and DuckDB).
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// `sqlite_query` over the source attached as `src`; `sql` is SQLite SQL.
fn scan(src: &str, sql: &str) -> String {
    format!("sqlite_query({}, {})", lit(src), lit(sql))
}

/// SQLite SQL: one table's rows with `v` split by storage class. `cols` are
/// the other columns (as text). `v` of a fulltext datom is a rowid into
/// `fulltext_values`; `ft` is the SQL for "this row is fulltext".
fn split_values(table: &str, cols: &str, ft: &str) -> String {
    format!(
        "SELECT {cols}, typeof(v) AS t, \
           CASE WHEN {ft} THEN (SELECT text FROM fulltext_values f WHERE f.rowid = v) \
                WHEN typeof(v) IN ('integer', 'text') THEN CAST(v AS TEXT) \
                WHEN typeof(v) = 'blob' THEN hex(v) END AS s, \
           CASE WHEN {ft} THEN 0 ELSE 1 END AS plain \
         FROM {table} WHERE NOT (typeof(v) = 'real' AND NOT ({ft}))"
    )
}

/// DuckDB SQL: the `v` UNION from [`split_values`]'s `t`, `s`, `plain`.
fn value_from_text() -> String {
    format!(
        "CASE WHEN plain::INTEGER = 0 OR t = 'text' THEN s::{VALUE_TYPE} \
              WHEN t = 'integer' THEN s::BIGINT::{VALUE_TYPE} \
              ELSE unhex(s)::{VALUE_TYPE} END"
    )
}

/// Copy one table (`e, a, v, ... ` with `cols` besides `v`) from the source
/// into `dest`. `cols_in` names them in SQLite, `cols_out` casts them in
/// DuckDB (aliased columns of [`split_values`]'s output), and `typed` adds the
/// typed `v_i, v_d, v_s` copies.
#[allow(clippy::too_many_arguments)]
fn copy_table(
    conn: &dyn SqlConn,
    src: &str,
    dest: &str,
    table: &str,
    cols_in: &str,
    cols_out: &str,
    ft: &str,
    typed: bool,
) -> Result<()> {
    let typed_text = if typed {
        format!(", {}", typed_values("v"))
    } else {
        String::new()
    };
    // Every value but a double (non-fulltext REAL), via text.
    conn.execute(
        &format!(
            "INSERT INTO {dest} SELECT {cols_out}{typed_text} FROM \
             (SELECT *, {} AS v FROM {})",
            value_from_text(),
            scan(src, &split_values(table, cols_in, ft))
        ),
        &[],
    )?;
    // Doubles, through their exact parts. `k` carries the other columns,
    // joined back by row position (rowid) in SQLite.
    let doubles = double_parts_sql(
        &format!("SELECT rowid AS k, v FROM {table} WHERE typeof(v) = 'real' AND NOT ({ft})"),
        &format!(
            "SELECT {cols_in}, parts.m AS m, parts.x AS x FROM {table} \
             JOIN parts ON parts.k = {table}.rowid"
        ),
    );
    conn.execute(
        &format!(
            "INSERT INTO {dest} SELECT {cols_out}{typed_text} FROM \
             (SELECT *, ({})::{VALUE_TYPE} AS v FROM {})",
            double_from_parts("m", "x"),
            scan(src, &doubles)
        ),
        &[],
    )?;
    Ok(())
}

impl DuckStore<'_> {
    /// Import the SQLite mentat store at `path` into this store, which must
    /// not exist yet or must hold nothing but its bootstrap transaction. One
    /// DuckDB transaction: on any error the store is left as it was. Needs
    /// DuckDB's `sqlite` extension (autoloaded where DuckDB may install
    /// extensions; otherwise `INSTALL sqlite; LOAD sqlite;` first).
    pub fn import_sqlite(&self, path: &str) -> Result<Imported> {
        // No Rust-side file check: it would see past DuckDB's file access
        // settings. A missing file fails at the first read, from SQLite.
        self.open()?;
        let lock = write_lock(self.schema_name());
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let conn = self.conn();
        // Attached databases are shared by every connection to the database,
        // so the name is per store; the store's write lock serializes it.
        let src = format!("{}_import_src", self.schema_name());
        conn.execute_batch(&format!("DETACH DATABASE IF EXISTS {src}"))?;
        conn.execute_batch(&format!(
            "ATTACH {} AS {src} (TYPE sqlite, READ_ONLY)",
            lit(path)
        ))
        .map_err(|e| {
            let m = e.to_string();
            if m.contains("xtension") {
                err(format!(
                    "mentat_import needs DuckDB's sqlite extension to read {path}. \
                     Run `INSTALL sqlite; LOAD sqlite;` (from a DuckDB that may download \
                     extensions, or with the extension file copied into the extension \
                     directory) and retry. DuckDB said: {m}"
                ))
            } else {
                err(format!("mentat_import: can't open {path} as SQLite: {m}"))
            }
        })?;
        // Detached on every exit, a panic included (it would otherwise stay
        // attached, instance-wide, until the process exits).
        let detach = OnDrop::new(conn, format!("DETACH DATABASE IF EXISTS {src}"));
        let out = self.import_attached(&src, path);
        let detached = detach.run();
        let out = out?;
        detached?;
        Ok(out)
    }

    fn import_attached(&self, src: &str, path: &str) -> Result<Imported> {
        let conn = self.conn();
        let one = |sql: &str| -> Result<String> {
            conn.query_opt(&format!("SELECT * FROM {}", scan(src, sql)), &[], |r| {
                r.get_text(0)
            })?
            .ok_or_else(|| err(format!("mentat_import: {path}: no result for {sql}")))
        };
        let version: i64 = one("PRAGMA user_version")
            .map_err(|e| {
                err(format!(
                    "mentat_import: {path} is not a SQLite database: {e}"
                ))
            })?
            .parse()
            .unwrap_or(0);
        // known_parts.idx (the partition high-water marks) came with version
        // 2; a version-1 store needs the embedded engine to upgrade it first.
        if version == 1 {
            return Err(err(format!(
                "mentat_import: {path} is a version-1 mentat store; open it once with \
                 mentat 1.10 or later (the CLI or the SQLite extension) to upgrade it"
            )));
        }
        let tables = one(
            "SELECT group_concat(name) FROM (SELECT name FROM sqlite_master \
             WHERE name IN ('datoms', 'timelined_transactions', 'idents', 'schema', 'known_parts') \
             ORDER BY name)",
        )
        .unwrap_or_default();
        if version < 2 || tables != "datoms,idents,known_parts,schema,timelined_transactions" {
            return Err(err(format!(
                "mentat_import: {path} is not a mentat store (user_version {version}, \
                 tables {tables:?})"
            )));
        }
        let s = self.schema_name();
        let user: i64 = conn
            .query_opt(
                &format!(
                    "SELECT count(*) FROM {s}.transactions WHERE tx > (SELECT min(tx) FROM {s}.transactions)"
                ),
                &[],
                |r| r.get_i64(0),
            )?
            .unwrap_or(0);
        if user > 0 {
            return Err(err(format!(
                "mentat_import: store {s} already has transactions; import into a new store"
            )));
        }
        self.in_tx(|st| {
            for t in ["datoms", "timelined_transactions", "idents", "schema", "known_parts"] {
                conn.execute(&format!("DELETE FROM {s}.{t}"), &[])?;
            }
            copy_table(
                conn,
                src,
                &format!(
                    "{s}.datoms (e, a, tx, value_type_tag, index_avet, index_vaet, \
                     index_fulltext, unique_value, v, v_i, v_d, v_s)"
                ),
                "datoms",
                "CAST(e AS TEXT) e, CAST(a AS TEXT) a, CAST(tx AS TEXT) tx, \
                 CAST(value_type_tag AS TEXT) tag, CAST(index_avet AS TEXT) avet, \
                 CAST(index_vaet AS TEXT) vaet, CAST(index_fulltext AS TEXT) ft, \
                 CAST(unique_value AS TEXT) uv",
                "e::BIGINT, a::BIGINT, tx::BIGINT, tag::SMALLINT, avet::INTEGER <> 0, \
                 vaet::INTEGER <> 0, ft::INTEGER <> 0, uv::INTEGER <> 0, v",
                "index_fulltext <> 0",
                true,
            )?;
            // The log has no index_fulltext column: a row is fulltext iff its
            // attribute is (`:db/fulltext` can't be altered).
            let log_ft = format!(
                "a IN (SELECT e FROM schema WHERE a = {} AND v = 1)",
                entids::DB_FULLTEXT
            );
            copy_table(
                conn,
                src,
                &format!(
                    "{s}.timelined_transactions (e, a, tx, added, value_type_tag, timeline, \
                     v, v_i, v_d, v_s)"
                ),
                "timelined_transactions",
                "CAST(e AS TEXT) e, CAST(a AS TEXT) a, CAST(tx AS TEXT) tx, \
                 CAST(added AS TEXT) added, CAST(value_type_tag AS TEXT) tag, \
                 CAST(timeline AS TEXT) tl",
                "e::BIGINT, a::BIGINT, tx::BIGINT, added::INTEGER <> 0, tag::SMALLINT, \
                 tl::SMALLINT, v",
                &log_ft,
                true,
            )?;
            for t in ["idents", "schema"] {
                copy_table(
                    conn,
                    src,
                    &format!("{s}.{t} (e, a, value_type_tag, v)"),
                    t,
                    "CAST(e AS TEXT) e, CAST(a AS TEXT) a, CAST(value_type_tag AS TEXT) tag",
                    "e::BIGINT, a::BIGINT, tag::SMALLINT, v",
                    "0",
                    false,
                )?;
            }
            conn.execute(
                &format!(
                    "INSERT INTO {s}.known_parts (part, start, \"end\", idx, allow_excision) \
                     SELECT part, start::BIGINT, \"end\"::BIGINT, idx::BIGINT, ax::SMALLINT FROM {}",
                    scan(
                        src,
                        "SELECT part, CAST(start AS TEXT) start, CAST(\"end\" AS TEXT) \"end\", \
                         CAST(idx AS TEXT) idx, CAST(allow_excision AS TEXT) ax FROM known_parts"
                    )
                ),
                &[],
            )?;
            // Row counts must match the source exactly.
            for t in ["datoms", "timelined_transactions", "idents", "schema", "known_parts"] {
                let src: i64 = one(&format!("SELECT count(*) FROM {t}"))?
                    .parse()
                    .map_err(|_| err("mentat_import: bad count"))?;
                let dst = conn
                    .query_opt(&format!("SELECT count(*) FROM {s}.{t}"), &[], |r| r.get_i64(0))?
                    .unwrap_or(-1);
                if src != dst {
                    return Err(err(format!(
                        "mentat_import: {t}: copied {dst} of {src} rows from {path}"
                    )));
                }
            }
            // A new generation: every cached schema of this store is stale.
            conn.execute(
                &format!(
                    "UPDATE {s}.meta SET value = ((hash(uuid()) >> 1)::BIGINT)::VARCHAR \
                     WHERE key = 'generation'"
                ),
                &[],
            )?;
            #[cfg(feature = "failpoints")]
            crate::failpoints::hit("import");
            // The engine must be able to read it back (this also checks
            // known_parts: start <= next entid <= end).
            st.read()
                .map_err(|e| err(format!("mentat_import: {path}: {e}")))?;
            let n = |sql: &str| -> Result<i64> {
                Ok(conn.query_opt(sql, &[], |r| r.get_opt_i64(0))?.flatten().unwrap_or(0))
            };
            Ok(Imported {
                datoms: n(&format!("SELECT count(*) FROM {s}.datoms"))?,
                transactions: n(&format!("SELECT count(DISTINCT tx) FROM {s}.transactions"))?,
                last_tx: n(&format!("SELECT max(tx) FROM {s}.transactions"))?,
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pow2_is_exact_text() {
        assert_eq!(pow2(3), "(CAST(8 AS REAL))");
        assert_eq!(
            pow2(63),
            "(CAST(4611686018427387904 AS REAL)*CAST(2 AS REAL))"
        );
        assert_eq!(pow2(-1), "(1.0/(CAST(2 AS REAL)))");
    }

    #[test]
    fn lit_quotes() {
        assert_eq!(lit("a'b"), "'a''b'");
    }
}
