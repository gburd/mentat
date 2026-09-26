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
// Run the shared Datomic-model suite against the fake backend.
// ---------------------------------------------------------------------------

/// A store-installed interpreter over a fresh fake backend.
fn faked() -> mino_rs::Interpreter {
    let backend: Rc<RefCell<dyn ScriptBackend>> = Rc::new(RefCell::new(Fake::new()));
    let mut it = mino_rs::Interpreter::sandboxed();
    install(&mut it, backend);
    it
}

use mentat_script::model_tests as m;

#[test]
fn db_is_an_immutable_value_not_the_conn() {
    m::db_is_an_immutable_value_not_the_conn(&mut faked());
}
#[test]
fn q_takes_a_db_value() {
    m::q_takes_a_db_value(&mut faked());
}
#[test]
fn pull_returns_a_map() {
    m::pull_returns_a_map(&mut faked());
}
#[test]
fn entity_returns_an_entity_map() {
    m::entity_returns_an_entity_map(&mut faked());
}
#[test]
fn read_returns_the_scalar() {
    m::read_returns_the_scalar(&mut faked());
}
#[test]
fn datoms_returns_tuples() {
    m::datoms_returns_tuples(&mut faked());
}
#[test]
fn with_is_speculative_and_does_not_commit() {
    m::with_is_speculative_and_does_not_commit(&mut faked());
}
#[test]
fn as_of_and_since_reflect_the_basis() {
    m::as_of_and_since_reflect_the_basis(&mut faked());
}
#[test]
fn tx_report_has_the_datomic_shape() {
    m::tx_report_has_the_datomic_shape(&mut faked());
}
#[test]
fn inst_and_uuid_builders_round_trip_through_the_reader() {
    m::inst_and_uuid_builders_round_trip_through_the_reader();
}
