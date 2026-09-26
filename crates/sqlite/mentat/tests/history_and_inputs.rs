// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Task 12: history patterns `[?e ?a ?v ?tx ?added]`, historical `q` (as-of /
//! since), and non-scalar `:in` bindings (`[?x ...]`, `[?a ?b]`, `[[?a ?b]]`) on
//! the SQLite query engine. Behaviors ported from pg_mentat's `history_tests.rs`,
//! `temporal_tests.rs`, and `input_parameter_tests.rs` (the shipped reference),
//! using mentat's public `Store` API rather than pg's SPI plumbing.

extern crate mentat;

use core_traits::Entid;

use mentat::{HasSchema, IntoResult, QueryInputs, Queryable, Store, TypedValue, Variable};

fn setup() -> Store {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[
        {:db/ident :hi/name :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
        {:db/ident :hi/val  :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
        {:db/ident :hi/tags :db/valueType :db.type/string  :db/cardinality :db.cardinality/many}
        {:db/ident :hi/dept :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
    ]"#,
        )
        .expect("schema");
    store
}

fn var(name: &str) -> Variable {
    Variable::from_valid_name(name)
}

// ============================================================================
// 1. History patterns: [?e ?a ?v ?tx ?added]
// ============================================================================

/// A plain 4-place pattern returns current state only (the latest value).
#[test]
fn test_plain_pattern_is_current_state_only() {
    let mut store = setup();
    let e = *store
        .transact(r#"[[:db/add "e" :hi/val 1]]"#)
        .expect("tx")
        .tempids
        .get("e")
        .unwrap();
    for i in 2..=5 {
        store
            .transact(&format!("[[:db/add {} :hi/val {}]]", e, i))
            .expect("update");
    }

    // Current value is 5 -- one row, not the whole history.
    let vals = store
        .q_once(
            "[:find [?v ...] :in ?e :where [?e :hi/val ?v]]",
            QueryInputs::with_value_sequence(vec![(var("?e"), TypedValue::Ref(e))]),
        )
        .into_coll_result()
        .expect("q")
        .into_iter()
        .filter_map(|b| b.into_long())
        .collect::<Vec<_>>();
    assert_eq!(vals, vec![5], "plain pattern is current-state only");
}

/// `[?e ?a ?v ?tx ?added]` over history returns both assertions (added=true) and
/// the retractions (added=false) that the transactor writes when a
/// cardinality-one value is superseded.
#[test]
fn test_history_pattern_shows_assertions_and_retractions() {
    let mut store = setup();
    let e = *store
        .transact(r#"[[:db/add "e" :hi/val 1]]"#)
        .expect("tx")
        .tempids
        .get("e")
        .unwrap();
    // Two updates => two supersede retractions (1->2, 2->3).
    store
        .transact(&format!("[[:db/add {} :hi/val 2]]", e))
        .expect("u2");
    store
        .transact(&format!("[[:db/add {} :hi/val 3]]", e))
        .expect("u3");

    // Ask for the whole history of this entity's :hi/val.
    let a = store
        .conn()
        .current_schema()
        .get_entid(&mentat::Keyword::namespaced("hi", "val"))
        .expect("attr")
        .0;

    let rows = store
        .q_once(
            "[:find ?v ?added :in ?e ?a :where [?e ?a ?v ?tx ?added]]",
            QueryInputs::with_value_sequence(vec![
                (var("?e"), TypedValue::Ref(e)),
                (var("?a"), TypedValue::Ref(a)),
            ]),
        )
        .into_rel_result()
        .expect("q");

    let mut asserted: Vec<i64> = vec![];
    let mut retracted: Vec<i64> = vec![];
    for row in rows.rows() {
        let v = row[0].clone().into_long().expect("v is long");
        let added = row[1].clone().into_boolean().expect("added is bool");
        if added {
            asserted.push(v);
        } else {
            retracted.push(v);
        }
    }
    asserted.sort();
    retracted.sort();
    assert_eq!(
        asserted,
        vec![1, 2, 3],
        "all asserted values are in history"
    );
    assert_eq!(retracted, vec![1, 2], "superseded values are retracted");
}

// ============================================================================
// 2. Historical q: as-of / since
// ============================================================================

/// as-of T sees the value that was current at tx T (not later updates).
#[test]
fn test_as_of_sees_value_at_that_tx() {
    let mut store = setup();
    let r1 = store
        .transact(r#"[[:db/add "e" :hi/val 25]]"#)
        .expect("tx1");
    let e = *r1.tempids.get("e").unwrap();
    let tx1 = r1.tx_id;
    let tx2 = store
        .transact(&format!("[[:db/add {} :hi/val 26]]", e))
        .expect("tx2")
        .tx_id;
    let tx3 = store
        .transact(&format!("[[:db/add {} :hi/val 27]]", e))
        .expect("tx3")
        .tx_id;

    let at = |tx: Entid| -> Option<i64> {
        store
            .q_once_as_of(
                "[:find ?v . :in ?e :where [?e :hi/val ?v]]",
                QueryInputs::with_value_sequence(vec![(var("?e"), TypedValue::Ref(e))]),
                tx,
            )
            .into_scalar_result()
            .expect("q")
            .and_then(|b| b.into_long())
    };
    assert_eq!(at(tx1), Some(25), "as-of tx1 sees the original value");
    assert_eq!(at(tx2), Some(26), "as-of tx2 sees the first update");
    assert_eq!(at(tx3), Some(27), "as-of tx3 sees the latest value");
}

/// A value that was explicitly retracted is gone from the as-of state at and
/// after the retracting tx, but still visible as-of the tx before it (exercises
/// the "no later retraction" correlated NOT EXISTS).
#[test]
fn test_as_of_respects_explicit_retraction() {
    let mut store = setup();
    let r1 = store.transact(r#"[[:db/add "e" :hi/val 7]]"#).expect("tx1");
    let e = *r1.tempids.get("e").unwrap();
    let tx1 = r1.tx_id;
    let tx2 = store
        .transact(&format!("[[:db/retract {} :hi/val 7]]", e))
        .expect("tx2")
        .tx_id;

    let at = |tx| {
        store
            .q_once_as_of(
                "[:find ?v . :in ?e :where [?e :hi/val ?v]]",
                QueryInputs::with_value_sequence(vec![(var("?e"), TypedValue::Ref(e))]),
                tx,
            )
            .into_scalar_result()
            .expect("q")
            .and_then(|b| b.into_long())
    };
    assert_eq!(
        at(tx1),
        Some(7),
        "before the retraction, the value is present"
    );
    assert_eq!(at(tx2), None, "as-of the retracting tx, the value is gone");
}

/// as-of a tx before an entity exists returns nothing.
#[test]
fn test_as_of_before_entity_created() {
    let mut store = setup();
    let r1 = store
        .transact(r#"[[:db/add "a" :hi/name "Alice"]]"#)
        .expect("tx1");
    let tx1 = r1.tx_id;
    store
        .transact(r#"[[:db/add "b" :hi/name "Bob"]]"#)
        .expect("tx2");

    // At tx1, Bob does not yet exist.
    let names = store
        .q_once_as_of("[:find [?n ...] :where [?e :hi/name ?n]]", None, tx1)
        .into_coll_result()
        .expect("q")
        .into_iter()
        .filter_map(|b| b.into_string().map(|s| (*s).clone()))
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["Alice".to_string()], "only Alice exists at tx1");
}

/// since T returns only datoms transacted after T, as `[?e ?a ?v ?tx ?added]`.
#[test]
fn test_since_returns_only_later_txs() {
    let mut store = setup();
    let r1 = store.transact(r#"[[:db/add "e" :hi/val 1]]"#).expect("tx1");
    let e = *r1.tempids.get("e").unwrap();
    let tx1 = r1.tx_id;
    store
        .transact(&format!("[[:db/add {} :hi/val 2]]", e))
        .expect("tx2");

    let rows = store
        .q_once_since(
            "[:find ?e ?a ?v ?tx ?added :where [?e ?a ?v ?tx ?added]]",
            None,
            tx1,
        )
        .into_rel_result()
        .expect("q");
    assert!(!rows.is_empty(), "there are datoms since tx1");
    for row in rows.rows() {
        let tx = row[3].clone().into_entid().expect("tx");
        assert!(tx > tx1, "every datom is from a tx after tx1");
    }
}

// ============================================================================
// 3. Non-scalar :in bindings: [?x ...], [?a ?b], [[?a ?b]]
// ============================================================================

fn setup_people(store: &mut Store) -> (Entid, Entid, Entid, Entid) {
    let mut e =
        |body: &str| -> Entid { *store.transact(body).expect("tx").tempids.get("e").unwrap() };
    let a = e(
        r#"[[:db/add "e" :hi/name "Alice"] [:db/add "e" :hi/val 100] [:db/add "e" :hi/dept "Eng"]]"#,
    );
    let b = e(
        r#"[[:db/add "e" :hi/name "Bob"]   [:db/add "e" :hi/val 200] [:db/add "e" :hi/dept "Eng"]]"#,
    );
    let c = e(
        r#"[[:db/add "e" :hi/name "Carol"] [:db/add "e" :hi/val 150] [:db/add "e" :hi/dept "Design"]]"#,
    );
    let d = e(
        r#"[[:db/add "e" :hi/name "Dave"]  [:db/add "e" :hi/val 300] [:db/add "e" :hi/dept "Product"]]"#,
    );
    (a, b, c, d)
}

/// `:in $ [?name ...]` binds a collection: match any entity whose name is in the list.
#[test]
fn test_in_collection_binding() {
    let mut store = setup();
    setup_people(&mut store);

    let inputs = QueryInputs::with_collection(
        var("?name"),
        vec![
            TypedValue::typed_string("Alice"),
            TypedValue::typed_string("Carol"),
        ],
    );
    let mut names = store
        .q_once(
            "[:find [?n ...] :in $ [?name ...] :where [?e :hi/name ?name] [?e :hi/name ?n]]",
            inputs,
        )
        .into_coll_result()
        .expect("q")
        .into_iter()
        .filter_map(|b| b.into_string().map(|s| (*s).clone()))
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(names, vec!["Alice".to_string(), "Carol".to_string()]);
}

/// `:in $ [?name ?dept]` binds a tuple: two scalars at once.
#[test]
fn test_in_tuple_binding() {
    let mut store = setup();
    setup_people(&mut store);

    let inputs = QueryInputs::with_tuple(
        vec![var("?name"), var("?dept")],
        vec![
            TypedValue::typed_string("Bob"),
            TypedValue::typed_string("Eng"),
        ],
    );
    let v = store
        .q_once(
            "[:find ?v . :in $ [?name ?dept] :where [?e :hi/name ?name] [?e :hi/dept ?dept] [?e :hi/val ?v]]",
            inputs,
        )
        .into_scalar_result()
        .expect("q")
        .and_then(|b| b.into_long());
    assert_eq!(v, Some(200), "Bob in Eng has val 200");
}

/// `:in $ [[?name ?dept]]` binds a relation: a table of rows.
#[test]
fn test_in_relation_binding() {
    let mut store = setup();
    setup_people(&mut store);

    let inputs = QueryInputs::with_relation(
        vec![var("?name"), var("?dept")],
        vec![
            vec![
                TypedValue::typed_string("Alice"),
                TypedValue::typed_string("Eng"),
            ],
            vec![
                TypedValue::typed_string("Carol"),
                TypedValue::typed_string("Design"),
            ],
            // Bob is in Eng but this pair says Design -- should not match.
            vec![
                TypedValue::typed_string("Bob"),
                TypedValue::typed_string("Design"),
            ],
        ],
    );
    let mut names = store
        .q_once(
            "[:find [?n ...] :in $ [[?name ?dept]] :where [?e :hi/name ?name] [?e :hi/dept ?dept] [?e :hi/name ?n]]",
            inputs,
        )
        .into_coll_result()
        .expect("q")
        .into_iter()
        .filter_map(|b| b.into_string().map(|s| (*s).clone()))
        .collect::<Vec<_>>();
    names.sort();
    assert_eq!(
        names,
        vec!["Alice".to_string(), "Carol".to_string()],
        "only the (name, dept) pairs that actually hold match"
    );
}
