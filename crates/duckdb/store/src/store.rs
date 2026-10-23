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
use edn::entities::Entity;
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
pub(crate) fn write_lock(schema: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let mut locks = LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    locks.entry(schema.to_string()).or_default().clone()
}

/// SQL run on a connection when this is dropped, unless `run` or `disarm`
/// ran first: the ROLLBACK of an open transaction, the DETACH of an import's
/// source. Dropped on a panic too, which matters: duckdb-rs catches a panic
/// at the function boundary and the extension reuses the connection, so an
/// open transaction left behind would wedge writes to the store.
pub(crate) struct OnDrop<'c> {
    conn: &'c dyn SqlConn,
    sql: Option<String>,
}

impl<'c> OnDrop<'c> {
    pub(crate) fn new(conn: &'c dyn SqlConn, sql: impl Into<String>) -> Self {
        OnDrop {
            conn,
            sql: Some(sql.into()),
        }
    }

    /// Run it now.
    pub(crate) fn run(mut self) -> Result<()> {
        match self.sql.take() {
            Some(sql) => Ok(self.conn.execute_batch(&sql)?),
            None => Ok(()),
        }
    }

    pub(crate) fn disarm(mut self) {
        self.sql = None;
    }
}

impl Drop for OnDrop<'_> {
    fn drop(&mut self) {
        if let Some(sql) = self.sql.take() {
            let _ = self.conn.execute_batch(&sql);
        }
    }
}

/// One store (a DuckDB schema) on one connection. Cheap to construct; holds
/// no state beyond the store's name, so the extension builds one per call.
pub struct DuckStore<'c> {
    conn: &'c dyn SqlConn,
    /// The store's name as the caller gave it (for messages).
    name: String,
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
    let clean: String = clean.chars().take(40).collect();
    format!("mentat_{clean}_{:08x}", fnv1a(name))
}

/// FNV-1a of `s`: stable across builds (unlike DefaultHasher).
pub(crate) fn fnv1a(s: &str) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in s.as_bytes() {
        h ^= u32::from(*b);
        h = h.wrapping_mul(0x01000193);
    }
    h
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
            let mut catalog = None;
            conn.query_cached(
                "SELECT database_name FROM duckdb_databases() \
                 WHERE NOT internal AND lower(database_name) = lower(?)",
                &[SqlValue::Text(db.to_string())],
                &mut |r| {
                    catalog = Some(r.get_text(0)?);
                    Ok(())
                },
            )?;
            // duckdb_databases() lists catalogs without scanning them, so
            // it is safe beside a SQLite DETACH (see `exists`).
            if let Some(catalog) = catalog {
                let in_home = DuckStore::at(conn, home, store);
                if !in_home.exists()? {
                    target = (catalog, rest);
                }
            }
        }
        Ok(DuckStore {
            name: store.to_string(),
            ..DuckStore::at(conn, target.0, target.1)
        })
    }

    fn at(conn: &'c dyn SqlConn, catalog: String, name: &str) -> Self {
        let schema = schema_for(name);
        DuckStore {
            conn,
            name: name.to_string(),
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

    /// `"catalog"."schema"`, for the store's own SQL.
    pub(crate) fn qualified_name(&self) -> &str {
        &self.qualified
    }

    pub(crate) fn conn(&self) -> &'c dyn SqlConn {
        self.conn
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

    /// Whether the store exists: its `meta` table, named. Not
    /// `duckdb_tables()` (nor `duckdb_schemas()`, `duckdb_indexes()`,
    /// `information_schema`): those scan every attached database, and on
    /// DuckDB 1.5.6 one that scans a SQLite database while another
    /// connection DETACHes it segfaults (`SQLiteSchemaEntry::Scan`), which
    /// `mentat_import` does on every call.
    fn exists(&self) -> Result<bool> {
        self.has("meta", "key")
    }

    /// Whether this store's `table` exists with column `col`: a missing one
    /// fails to bind. Not inside a transaction: DuckDB 2.0 aborts one on any
    /// error. Read from DuckDB's error text (duckdb-rs drops the error type),
    /// in either form: plain, or JSON when `errors_as_json` is on and the
    /// pool's per-session override couldn't be set (`lock_configuration`).
    fn has(&self, table: &str, col: &str) -> Result<bool> {
        let sql = format!("SELECT {col} FROM {}.{table} LIMIT 0", self.qualified);
        match self.conn.query(&sql, &[], &mut |_| Ok(())) {
            Ok(()) => Ok(true),
            Err(e) if is_bind_miss(&e.message) => Ok(false),
            Err(e) => Err(e.into()),
        }
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
                let lock = write_lock(&self.qualified);
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                // Checked under the lock: another thread may have just made it.
                if !self.exists()? {
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
        self.drop_legacy_indexes()?;
        let upgraded = self.upgrade_views()?;
        let opened = self.read()?;
        // Cache only once the store is up to date: an upgrade a read-only
        // database couldn't make must run again on this thread's next open,
        // not after the next schema change.
        if upgraded {
            OPENED.with(|c| {
                c.borrow_mut()
                    .insert(self.qualified.clone(), (gen, opened.clone()))
            });
        }
        Ok((gen, opened))
    }

    /// Drop the secondary indexes that 1.11.0-1.14.0 created (see
    /// `schema::LEGACY_INDEXES`), once per store: `meta` then records it
    /// (a store created since 1.18 has the row from the start; 1.15-1.17
    /// never wrote it, so a missing row alone doesn't mean an index is
    /// there). Runs on a schema-cache miss. This also repairs a store whose
    /// index #26828 already corrupted.
    fn drop_legacy_indexes(&self) -> Result<()> {
        let done = self.conn.query_opt(
            &format!("SELECT 1 FROM {}.meta WHERE key = ?", self.qualified),
            &[SqlValue::Text(schema::LEGACY_INDEXES_DROPPED.into())],
            |r| r.get_i64(0),
        )?;
        if done.is_some() {
            return Ok(());
        }
        let lock = write_lock(&self.qualified);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // By name, not found with duckdb_indexes() (see `exists`; it also
        // fails while a mentat SQLite file is attached). Dropping a missing
        // index writes nothing, so a read-only database with none of them
        // opens. One that still has one (a 1.11-1.14 store never opened
        // writable since) can't drop it, and mustn't be queried with it
        // (#26828): DuckDB refuses the DROP, and that is reported as such.
        // Nothing is cached then, so a later writable open drops it.
        for i in schema::LEGACY_INDEXES {
            if let Err(e) = self
                .conn
                .execute_batch(&format!("DROP INDEX IF EXISTS {}.{i}", self.qualified))
            {
                return Err(if self.read_only()? {
                    MentatError::IoError(std::io::Error::other(format!(
                        "mentat: store '{}' in database '{}' was created by mentat \
                         1.11-1.14 and still has indexes that DuckDB 1.4-1.5 can corrupt \
                         (duckdb/duckdb#26828). Open the database writable once to remove \
                         them; read-only opens work after that.",
                        self.name, self.catalog
                    )))
                } else {
                    e.into()
                });
            }
        }
        // Under the write lock, so it can't conflict with a transact. Best
        // effort: a read-only database can't record it, and then the check
        // just runs again on the next open.
        let _ = self.conn.execute(
            &format!(
                "INSERT INTO {}.meta VALUES (?, '1') ON CONFLICT DO NOTHING",
                self.qualified
            ),
            &[SqlValue::Text(schema::LEGACY_INDEXES_DROPPED.into())],
        );
        Ok(())
    }

    /// Replace the pre-1.18 views, once per store: the `fulltext_values`
    /// table (always empty) becomes the view `(fulltext ...)` queries join,
    /// and view bodies that name the schema are rewritten unqualified (see
    /// `schema::views`). `meta` then records it (a store created since has the row from the start). Runs on
    /// a schema-cache miss. A read-only database (`duckdb -readonly`,
    /// `ATTACH ... (READ_ONLY)`) is left as it is, and every query still
    /// works (`(fulltext ...)` reads the texts from `datoms`, not this table
    /// or view); returns false then, so a later writable open upgrades it.
    fn upgrade_views(&self) -> Result<bool> {
        let done = self.conn.query_opt(
            &format!("SELECT 1 FROM {}.meta WHERE key = ?", self.qualified),
            &[SqlValue::Text(schema::VIEWS_1_18.into())],
            |r| r.get_i64(0),
        )?;
        if done.is_some() {
            return Ok(true);
        }
        if self.read_only()? {
            return Ok(false);
        }
        let lock = write_lock(&self.qualified);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        // The old table has a `searchid` column; the view doesn't.
        let table = self.has("fulltext_values", "searchid")?;
        self.in_tx(|s| {
            if table {
                s.conn
                    .execute_batch(&format!("DROP TABLE {}.fulltext_values", s.qualified))?;
                s.conn
                    .execute_batch(&schema::fulltext_values_view(&s.qualified))?;
            }
            for v in schema::views(&s.qualified, "CREATE OR REPLACE") {
                s.conn.execute_batch(&v)?;
            }
            s.conn.execute(
                &format!(
                    "INSERT INTO {}.meta VALUES (?, '1') ON CONFLICT DO NOTHING",
                    s.qualified
                ),
                &[SqlValue::Text(schema::VIEWS_1_18.into())],
            )?;
            Ok(())
        })?;
        Ok(true)
    }

    /// Whether the store's database is attached read-only (`duckdb
    /// -readonly`, `ATTACH ... (READ_ONLY)`). duckdb_databases() lists
    /// catalogs without scanning them (see `new`).
    fn read_only(&self) -> Result<bool> {
        Ok(self.conn.query_opt(
            "SELECT readonly::INTEGER FROM duckdb_databases() WHERE database_name = ?",
            &[SqlValue::Text(self.catalog.clone())],
            |r| r.get_bool(0),
        )? == Some(true))
    }

    /// Under the write lock, in a transaction (see `open_gen`).
    fn create(&self) -> Result<()> {
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
        let mut bad = None;
        self.conn.query_cached(
            "SELECT part, start, \"end\", idx, allow_excision FROM known_parts",
            &[],
            &mut |r| {
                let (start, end, idx) = (r.get_i64(1)?, r.get_i64(2)?, r.get_i64(3)?);
                // Partition::new asserts this; an imported file may break it.
                if !(start <= idx && idx <= end) {
                    bad = Some(format!(
                        "partition {}: start {start}, end {end}, next entid {idx}",
                        r.get_text(0)?
                    ));
                    return Ok(());
                }
                parts.insert(
                    r.get_text(0)?,
                    Partition::new(start, end, idx, r.get_i64(4)? != 0),
                );
                Ok(())
            },
        )?;
        if let Some(bad) = bad {
            return Err(MentatError::IoError(std::io::Error::other(format!(
                "bad known_parts row ({bad}): need start <= next entid <= end"
            ))));
        }
        Ok(parts)
    }

    /// Read the partition map and schema (the `idents`/`schema` views).
    pub(crate) fn read(&self) -> Result<Opened> {
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
    /// back otherwise, panics included.
    pub(crate) fn in_tx<T>(&self, f: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        let rollback = self.begin()?;
        let v = f(self)?;
        self.conn.execute_batch("COMMIT")?;
        rollback.disarm();
        Ok(v)
    }

    /// BEGIN, and the ROLLBACK that undoes it when dropped.
    fn begin(&self) -> Result<OnDrop<'c>> {
        self.conn.execute_batch("BEGIN TRANSACTION")?;
        Ok(OnDrop::new(self.conn, "ROLLBACK"))
    }

    /// Transact `edn` (a vector of tx data). Atomic: one DuckDB transaction,
    /// so not inside one already open on the connection (see
    /// `with_speculative`).
    pub fn transact(&self, edn: &str) -> Result<TxReport> {
        let entities = edn::parse::entities(edn)?;
        let (gen, opened) = self.open_gen()?;
        let lock = write_lock(&self.qualified);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        self.in_tx(|s| s.apply(entities, gen, &opened.schema))
    }

    /// Speculatively transact `edns` in order (Datomic `d/with`) and return
    /// the last report. They run in one DuckDB transaction on this
    /// connection that is always rolled back: no other connection sees them,
    /// and nothing is left behind, on success or error (partition counters
    /// and the schema generation included). Holds the store's write lock, as
    /// `transact` does.
    ///
    /// DuckDB has no savepoints, so this (like `transact`) can't nest inside
    /// a transaction already open on the connection: its BEGIN fails with
    /// "cannot start a transaction within a transaction", and DuckDB then
    /// aborts the caller's transaction too, losing its uncommitted writes
    /// (its COMMIT fails). Not guarded: detecting an open transaction takes
    /// two extra statements per write, and the extension's connections never
    /// have one. A direct Rust caller must not call this inside its own
    /// transaction.
    pub fn with_speculative(&self, edns: &[&str]) -> Result<TxReport> {
        let txs = edns
            .iter()
            .map(|edn| edn::parse::entities(edn))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let (gen, opened) = self.open_gen()?;
        let lock = write_lock(&self.qualified);
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let rollback = self.begin()?;
        let report = txs.into_iter().try_fold(None, |_, entities| {
            self.apply(entities, gen, &opened.schema).map(Some)
        });
        let rolled_back = rollback.run();
        let report = report?;
        rolled_back?;
        report.ok_or_else(|| MentatError::NotYetImplemented("with: no tx-data".into()))
    }

    /// Apply `edn` inside the caller's DuckDB transaction (no BEGIN/COMMIT).
    /// `schema` is the store's as of generation `gen`; it is reread if
    /// another writer (or an earlier `apply` in this transaction) changed it.
    fn apply(
        &self,
        entities: Vec<Entity<edn::ValueAndSpan>>,
        gen: i64,
        schema: &Schema,
    ) -> Result<TxReport> {
        // The partition map is read inside the writing transaction.
        let partition_map = self.read_partition_map()?;
        let reread;
        let schema = if self.generation()? == Some(gen) {
            schema
        } else {
            reread = self.read()?.schema;
            &reread
        };
        let storing = DuckStoring {
            conn: self.conn,
            read: Some(partition_map.clone()),
        };
        let (report, _next_partition_map, next_schema, _w) = mentat_db::transact(
            &storing,
            partition_map,
            schema,
            schema,
            NullWatcher(),
            entities,
        )?;
        #[cfg(feature = "failpoints")]
        crate::failpoints::hit("apply");
        if next_schema.is_some() {
            self.conn.execute(
                "UPDATE meta SET value = ((hash(uuid()) >> 1)::BIGINT)::VARCHAR WHERE key = 'generation'",
                &[],
            )?;
        }
        Ok(report)
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

/// A Catalog or Binder error: the table or column isn't there. DuckDB reports
/// it as `Catalog Error: ...` / `Binder Error: ...`, or with `errors_as_json`
/// as JSON, `{"exception_type":"Catalog",...}`, which can arrive wrapped
/// (`Invalid Input Error: {...}`), so the JSON form is searched for.
fn is_bind_miss(message: &str) -> bool {
    ["Catalog Error", "Binder Error"]
        .iter()
        .any(|k| message.starts_with(k))
        || [
            "\"exception_type\":\"Catalog\"",
            "\"exception_type\":\"Binder\"",
        ]
        .iter()
        .any(|k| message.contains(k))
}

#[cfg(test)]
mod tests {
    use super::is_bind_miss;

    #[test]
    fn bind_miss_plain_and_json() {
        assert!(is_bind_miss(
            "Catalog Error: Table with name meta does not exist!"
        ));
        assert!(is_bind_miss(
            "Binder Error: Referenced column \"key\" not found"
        ));
        // errors_as_json on, as DuckDB 1.5.6 reports it (also wrapped).
        assert!(is_bind_miss(
            r#"{"exception_type":"Catalog","exception_message":"Table with name \"nope.meta\" does not exist"}"#
        ));
        assert!(is_bind_miss(
            r#"Invalid Input Error: {"exception_type":"Binder","exception_message":"Referenced column"}"#
        ));
        assert!(!is_bind_miss("IO Error: Could not set lock on file"));
        assert!(!is_bind_miss(
            r#"{"exception_type":"IO","exception_message":"x"}"#
        ));
        assert!(!is_bind_miss("Permission Error: read-only"));
    }
}
