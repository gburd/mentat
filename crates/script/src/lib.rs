//! Backend-independent mino `mentat.store/*` scripting glue (plan § 1.19).
//!
//! Both Mentat backends — the embedded SQLite [`Store`] and the pg_mentat
//! engine — expose the same Datomic-a-like `mentat.store/*` language surface
//! (14 prims). This crate holds everything that surface needs that does **not**
//! depend on the storage engine:
//!
//!   * the db-value map shape and its destructuring ([`values::DbRef`],
//!     [`values::db_value`], [`values::destructure_db`]);
//!   * the tx-report projection and the tagged inst/uuid/keyword value builders
//!     ([`values`]);
//!   * argument parsing (conn handle, eid, keyword, tx id);
//!   * [`install`], which registers all 14 `mentat.store/*` prims onto a mino
//!     interpreter, driving each through a [`ScriptBackend`].
//!
//! A backend implements [`ScriptBackend`] (storage + its own result
//! conversion: `Binding→Value` for Mentat, `JSON→Value` for pg_mentat) and
//! calls [`install`]. The Datomic-model test suite (`tests/model.rs`) runs
//! against a fake in-memory backend here, and against both real backends in
//! their own crates — one suite, proven three ways.

pub mod model_tests;
pub mod prims;
pub mod values;

pub use prims::install;
pub use values::{db_value, destructure_db, inst_value, kw_ns, str_val, tx_report_value, DbRef, TxReport};

use mino_rs::Value;

/// The storage seam between the shared `mentat.store/*` surface and a concrete
/// engine. A backend owns storage and *its own* result conversion (each read
/// method returns an already-built mino [`Value`]); the shared [`install`]
/// owns argument parsing, the db-value shape, and result wrapping.
///
/// `entity`/`read`/`entities` have default implementations on top of `q`/`pull`
/// so a minimal backend (the fake, pg) needs only the core methods; the SQLite
/// backend overrides them because its as-of/since reads reconstruct the basis
/// from the transaction log rather than running Datalog.
pub trait ScriptBackend {
    /// `(open)` / `(open "path")` → a conn handle. In-memory when `path` is
    /// `None`/empty.
    fn open(&mut self, path: Option<&str>) -> Result<i64, String>;

    /// `(close conn)`. Drop/forget the store, if the backend owns one.
    fn close(&mut self, conn: i64) -> Result<(), String>;

    /// The current basis (max committed tx) for a conn.
    fn basis_tx(&self, conn: i64) -> Result<i64, String>;

    /// Commit `edn` against `conn`; return the report.
    fn transact(&mut self, conn: i64, edn: &str) -> Result<TxReport, String>;

    /// Speculatively transact `edn` against `db` WITHOUT committing; return the
    /// resulting basis tx and the report.
    fn with(&mut self, db: &DbRef, edn: &str) -> Result<(i64, TxReport), String>;

    /// Run Datalog `query_edn` against `db`; return the result already
    /// converted to a mino [`Value`] in Datomic result shape.
    fn q(&self, db: &DbRef, query_edn: &str) -> Result<Value, String>;

    /// Pull `pattern_edn` for a single `eid` against `db` → a mino map.
    fn pull(&self, db: &DbRef, eid: i64, pattern_edn: &str) -> Result<Value, String>;

    /// `(datoms db)` → a vector of `[e a v …]` tuple vectors for the basis.
    fn datoms(&self, db: &DbRef) -> Result<Value, String>;

    /// Resolve an eid argument (integer, ident keyword, or `[:attr val]`
    /// lookup-ref) to a numeric entid. Backend-specific because it consults the
    /// schema / a lookup query.
    fn resolve_eid(&self, db: &DbRef, arg: &Value) -> Result<i64, String>;

    /// `(entity db eid)` → `{:db/id eid :attr v …}`. Default: pull `[*]`,
    /// then splice in `:db/id`. A backend whose historical reads reconstruct
    /// the basis overrides this.
    fn entity(&self, db: &DbRef, eid: i64) -> Result<Value, String> {
        use mino_rs::collections::map::PMap;
        use mino_rs::{Gc, Value as V};
        let pulled = self.pull(db, eid, "[*]")?;
        let mut m = PMap::empty().assoc(values::kw_ns("db", "id"), V::Int(eid));
        if let V::Map(ref pm) = pulled {
            for (k, v) in pm.entries() {
                m = m.assoc(k.clone(), v.clone());
            }
        }
        Ok(V::Map(Gc::new(m)))
    }

    /// `(read db eid attr)` → the value(s) for that eid/attr: cardinality-one
    /// collapses to the scalar (nil if absent), many stays a set. Default: a
    /// collection query over `attr`.
    fn read(&self, db: &DbRef, eid: i64, attr: &str) -> Result<Value, String> {
        use mino_rs::collections::map::PSet;
        use mino_rs::{Gc, Value as V};
        let query = format!("[:find [?v ...] :where [{eid} {attr} ?v]]");
        // The collection query yields a vector of the attr's values.
        match self.q(db, &query)? {
            V::Vector(ref v) => match v.len() {
                0 => Ok(V::Nil),
                1 => Ok(v.nth(0).cloned().unwrap_or(V::Nil)),
                _ => {
                    let mut set = PSet::empty();
                    for x in v.iter() {
                        set = set.conj(x.clone());
                    }
                    Ok(V::Set(Gc::new(set)))
                }
            },
            V::Nil => Ok(V::Nil),
            other => Ok(other),
        }
    }

    /// `(entities db attr)` → a set of eids having `attr`. Default: a
    /// collection query.
    fn entities(&self, db: &DbRef, attr: &str) -> Result<Value, String> {
        use mino_rs::collections::map::PSet;
        use mino_rs::{Gc, Value as V};
        let query = format!("[:find [?e ...] :where [?e {attr} _]]");
        let mut set = PSet::empty();
        if let V::Vector(ref v) = self.q(db, &query)? {
            for e in v.iter() {
                set = set.conj(e.clone());
            }
        }
        Ok(V::Set(Gc::new(set)))
    }
}
