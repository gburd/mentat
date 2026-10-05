// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! `MentatStoring` on DuckDB: the SQLite store's transaction-application SQL
//! (`mentat_db::db`, `impl MentatStoring for rusqlite::Connection`) in DuckDB
//! syntax. Each statement mirrors its SQLite counterpart; the differences:
//!
//! - `IS 1` / `IS 0` / `IS NOT v` (SQLite) -> `= true` / `= false` /
//!   `IS DISTINCT FROM v`.
//! - Temp tables are `CREATE TEMP TABLE x` (see `schema::begin_tx_statements`).
//! - Flags are BOOLEAN columns; `flags0 & bit <> 0` computes them.
//! - No `rowid` alias needed: DuckDB has `rowid` on base tables.

use std::collections::HashMap;

use core_traits::{attribute, AttributeBitFlags, Entid, TypedValue};
use db_traits::errors::{DbErrorKind, Result};
use mentat_core::Schema;
use mentat_db::db::TypedSQLValue;
use mentat_db::entids;
use mentat_db::{
    AttributeAlteration, MentatStoring, MetadataReport, PartitionMap, ReducedEntity,
    SchemaBuilding, SearchType,
};
use mentat_sql::{SqlConn, SqlConnExt, SqlRow, SqlValue};

use crate::schema;

/// One store's tables, on one DuckDB connection (the connection's search
/// path already points at the store's schema).
pub struct DuckStoring<'c> {
    pub conn: &'c dyn SqlConn,
}

fn values_tuple(n: usize) -> String {
    format!("({})", vec!["?"; n].join(", "))
}

fn values(per: usize, count: usize) -> String {
    vec![values_tuple(per); count].join(", ")
}

fn quad(
    row: &dyn SqlRow,
) -> std::result::Result<(Entid, Entid, TypedValue, bool), mentat_sql::SqlError> {
    let tag = row.get_i64(3)? as i32;
    let v = TypedValue::from_sql(row.get_value(2)?, tag)
        .map_err(|e| mentat_sql::SqlError::new(e.to_string()))?;
    Ok((row.get_i64(0)?, row.get_i64(1)?, v, row.get_bool(4)?))
}

impl DuckStoring<'_> {
    /// Run a statement; a failure reports `kind` (as SQLite's store does) with
    /// DuckDB's message appended, which `context` alone would drop.
    fn exec(&self, sql: &str, params: &[SqlValue], kind: DbErrorKind) -> Result<()> {
        self.conn.execute(sql, params).map_err(|e| {
            db_traits::errors::DbError::from(DbErrorKind::RusqliteError(format!("{kind}: {e}")))
        })?;
        Ok(())
    }

    fn insert_searches(&self, entities: &[ReducedEntity], search_type: SearchType) -> Result<()> {
        let per = 6;
        let table = if search_type == SearchType::Exact {
            "temp.exact_searches"
        } else {
            "temp.inexact_searches"
        };
        for chunk in entities.chunks((self.conn.max_variables() / per).max(1)) {
            let mut params = Vec::with_capacity(chunk.len() * per);
            for &(e, a, attribute, ref typed_value, added) in chunk {
                let (v, tag) = typed_value.to_sql();
                params.extend([
                    SqlValue::Integer(e),
                    SqlValue::Integer(a),
                    v,
                    SqlValue::Integer(tag.into()),
                    SqlValue::Integer(added.into()),
                    SqlValue::Integer(attribute.flags().into()),
                ]);
            }
            let sql = format!(
                "INSERT INTO {table} (e0, a0, v0, value_type_tag0, added0, flags0) VALUES {}",
                values(per, chunk.len())
            );
            self.exec(
                &sql,
                &params,
                DbErrorKind::NonFtsInsertionIntoTempSearchTableFailed,
            )?;
        }
        Ok(())
    }

    /// SQLite's `search`: pair each search with the matching datom(s).
    fn search(&self) -> Result<()> {
        let s = "
          INSERT INTO temp.search_results
          SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':db.cardinality/many', d.rowid, d.v
          FROM temp.exact_searches AS t
          LEFT JOIN datoms AS d
          ON t.e0 = d.e AND t.a0 = d.a AND t.value_type_tag0 = d.value_type_tag AND t.v0 = d.v

          UNION ALL

          SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':db.cardinality/one', d.rowid, d.v
          FROM temp.inexact_searches AS t
          LEFT JOIN datoms AS d
          ON t.e0 = d.e AND t.a0 = d.a";
        self.exec(s, &[], DbErrorKind::CouldNotSearch)
    }

    fn insert_transaction(&self, tx: Entid) -> Result<()> {
        let s = "
          INSERT INTO timelined_transactions (e, a, v, tx, added, value_type_tag)
          SELECT e0, a0, v0, ?, true, value_type_tag0
          FROM temp.search_results
          WHERE added0 AND ((rid IS NULL) OR (v0 IS DISTINCT FROM v))";
        self.exec(
            s,
            &[SqlValue::Integer(tx)],
            DbErrorKind::TxInsertFailedToAddMissingDatoms,
        )?;
        let s = "
          INSERT INTO timelined_transactions (e, a, v, tx, added, value_type_tag)
          SELECT DISTINCT e0, a0, v, ?, false, value_type_tag0
          FROM temp.search_results
          WHERE rid IS NOT NULL AND
                ((NOT added0) OR
                 (added0 AND search_type = ':db.cardinality/one' AND v0 IS DISTINCT FROM v))";
        self.exec(
            s,
            &[SqlValue::Integer(tx)],
            DbErrorKind::TxInsertFailedToRetractDatoms,
        )
    }

    fn update_datoms(&self, tx: Entid) -> Result<()> {
        let s = "
            DELETE FROM datoms WHERE rowid IN (
              SELECT rid FROM temp.search_results
              WHERE rid IS NOT NULL AND
                    ((NOT added0) OR
                     (added0 AND search_type = ':db.cardinality/one' AND v0 IS DISTINCT FROM v)))";
        self.exec(s, &[], DbErrorKind::DatomsUpdateFailedToRetract)?;
        let s = format!(
            "
          INSERT INTO datoms (e, a, v, tx, value_type_tag, index_avet, index_vaet, index_fulltext, unique_value)
          SELECT e0, a0, v0, ?, value_type_tag0,
                 (flags0 & {}) <> 0, (flags0 & {}) <> 0, (flags0 & {}) <> 0, (flags0 & {}) <> 0
          FROM temp.search_results
          WHERE added0 AND ((rid IS NULL) OR (v0 IS DISTINCT FROM v))",
            AttributeBitFlags::IndexAVET as u8,
            AttributeBitFlags::IndexVAET as u8,
            AttributeBitFlags::IndexFulltext as u8,
            AttributeBitFlags::UniqueValue as u8
        );
        self.exec(
            &s,
            &[SqlValue::Integer(tx)],
            DbErrorKind::DatomsUpdateFailedToAdd,
        )
    }
}

impl MentatStoring for DuckStoring<'_> {
    fn resolve_avs<'a>(
        &self,
        avs: &'a [&'a mentat_db::types::AVPair],
    ) -> Result<mentat_db::types::AVMap<'a>> {
        let per = 4;
        let initial_search_id = 2000i64;
        let mut m: mentat_db::types::AVMap<'a> = HashMap::new();
        let chunk_len = (self.conn.max_variables() / per).max(1);
        for (chunk_no, chunk) in avs.chunks(chunk_len).enumerate() {
            let mut params = Vec::with_capacity(chunk.len() * per);
            for (i, &&(a, ref v)) in chunk.iter().enumerate() {
                let (value, tag) = v.to_sql();
                params.extend([
                    SqlValue::Integer(initial_search_id + (chunk_no * chunk_len + i) as i64),
                    SqlValue::Integer(a),
                    value,
                    SqlValue::Integer(tag.into()),
                ]);
            }
            // The VALUES list can't carry the store's UNION type for `v`, so
            // compare through the same member: cast the probe into it.
            let sql = format!(
                "WITH t(search_id, a, v, value_type_tag) AS (VALUES {}) \
                 SELECT t.search_id, d.e FROM t, all_datoms AS d \
                 WHERE d.index_avet AND d.a = t.a AND d.value_type_tag = t.value_type_tag \
                   AND d.v = t.v::{}",
                values(per, chunk.len()),
                schema::VALUE_TYPE
            );
            let rows = self
                .conn
                .query_rows(&sql, &params, |r| Ok((r.get_i64(0)?, r.get_i64(1)?)))?;
            for (search_id, e) in rows {
                m.insert(avs[(search_id - initial_search_id) as usize], e);
            }
        }
        Ok(m)
    }

    fn begin_tx_application(&self) -> Result<()> {
        for s in schema::begin_tx_statements() {
            self.exec(&s, &[], DbErrorKind::FailedToCreateTempTables)?;
        }
        Ok(())
    }

    fn insert_non_fts_searches(
        &self,
        entities: &[ReducedEntity],
        search_type: SearchType,
    ) -> Result<()> {
        self.insert_searches(entities, search_type)
    }

    /// Fulltext values are stored as plain strings on DuckDB (no FTS4).
    fn insert_fts_searches(
        &self,
        entities: &[ReducedEntity],
        search_type: SearchType,
    ) -> Result<()> {
        for (_, _, _, v, _) in entities {
            if !matches!(v, TypedValue::String(_)) {
                bail_db(DbErrorKind::WrongTypeValueForFtsAssertion)?;
            }
        }
        self.insert_searches(entities, search_type)
    }

    fn materialize_mentat_transaction(&self, tx_id: Entid) -> Result<()> {
        self.search()?;
        self.update_datoms(tx_id)
    }

    fn commit_mentat_transaction(&self, tx_id: Entid) -> Result<()> {
        self.insert_transaction(tx_id)
    }

    fn resolved_metadata_assertions(&self) -> Result<Vec<(Entid, Entid, TypedValue, bool)>> {
        let list = entids::METADATA_SQL_LIST.as_str();
        let s = format!(
            "SELECT e, a, v, value_type_tag, added FROM (
                SELECT e0 AS e, a0 AS a, v0 AS v, value_type_tag0 AS value_type_tag, true AS added
                FROM temp.search_results
                WHERE a0 IN {list} AND added0 AND ((rid IS NULL) OR (v0 IS DISTINCT FROM v))
              UNION
                SELECT e0 AS e, a0 AS a, v, value_type_tag0 AS value_type_tag, false AS added
                FROM temp.search_results
                WHERE a0 IN {list} AND rid IS NOT NULL AND
                      ((NOT added0) OR (added0 AND search_type = ':db.cardinality/one' AND v0 IS DISTINCT FROM v))
             ) ORDER BY e, a, value_type_tag, added"
        );
        Ok(self.conn.query_rows(&s, &[], quad)?)
    }

    fn write_partition_map(&self, partition_map: &PartitionMap) -> Result<()> {
        for (part, partition) in partition_map.iter() {
            self.conn.execute(
                "UPDATE known_parts SET idx = ? WHERE part = ?",
                &[
                    SqlValue::Integer(partition.next_entid()),
                    SqlValue::Text(part.clone()),
                ],
            )?;
        }
        Ok(())
    }

    fn committed_metadata_assertions(
        &self,
        tx_id: Entid,
    ) -> Result<Vec<(Entid, Entid, TypedValue, bool)>> {
        let s = format!(
            "SELECT e, a, v, value_type_tag, added FROM transactions \
             WHERE tx = ? AND a IN {} ORDER BY e, a, value_type_tag, added",
            entids::METADATA_SQL_LIST.as_str()
        );
        Ok(self
            .conn
            .query_rows(&s, &[SqlValue::Integer(tx_id)], quad)?)
    }

    fn update_metadata(
        &self,
        _old_schema: &Schema,
        new_schema: &Schema,
        report: &MetadataReport,
    ) -> Result<()> {
        if !report.idents_altered.is_empty() {
            self.conn.execute("DELETE FROM idents", &[])?;
            self.conn.execute(
                &format!(
                    "INSERT INTO idents SELECT e, a, v, value_type_tag FROM datoms WHERE a IN {}",
                    entids::IDENTS_SQL_LIST.as_str()
                ),
                &[],
            )?;
        }
        if !report.attributes_installed.is_empty()
            || !report.attributes_altered.is_empty()
            || !report.idents_altered.is_empty()
        {
            self.conn.execute("DELETE FROM schema", &[])?;
            self.conn.execute(
                &format!(
                    "WITH s(e) AS (SELECT e FROM datoms WHERE a = {}) \
                     INSERT INTO schema SELECT s.e, a, v, value_type_tag FROM datoms, s \
                     WHERE s.e = datoms.e AND a IN {}",
                    entids::DB_VALUE_TYPE,
                    entids::SCHEMA_SQL_LIST.as_str()
                ),
                &[],
            )?;
        }
        for (&entid, alterations) in &report.attributes_altered {
            let attribute = new_schema.require_attribute_for_entid(entid)?;
            for alteration in alterations {
                match alteration {
                    AttributeAlteration::Index => {
                        self.conn.execute(
                            "UPDATE datoms SET index_avet = ? WHERE a = ?",
                            &[
                                SqlValue::Integer(attribute.index.into()),
                                SqlValue::Integer(entid),
                            ],
                        )?;
                    }
                    AttributeAlteration::Unique => {
                        // SQLite enforces this through a partial unique index; DuckDB has none, so check.
                        if attribute.unique.is_some() {
                            let dup: Option<i64> = self.conn.query_opt(
                                "SELECT 1 FROM datoms WHERE a = ? GROUP BY value_type_tag, v HAVING count(*) > 1 LIMIT 1",
                                &[SqlValue::Integer(entid)],
                                |r| r.get_i64(0),
                            )?;
                            if dup.is_some() {
                                let what = match attribute.unique {
                                    Some(attribute::Unique::Value) => ":db.unique/value",
                                    _ => ":db.unique/identity",
                                };
                                bail_db(DbErrorKind::SchemaAlterationFailed(format!(
                                    "Cannot alter schema attribute {entid} to be {what}"
                                )))?;
                            }
                        }
                        self.conn.execute(
                            "UPDATE datoms SET unique_value = ? WHERE a = ?",
                            &[
                                SqlValue::Integer(attribute.unique.is_some().into()),
                                SqlValue::Integer(entid),
                            ],
                        )?;
                    }
                    AttributeAlteration::Cardinality => {
                        if !attribute.multival {
                            let conflict: Option<i64> = self.conn.query_opt(
                                "SELECT 1 FROM datoms AS l, datoms AS r \
                                 WHERE l.a = ? AND l.a = r.a AND l.e = r.e AND l.v <> r.v LIMIT 1",
                                &[SqlValue::Integer(entid)],
                                |r| r.get_i64(0),
                            )?;
                            if conflict.is_some() {
                                bail_db(DbErrorKind::SchemaAlterationFailed(format!(
                                    "Cannot alter schema attribute {entid} to be :db.cardinality/one"
                                )))?;
                            }
                        }
                    }
                    AttributeAlteration::NoHistory | AttributeAlteration::IsComponent => {}
                }
            }
        }
        Ok(())
    }

    fn current_values(&self, e: Entid, a: Entid) -> Result<Vec<TypedValue>> {
        Ok(self.conn.query_rows(
            "SELECT v, value_type_tag FROM datoms WHERE e = ? AND a = ?",
            &[SqlValue::Integer(e), SqlValue::Integer(a)],
            |r| {
                TypedValue::from_sql(r.get_value(0)?, r.get_i64(1)? as i32)
                    .map_err(|e| mentat_sql::SqlError::new(e.to_string()))
            },
        )?)
    }

    fn entity_datoms(&self, e: Entid) -> Result<Vec<(Entid, TypedValue)>> {
        Ok(self.conn.query_rows(
            "SELECT a, v, value_type_tag FROM datoms WHERE e = ?",
            &[SqlValue::Integer(e)],
            |r| {
                let v = TypedValue::from_sql(r.get_value(1)?, r.get_i64(2)? as i32)
                    .map_err(|e| mentat_sql::SqlError::new(e.to_string()))?;
                Ok((r.get_i64(0)?, v))
            },
        )?)
    }
}

fn bail_db(kind: DbErrorKind) -> Result<()> {
    Err(kind.into())
}
