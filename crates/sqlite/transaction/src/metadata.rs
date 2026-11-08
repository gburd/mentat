/// Connection metadata required to query from, or apply transactions to, a Mentat store.
///
/// Owned data for the volatile parts (generation and partition map), and `Arc` for the infrequently
/// changing parts (schema) that we want to share across threads.
///
/// See https://github.com/mozilla/mentat/wiki/Thoughts:-modeling-db-conn-in-Rust.
use std::sync::Arc;

use mentat_core::Schema;

use mentat_db::PartitionMap;

use mentat_db::cache::SQLiteAttributeCache;

pub struct Metadata {
    pub generation: u64,
    pub partition_map: PartitionMap,
    pub schema: Arc<Schema>,
    pub attribute_cache: SQLiteAttributeCache,
    /// The SQLite connection (its handle's address) and its
    /// `PRAGMA data_version` when this metadata was read or last committed.
    /// The version changes when ANOTHER connection commits to the file, so a
    /// different pair means the partition map and schema may be stale.
    pub data_version: Option<(usize, i64)>,
}

impl Metadata {
    // Intentionally not public.
    pub fn new(
        generation: u64,
        partition_map: PartitionMap,
        schema: Arc<Schema>,
        cache: SQLiteAttributeCache,
    ) -> Metadata {
        Metadata {
            generation,
            partition_map,
            schema,
            attribute_cache: cache,
            data_version: None,
        }
    }
}
