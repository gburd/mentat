//! Backend-independent mino `Value` builders and the `DbRef` shape shared by
//! both Mentat scripting backends (SQLite `Store` and pg_mentat's engine).
//!
//! Everything here is pure value construction/destructuring — no storage. The
//! db-value map shape, the tx-report projection, and the tagged inst/uuid
//! builders live here so the two backends stop drifting (§ 1.19).

use mino_rs::collections::map::PMap;
use mino_rs::error::throw_str;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Throw, Value};

/// A destructured db value: which conn, its basis tx, and any temporal bound.
///
/// `conn` is the opaque handle from `mentat.store/open`. The SQLite backend
/// keys its store table by it; pg_mentat has exactly one database and uses `1`.
#[derive(Clone, Copy, Debug)]
pub struct DbRef {
    pub conn: i64,
    pub basis_tx: i64,
    pub as_of: Option<i64>,
    pub since: Option<i64>,
}

/// A backend-independent transaction report: the tx id and tempid resolutions.
///
/// Each backend converts its native report (mentat `TxReport`, pg engine JSON)
/// into this shape; the shared layer projects it to the `:mentat.store/*` map.
#[derive(Clone, Debug, Default)]
pub struct TxReport {
    pub tx_id: i64,
    pub tempids: Vec<(String, i64)>,
}

/// A namespaced keyword `Value`, e.g. `:mentat.store/conn`.
pub fn kw_ns(ns: &str, name: &str) -> Value {
    Value::Keyword(Symbol::namespaced(ns, name))
}

/// A string `Value`.
pub fn str_val(s: &str) -> Value {
    Value::Str(Gc::new(s.to_string()))
}

/// Build a db-value mino map:
/// `{:mentat.store/db true :mentat.store/conn N :mentat.store/basis-tx T
///   :mentat.store/as-of A :mentat.store/since S}`.
pub fn db_value(conn: i64, basis_tx: i64, as_of: Option<i64>, since: Option<i64>) -> Value {
    let opt = |o: Option<i64>| o.map(Value::Int).unwrap_or(Value::Nil);
    let m = PMap::empty()
        .assoc(kw_ns("mentat.store", "db"), Value::Bool(true))
        .assoc(kw_ns("mentat.store", "conn"), Value::Int(conn))
        .assoc(kw_ns("mentat.store", "basis-tx"), Value::Int(basis_tx))
        .assoc(kw_ns("mentat.store", "as-of"), opt(as_of))
        .assoc(kw_ns("mentat.store", "since"), opt(since));
    Value::Map(Gc::new(m))
}

/// Destructure a db value, accepting a bare conn `Int` (treated as the current
/// db: `basis_tx` left 0, as-of/since nil — backends re-read the live basis
/// when they need it). A db map carries its conn, basis, and temporal bound.
pub fn destructure_db(prim: &str, arg: Option<&Value>) -> Result<DbRef, Throw> {
    match arg {
        Some(Value::Int(n)) => Ok(DbRef {
            conn: *n,
            basis_tx: 0,
            as_of: None,
            since: None,
        }),
        Some(Value::Map(m)) => {
            let conn = match m.get(&kw_ns("mentat.store", "conn")) {
                Some(Value::Int(n)) => *n,
                _ => {
                    return Err(throw_str(&format!(
                        "{prim}: db value missing :mentat.store/conn"
                    )))
                }
            };
            let basis_tx = match m.get(&kw_ns("mentat.store", "basis-tx")) {
                Some(Value::Int(n)) => *n,
                _ => 0,
            };
            let opt = |k: &str| match m.get(&kw_ns("mentat.store", k)) {
                Some(Value::Int(n)) => Some(*n),
                _ => None,
            };
            Ok(DbRef {
                conn,
                basis_tx,
                as_of: opt("as-of"),
                since: opt("since"),
            })
        }
        _ => Err(throw_str(&format!(
            "{prim}: first arg must be a db value (from mentat.store/db) or a conn handle"
        ))),
    }
}

/// A [`TxReport`] as a mino map
/// `{:mentat.store/tx-id N :mentat.store/tempids {...}}`, optionally carrying a
/// `:mentat.store/db-after`.
pub fn tx_report_value(report: &TxReport, db_after: Option<Value>) -> Value {
    let mut tempids = PMap::empty();
    for (k, v) in &report.tempids {
        tempids = tempids.assoc(str_val(k), Value::Int(*v));
    }
    let mut m = PMap::empty()
        .assoc(kw_ns("mentat.store", "tx-id"), Value::Int(report.tx_id))
        .assoc(
            kw_ns("mentat.store", "tempids"),
            Value::Map(Gc::new(tempids)),
        );
    if let Some(db) = db_after {
        m = m.assoc(kw_ns("mentat.store", "db-after"), db);
    }
    Value::Map(Gc::new(m))
}

/// The instant reader form `(clojure.instant/read-instant-date "RFC3339")` —
/// the exact value `read_one("#inst \"RFC3339\"")` yields, so it round-trips
/// through the reader. (mino has no distinct `Value::Instant`; `#inst` expands
/// to this constructor call at read time — see `crates/mino/src/reader.rs`.)
pub fn inst_value(rfc3339: &str) -> Value {
    cons_call(
        Symbol::namespaced("clojure.instant", "read-instant-date"),
        str_val(rfc3339),
    )
}

/// A real UUID `Value` — the same value `read_one("#uuid \"…\"")` yields since
/// the mino refresh (Task 8), so it prints back as `#uuid "…"` and round-trips.
/// An unparseable string falls back to the `parse-uuid` reader form so callers
/// still get a value that re-reads to the intended UUID.
pub fn uuid_value(uuid: &str) -> Value {
    match parse_uuid_bytes(uuid) {
        Some(bytes) => Value::Uuid(Gc::new(mino_rs::value::UuidVal(bytes))),
        None => cons_call(Symbol::plain("parse-uuid"), str_val(uuid)),
    }
}

/// Parse a canonical `8-4-4-4-12` hyphenated UUID string to its 16 bytes.
/// Rejects non-canonical forms (the mino `#uuid` reader is equally strict).
fn parse_uuid_bytes(s: &str) -> Option<[u8; 16]> {
    if s.len() != 36 || s.as_bytes().iter().filter(|&&b| b == b'-').count() != 4 {
        return None;
    }
    let hex: String = s.chars().filter(|&c| c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

/// Build `(sym arg)` as a proper one-arg cons list.
fn cons_call(sym: Symbol, arg: Value) -> Value {
    Value::Cons(Gc::new((
        Value::Sym(sym),
        Value::Cons(Gc::new((arg, Value::EmptyList))),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mino_rs::printer::print_str;
    use mino_rs::reader::read_one;

    #[test]
    fn uuid_builder_round_trips_through_the_reader() {
        // The builder emits the same value the #uuid literal reads to.
        let lit = "12345678-1234-5678-1234-567812345678";
        let built = uuid_value(lit);
        assert_eq!(print_str(&built), format!("#uuid \"{lit}\""));
        let (from_lit, _) = read_one(&format!("#uuid \"{lit}\"")).unwrap();
        assert_eq!(print_str(&from_lit), print_str(&built));
    }

    #[test]
    fn inst_builder_matches_the_reader_form() {
        let built = inst_value("2017-01-01T00:00:00Z");
        assert_eq!(
            print_str(&built),
            "(clojure.instant/read-instant-date \"2017-01-01T00:00:00Z\")"
        );
        let (from_lit, _) = read_one(r#"#inst "2017-01-01T00:00:00Z""#).unwrap();
        assert_eq!(print_str(&from_lit), print_str(&built));
    }

    #[test]
    fn db_value_round_trips_through_destructure() {
        let v = db_value(1, 42, Some(7), None);
        let d = destructure_db("t", Some(&v)).unwrap();
        assert_eq!((d.conn, d.basis_tx, d.as_of, d.since), (1, 42, Some(7), None));
        // A bare conn int is the current db.
        let d2 = destructure_db("t", Some(&Value::Int(3))).unwrap();
        assert_eq!((d2.conn, d2.as_of, d2.since), (3, None, None));
    }

    #[test]
    fn tx_report_projects_tx_id_and_tempids() {
        let r = TxReport {
            tx_id: 268435458,
            tempids: vec![("t".to_string(), 268435460)],
        };
        let s = print_str(&tx_report_value(&r, None));
        assert!(s.contains(":mentat.store/tx-id 268435458"), "{s}");
        assert!(s.contains("\"t\" 268435460"), "{s}");
    }

    #[test]
    fn bad_uuid_falls_back_to_the_reader_call_form() {
        let s = print_str(&uuid_value("not-a-uuid"));
        assert_eq!(s, "(parse-uuid \"not-a-uuid\")");
    }
}
