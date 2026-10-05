// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! mentat's storage on DuckDB tables: the backend of the DuckDB extension.
//!
//! The engine (transactor, algebrizer, projector, pull) is the SQLite engine's,
//! reached through `mentat_db::MentatStoring` (writes) and `mentat_sql::SqlConn`
//! (queries). This crate supplies the DuckDB schema and the DuckDB SQL for the
//! storage operations; see `docs/duckdb-native-storage-plan.md`.
//!
//! A store is a DuckDB schema (`mentat` for the default store, `mentat_<name>`
//! otherwise) holding the same tables as the SQLite store: `datoms`,
//! `timelined_transactions` (+ the `transactions` view), `idents`, `schema`,
//! `known_parts`. Every query runs with that schema first on the search path,
//! so the engine's unqualified table names resolve to the store's tables.

pub use mentat_sql::{Dialect, SqlConn, SqlConnExt, SqlError, SqlRow, SqlValue};

mod schema;
mod store;
mod storing;

pub use store::{DuckStore, Opened};
