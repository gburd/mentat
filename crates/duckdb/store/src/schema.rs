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
//! - `v` is a `UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)` ([`VALUE_TYPE`]):
//!   SQLite's storage classes, with the same mapping (ref/bool/long/instant ->
//!   `i`, double -> `d`, string/keyword -> `s`, uuid/bytes -> `b`). It is
//!   written out inline, not as a named `CREATE TYPE`: on DuckDB 1.5.6 a type
//!   created in a non-`main` schema makes WAL replay fail ("Schema with name
//!   ... does not exist"), so a store whose database closed without a
//!   checkpoint wouldn't reopen.
//! - No secondary indexes: DuckDB filters and joins `datoms` by scanning,
//!   with zone maps on the BIGINT columns. (It can't index the `v` UNION or
//!   build partial indexes.) Stores from 1.11.0-1.14.0 had ART indexes on
//!   `datoms (e, a)`, `datoms (a)` and `timelined_transactions (tx)`. DuckDB
//!   1.5.6 planned sequential scans anyway: lookups timed the same at 12M rows,
//!   and inserts were 8.5x slower with them. The indexes also tripped
//!   duckdb/duckdb#26828: after a WAL replay is checkpointed, `a = X` returns no
//!   rows and DELETEs fail FATAL. `LEGACY_INDEXES` lists them so `open` can
//!   drop them.
//! - `known_parts."end"`: `end` is reserved in DuckDB.
//! - Fulltext (`:db/fulltext`): `fulltext_values` / `fulltext_datoms` exist so
//!   the engine's SQL resolves, and a fulltext value is stored as a plain
//!   string in `datoms`, not tokenized. `(fulltext ...)` queries are rejected
//!   (see `storing.rs`).

/// The store's schema version (kept in `<schema>.meta`).
pub const VERSION: i64 = 1;

/// Secondary indexes created by 1.11.0-1.14.0; dropped on open (see above).
pub const LEGACY_INDEXES: [&str; 3] = ["idx_datoms_ea", "idx_datoms_a", "idx_tt_tx"];

/// The `meta` key recording that a store has none of `LEGACY_INDEXES`
/// (written at creation, or once `open` has dropped them).
pub const LEGACY_INDEXES_DROPPED: &str = "legacy_indexes_dropped";

/// The type of every `v` column.
pub const VALUE_TYPE: &str = "UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)";

/// Plain copies of `v`'s members, on `datoms` and `timelined_transactions`.
/// DuckDB handles a UNION column slowly (scanned, hashed and joined as a
/// struct): a ref traversal over 100k datoms took 16 ms on `v` and 3.3 ms on
/// plain columns. The DuckDB query dialect filters, joins, compares and
/// aggregates on these whenever the value's type is known, and falls back to
/// `v` otherwise. Written with `v` on every insert, so they always agree.
pub const TYPED_COLUMNS: &str = "v_i BIGINT, v_d DOUBLE, v_s VARCHAR";

/// The typed columns' values for a `v`-typed expression `x`, for an INSERT.
pub fn typed_values(x: &str) -> String {
    format!("union_extract({x}, 'i'), union_extract({x}, 'd'), union_extract({x}, 's')")
}

/// DDL for a new store in DuckDB schema `s`.
pub fn create_statements(s: &str) -> Vec<String> {
    vec![
        format!("CREATE SCHEMA IF NOT EXISTS {s}"),
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
             v {VALUE_TYPE} NOT NULL, tx BIGINT NOT NULL, value_type_tag SMALLINT NOT NULL, \
             index_avet BOOLEAN NOT NULL DEFAULT false, index_vaet BOOLEAN NOT NULL DEFAULT false, \
             index_fulltext BOOLEAN NOT NULL DEFAULT false, unique_value BOOLEAN NOT NULL DEFAULT false, \
             {TYPED_COLUMNS})"
        ),
        format!(
            "CREATE TABLE {s}.timelined_transactions (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {VALUE_TYPE} NOT NULL, tx BIGINT NOT NULL, added BOOLEAN NOT NULL DEFAULT true, \
             value_type_tag SMALLINT NOT NULL, timeline SMALLINT NOT NULL DEFAULT 0, \
             {TYPED_COLUMNS})"
        ),
        format!(
            "CREATE VIEW {s}.transactions AS SELECT e, a, v, value_type_tag, tx, added, \
             v_i, v_d, v_s FROM {s}.timelined_transactions WHERE timeline = 0"
        ),
        // Fulltext: not tokenized on DuckDB (see module docs). The views keep
        // the engine's `all_datoms` / `fulltext_datoms` references valid.
        format!(
            "CREATE TABLE {s}.fulltext_values (rowid_ BIGINT, text VARCHAR NOT NULL, searchid BIGINT)"
        ),
        format!(
            "CREATE VIEW {s}.fulltext_datoms AS SELECT e, a, v, tx, value_type_tag, index_avet, \
             index_vaet, index_fulltext, unique_value, v_i, v_d, v_s FROM {s}.datoms WHERE index_fulltext"
        ),
        format!(
            "CREATE VIEW {s}.all_datoms AS SELECT e, a, v, tx, value_type_tag, index_avet, \
             index_vaet, index_fulltext, unique_value, v_i, v_d, v_s FROM {s}.datoms"
        ),
        format!(
            "CREATE TABLE {s}.idents (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {VALUE_TYPE} NOT NULL, value_type_tag SMALLINT NOT NULL)"
        ),
        format!(
            "CREATE TABLE {s}.schema (e BIGINT NOT NULL, a BIGINT NOT NULL, \
             v {VALUE_TYPE} NOT NULL, value_type_tag SMALLINT NOT NULL)"
        ),
        format!(
            "CREATE TABLE {s}.known_parts (part VARCHAR NOT NULL PRIMARY KEY, start BIGINT NOT NULL, \
             \"end\" BIGINT NOT NULL, idx BIGINT NOT NULL, allow_excision SMALLINT NOT NULL)"
        ),
        format!(
            "INSERT INTO {s}.meta VALUES ('version', '{VERSION}')"
        ),
        format!("INSERT INTO {s}.meta VALUES ('{LEGACY_INDEXES_DROPPED}', '1')"),
    ]
}

/// The per-transaction scratch tables (SQLite's `temp.*_searches` /
/// `temp.search_results`), as DuckDB TEMP tables. Recreated per transaction.
pub fn begin_tx_statements() -> Vec<String> {
    let cols = format!(
        "e0 BIGINT NOT NULL, a0 BIGINT NOT NULL, v0 {VALUE_TYPE} NOT NULL, \
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
             rid BIGINT, v {VALUE_TYPE})"
        ),
    ]
}
