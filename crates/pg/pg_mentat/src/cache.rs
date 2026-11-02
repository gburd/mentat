use crate::functions::store_management::{get_schema_for_store, quote_ident};
use parking_lot::RwLock;
use pgrx::pg_sys;
use pgrx::prelude::*;
use pgrx::spi::Spi;
use std::cell::Cell;
use std::collections::HashMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};

/// Run a single-column `SELECT` through *read-only* SPI and return the first
/// `i64`, or `None`.
///
/// Why this exists instead of `Spi::get_one`: `Spi::get_one` routes through
/// `Spi::connect_mut` -> `SpiClient::update` -> `Spi::mark_mutable` ->
/// `pg_sys::GetCurrentTransactionId()`, which *assigns* an XID and raises
/// "cannot assign TransactionIds during recovery" on a hot-standby. The cache
/// generation check runs on every query, so the entire read path was tainted.
/// `client.select` executes with `SPI_execute(..., read_only = true, ...)` and
/// never marks the transaction mutable, so it is standby-safe.
fn select_one_i64(sql: &str) -> Option<i64> {
    Spi::connect(|client| client.select(sql, Some(1), &[])?.first().get_one::<i64>())
        .ok()
        .flatten()
}

thread_local! {
    /// When `true`, `SchemaCache::ensure_fresh` skips the remote
    /// `cache_generation` round-trip for the duration of one
    /// `execute_transaction_inner` call.
    ///
    /// Rationale: within a single PostgreSQL transaction the schema cannot
    /// change from under us (MVCC snapshot). The only cross-backend change
    /// we need to detect is one that happened *before* this transaction
    /// started, and we detect that with the single upfront check performed
    /// by `begin_tx_cache_bypass`. Suppressing the repeated DB query saves
    /// ~17,870 round-trips per typical `mentat.t()` call.
    static TX_GEN_BYPASS: Cell<bool> = const { Cell::new(false) };

    /// Bumped by every change to a schema, table or role (syscache
    /// callbacks): a cached access check older than this is redone.
    static ACL_GEN: Cell<u32> = const { Cell::new(0) };
    /// Like ACL_GEN, but only roles. A kept plan's schema USAGE was checked
    /// when it was parsed; PostgreSQL re-parses kept plans after a schema
    /// change (a REVOKE USAGE), and checks table privileges each time a plan
    /// runs, but a role's membership can change under a kept plan.
    static ROLE_GEN: Cell<u32> = const { Cell::new(0) };
    static CALLBACKS: Cell<bool> = const { Cell::new(false) };
}

/// Register, once per backend, the callbacks the caches depend on: syscache
/// invalidations of schemas, tables and roles (#402).
fn register_callbacks() {
    if CALLBACKS.with(|c| c.replace(true)) {
        return;
    }
    #[pg_guard]
    unsafe extern "C-unwind" fn acl_changed(_arg: pg_sys::Datum, cacheid: i32, _hash: u32) {
        ACL_GEN.with(|g| g.set(g.get().wrapping_add(1)));
        if cacheid == pg_sys::SysCacheIdentifier::AUTHOID as i32
            || cacheid == pg_sys::SysCacheIdentifier::AUTHMEMROLEMEM as i32
        {
            ROLE_GEN.with(|g| g.set(g.get().wrapping_add(1)));
        }
    }
    // SAFETY: a plain C callback with no captured state, registered once per
    // backend; it only bumps thread-local counters.
    unsafe {
        for id in [
            pg_sys::SysCacheIdentifier::NAMESPACEOID,
            pg_sys::SysCacheIdentifier::RELOID,
            pg_sys::SysCacheIdentifier::AUTHOID,
            pg_sys::SysCacheIdentifier::AUTHMEMROLEMEM,
        ] {
            pg_sys::CacheRegisterSyscacheCallback(
                id as _,
                Some(acl_changed),
                pg_sys::Datum::from(0),
            );
        }
    }
}

/// The current user and the role generation, for the plan cache: a plan is
/// reused only by the user it was prepared for, and only while no role
/// changed since (#402).
pub(crate) fn plan_cache_scope() -> (u32, u32) {
    register_callbacks();
    // SAFETY: reads backend-local state.
    let user = unsafe { pg_sys::GetUserId() }.to_u32();
    (user, ROLE_GEN.with(Cell::get))
}

/// Perform **one** generation check for `store_name`, reload the cache if
/// stale, then arm the per-transaction bypass so subsequent `ensure_fresh`
/// calls within this transaction skip the DB round-trip.
///
/// Must be paired with `end_tx_cache_bypass()`. Use `TxContextGuard` in
/// `transact.rs` to ensure the pairing via RAII.
pub fn begin_tx_cache_bypass(store_name: &str) {
    get_cache_for_store(store_name).refresh_if_stale();
    TX_GEN_BYPASS.with(|f| f.set(true));
}

/// Clear the per-transaction generation bypass.
///
/// Called automatically by `TxContextGuard::drop` in `transact.rs`.
pub fn end_tx_cache_bypass() {
    TX_GEN_BYPASS.with(|f| f.set(false));
}

/// Attribute metadata cache entry
#[derive(Clone, Debug, PartialEq)]
pub struct AttributeInfo {
    pub value_type: String,
    pub cardinality: String,
    pub unique_constraint: Option<String>,
    pub fulltext: bool,
    pub indexed: bool,
    pub component: bool,
    pub no_history: bool,
}

/// Per-store schema and ident cache.
///
/// On first access the cache bulk-loads every row from `<schema>.schema` and
/// `<schema>.idents` in two queries, so subsequent attribute / ident lookups are
/// pure in-memory HashMap reads (O(1)).  After `invalidate()` the next access
/// triggers a fresh bulk load.
pub struct SchemaCache {
    /// The PostgreSQL schema name this cache loads from (e.g. "mentat", "mentat_my_store").
    db_schema: String,
    /// The same schema name, unquoted, for the access check.
    nsp_name: CString,
    /// (ACL_GEN << 32) | user of the last user whose access was checked (or
    /// who loaded the cache); 0 = none.
    access_ok: AtomicU64,
    /// The logical store name (e.g. "default", "my_store") for generation lookups.
    store_name: String,
    /// True once the initial bulk load has completed.
    warmed: AtomicBool,
    /// Last known cache generation (from mentat.cache_generation table).
    /// Used for cross-backend invalidation: if the remote gen exceeds this,
    /// we know another backend modified the schema and must reload.
    local_gen: AtomicI64,
    /// Map from attribute entid to attribute metadata
    attrs_by_id: RwLock<HashMap<i64, AttributeInfo>>,
    /// Map from ident string (e.g., ":person/name") to entid
    idents_to_entid: RwLock<HashMap<String, i64>>,
    /// Map from entid to ident string
    entids_to_ident: RwLock<HashMap<i64, String>>,
}

impl SchemaCache {
    pub fn new(db_schema: String, nsp_name: &str, store_name: String) -> Self {
        Self {
            db_schema,
            nsp_name: CString::new(nsp_name).unwrap_or_default(),
            access_ok: AtomicU64::new(0),
            store_name,
            warmed: AtomicBool::new(false),
            local_gen: AtomicI64::new(0),
            attrs_by_id: RwLock::new(HashMap::new()),
            idents_to_entid: RwLock::new(HashMap::new()),
            entids_to_ident: RwLock::new(HashMap::new()),
        }
    }

    /// The quoted PostgreSQL schema this cache loads from.
    pub fn db_schema(&self) -> &str {
        &self.db_schema
    }

    /// Bulk-load all schema attributes and idents from the database.
    ///
    /// Called automatically on the first cache miss.  Uses two queries to
    /// populate all three maps so that every subsequent lookup is O(1).
    fn warm(&self) {
        if self.warmed.load(Ordering::Acquire) {
            return;
        }

        let schema = &self.db_schema;
        let access_key = access_key();

        // Record the generation first: a schema change committed while this
        // loads then makes the cache look stale (reloaded), never fresh.
        let gen = select_one_i64(&format!(
            "SELECT gen FROM mentat.cache_generation WHERE store_name = '{}'",
            self.store_name
        ))
        .unwrap_or(1);

        // Load all attributes from <schema>.schema in a single query
        let _ = Spi::connect(|client| {
            let query = format!(
                "SELECT entid, ident, value_type::TEXT, cardinality::TEXT, \
                 unique_constraint::TEXT, fulltext, indexed, component, no_history \
                 FROM {}.schema",
                schema
            );
            let rows = client.select(&query, None, &[])?;

            let mut attrs = self.attrs_by_id.write();
            let mut idents = self.idents_to_entid.write();
            let mut entids = self.entids_to_ident.write();

            for row in rows {
                let entid: i64 = match row.get(1) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };
                let ident: String = match row.get(2) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };
                let value_type: String = match row.get(3) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };
                let cardinality: String = match row.get(4) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };
                let unique_constraint: Option<String> = row.get(5).ok().flatten();
                let fulltext: bool = row.get(6).ok().flatten().unwrap_or(false);
                let indexed: bool = row.get(7).ok().flatten().unwrap_or(false);
                let component: bool = row.get(8).ok().flatten().unwrap_or(false);
                let no_history: bool = row.get(9).ok().flatten().unwrap_or(false);

                attrs.insert(
                    entid,
                    AttributeInfo {
                        value_type,
                        cardinality,
                        unique_constraint,
                        fulltext,
                        indexed,
                        component,
                        no_history,
                    },
                );

                idents.insert(ident.clone(), entid);
                entids.insert(entid, ident);
            }

            Ok::<_, pgrx::spi::SpiError>(())
        });

        // Also load idents that are not in <schema>.schema (e.g., bootstrap
        // entries that only live in <schema>.idents).
        let _ = Spi::connect(|client| {
            let query = format!("SELECT ident, entid FROM {}.idents", schema);
            let rows = client.select(&query, None, &[])?;

            let mut idents = self.idents_to_entid.write();
            let mut entids = self.entids_to_ident.write();

            for row in rows {
                let ident: String = match row.get(1) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };
                let entid: i64 = match row.get(2) {
                    Ok(Some(v)) => v,
                    _ => continue,
                };

                // Only insert if not already present from the schema load
                idents.entry(ident.clone()).or_insert(entid);
                entids.entry(entid).or_insert(ident);
            }

            Ok::<_, pgrx::spi::SpiError>(())
        });

        self.local_gen.store(gen, Ordering::Release);

        // The two SELECTs above ran as this user: they checked its access.
        self.access_ok.store(access_key, Ordering::Release);
        self.warmed.store(true, Ordering::Release);
    }

    /// Before answering from the cache: the current user may read this
    /// store's schema (USAGE on its schema, SELECT on its schema and idents
    /// tables), as loading it would have required. Otherwise a role without
    /// those rights learned from another role's cache whether an attribute
    /// exists and its type (#402). Raises PostgreSQL's own permission error.
    /// Checked once per user until a schema, table or role changes.
    fn check_access(&self) {
        let key = access_key();
        if self.access_ok.load(Ordering::Acquire) == key {
            return;
        }
        // SAFETY: C strings outlive the calls; the checks raise an ERROR
        // (a Rust panic here) on a missing privilege, as a query would.
        unsafe {
            // Checks USAGE ("permission denied for schema ...").
            let nsp = pg_sys::LookupExplicitNamespace(self.nsp_name.as_ptr(), true);
            if nsp == pg_sys::InvalidOid {
                // The schema is gone: let the reload's SQL report it.
                self.invalidate();
                return;
            }
            // shortcut: needs table-level SELECT; column-level grants on all
            // the columns would also do for the load, upgrade if anyone uses them.
            for rel in [c"schema", c"idents"] {
                let relid = pg_sys::get_relname_relid(rel.as_ptr(), nsp);
                if relid != pg_sys::InvalidOid
                    && pg_sys::pg_class_aclcheck(
                        relid,
                        pg_sys::GetUserId(),
                        pg_sys::ACL_SELECT as _,
                    ) != pg_sys::AclResult::ACLCHECK_OK
                {
                    pg_sys::aclcheck_error(
                        pg_sys::AclResult::ACLCHECK_NO_PRIV,
                        pg_sys::ObjectType::OBJECT_TABLE,
                        rel.as_ptr(),
                    );
                }
            }
        }
        self.access_ok.store(key, Ordering::Release);
    }

    /// Reload the cache if another backend committed a schema change since it was
    /// loaded (the generation moved), even inside a transaction's bypass. Returns
    /// true if it (re)loaded.
    pub fn refresh_if_stale(&self) -> bool {
        if self.warmed.load(Ordering::Acquire) {
            let remote_gen = select_one_i64(&format!(
                "SELECT gen FROM mentat.cache_generation WHERE store_name = '{}'",
                self.store_name
            ))
            .unwrap_or(1);
            if remote_gen <= self.local_gen.load(Ordering::Acquire) {
                return false;
            }
            self.invalidate();
        }
        // `warm()` reads the generation internally.
        self.warm();
        true
    }

    /// Ensure the cache is fresh: warm it if never loaded, or reload if a
    /// remote backend has bumped the generation counter (cross-backend
    /// invalidation).
    fn ensure_fresh(&self) {
        if self.warmed.load(Ordering::Acquire) {
            self.check_access();
        }
        // If never warmed, warm unconditionally.
        if !self.warmed.load(Ordering::Acquire) {
            self.warm();
            return;
        }
        // Inside a transaction the generation was already validated once by
        // `begin_tx_cache_bypass`. Skip the round-trip for every subsequent
        // cache access within the same transaction (~17,870 saved per typical
        // `mentat.t()` call).
        if TX_GEN_BYPASS.with(|f| f.get()) {
            return;
        }
        // Check remote generation (cheap: 1-row table, always in shared_buffers).
        // If the remote gen exceeds our local gen, another backend modified the
        // schema and we must reload.
        let remote_gen = select_one_i64(&format!(
            "SELECT gen FROM mentat.cache_generation WHERE store_name = '{}'",
            self.store_name
        ))
        .unwrap_or(1);

        if remote_gen > self.local_gen.load(Ordering::Acquire) {
            self.invalidate();
            self.warm();
        }
    }

    /// Look up attribute metadata.
    ///
    /// On first call, bulk-loads all schema data.  Subsequent calls are pure
    /// HashMap lookups behind a read lock.
    pub fn get_attribute(&self, attr_id: i64) -> Option<AttributeInfo> {
        self.ensure_fresh();

        let cache = self.attrs_by_id.read();
        cache.get(&attr_id).cloned()
    }

    /// Look up entid by ident string.
    ///
    /// On first call, bulk-loads all schema data.  Subsequent calls are pure
    /// HashMap lookups behind a read lock.
    pub fn resolve_ident(&self, ident: &str) -> Option<i64> {
        self.ensure_fresh();

        let cache = self.idents_to_entid.read();
        cache.get(ident).copied()
    }

    /// Look up ident by entid.
    pub fn get_ident(&self, entid: i64) -> Option<String> {
        self.ensure_fresh();

        let cache = self.entids_to_ident.read();
        cache.get(&entid).cloned()
    }

    /// Look up attribute metadata by ident string.
    pub fn get_attribute_by_ident(&self, ident: &str) -> Option<AttributeInfo> {
        self.ensure_fresh();

        let ident_map = self.idents_to_entid.read();
        let entid = ident_map.get(ident).copied()?;
        drop(ident_map);

        let cache = self.attrs_by_id.read();
        cache.get(&entid).cloned()
    }

    /// Return true if the cache has been warmed (bulk-loaded).
    pub fn is_warmed(&self) -> bool {
        self.warmed.load(Ordering::Acquire)
    }

    /// Invalidate all caches (call after schema changes).
    ///
    /// The next access will trigger a fresh bulk load from the database.
    pub fn invalidate(&self) {
        let mut attrs = self.attrs_by_id.write();
        let mut idents = self.idents_to_entid.write();
        let mut entids = self.entids_to_ident.write();
        attrs.clear();
        idents.clear();
        entids.clear();
        self.warmed.store(false, Ordering::Release);
    }
}

/// Global store-aware schema cache map.
///
/// Maps store names (e.g. "default", "my_store") to their per-store SchemaCache.
/// Thread-safe via RwLock; each individual SchemaCache is also internally locked.
struct StoreCacheMap {
    caches: RwLock<HashMap<String, &'static SchemaCache>>,
}

impl StoreCacheMap {
    fn new() -> Self {
        Self {
            caches: RwLock::new(HashMap::new()),
        }
    }

    /// Get or create a SchemaCache for the given store name.
    fn get_or_create(&self, store_name: &str) -> &'static SchemaCache {
        // Fast path: read lock
        {
            let caches = self.caches.read();
            if let Some(cache) = caches.get(store_name) {
                return cache;
            }
        }

        // Slow path: write lock, create new cache
        let mut caches = self.caches.write();
        // Double-check after acquiring write lock
        if let Some(cache) = caches.get(store_name) {
            return cache;
        }

        let nsp_name = get_schema_for_store(store_name);
        let cache = Box::leak(Box::new(SchemaCache::new(
            quote_ident(&nsp_name),
            &nsp_name,
            store_name.to_string(),
        )));
        caches.insert(store_name.to_string(), cache);
        cache
    }

    /// Invalidate the cache for a specific store.
    fn invalidate_store(&self, store_name: &str) {
        let caches = self.caches.read();
        if let Some(cache) = caches.get(store_name) {
            cache.invalidate();
        }
    }

    /// Invalidate all store caches.
    fn invalidate_all(&self) {
        let caches = self.caches.read();
        for cache in caches.values() {
            cache.invalidate();
        }
    }
}

/// Global store cache map instance
static STORE_CACHES: std::sync::LazyLock<StoreCacheMap> =
    std::sync::LazyLock::new(StoreCacheMap::new);

/// Get the schema cache for the default store.
///
/// This is backwards-compatible with code that used the old single-cache API.
pub fn get_cache() -> &'static SchemaCache {
    get_cache_for_store("default")
}

/// Get the schema cache for a named store.
///
/// Creates the cache on first access; subsequent calls return the same instance.
/// The cache is lazily warmed on first attribute/ident lookup.
pub fn get_cache_for_store(store_name: &str) -> &'static SchemaCache {
    register_callbacks();
    STORE_CACHES.get_or_create(store_name)
}

/// Invalidate the schema cache for a specific store.
///
/// Call this after schema changes in a store so the cache is refreshed on next access.
pub fn invalidate_store_cache(store_name: &str) {
    STORE_CACHES.invalidate_store(store_name);
}

/// Invalidate all store schema caches.
///
/// Call this when a global event requires all caches to be refreshed.
#[expect(dead_code, reason = "Public API for future multi-store invalidation")]
pub fn invalidate_all_caches() {
    STORE_CACHES.invalidate_all();
}

/// The access-check memo key for the current user: (ACL_GEN << 32) | user.
/// A user oid is never 0, so 0 matches no key.
fn access_key() -> u64 {
    // SAFETY: reads backend-local state.
    let user = unsafe { pg_sys::GetUserId() }.to_u32();
    (u64::from(ACL_GEN.with(Cell::get)) << 32) | u64::from(user)
}

/// Lock the store's `cache_generation` row in the mode `bump_store_generation`'s
/// UPDATE takes, until the transaction ends. A schema-changing `edn_t` call takes
/// it before it installs an ident or takes a write lock, so two schema-changing
/// transactions queue here, not in a cycle through each other's idents (#413).
pub fn lock_store_generation(store_name: &str) -> Result<(), pgrx::spi::SpiError> {
    Spi::run_with_args(
        "SELECT 1 FROM mentat.cache_generation WHERE store_name = $1 FOR NO KEY UPDATE",
        &[pgrx::datum::DatumWithOid::from(store_name)],
    )
}

/// Bump the generation counter for a store in the shared `cache_generation` table.
///
/// Called after a transaction modifies schema-defining attributes (`:db/valueType`,
/// `:db/cardinality`, `:db/unique`, `:db/ident`, `:db.install/attribute`).
/// Other backends will detect this bump on their next cache access and reload.
pub fn bump_store_generation(store_name: &str) {
    let _ = Spi::run(&format!(
        "UPDATE mentat.cache_generation SET gen = gen + 1 WHERE store_name = '{}'",
        store_name
    ));
}
