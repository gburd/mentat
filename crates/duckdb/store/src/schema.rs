// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! The DuckDB schema of one store, mirroring the SQLite store's tables so the
//! engine's SQL resolves unchanged.
//!
//! - `v` is `mentat_value`, a `UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)`:
//!   SQLite's storage classes, with the same mapping (ref/bool/long/instant ->
//!   `i`, double -> `d`, string/keyword -> `s`, uuid/bytes -> `b`).
//! - No indexes on `v` (DuckDB can't index a UNION) and no partial indexes
//!   (unsupported): DuckDB filters and joins `datoms` by scanning, with zone
//!   maps on the BIGINT columns.
//! - `known_parts."end"`: `end` is reserved in DuckDB.
//! - Fulltext (`:db/fulltext`): `fulltext_values` / `fulltext_datoms` exist so
//!   the engine's SQL resolves, and a fulltext value is stored as a plain
//!   string in `datoms`, not tokenized. `(fulltext ...)` queries are rejected
//!   (see `storing.rs`).

/// The store's schema version (kept in `<schema>.meta`).
pub const VERSION: i64 = 1;

/// DDL for a new store in DuckDB schema `s`.
pub fn create_statements(s: &str) -> Vec<String> {
    vec![
        format!("CREATE SCHEMA IF NOT EXISTS {s}"),
        format!(
            "CREATE TYPE {s}.mentat_value AS UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)"
        ),
        // A numeric view of a value, for comparisons that SQLite makes across
        // INTEGER and REAL (5 = 5.0, 9.5 < 10). NULL for non-numbers.
        format!(
            "CREATE OR REPLACE MACRO {s}.mentat_num(x) AS CASE union_tag(x) \
             WHEN 'i' THEN union_extract(x, 'i')::DOUBLE \
             WHEN 'd' THEN union_extract(x, 'd') END"
        ),
        format!(
            "CREATE TABLE {s}.meta (key VARCHAR PRIMARY KEY, value VARCHAR NOT NULL)"
        ),
        format!(
            "CREATE TABLE {s}.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {s}.mentat_value NOT NULL, tx BIGINT NOT NULL, value_type_tag SMALLINT NOT NULL, \
             index_avet BOOLEAN NOT NULL DEFAULT false, index_vaet BOOLEAN NOT NULL DEFAULT false, \
             index_fulltext BOOLEAN NOT NULL DEFAULT false, unique_value BOOLEAN NOT NULL DEFAULT false)"
        ),
        // The SQLite store's (e, a, ...) indexes, minus `v`. Point lookups by
        // entity (pull, retractEntity, cardinality checks) stay index scans.
        format!("CREATE INDEX idx_datoms_ea ON {s}.datoms (e, a)"),
        format!("CREATE INDEX idx_datoms_a ON {s}.datoms (a)"),
        format!(
            "CREATE TABLE {s}.timelined_transactions (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {s}.mentat_value NOT NULL, tx BIGINT NOT NULL, added BOOLEAN NOT NULL DEFAULT true, \
             value_type_tag SMALLINT NOT NULL, timeline SMALLINT NOT NULL DEFAULT 0)"
        ),
        format!("CREATE INDEX idx_tt_tx ON {s}.timelined_transactions (tx)"),
        format!(
            "CREATE VIEW {s}.transactions AS SELECT e, a, v, value_type_tag, tx, added \
             FROM {s}.timelined_transactions WHERE timeline = 0"
        ),
        // Fulltext: not tokenized on DuckDB (see module docs). The views keep
        // the engine's `all_datoms` / `fulltext_datoms` references valid.
        format!(
            "CREATE TABLE {s}.fulltext_values (rowid_ BIGINT, text VARCHAR NOT NULL, searchid BIGINT)"
        ),
        format!(
            "CREATE VIEW {s}.fulltext_datoms AS SELECT e, a, v, tx, value_type_tag, index_avet, \
             index_vaet, index_fulltext, unique_value FROM {s}.datoms WHERE index_fulltext"
        ),
        format!(
            "CREATE VIEW {s}.all_datoms AS SELECT e, a, v, tx, value_type_tag, index_avet, \
             index_vaet, index_fulltext, unique_value FROM {s}.datoms"
        ),
        format!(
            "CREATE TABLE {s}.idents (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {s}.mentat_value NOT NULL, value_type_tag SMALLINT NOT NULL)"
        ),
        format!(
            "CREATE TABLE {s}.schema (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {s}.mentat_value NOT NULL, value_type_tag SMALLINT NOT NULL)"
        ),
        format!(
            "CREATE TABLE {s}.known_parts (part VARCHAR NOT NULL PRIMARY KEY, start BIGINT NOT NULL, \
             \"end\" BIGINT NOT NULL, idx BIGINT NOT NULL, allow_excision SMALLINT NOT NULL)"
        ),
        format!(
            "INSERT INTO {s}.meta VALUES ('version', '{VERSION}')"
        ),
    ]
}

/// The per-transaction scratch tables (SQLite's `temp.*_searches` /
/// `temp.search_results`), as DuckDB TEMP tables. `v0`/`v` use the store's
/// value type. Recreated per transaction.
pub fn begin_tx_statements(s: &str) -> Vec<String> {
    let cols = format!(
        "e0 BIGINT NOT NULL, a0 BIGINT NOT NULL, v0 {s}.mentat_value NOT NULL, \
         value_type_tag0 SMALLINT NOT NULL, added0 BOOLEAN NOT NULL, flags0 SMALLINT NOT NULL"
    );
    vec![
        "DROP TABLE IF EXISTS temp.exact_searches".to_string(),
        format!("CREATE TEMP TABLE exact_searches ({cols})"),
        "DROP TABLE IF EXISTS temp.inexact_searches".to_string(),
        format!("CREATE TEMP TABLE inexact_searches ({cols})"),
        "DROP TABLE IF EXISTS temp.search_results".to_string(),
        format!(
            "CREATE TEMP TABLE search_results ({cols}, search_type VARCHAR NOT NULL, \
             rid BIGINT, v {s}.mentat_value)"
        ),
    ]
}
