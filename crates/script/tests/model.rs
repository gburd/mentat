//! The Datomic-model test suite for the shared `mentat.store/*` scripting
//! layer, run against an in-memory FAKE backend that implements
//! [`ScriptBackend`] with no storage engine at all — just a `Vec` of
//! `(tx, e, a, v)` triples in mino `Value` space.
//!
//! This proves the shared layer (arg parsing, db-value shape, prim dispatch,
//! tx-report/with wrapping, as-of/since, inst/uuid builders) without any DB
//! toolchain. The same behaviors are re-proven against the real SQLite and
//! pg_mentat backends in their own crates (`script_model.rs` / a `#[pg_test]`
//! module), so the two backends stop drifting: one suite, three backends.
//!
//! The fake is a toy triple store, not a Datalog engine. It understands only
//! the handful of query shapes these model tests use; that is enough to
//! exercise the shared wiring, which is the point.

use std::cell::RefCell;
use std::rc::Rc;

use mino_rs::collections::map::{PMap, PSet};
use mino_rs::collections::vector::PVec;
use mino_rs::printer::print_str;
use mino_rs::reader::read_one;
use mino_rs::{Gc, Value};

use mentat_script::{install, DbRef, ScriptBackend, TxReport};

// ---------------------------------------------------------------------------
// The fake backend: an in-memory (tx, e, a, v) triple store in mino Value space
// ---------------------------------------------------------------------------

/// The first synthetic tx id, mirroring Mentat's `TX0` so basis ids look real.
const TX0: i64 = 0x1000_0000;

#[derive(Default)]
struct FakeState {
    /// `(tx, e, a, v)` assertions, in insertion order. The value is stored as
    /// its printed EDN TEXT (not a mino `Value`) so the fake holds NO `Gc`
    /// roots across the interpreter's lifetime — a `Gc` in thread-local
    /// state would touch the GC TLS on thread teardown and abort the test.
    datoms: Vec<(i64, i64, String, String)>,
    next_e: i64,
    next_tx: i64,
}

struct Fake {
    st: RefCell<FakeState>,
}

impl Fake {
    fn new() -> Fake {
        Fake {
            st: RefCell::new(FakeState {
                datoms: Vec::new(),
                next_e: 1000,
                next_tx: TX0,
            }),
        }
    }

    /// Live `(e, a, v)` rows visible at `db`'s basis, with `v` re-read from its
    /// stored EDN text. as-of includes tx <= T; since includes tx > S; current
    /// includes all.
    fn rows(&self, db: &DbRef) -> Vec<(i64, String, Value)> {
        let st = self.st.borrow();
        st.datoms
            .iter()
            .filter(|(tx, _, _, _)| match (db.as_of, db.since) {
                (Some(t), _) => *tx <= t,
                (_, Some(s)) => *tx > s,
                _ => true,
            })
            .map(|(_, e, a, v)| (*e, a.clone(), read_one(v).map(|(x, _)| x).unwrap_or(Value::Nil)))
            .collect()
    }

    /// Apply tx-data (a vector of maps `{:attr v ...}`) as a new tx; return the
    /// report. Only the entity-map form the model tests use is supported.
    fn apply(&self, edn: &str, commit: bool) -> Result<(i64, TxReport), String> {
        let (form, _) = read_one(edn).map_err(|e| format!("bad tx-data: {e}"))?;
        let entities = match &form {
            Value::Vector(v) => v.iter().cloned().collect::<Vec<_>>(),
            _ => return Err("tx-data must be a vector".into()),
        };
        let mut st = self.st.borrow_mut();
        let tx = st.next_tx + 1;
        let base_e = st.next_e;
        let mut staged: Vec<(i64, i64, String, String)> = Vec::new();
        for (i, ent) in entities.iter().enumerate() {
            let m = match ent {
                Value::Map(m) => m,
                _ => return Err("tx entity must be a map".into()),
            };
            let e = base_e + 1 + i as i64;
            for (k, val) in m.entries() {
                let a = match k {
                    Value::Keyword(sym) => kw_text(sym),
                    _ => continue,
                };
                // Store the value as EDN text (no Gc root held in state).
                staged.push((tx, e, a, print_str(val)));
            }
        }
        if commit {
            for row in staged {
                st.datoms.push(row);
            }
            st.next_e = base_e + entities.len() as i64;
            st.next_tx = tx;
        }
        Ok((
            tx,
            TxReport {
                tx_id: tx,
                tempids: Vec::new(),
            },
        ))
    }
}

fn kw_text(sym: &mino_rs::symbol::Symbol) -> String {
    match sym.ns.as_deref() {
        Some(ns) => format!(":{ns}/{}", sym.name),
        None => format!(":{}", sym.name),
    }
}

impl ScriptBackend for Fake {
    fn open(&mut self, _path: Option<&str>) -> Result<i64, String> {
        Ok(1)
    }
    fn close(&mut self, _conn: i64) -> Result<(), String> {
        Ok(())
    }
    fn basis_tx(&self, _conn: i64) -> Result<i64, String> {
        Ok(self.st.borrow().next_tx)
    }
    fn transact(&mut self, _conn: i64, edn: &str) -> Result<TxReport, String> {
        Ok(self.apply(edn, true)?.1)
    }
    fn with(&mut self, _db: &DbRef, edn: &str) -> Result<(i64, TxReport), String> {
        // Speculative: compute the report at a new basis WITHOUT committing.
        self.apply(edn, false)
    }

    fn q(&self, db: &DbRef, query_edn: &str) -> Result<Value, String> {
        let rows = self.rows(db);
        // Parse the query enough to recognize the shapes the model tests use.
        let (form, _) = read_one(query_edn).map_err(|e| format!("bad query: {e}"))?;
        let items = match &form {
            Value::Vector(v) => v.iter().cloned().collect::<Vec<_>>(),
            _ => return Err("query must be a vector".into()),
        };
        // Split at :where.
        let where_pos = items
            .iter()
            .position(|x| matches!(x, Value::Keyword(s) if kw_text(s) == ":where"))
            .ok_or("query missing :where")?;
        let find = &items[1..where_pos];
        let clauses = &items[where_pos + 1..];
        // Single clause [?e :attr ?v] or [?e :attr "lit"] or [?e :attr _].
        let clause = clauses
            .iter()
            .find_map(|c| match c {
                Value::Vector(v) if v.len() == 3 => Some(v),
                _ => None,
            })
            .ok_or("unsupported query: need one 3-place clause")?;
        let attr = match clause.nth(1) {
            Some(Value::Keyword(s)) => kw_text(s),
            _ => return Err("clause attr must be a keyword".into()),
        };
        // Which var is bound to ?v, and any literal value filter.
        let v_lit = clause.nth(2).cloned();
        let matches: Vec<(i64, Value)> = rows
            .iter()
            .filter(|(_, a, _)| *a == attr)
            .filter(|(_, _, v)| match &v_lit {
                Some(Value::Sym(_)) => true, // ?v variable
                Some(lit) => print_str(v) == print_str(lit),
                None => true,
            })
            .map(|(e, _, v)| (*e, v.clone()))
            .collect();
        Ok(project_find(find, &matches))
    }

    fn pull(&self, db: &DbRef, eid: i64, pattern_edn: &str) -> Result<Value, String> {
        let rows = self.rows(db);
        let (pat, _) = read_one(pattern_edn).map_err(|e| format!("bad pattern: {e}"))?;
        let wants: Option<Vec<String>> = match &pat {
            Value::Vector(v) => {
                if v.iter()
                    .any(|x| matches!(x, Value::Sym(s) if &*s.name == "*"))
                {
                    None // wildcard
                } else {
                    Some(
                        v.iter()
                            .filter_map(|x| match x {
                                Value::Keyword(s) => Some(kw_text(s)),
                                _ => None,
                            })
                            .collect(),
                    )
                }
            }
            _ => return Err("pull pattern must be a vector".into()),
        };
        let mut m = PMap::empty();
        for (e, a, v) in &rows {
            if *e != eid {
                continue;
            }
            if let Some(ref keys) = wants {
                if !keys.contains(a) {
                    continue;
                }
            }
            m = m.assoc(kw_value(a), v.clone());
        }
        Ok(Value::Map(Gc::new(m)))
    }

    fn datoms(&self, db: &DbRef) -> Result<Value, String> {
        let rows = self.rows(db);
        let tuples: Vec<Value> = rows
            .into_iter()
            .map(|(e, a, v)| {
                Value::Vector(Gc::new(PVec::from_vec(vec![Value::Int(e), kw_value(&a), v])))
            })
            .collect();
        Ok(Value::Vector(Gc::new(PVec::from_vec(tuples))))
    }

    fn resolve_eid(&self, db: &DbRef, arg: &Value) -> Result<i64, String> {
        match arg {
            Value::Int(n) => Ok(*n),
            Value::Vector(v) if v.len() == 2 => {
                // [:attr val] lookup ref.
                let attr = match v.nth(0) {
                    Some(Value::Keyword(s)) => kw_text(s),
                    _ => return Err("lookup-ref attr must be a keyword".into()),
                };
                let val = v.nth(1).cloned().unwrap_or(Value::Nil);
                self.rows(db)
                    .iter()
                    .find(|(_, a, v)| *a == attr && print_str(v) == print_str(&val))
                    .map(|(e, _, _)| *e)
                    .ok_or_else(|| "lookup-ref resolved to no entity".into())
            }
            _ => Err("eid must be an integer or [:attr val] lookup-ref".into()),
        }
    }
}

/// Project a `[e v]` match set onto a find spec: `[?x .]` scalar, `[?x ...]`
/// collection, `[?x ?y]` relation.
fn project_find(find: &[Value], matches: &[(i64, Value)]) -> Value {
    let sym = |v: &Value| matches!(v, Value::Sym(_));
    // [:find ?v .] -> scalar.
    if find.len() == 2 && sym(&find[0]) && matches!(&find[1], Value::Sym(s) if &*s.name == ".") {
        return matches
            .first()
            .map(|(e, _)| Value::Int(*e))
            .unwrap_or(Value::Nil);
    }
    // [:find [?v ...]] -> collection vector.
    if let [Value::Vector(inner)] = find {
        if inner.len() == 2
            && matches!(inner.nth(1), Some(Value::Sym(s)) if &*s.name == "...")
        {
            let vals: Vec<Value> = matches.iter().map(|(_, v)| v.clone()).collect();
            return Value::Vector(Gc::new(PVec::from_vec(vals)));
        }
    }
    // [:find ?v] (single var) -> relation: set of 1-tuples of the value.
    let mut set = PSet::empty();
    for (_, v) in matches {
        set = set.conj(Value::Vector(Gc::new(PVec::from_vec(vec![v.clone()]))));
    }
    Value::Set(Gc::new(set))
}

fn kw_value(text: &str) -> Value {
    let rest = text.strip_prefix(':').unwrap_or(text);
    match rest.split_once('/') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => {
            Value::Keyword(mino_rs::symbol::Symbol::namespaced(ns, name))
        }
        _ => Value::Keyword(mino_rs::symbol::Symbol::plain(rest)),
    }
}

// ---------------------------------------------------------------------------
// Harness: build an interpreter over the fake, seeded with Alice.
// ---------------------------------------------------------------------------

fn seeded() -> mino_rs::Interpreter {
    let backend: Rc<RefCell<dyn ScriptBackend>> = Rc::new(RefCell::new(Fake::new()));
    let mut it = mino_rs::Interpreter::sandboxed();
    install(&mut it, backend);
    it.eval("(def c (mentat.store/open))").unwrap();
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :person/name :db/valueType :db.type/string \
             :db/cardinality :db.cardinality/one}])",
    )
    .expect("schema transact");
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Alice\"}])")
        .expect("data transact");
    it
}

fn eid_of(it: &mut mino_rs::Interpreter, name: &str) -> String {
    it.eval_to_string(&format!(
        "(mentat.store/q (mentat.store/db c) '[:find ?e . :where [?e :person/name \"{name}\"]])"
    ))
    .expect("eid query")
}

// ---------------------------------------------------------------------------
// The model tests (parameterized by the fake backend).
// ---------------------------------------------------------------------------

#[test]
fn db_is_an_immutable_value_not_the_conn() {
    let mut it = seeded();
    assert_eq!(it.eval_to_string("c").unwrap(), "1");
    let db = it.eval_to_string("(mentat.store/db c)").unwrap();
    assert!(db.contains(":mentat.store/db true"), "{db}");
    assert!(db.contains(":mentat.store/basis-tx"), "{db}");
    assert!(db.contains(":mentat.store/conn 1"), "{db}");
    assert_ne!(db, "1");
    let basis = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap();
    assert!(basis.parse::<i64>().unwrap() > TX0, "basis {basis}");
}

#[test]
fn q_takes_a_db_value() {
    let mut it = seeded();
    assert_eq!(
        it.eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])"
        )
        .unwrap(),
        "#{[\"Alice\"]}"
    );
    // A bare conn is accepted.
    assert_eq!(
        it.eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
            .unwrap(),
        "#{[\"Alice\"]}"
    );
}

#[test]
fn pull_returns_a_map() {
    let mut it = seeded();
    let eid = eid_of(&mut it, "Alice");
    assert_eq!(
        it.eval_to_string(&format!(
            "(mentat.store/pull (mentat.store/db c) {eid} [:person/name])"
        ))
        .unwrap(),
        "{:person/name \"Alice\"}"
    );
}

#[test]
fn entity_returns_an_entity_map() {
    let mut it = seeded();
    let eid = eid_of(&mut it, "Alice");
    let ent = it
        .eval_to_string(&format!("(mentat.store/entity (mentat.store/db c) {eid})"))
        .unwrap();
    assert!(ent.contains(&format!(":db/id {eid}")), "{ent}");
    assert!(ent.contains(":person/name \"Alice\""), "{ent}");
}

#[test]
fn read_returns_the_scalar() {
    let mut it = seeded();
    let eid = eid_of(&mut it, "Alice");
    assert_eq!(
        it.eval_to_string(&format!(
            "(mentat.store/read (mentat.store/db c) {eid} :person/name)"
        ))
        .unwrap(),
        "\"Alice\""
    );
}

#[test]
fn datoms_returns_tuples() {
    let mut it = seeded();
    let ds = it
        .eval_to_string("(mentat.store/datoms (mentat.store/db c))")
        .unwrap();
    assert!(ds.starts_with('['), "{ds}");
    assert!(ds.contains(":person/name \"Alice\""), "{ds}");
}

#[test]
fn with_is_speculative_and_does_not_commit() {
    let mut it = seeded();
    let before = it
        .eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
        .unwrap();
    assert_eq!(before, "#{[\"Alice\"]}");
    let with_result = it
        .eval_to_string("(mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])")
        .unwrap();
    assert!(with_result.contains(":mentat.store/db-after"), "{with_result}");
    assert!(
        with_result.contains(":mentat.store/tx-report"),
        "{with_result}"
    );
    let basis_now: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    let db_after_basis: i64 = it
        .eval_to_string(
            "(:mentat.store/basis-tx (:mentat.store/db-after \
               (mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])))",
        )
        .unwrap()
        .parse()
        .unwrap();
    assert!(db_after_basis > basis_now, "{db_after_basis} > {basis_now}");
    // The store is UNCHANGED: still only Alice.
    let after = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])",
        )
        .unwrap();
    assert_eq!(after, "#{[\"Alice\"]}", "with must not commit");
}

#[test]
fn as_of_and_since_reflect_the_basis() {
    let mut it = seeded();
    let basis_alice: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Bob\"}])")
        .unwrap();
    let basis_bob: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    assert!(basis_bob > basis_alice);

    let as_of_alice = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis_alice}))"
        ))
        .unwrap();
    assert!(as_of_alice.contains("Alice"), "{as_of_alice}");
    assert!(!as_of_alice.contains("Bob"), "{as_of_alice}");

    let as_of_bob = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis_bob}))"
        ))
        .unwrap();
    assert!(
        as_of_bob.contains("Alice") && as_of_bob.contains("Bob"),
        "{as_of_bob}"
    );

    let since_alice = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/since (mentat.store/db c) {basis_alice}))"
        ))
        .unwrap();
    assert!(since_alice.contains("Bob"), "{since_alice}");
    assert!(!since_alice.contains("Alice"), "{since_alice}");

    let as_of_db = it
        .eval_to_string(&format!(
            "(mentat.store/as-of (mentat.store/db c) {basis_alice})"
        ))
        .unwrap();
    assert!(
        as_of_db.contains(&format!(":mentat.store/as-of {basis_alice}")),
        "{as_of_db}"
    );
    let since_db = it
        .eval_to_string(&format!(
            "(mentat.store/since (mentat.store/db c) {basis_alice})"
        ))
        .unwrap();
    assert!(
        since_db.contains(&format!(":mentat.store/since {basis_alice}")),
        "{since_db}"
    );
}

#[test]
fn tx_report_has_the_datomic_shape() {
    let mut it = seeded();
    let report = it
        .eval_to_string("(mentat.store/transact c [{:person/name \"Carol\"}])")
        .unwrap();
    assert!(report.contains(":mentat.store/tx-id"), "{report}");
    assert!(report.contains(":mentat.store/tempids"), "{report}");
}

/// The inst/uuid VALUE BUILDERS emit values that round-trip through the mino
/// reader: `read_one(pr-str v) == v`. #uuid is a real `Value::Uuid`; #inst is
/// the `clojure.instant/read-instant-date` constructor form.
#[test]
fn inst_and_uuid_builders_round_trip_through_the_reader() {
    use mino_rs::printer::print_str;
    let uuid = mentat_script::values::uuid_value("12345678-1234-5678-1234-567812345678");
    assert_eq!(
        print_str(&uuid),
        "#uuid \"12345678-1234-5678-1234-567812345678\""
    );
    let inst = mentat_script::values::inst_value("2017-01-01T00:00:00Z");
    assert_eq!(
        print_str(&inst),
        "(clojure.instant/read-instant-date \"2017-01-01T00:00:00Z\")"
    );
    for v in [uuid, inst] {
        let (v2, _) = read_one(&print_str(&v)).unwrap();
        assert_eq!(print_str(&v), print_str(&v2));
    }
}
