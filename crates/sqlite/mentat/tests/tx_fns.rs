// Copyright 2016-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Built-in transaction functions `:db.fn/cas` and `:db/retractEntity` on the
//! SQLite transactor. Ported from pg_mentat's `cas_tests.rs`,
//! `retraction_tests.rs`, and `comprehensive_retract_tests.rs` (the shipped
//! reference for the semantics), using mentat's public `Store` transact/query
//! API rather than pg's SPI plumbing.

extern crate mentat;

use core_traits::Entid;

use db_traits::errors::DbErrorKind;

use mentat::{IntoResult, Queryable, Store, TypedValue};

use public_traits::errors::MentatError;

/// Transact a schema covering the value types the CAS/retract tests exercise.
fn setup() -> Store {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[
        {:db/ident :t/name  :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
        {:db/ident :t/val   :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
        {:db/ident :t/flag  :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
        {:db/ident :t/kw    :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}
        {:db/ident :t/tags  :db/valueType :db.type/string  :db/cardinality :db.cardinality/many}
        {:db/ident :t/ref   :db/valueType :db.type/ref     :db/cardinality :db.cardinality/one}
        {:db/ident :t/child :db/valueType :db.type/ref     :db/cardinality :db.cardinality/one
                            :db/isComponent true}
    ]"#,
        )
        .expect("schema");
    store
}

/// Transact `[:db/add "e" a v]` and return the allocated entid for tempid "e".
fn make_entity(store: &mut Store, body: &str) -> Entid {
    let report = store.transact(body).expect("tx");
    *report.tempids.get("e").expect("tempid e")
}

/// The current long value of `[e :t/val]`, if any.
fn val(store: &Store, e: Entid) -> Option<i64> {
    store
        .q_once(
            "[:find ?v . :in ?e :where [?e :t/val ?v]]",
            mentat::QueryInputs::with_value_sequence(vec![(
                mentat::Variable::from_valid_name("?e"),
                TypedValue::Ref(e),
            )]),
        )
        .into_scalar_result()
        .expect("q")
        .and_then(|b| b.into_long())
}

/// The current string value of `[e :t/name]`, if any.
fn name(store: &Store, e: Entid) -> Option<String> {
    store
        .q_once(
            "[:find ?v . :in ?e :where [?e :t/name ?v]]",
            mentat::QueryInputs::with_value_sequence(vec![(
                mentat::Variable::from_valid_name("?e"),
                TypedValue::Ref(e),
            )]),
        )
        .into_scalar_result()
        .expect("q")
        .and_then(|b| b.into_string())
        .map(|s| (*s).clone())
}

/// Count of *all* live datoms with `e` as subject.
fn datom_count(store: &Store, e: Entid) -> i64 {
    store
        .q_once(
            "[:find (count ?a) . :in ?e :where [?e ?a _]]",
            mentat::QueryInputs::with_value_sequence(vec![(
                mentat::Variable::from_valid_name("?e"),
                TypedValue::Ref(e),
            )]),
        )
        .into_scalar_result()
        .expect("q")
        .and_then(|b| b.into_long())
        .unwrap_or(0)
}

// ============================================================================
// CAS
// ============================================================================

#[test]
fn test_cas_success_asserts_new() {
    let mut store = setup();
    let e = make_entity(&mut store, r#"[[:db/add "e" :t/val 10]]"#);
    store
        .transact(&format!("[[:db.fn/cas {} :t/val 10 20]]", e))
        .expect("cas 10->20");
    assert_eq!(val(&store, e), Some(20));
}

#[test]
fn test_cas_short_spelling_success() {
    // `:db/cas` must work just like `:db.fn/cas`.
    let mut store = setup();
    let e = make_entity(&mut store, r#"[[:db/add "e" :t/name "old"]]"#);
    store
        .transact(&format!("[[:db/cas {} :t/name \"old\" \"new\"]]", e))
        .expect("cas old->new");
    assert_eq!(name(&store, e), Some("new".to_string()));
}

#[test]
fn test_cas_mismatch_aborts_whole_tx() {
    let mut store = setup();
    let e = make_entity(&mut store, r#"[[:db/add "e" :t/val 42]]"#);

    // A CAS with the wrong expected value, in the same tx as an add that must
    // NOT land because the whole transaction aborts.
    let err = store
        .transact(&format!(
            "[[:db.fn/cas {e} :t/val 99 100] [:db/add {e} :t/name \"should-not-land\"]]",
            e = e
        ))
        .expect_err("cas mismatch must fail");
    match err {
        MentatError::DbError(db) => match db.kind() {
            DbErrorKind::CasMismatch { e: ce, a: _, .. } => assert_eq!(ce, e),
            other => panic!("expected CasMismatch, got {:?}", other),
        },
        other => panic!("expected DbError, got {:?}", other),
    }

    // Nothing committed: value unchanged, sibling assertion absent.
    assert_eq!(val(&store, e), Some(42));
    assert_eq!(name(&store, e), None);
}

#[test]
fn test_cas_from_nil_asserts_when_absent() {
    let mut store = setup();
    let e = make_entity(&mut store, r#"[[:db/add "e" :t/name "x"]]"#);
    // :t/val is absent, so cas from nil must succeed.
    store
        .transact(&format!("[[:db.fn/cas {} :t/val nil 42]]", e))
        .expect("cas nil->42");
    assert_eq!(val(&store, e), Some(42));
}

#[test]
fn test_cas_from_nil_fails_when_present() {
    let mut store = setup();
    let e = make_entity(&mut store, r#"[[:db/add "e" :t/val 42]]"#);
    // :t/val is present, so cas from nil must fail.
    let err = store
        .transact(&format!("[[:db.fn/cas {} :t/val nil 99]]", e))
        .expect_err("cas from nil must fail when present");
    match err {
        MentatError::DbError(db) => {
            assert!(matches!(db.kind(), DbErrorKind::CasMismatch { .. }))
        }
        other => panic!("expected DbError, got {:?}", other),
    }
    assert_eq!(val(&store, e), Some(42));
}

// ============================================================================
// retractEntity
// ============================================================================

#[test]
fn test_retract_entity_removes_all_subject_datoms() {
    let mut store = setup();
    let e = make_entity(
        &mut store,
        r#"[{:db/id "e" :t/name "Entity" :t/val 42 :t/flag true :t/kw :status/active}]"#,
    );
    assert!(datom_count(&store, e) >= 4);
    store
        .transact(&format!("[[:db/retractEntity {}]]", e))
        .expect("retractEntity");
    assert_eq!(datom_count(&store, e), 0);
    assert_eq!(val(&store, e), None);
    assert_eq!(name(&store, e), None);
}

#[test]
fn test_retract_entity_short_and_fn_spellings() {
    let mut store = setup();
    let e1 = make_entity(&mut store, r#"[[:db/add "e" :t/name "one"]]"#);
    store
        .transact(&format!("[[:db.fn/retractEntity {}]]", e1))
        .expect(":db.fn/retractEntity");
    assert_eq!(name(&store, e1), None);

    let e2 = make_entity(&mut store, r#"[[:db/add "e" :t/name "two"]]"#);
    store
        .transact(&format!("[[:db/retractEntity {}]]", e2))
        .expect(":db/retractEntity");
    assert_eq!(name(&store, e2), None);
}

#[test]
fn test_retract_entity_recurses_into_component() {
    let mut store = setup();
    // Parent with a component child (:t/child :db/isComponent true) and a
    // non-component ref (:t/ref). retractEntity must cascade into the component
    // child but NOT the plain ref target.
    let report = store
        .transact(
            r#"[
        {:db/id "child"  :t/name "child"}
        {:db/id "friend" :t/name "friend"}
        {:db/id "e" :t/name "parent" :t/child "child" :t/ref "friend"}
    ]"#,
        )
        .expect("tx");
    let e = *report.tempids.get("e").expect("e");
    let child = *report.tempids.get("child").expect("child");
    let friend = *report.tempids.get("friend").expect("friend");

    store
        .transact(&format!("[[:db/retractEntity {}]]", e))
        .expect("retractEntity");

    // Parent and its component child are gone.
    assert_eq!(datom_count(&store, e), 0);
    assert_eq!(datom_count(&store, child), 0);
    // The plain (non-component) ref target survives.
    assert_eq!(name(&store, friend), Some("friend".to_string()));
}

#[test]
fn test_retract_entity_leaves_incoming_refs() {
    // pg_mentat retracts datoms with `e` as subject + component children only,
    // NOT datoms where `e` is a ref *value*. We match pg: an incoming ref
    // survives (the referrer keeps its dangling ref datom).
    let mut store = setup();
    let report = store
        .transact(
            r#"[
        {:db/id "target"   :t/name "target"}
        {:db/id "referrer" :t/name "referrer" :t/ref "target"}
    ]"#,
        )
        .expect("tx");
    let target = *report.tempids.get("target").expect("target");
    let referrer = *report.tempids.get("referrer").expect("referrer");

    store
        .transact(&format!("[[:db/retractEntity {}]]", target))
        .expect("retractEntity target");

    // target's own datoms gone; referrer's [:t/ref target] datom remains.
    assert_eq!(datom_count(&store, target), 0);
    let refs: Vec<i64> = store
        .q_once(
            "[:find [?v ...] :in ?e :where [?e :t/ref ?v]]",
            mentat::QueryInputs::with_value_sequence(vec![(
                mentat::Variable::from_valid_name("?e"),
                TypedValue::Ref(referrer),
            )]),
        )
        .into_coll_result()
        .expect("q")
        .into_iter()
        .filter_map(|b| b.into_entid())
        .collect();
    assert_eq!(refs, vec![target]);
}
