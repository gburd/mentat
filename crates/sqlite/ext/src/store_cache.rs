//! Per-thread cache of open `Store`s, keyed by canonical path.
//!
//! Shared verbatim by the SQLite and DuckDB extensions (the DuckDB crate
//! includes this file with `#[path]`; both are cdylibs, so neither can depend
//! on the other).
//!
//! Each host thread keeps its own stores, so calls never contend on a lock and
//! threads run in parallel on their own connections. A global `Mutex` pool
//! (stores checked out and returned) measured 1.4x slower at 8 and 24 DuckDB
//! threads (135K vs 193K, 236K vs 329K `edn_pull`/s; equal at 1 thread).
//! `MENTAT_STORE_CACHE` = stores kept per thread (default 16; 0 = open per
//! call); least recently used are dropped first. A store is removed from the
//! cache while a call uses it, so a re-entrant call opens its own.
//!
//! Staleness: a `Store` caches its schema and partition map. If another
//! connection (a thread with its own store, or another process) commits, a
//! cached store would query with an old schema and, worse, allocate entids
//! that are already taken. Every committed transaction advances the persisted
//! tx high-water mark (`known_parts.idx` for `:db.part/tx`), so a store is
//! current iff that mark equals its own `last_tx_id() + 1`: one primary-key
//! read of a three-row table. Reads check before running; writes check INSIDE
//! their `BEGIN IMMEDIATE` (nobody can commit between the check and ours). A
//! stale store is reopened. Freshly opened stores are checked too:
//! `Store::open` reads its metadata with several statements, and a commit
//! between them would otherwise go unnoticed. The cache key includes the
//! file's inode, so a store file deleted and recreated at the same path is
//! opened afresh. A store whose call fails is dropped, not cached.

use std::cell::RefCell;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use mentat::{Store, TxReport};

type Res<T> = Result<T, String>;

thread_local! {
    /// Most recently used last.
    static CACHE: RefCell<Vec<(Key, Store)>> = const { RefCell::new(Vec::new()) };
}

/// Canonical path + inode (0 off unix).
type Key = (PathBuf, u64);

fn key(path: &str) -> Option<Key> {
    let p = std::fs::canonicalize(Path::new(path)).ok()?;
    #[cfg(unix)]
    let ino = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&p).ok()?);
    #[cfg(not(unix))]
    let ino = 0;
    Some((p, ino))
}

/// Persisted next tx entid; maintained by every commit.
const NEXT_TX: &str = "SELECT idx FROM known_parts WHERE part = ':db.part/tx'";
/// Reopen attempts when the store keeps changing underneath us.
const RETRIES: usize = 8;

fn capacity() -> usize {
    static CAP: OnceLock<usize> = OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("MENTAT_STORE_CACHE")
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .unwrap_or(16)
    })
}

fn open(path: &str) -> Res<Store> {
    #[cfg(test)]
    tests::OPENS.with(|n| n.set(n.get() + 1));
    Store::open(path).map_err(|e| format!("opening store {path:?}: {e}"))
}

/// Take this thread's store for `path` (or open one), run `f`, and cache it
/// again if `f` succeeded.
/// Uncached when the cache is off or the path has no canonical form yet (an
/// in-memory store, a file about to be created, a `file:` URI).
fn with_store<T>(path: &str, f: impl FnOnce(&mut Store) -> Res<T>) -> Res<T> {
    let Some(key) = key(path).filter(|_| capacity() > 0) else {
        return f(&mut open(path)?);
    };
    let cached = CACHE.with_borrow_mut(|c| {
        c.iter()
            .rposition(|(k, _)| *k == key)
            .map(|i| c.remove(i).1)
    });
    let mut store = match cached {
        Some(s) => s,
        None => open(path)?,
    };
    let r = f(&mut store);
    if r.is_ok() {
        CACHE.with_borrow_mut(|c| {
            c.push((key, store));
            let over = c.len().saturating_sub(capacity());
            c.drain(..over);
        });
    }
    r
}

/// Run a read-only `f` against a current store for `path`. The check and `f`
/// share one read transaction, so they see the same snapshot.
pub fn read<T, E: Display>(path: &str, f: impl FnOnce(&Store) -> Result<T, E>) -> Res<T> {
    with_store(path, |store| {
        let sql = |s: &Store, q: &str| {
            s.sqlite_ref()
                .execute_batch(q)
                .map_err(|e| format!("{path:?}: {e}"))
        };
        for _ in 0..RETRIES {
            sql(store, "BEGIN")?;
            let next: i64 = store
                .sqlite_ref()
                .query_row(NEXT_TX, [], |r| r.get(0))
                .map_err(|e| format!("reading {path:?}: {e}"))?;
            if next == store.last_tx_id() + 1 {
                let r = f(store).map_err(|e| e.to_string());
                sql(store, "COMMIT")?;
                return r;
            }
            *store = open(path)?; // the old connection's drop ends its transaction
        }
        Err(format!(
            "store {path:?} kept changing while reopening; retry"
        ))
    })
}

/// Transact `edn` against `path`, with the staleness check inside the write lock.
pub fn transact(path: &str, edn: &str) -> Res<TxReport> {
    with_store(path, |store| {
        for _ in 0..RETRIES {
            let mut ip = store.begin_transaction().map_err(|e| e.to_string())?;
            let next: i64 = ip
                .transaction
                .query_row(NEXT_TX, [], |r| r.get(0))
                .map_err(|e| format!("reading {path:?}: {e}"))?;
            if next == ip.last_tx_id() + 1 {
                let report = ip.transact(edn).map_err(|e| e.to_string())?;
                ip.commit().map_err(|e| e.to_string())?;
                return Ok(report);
            }
            drop(ip); // rolls back
            *store = open(path)?;
        }
        Err(format!(
            "store {path:?} kept changing while reopening; retry"
        ))
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use mentat::{QueryResults, Queryable};
    use std::cell::Cell;

    thread_local! {
        /// `Store::open` calls made by this thread through the cache.
        pub(crate) static OPENS: Cell<usize> = const { Cell::new(0) };
    }

    fn opens() -> usize {
        OPENS.with(Cell::get)
    }

    struct TempStore(String);
    impl TempStore {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "mentat_store_cache_{tag}_{}.db",
                std::process::id()
            ));
            let s = TempStore(p.to_str().unwrap().to_owned());
            s.drop_files();
            Store::open(&s.0).unwrap(); // create it, so it has a canonical path
            s
        }
        fn drop_files(&self) {
            for ext in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{ext}", self.0));
            }
        }
    }
    impl Drop for TempStore {
        fn drop(&mut self) {
            self.drop_files();
        }
    }

    fn count(path: &str, q: &str) -> i64 {
        read(path, |s| s.q_once(q, None))
            .map(|o| match o.results {
                QueryResults::Scalar(Some(mentat::Binding::Scalar(mentat::TypedValue::Long(
                    n,
                )))) => n,
                QueryResults::Scalar(None) => 0,
                other => panic!("{other:?}"),
            })
            .unwrap()
    }

    const NAME: &str =
        "[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]";

    #[test]
    fn repeated_calls_reuse_the_store() {
        let t = TempStore::new("reuse");
        transact(&t.0, NAME).unwrap();
        let before = opens();
        for i in 0..20 {
            transact(&t.0, &format!(r#"[{{:p/name "n{i}"}}]"#)).unwrap();
            assert_eq!(
                count(&t.0, "[:find (count ?e) . :where [?e :p/name _]]"),
                i + 1
            );
        }
        assert_eq!(
            opens(),
            before,
            "every call after the first reused the cached store"
        );
        // A failing call drops its store; the next call reopens.
        assert!(transact(&t.0, "[[:db/add 1 :no/such 1]]").is_err());
        count(&t.0, "[:find (count ?e) . :where [?e :p/name _]]");
        assert_eq!(opens(), before + 1);
    }

    /// Two stores on one file: A (outside the cache) adds an attribute and an
    /// entity after B (cached) has read its metadata. B must see the new
    /// attribute and must not reuse A's entids. Without the staleness check B
    /// fails with an unknown attribute, and reuses A's tx and entity ids.
    #[test]
    fn another_connection_commits_between_calls() {
        let t = TempStore::new("stale");
        transact(&t.0, NAME).unwrap();
        count(&t.0, "[:find (count ?e) . :where [?e :p/name _]]"); // B cached

        let mut a = Store::open(&t.0).unwrap();
        a.transact(
            "[{:db/ident :p/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        )
        .unwrap();
        a.transact(r#"[{:p/name "Ann" :p/age 30}]"#).unwrap();

        // B reads with the new attribute...
        assert_eq!(count(&t.0, "[:find (count ?e) . :where [?e :p/age _]]"), 1);
        // ...and, stale again, writes with a fresh entity and a fresh tx.
        let ra = a.transact(r#"[{:db/id "x" :p/name "Ann2"}]"#).unwrap();
        let rb = transact(&t.0, r#"[{:db/id "y" :p/name "Bob" :p/age 40}]"#).unwrap();
        assert_ne!(rb.tempids["y"], ra.tempids["x"]);
        assert!(rb.tx_id > ra.tx_id, "{} vs {}", rb.tx_id, ra.tx_id);
        assert_eq!(count(&t.0, "[:find (count ?e) . :where [?e :p/name _]]"), 3);
        assert_eq!(count(&t.0, "[:find (sum ?a) . :where [_ :p/age ?a]]"), 70);
    }

    #[test]
    fn recreated_file_is_reopened() {
        let t = TempStore::new("recreate");
        transact(&t.0, NAME).unwrap();
        transact(&t.0, r#"[{:p/name "old"}]"#).unwrap();
        t.drop_files();
        transact(&t.0, NAME).unwrap(); // path absent: uncached open creates it
        assert_eq!(count(&t.0, "[:find (count ?e) . :where [?e :p/name _]]"), 0);
    }

    #[test]
    fn cache_is_bounded() {
        let stores: Vec<_> = (0..capacity() + 1)
            .map(|i| TempStore::new(&format!("lru{i}")))
            .collect();
        for s in &stores {
            count(&s.0, "[:find (count ?e) . :where [?e :db/ident _]]");
        }
        let before = opens();
        // The first one was evicted; the last one is still cached.
        count(
            &stores[capacity()].0,
            "[:find (count ?e) . :where [?e :db/ident _]]",
        );
        assert_eq!(opens(), before);
        count(&stores[0].0, "[:find (count ?e) . :where [?e :db/ident _]]");
        assert_eq!(opens(), before + 1);
    }
}
