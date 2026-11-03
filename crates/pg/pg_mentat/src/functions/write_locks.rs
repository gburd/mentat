//! Transaction-scoped advisory locks that keep the transactor's
//! read-check-write steps correct under READ COMMITTED (#376).
//!
//! `edn_t` runs at the caller's isolation level. Without locks, two sessions
//! replacing one cardinality-one value both read the old value, both retract
//! it and both assert their own: two live values. The same goes for two
//! `:db/cas` calls from one old value, two first assertions of one (e, a),
//! two new entities claiming one `:db/unique` value, and a `many -> one` (or
//! add-`:db/unique`) alteration checked while another session writes the
//! attribute. A writer takes the locks below before it reads; the second
//! writer waits for the first to commit, and under READ COMMITTED its next
//! statement sees the first's data, so it replaces the value instead of
//! adding a second one.
//!
//! # Key scheme
//!
//! Every lock is a two-int4 advisory lock (what `pg_advisory_xact_lock(int4,
//! int4)` takes), acquired with `LockAcquire` directly: one key costs a hash
//! and a lock-table insert, no SQL. In `pg_locks` they show as
//! `locktype = 'advisory'`, `objsubid = 2`, `classid` = the first key,
//! `objid` = the second.
//!
//! * `classid` is a namespace: [`NS_STORE`], [`NS_ATTR`], [`NS_UNIQUE`] or
//!   [`NS_EA`] (ASCII "Mnt0".."Mnt3", 0x4D6E7430..), so other users of
//!   two-int advisory locks collide with pg_mentat only if they pick the same
//!   32-bit tags. The one-bigint form (`objsubid = 1`) never collides with
//!   these.
//! * `objid` is a 32-bit FNV-1a hash of the locked tuple: (store_id) for
//!   [`NS_STORE`], (store_id, a) for [`NS_ATTR`], (store_id, a, value type
//!   tag, value) for [`NS_UNIQUE`], and (store_id, e, a) for [`NS_EA`]. Two
//!   tuples can share a hash; the only cost is that two unrelated writers
//!   wait for each other.
//!
//! What a transaction takes, before it reads:
//!
//! * [`NS_STORE`] shared, once per `edn_t` call, before it resolves any ident
//!   or reads the schema cache. Writers never conflict on it.
//!   `mentat.repair_moved_idents` takes it exclusive (computing the same key
//!   in plpgsql), so a writer waits before it resolves an ident the repair
//!   is pointing elsewhere, and then reloads the cache the repair made stale.
//! * [`NS_ATTR`] shared, for every attribute it writes; exclusive, for an
//!   attribute it alters from many to one or makes `:db/unique` (so that
//!   alteration's check of the existing data runs with no writer of the
//!   attribute in flight and none starting).
//! * [`NS_UNIQUE`] exclusive, for every `:db/unique` value it asserts, before
//!   the upsert lookup.
//! * [`NS_EA`] exclusive, for every cardinality-one (e, a) it asserts or
//!   retracts and every (e, a) a `:db/cas` reads, after upsert resolution has
//!   fixed the entities.
//!
//! Per-value keys are bounded, since each takes a slot in the shared lock
//! table (sized `max_locks_per_transaction` per connection): once a top-level
//! transaction would hold more than `max_locks_per_transaction` per-value
//! keys (64 by default), an `edn_t` call takes [`NS_ATTR`] exclusive for each
//! attribute that needs per-value keys instead (see [`within_budget`]). Such
//! an attribute then has one writer at a time until the transaction ends.
//! Raising `max_locks_per_transaction` raises the bound.
//!
//! # Deadlocks
//!
//! An `edn_t` call that changes schema first locks its store's row in
//! `mentat.cache_generation` (which it bumps when it ends; see
//! `cache::lock_store_generation`), held until the transaction ends. Every
//! call first takes its store's [`NS_STORE`] key shared, before that row: the
//! repair takes the key exclusive before it locks the row, so a writer never
//! holds the row while waiting for the key. `mentat_excise`, `drop_store` and
//! `rename_store` lock the row too, so they take the key first as well. Then a
//! call takes its keys in three steps, each in ascending (namespace, hash)
//! order: an alteration's [`NS_ATTR`] keys; then the written attributes'
//! [`NS_ATTR`] keys and the [`NS_UNIQUE`] keys; then the [`NS_EA`] keys.
//! Writers that alter no schema and stay within the budget can't deadlock on
//! these advisory locks: the only exclusive keys they hold while waiting sort
//! below the key they wait for. That claim covers the advisory locks only,
//! not PostgreSQL's row and index locks, though the transactor writes each
//! projection table's rows in key order so writers of the same
//! cardinality-many values don't deadlock on them (#411). Transactions whose
//! first schema-changing call comes before any data call ("install schema,
//! then write") can't deadlock with each other through schema: the second
//! waits for the store's row before it has installed or locked anything
//! (#413). A schema alteration, a call over the budget, a transaction that
//! writes data and then changes schema, or one that runs several `edn_t`
//! calls (or takes its own locks) can deadlock with another; PostgreSQL
//! detects it and aborts one transaction (SQLSTATE 40P01), and the client
//! retries.
//!
//! # Isolation levels
//!
//! Under READ COMMITTED each statement takes a new snapshot, so a writer that
//! waited reads the committed result. Under SERIALIZABLE PostgreSQL's own
//! checks already abort one of two conflicting writers; the locks change
//! which one, and when. Under REPEATABLE READ a transaction keeps its first
//! snapshot: a replacement or `:db/cas` that waited fails with "could not
//! serialize access" (it deletes a row another transaction deleted), but two
//! first assertions of one (e, a), two claims of one unique value, or a
//! write racing a `many -> one` alteration can still both commit.

use pgrx::pg_sys;
use std::cell::Cell;

/// Namespace of the per-store key: writers shared, repair_moved_idents exclusive.
pub const NS_STORE: u32 = u32::from_be_bytes(*b"Mnt0");
/// Namespace of the per-(store_id, a) key: one attribute and its schema.
pub const NS_ATTR: u32 = u32::from_be_bytes(*b"Mnt1");
/// Namespace of the per-(store_id, a, v) key: one `:db/unique` value.
pub const NS_UNIQUE: u32 = u32::from_be_bytes(*b"Mnt2");
/// Namespace of the per-(store_id, e, a) key: one cardinality-one value.
pub const NS_EA: u32 = u32::from_be_bytes(*b"Mnt3");

thread_local! {
    /// (top-level FullTransactionId, per-value keys it has taken).
    static TAKEN: Cell<(u64, usize)> = const { Cell::new((0, 0)) };
}

/// The current top-level transaction's id (0 if it has none yet).
fn top_xid() -> u64 {
    // SAFETY: reads backend-local transaction state.
    unsafe { pg_sys::GetTopFullTransactionIdIfAny().value }
}

/// Can this transaction take `n` more per-value keys within its budget of
/// `max_locks_per_transaction`? (See the module doc.)
pub fn within_budget(n: usize) -> bool {
    let xid = top_xid();
    let taken = TAKEN.with(|t| t.get());
    let taken = if taken.0 == xid { taken.1 } else { 0 };
    // SAFETY: a GUC variable, read in the backend's own thread.
    let budget = usize::try_from(unsafe { pg_sys::max_locks_per_xact }).unwrap_or(0);
    taken + n <= budget
}

/// Record that this transaction took `n` more per-value keys.
fn charge(n: usize) {
    let xid = top_xid();
    TAKEN.with(|t| {
        let (x, taken) = t.get();
        t.set((xid, if x == xid { taken + n } else { n }));
    });
}

/// 32-bit FNV-1a, fixed across builds and platforms (unlike `DefaultHasher`).
pub fn fnv1a(parts: &[&[u8]]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for part in parts {
        for &b in *part {
            h ^= u32::from(b);
            h = h.wrapping_mul(0x0100_0193);
        }
    }
    h
}

/// The key of one (store_id, e, a).
pub fn ea_key(store_id: i64, e: i64, a: i64) -> (u32, u32) {
    let h = fnv1a(&[&store_id.to_le_bytes(), &e.to_le_bytes(), &a.to_le_bytes()]);
    (NS_EA, h)
}

/// The key of one store (sql/32_moved_idents.sql computes the same).
pub fn store_key(store_id: i64) -> (u32, u32) {
    (NS_STORE, fnv1a(&[&store_id.to_le_bytes()]))
}

/// Take the store's key shared, until the transaction ends. Not a per-value
/// key: it costs no budget.
pub fn lock_store(store_id: i64) {
    let (ns, h) = store_key(store_id);
    acquire(ns, h, false);
}

/// The key of one attribute's schema.
pub fn attr_key(store_id: i64, a: i64) -> (u32, u32) {
    (NS_ATTR, fnv1a(&[&store_id.to_le_bytes(), &a.to_le_bytes()]))
}

/// The key of one unique value: `tag` is the value-type tag, `bytes` the value.
pub fn unique_key(store_id: i64, a: i64, tag: i16, bytes: &[u8]) -> (u32, u32) {
    let h = fnv1a(&[
        &store_id.to_le_bytes(),
        &a.to_le_bytes(),
        &tag.to_le_bytes(),
        bytes,
    ]);
    (NS_UNIQUE, h)
}

/// Take the transaction-scoped advisory locks `exclusive` and `shared`, in
/// sorted order, waiting for each. A key in both is taken exclusive only.
/// Released at the end of the (top-level) transaction. Per-value keys count
/// against the transaction's budget.
pub fn lock_all(mut exclusive: Vec<(u32, u32)>, mut shared: Vec<(u32, u32)>) {
    exclusive.sort_unstable();
    exclusive.dedup();
    shared.sort_unstable();
    shared.dedup();
    shared.retain(|k| exclusive.binary_search(k).is_err());
    let mut all: Vec<((u32, u32), bool)> = exclusive
        .into_iter()
        .map(|k| (k, true))
        .chain(shared.into_iter().map(|k| (k, false)))
        .collect();
    all.sort_unstable();
    charge(all.iter().filter(|((ns, _), _)| *ns != NS_ATTR).count());
    for ((ns, h), excl) in all {
        acquire(ns, h, excl);
    }
}

/// `pg_advisory_xact_lock(ns, h)` (or `_shared`), without the SQL round trip:
/// the LOCKTAG `SET_LOCKTAG_INT32` builds for the two-int4 form.
fn acquire(ns: u32, h: u32, exclusive: bool) {
    let tag = pg_sys::LOCKTAG {
        // SAFETY: set in any backend that runs SQL.
        locktag_field1: unsafe { pg_sys::MyDatabaseId }.to_u32(),
        locktag_field2: ns,
        locktag_field3: h,
        locktag_field4: 2,
        locktag_type: pg_sys::LockTagType::LOCKTAG_ADVISORY as u8,
        locktag_lockmethodid: pg_sys::USER_LOCKMETHOD as u8,
    };
    let mode = if exclusive {
        pg_sys::ExclusiveLock
    } else {
        pg_sys::ShareLock
    };
    // SAFETY: pg_guard'ed by pgrx: an ERROR (out of shared lock memory, a
    // deadlock, a cancel while waiting) unwinds into the caller's ERROR.
    unsafe { pg_sys::LockAcquire(&tag, mode as pg_sys::LOCKMODE, false, false) };
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn namespaces_are_distinct_ascii_tags() {
        assert_eq!(NS_STORE, 0x4D6E_7430);
        assert_eq!(NS_ATTR, NS_STORE + 1);
        assert_eq!(NS_ATTR, 0x4D6E_7431);
        assert_eq!(NS_UNIQUE, NS_ATTR + 1);
        assert_eq!(NS_EA, NS_ATTR + 2);
    }

    #[test]
    fn fnv1a_matches_reference_vectors() {
        assert_eq!(fnv1a(&[b""]), 0x811c_9dc5);
        assert_eq!(fnv1a(&[b"a"]), 0xe40c_292c);
        assert_eq!(fnv1a(&[b"foobar"]), 0xbf9c_f968);
        assert_eq!(fnv1a(&[b"foo", b"bar"]), fnv1a(&[b"foobar"]));
    }

    #[test]
    fn keys_separate_their_inputs() {
        assert_ne!(ea_key(1, 2, 3), ea_key(1, 3, 2));
        assert_ne!(ea_key(1, 2, 3), ea_key(2, 2, 3));
        assert_ne!(attr_key(1, 7), attr_key(2, 7));
        assert_ne!(store_key(0), store_key(1));
        // FNV-1a of the eight zero bytes of store_id 0 (the default store).
        assert_eq!(store_key(0), (NS_STORE, 0x9be1_7165));
        assert_ne!(unique_key(1, 7, 7, b"x"), unique_key(1, 7, 8, b"x"));
        assert_eq!(ea_key(1, 2, 3).0, NS_EA);
    }
}
