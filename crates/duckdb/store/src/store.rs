// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! A mentat store in DuckDB tables: open (creating it on first use), transact,
//! query, pull.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use core_traits::{Entid, TypedValue};
use mentat_core::{IdentMap, Schema, TxReport};
use mentat_db::db::TypedSQLValue;
use mentat_db::{entids, NullWatcher, Partition, PartitionMap, SchemaBuilding};
use mentat_query_algebrizer::{Known, QueryInputs, TemporalBound};
use mentat_sql::{SqlConn, SqlConnExt, SqlValue};
use mentat_transaction::query::{q_once_on, QueryOutput};
use public_traits::errors::{MentatError, Result};

use crate::schema;
use crate::storing::DuckStoring;

/// A store's metadata, as read from (or created in) its DuckDB schema.
#[derive(Clone)]
pub struct Opened {
    pub schema: Schema,
    pub partition_map: PartitionMap,
}

thread_local! {
    /// Per thread (DuckDB calls a function on its worker threads): store
    /// schema name -> (generation, opened metadata).
    static OPENED: RefCell<HashMap<String, (i64, Opened)>> = RefCell::new(HashMap::new());
}

/// One lock per store schema, held while a store is created and for a whole
/// `transact`. Two DuckDB connections writing one store at once conflict
/// ("Conflict on update" on `known_parts`, which every transaction updates),
/// so writes to a store take turns; reads, and writes to other stores, don't
/// wait. Per process: another process can't open the same database file
/// for writing (DuckDB locks it).
fn write_lock(schema: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.entry(schema.to_string()).or_default().clone()
}

/// One store (a DuckDB schema) on one connection. Cheap to construct; holds
/// no state beyond the store's name, so the extension builds one per call.
pub struct DuckStore<'c> {
    conn: &'c dyn SqlConn,
    /// The database (catalog) holding the store.
    catalog: String,
    /// The store's schema in it (`schema_for`).
    schema: String,
    /// `"catalog"."schema"`: what the store's own SQL names, and the search
    /// path the engine's unqualified SQL runs under.
    qualified: String,
}

/// The DuckDB schema for store `name`: `mentat` for the default store,
/// `mentat_<name>` otherwise, with anything but `[A-Za-z0-9_]` mapped to `_`
/// so a path-like name (`/tmp/demo.mentat`, the old file argument) is a valid
/// identifier. A short hash of the full name keeps distinct names distinct.
pub fn schema_for(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() || name == "default" {
        return "mentat".to_string();
    }
    let clean: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_matches('_');
    // FNV-1a: stable across builds (unlike DefaultHasher).
    let mut h: u32 = 0x811c9dc5;
    for b in name.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x01000193);
    }
    let clean: String = clean.chars().take(40).collect();
    format!("mentat_{clean}_{h:08x}")
}

fn decode(v: SqlValue, tag: i64) -> std::result::Result<TypedValue, mentat_sql::SqlError> {
    TypedValue::from_sql(v, tag as i32).map_err(|e| mentat_sql::SqlError::new(e.to_string()))
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

impl<'c> DuckStore<'c> {
    /// Store `store`: `'db.name'` is store `name` in the attached database
    /// `db`, when a database of that name (any case) is attached; any other
    /// value is a store in the connection's own database (`home_catalog`).
    /// `'db.'` and `'db.default'` are `db`'s default store. A store that
    /// already exists in the home database under the whole name (a
    /// path-like name, or one made before `db` was attached) keeps it.
    pub fn new(conn: &'c dyn SqlConn, store: &str) -> Result<Self> {
        let home = conn.home_catalog()?;
        let mut target = (home.clone(), store);
        if let Some((db, rest)) = store
            .trim()
            .split_once('.')
            .filter(|(db, _)| !db.is_empty())
        {
            let catalog = conn.query_opt(
                "SELECT database_name FROM duckdb_databases() \
                 WHERE NOT internal AND lower(database_name) = lower(?)",
                &[SqlValue::Text(db.to_string())],
                |r| r.get_text(0),
            )?;
            if let Some(catalog) = catalog {
                let in_home = DuckStore::at(conn, home, store);
                if !in_home.exists()? {
                    target = (catalog, rest);
                }
            }
        }
        Ok(DuckStore::at(conn, target.0, target.1))
    }

    fn at(conn: &'c dyn SqlConn, catalog: String, name: &str) -> Self {
        let schema = schema_for(name);
        DuckStore {
            conn,
            qualified: format!("{}.{}", quote(&catalog), quote(&schema)),
            catalog,
            schema,
        }
    }

    /// The store's schema, unqualified.
    pub fn schema_name(&self) -> &str {
        &self.schema
    }

    /// The database (catalog) holding the store.
    pub fn catalog_name(&self) -> &str {
        &self.catalog
    }

    fn storing(&self) -> DuckStoring<'_> {
        DuckStoring {
            conn: self.conn,
            read: None,
        }
    }

    /// Point the connection's search path at this store, so the engine's
    /// unqualified table names resolve to its tables.
    fn use_schema(&self) -> Result<()> {
        self.conn.use_schema(&self.qualified)?;
        Ok(())
    }

    /// `[catalog, schema]`, bound as `$1`, `$2`.
    fn names(&self) -> [SqlValue; 2] {
        [
            SqlValue::Text(self.catalog.clone()),
            SqlValue::Text(self.schema.clone()),
        ]
    }

    fn exists(&self) -> Result<bool> {
        Ok(self
            .conn
            .query_opt(
                "SELECT 1 FROM duckdb_tables() WHERE database_name = $1 AND schema_name = $2 \
                 AND table_name = 'meta'",
                &self.names(),
                |r| r.get_i64(0),
            )?
            .is_some())
    }

    /// The store's generation: a random value, replaced by every transaction
    /// that changes the schema or idents (in that same DuckDB transaction), so a
    /// cached schema is current iff the generation matches. Random rather than
    /// a counter so two databases' stores of the same name never share one.
    /// One primary-key read.
    fn generation(&self) -> Result<Option<i64>> {
        // Cached: one table, the key bound, and every store has the row from
        // creation on.
        let mut gen = None;
        self.conn.query_cached(
            &format!(
                "SELECT value::BIGINT FROM {}.meta WHERE key = ?",
                self.qualified
            ),
            &[SqlValue::Text("generation".into())],
            &mut |r| {
                gen = Some(r.get_i64(0)?);
                Ok(())
            },
        )?;
        Ok(gen)
    }

    /// Open the store, creating and bootstrapping it on first use. Cheap when
    /// this thread has opened it before: a cached schema, revalidated by one
    /// generation read. (Another connection that changes the schema bumps the
    /// generation, so it is seen here.)
    pub fn open(&self) -> Result<Opened> {
        Ok(self.open_gen()?.1)
    }

    /// `open`, also returning the generation it validated against.
    fn open_gen(&self) -> Result<(i64, Opened)> {
        // A missing store reads as a missing meta table: create it.
        let gen = match self.generation() {
            Ok(Some(g)) => g,
            _ => {
                if !self.exists()? {
                    let lock = write_lock(&self.qualified);
                    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    self.in_tx(|s| s.create())?;
                }
                self.generation()?.unwrap_or(0)
            }
        };
        self.use_schema()?;
        let cached = OPENED.with(|c| {
            c.borrow()
                .get(&self.qualified)
                .filter(|(g, _)| *g == gen)
                .map(|(_, o)| o.clone())
        });
        if let Some(opened) = cached {
            return Ok((gen, opened));
        }
        self.upgrade()?;
        let opened = self.read()?;
        OPENED.with(|c| {
            c.borrow_mut()
                .insert(self.qualified.clone(), (gen, opened.clone()))
        });
        Ok((gen, opened))
    }

    /// Bring an older store's layout up to date. Runs on a schema-cache miss,
    /// so once per thread and generation; a no-op once done.
    /// - Drop the secondary indexes that 1.11.0-1.14.0 created (see
    ///   `schema::LEGACY_INDEXES`). This also repairs a store whose index
    ///   #26828 already corrupted.
    /// - Replace the pre-1.18 `fulltext_values` table (always empty) with the
    ///   view `(fulltext ...)` queries join.
    fn upgrade(&self) -> Result<()> {
        let stale = self.conn.query_opt(
            &format!(
                "SELECT (SELECT count(*) FROM duckdb_indexes() WHERE database_name = $1 \
                   AND schema_name = $2 AND index_name IN ({})) \
                 + (SELECT count(*) FROM duckdb_tables() WHERE database_name = $1 \
                   AND schema_name = $2 AND table_name = 'fulltext_values')",
                schema::LEGACY_INDEXES.map(|i| format!("'{i}'")).join(", ")
            ),
            &self.names(),
            |r| r.get_i64(0),
        )?;
        if stale.unwrap_or(0) == 0 {
            return Ok(());
        }
        let lock = write_lock(&self.qualified);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        for i in schema::LEGACY_INDEXES {
            self.conn
                .execute_batch(&format!("DROP INDEX IF EXISTS {}.{i}", self.qualified))?;
        }
        let table: Option<i64> = self.conn.query_opt(
            "SELECT 1 FROM duckdb_tables() WHERE database_name = $1 AND schema_name = $2 \
             AND table_name = 'fulltext_values'",
            &self.names(),
            |r| r.get_i64(0),
        )?;
        if table.is_some() {
            self.in_tx(|s| {
                s.conn
                    .execute_batch(&format!("DROP TABLE {}.fulltext_values", s.qualified))?;
                s.conn
                    .execute_batch(&schema::fulltext_values_view(&s.qualified))?;
                Ok(())
            })?;
        }
        Ok(())
    }

    fn create(&self) -> Result<()> {
        // A concurrent opener may have created it since we checked.
        if self.exists()? {
            return Ok(());
        }
        for stmt in schema::create_statements(&self.qualified) {
            self.conn.execute_batch(&stmt)?;
        }
        self.use_schema()?;
        self.conn.execute(
            &format!(
                "INSERT INTO {}.meta VALUES ('generation', ((hash(uuid()) >> 1)::BIGINT)::VARCHAR)",
                self.qualified
            ),
            &[],
        )?;
        let partition_map = mentat_db::bootstrap_partition_map();
        for (part, p) in partition_map.iter() {
            self.conn.execute(
                "INSERT INTO known_parts (part, start, \"end\", idx, allow_excision) VALUES (?, ?, ?, ?, ?)",
                &[
                    SqlValue::Text(part.clone()),
                    SqlValue::Integer(p.start),
                    SqlValue::Integer(p.end),
                    SqlValue::Integer(p.next_entid()),
                    SqlValue::Integer(p.allow_excision.into()),
                ],
            )?;
        }
        let bootstrap_schema = mentat_db::bootstrap_schema();
        let empty = Schema::default();
        let storing = self.storing();
        let (_report, next_partition_map, next_schema, _w) = mentat_db::transact(
            &storing,
            partition_map,
            &empty,
            &bootstrap_schema,
            NullWatcher(),
            mentat_db::bootstrap_entities(),
        )?;
        if next_schema.is_some_and(|s| s != bootstrap_schema) {
            return Err(MentatError::DbError(
                db_traits::errors::DbErrorKind::NotYetImplemented(
                    "initial bootstrap transaction did not produce the bootstrap schema".into(),
                )
                .into(),
            ));
        }
        // `transact` committed the partition high-water marks itself.
        let _ = next_partition_map;
        Ok(())
    }

    /// Read the partition map (3 rows; cached: no filter, one table).
    fn read_partition_map(&self) -> Result<PartitionMap> {
        let mut parts = PartitionMap::default();
        self.conn.query_cached(
            "SELECT part, start, \"end\", idx, allow_excision FROM known_parts",
            &[],
            &mut |r| {
                let p = Partition::new(
                    r.get_i64(1)?,
                    r.get_i64(2)?,
                    r.get_i64(3)?,
                    r.get_i64(4)? != 0,
                );
                parts.insert(r.get_text(0)?, p);
                Ok(())
            },
        )?;
        Ok(parts)
    }

    /// Read the partition map and schema (the `idents`/`schema` views).
    fn read(&self) -> Result<Opened> {
        let partition_map = self.read_partition_map()?;
        let triples = |table: &str| {
            self.conn.query_rows(
                &format!("SELECT e, a, v, value_type_tag FROM {table}"),
                &[],
                |r| {
                    Ok((
                        r.get_i64(0)?,
                        r.get_i64(1)?,
                        decode(r.get_value(2)?, r.get_i64(3)?)?,
                    ))
                },
            )
        };
        let mut ident_map = IdentMap::default();
        for (e, a, v) in triples("idents")? {
            match (a, v) {
                (entids::DB_IDENT, TypedValue::Keyword(k)) => {
                    ident_map.insert(k.as_ref().clone(), e);
                }
                (a, v) => {
                    return Err(MentatError::DbError(
                        db_traits::errors::DbErrorKind::NotYetImplemented(format!(
                            "bad idents row: [{e} {a} {v:?}]"
                        ))
                        .into(),
                    ))
                }
            }
        }
        let attribute_map = mentat_db::db::read_attribute_map_from(triples("schema")?)?;
        let schema = Schema::from_ident_map_and_attribute_map(ident_map, attribute_map)?;
        Ok(Opened {
            schema,
            partition_map,
        })
    }

    /// Run `f` in one DuckDB transaction: committed if it returns Ok, rolled
    /// back otherwise.
    fn in_tx<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        match f(self) {
            Ok(v) => {
                self.conn.execute_batch("COMMIT")?;
                Ok(v)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(e)
            }
        }
    }

    /// Transact `edn` (a vector of tx data). Atomic: one DuckDB transaction.
    pub fn transact(&self, edn: &str) -> Result<TxReport> {
        let entities = edn::parse::entities(edn)?;
        let (gen, opened) = self.open_gen()?;
        let lock = write_lock(&self.qualified);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.in_tx(|s| {
            // The partition map is read inside the writing transaction; the
            // schema is reused unless another writer changed it since `open`.
            let partition_map = s.read_partition_map()?;
            let schema = if s.generation()? == Some(gen) {
                opened.schema
            } else {
                s.read()?.schema
            };
            let storing = DuckStoring {
                conn: s.conn,
                read: Some(partition_map.clone()),
            };
            let (report, _next_partition_map, next_schema, _w) = mentat_db::transact(
                &storing,
                partition_map,
                &schema,
                &schema,
                NullWatcher(),
                entities,
            )?;
            if next_schema.is_some() {
                s.conn.execute(
                    "UPDATE meta SET value = ((hash(uuid()) >> 1)::BIGINT)::VARCHAR WHERE key = 'generation'",
                    &[],
                )?;
            }
            Ok(report)
        })
    }

    /// Run a Datalog query with the shared JSON options
    /// (`{"inputs": [...], "asOf": T, "since": T}`), opening the store once.
    pub fn q_json(&self, query: &str, options: &serde_json::Value) -> Result<QueryOutput> {
        let Opened { schema, .. } = self.open()?;
        let (inputs, temporal) =
            mentat_transaction::options::options_from_json(&schema, query, options)?;
        q_once_on(
            self.conn,
            Known::for_schema(&schema),
            query,
            Some(inputs),
            temporal,
        )
    }

    /// Run a Datalog query with inputs, optionally against a historical basis,
    /// on an already-opened store.
    pub fn q_opened(
        &self,
        opened: &Opened,
        query: &str,
        inputs: Option<QueryInputs>,
        temporal: Option<TemporalBound>,
    ) -> Result<QueryOutput> {
        q_once_on(
            self.conn,
            Known::for_schema(&opened.schema),
            query,
            inputs,
            temporal,
        )
    }

    /// Run a Datalog query, optionally against a historical basis.
    pub fn q(
        &self,
        query: &str,
        inputs: Option<QueryInputs>,
        temporal: Option<TemporalBound>,
    ) -> Result<QueryOutput> {
        let Opened { schema, .. } = self.open()?;
        q_once_on(
            self.conn,
            Known::for_schema(&schema),
            query,
            inputs,
            temporal,
        )
    }

    /// The store's schema (for parsing query inputs).
    pub fn current_schema(&self) -> Result<Schema> {
        Ok(self.open()?.schema)
    }

    /// The latest transaction id.
    pub fn last_tx(&self) -> Result<Entid> {
        self.open()?;
        Ok(self
            .conn
            .query_opt("SELECT max(tx) FROM transactions", &[], |r| {
                r.get_opt_i64(0)
            })?
            .flatten()
            .unwrap_or(0))
    }
}
