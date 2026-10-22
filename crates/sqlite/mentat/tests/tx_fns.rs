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

// [:db/cas e a v v] with v current is a no-op, as in Datomic, on a cardinality-one and a
// cardinality-many attribute; it was an AddRetractConflict. A mismatch still fails.
#[test]
fn test_cas_to_the_same_value_is_a_no_op() {
    let mut store = setup();
    let e = make_entity(
        &mut store,
        r#"[[:db/add "e" :t/val 10] [:db/add "e" :t/tags "a"]]"#,
    );
    for tx in [
        format!("[[:db/cas {e} :t/val 10 10]]"),
        format!("[[:db/cas {e} :t/tags \"a\" \"a\"]]"),
    ] {
        let r = store.transact(&tx).unwrap_or_else(|e| panic!("{tx}: {e}"));
        assert_eq!((r.datoms_asserted, r.datoms_retracted), (0, 0), "{tx}");
    }
    assert_eq!(val(&store, e), Some(10));
    assert_eq!(datom_count(&store, e), 2);
    assert!(store
        .transact(&format!("[[:db/cas {e} :t/val 11 11]]"))
        .is_err());
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
fn test_retract_entity_retracts_incoming_refs() {
    // Issue #326: like Datomic (and pg_mentat), retractEntity also retracts
    // datoms where `e` is a ref *value*, so no dangling ref survives. The
    // referrer itself stays.
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

    // target's own datoms gone, and so is referrer's [:t/ref target].
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
    assert_eq!(refs, Vec::<i64>::new(), "inbound ref retracted");
    assert_eq!(name(&store, referrer), Some("referrer".to_string()));
}

// Retracting a value the attribute doesn't have changed nothing on a cardinality-many
// attribute, but on a cardinality-one attribute it retracted the current value.
#[test]
fn test_retract_of_absent_value_keeps_current_value() {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :r/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/ident :r/s :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/fulltext true :db/index true}]"#,
        )
        .expect("schema");
    let r = store
        .transact(r#"[{:db/id "a" :r/n 1 :r/s "x"}]"#)
        .expect("a");
    let a = *r.tempids.get("a").unwrap();
    store
        .transact(format!("[[:db/retract {a} :r/n 7] [:db/retract {a} :r/s \"y\"]]").as_str())
        .expect("retract");
    fn q(store: &Store, q: &str) -> mentat::RelResult<mentat::Binding> {
        store.q_once(q, None).into_rel_result().expect(q)
    }
    assert_eq!(
        q(
            &store,
            &format!("[:find ?n ?s :where [{a} :r/n ?n] [{a} :r/s ?s]]")
        ),
        vec![vec![TypedValue::Long(1), TypedValue::typed_string("x")]].into()
    );
    // Nothing was logged for the retractions.
    assert_eq!(
        q(
            &store,
            &format!("[:find ?v ?added :where [{a} :r/n ?v ?tx ?added]]")
        ),
        vec![vec![TypedValue::Long(1), TypedValue::Boolean(true)]].into()
    );
    // The right value still retracts.
    store
        .transact(format!("[[:db/retract {a} :r/n 1] [:db/retract {a} :r/s \"x\"]]").as_str())
        .expect("retract");
    assert_eq!(
        q(&store, &format!("[:find ?v :where [{a} ?a ?v]]")).row_count(),
        0
    );
}

// :db/retractEntity on an entity with a fulltext value read the rowid as a string.
#[test]
fn test_retract_entity_with_fulltext_value() {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :r/s :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/fulltext true :db/index true}]"#,
        )
        .expect("schema");
    let r = store.transact(r#"[{:db/id "a" :r/s "x"}]"#).expect("a");
    let a = *r.tempids.get("a").unwrap();
    store
        .transact(format!("[[:db/retractEntity {a}]]").as_str())
        .expect("retractEntity");
    assert_eq!(datom_count(&store, a), 0);
}

// #152: Datomic's `[a v]` lookup refs, in entity position, :db/id, value position (a
// cardinality-one ref), a list of them (cardinality-many), and reverse notation.
#[test]
fn test_vector_lookup_refs() {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :t/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity :db/index true}
                {:db/ident :t/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/ident :t/ref :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
                {:db/ident :t/refs :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
                {:db/ident :t/kws :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}]"#,
        )
        .expect("schema");
    store
        .transact(r#"[{:t/id "e1"} {:t/id "e2"} {:t/id "e3"}]"#)
        .expect("data");
    for tx in [
        r#"[[:db/add [:t/id "e1"] :t/n 5]]"#,
        r#"[{:db/id [:t/id "e2"] :t/n 6}]"#,
        r#"[{:t/id "x" :t/ref [:t/id "e1"]}]"#,
        r#"[{:t/id "y" :t/refs [[:t/id "e1"] [:t/id "e2"]]}]"#,
        r#"[[:db/add [:t/id "y"] :t/refs [:t/id "e3"]]]"#,
        r#"[{:t/id "e3" :t/_ref [:t/id "e2"]}]"#,
        // Not a lookup ref: a keyword attribute gets a list of keywords.
        r#"[{:t/id "z" :t/kws [:k/a :k/b]}]"#,
    ] {
        store.transact(tx).unwrap_or_else(|e| panic!("{tx}: {e}"));
    }
    let q = |q: &str| -> Vec<String> {
        let mut v: Vec<String> = store
            .q_once(q, None)
            .into_rel_result()
            .unwrap_or_else(|e| panic!("{q}: {e}"))
            .rows()
            .map(|r| format!("{:?}", r))
            .collect();
        v.sort();
        v
    };
    assert_eq!(
        q(r#"[:find ?i ?n :where [?e :t/id ?i] [?e :t/n ?n]]"#),
        [
            r#"[Scalar(String("e1")), Scalar(Long(5))]"#,
            r#"[Scalar(String("e2")), Scalar(Long(6))]"#
        ]
    );
    assert_eq!(
        q(r#"[:find ?a ?b :where [?x :t/id ?a] [?x :t/ref ?r] [?r :t/id ?b]]"#),
        [
            r#"[Scalar(String("e2")), Scalar(String("e3"))]"#,
            r#"[Scalar(String("x")), Scalar(String("e1"))]"#
        ]
    );
    assert_eq!(
        q(r#"[:find ?b :where [?x :t/id "y"] [?x :t/refs ?r] [?r :t/id ?b]]"#),
        [
            r#"[Scalar(String("e1"))]"#,
            r#"[Scalar(String("e2"))]"#,
            r#"[Scalar(String("e3"))]"#
        ]
    );
    assert_eq!(
        q(r#"[:find ?k :where [?x :t/id "z"] [?x :t/kws ?k]]"#).len(),
        2
    );
    // A lookup ref that matches nothing, or a non-unique attribute, is an error.
    assert!(store
        .transact(r#"[[:db/add [:t/id "nope"] :t/n 1]]"#)
        .is_err());
    assert!(store.transact(r#"[[:db/add [:t/n 5] :t/n 1]]"#).is_err());
    // A vector of values for a cardinality-one attribute is still rejected.
    assert!(store.transact(r#"[{:t/id "x" :t/ref [1 2]}]"#).is_err());
    // (lookup-ref a v) keeps working.
    store
        .transact(r#"[[:db/add (lookup-ref :t/id "e3") :t/n 7]]"#)
        .expect("lookup-ref");
}

// #362: a retraction of an absent value leaves an attribute's cache alone, and an observer of
// the attribute hears nothing, because nothing changed.
#[test]
fn test_retract_of_absent_value_keeps_cache_and_observers_quiet() {
    use mentat::{kw, CacheDirection, HasSchema, TxObserver};
    use std::collections::BTreeSet;
    use std::sync::{mpsc, Arc, Mutex};

    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :r/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                {:db/ident :r/m :db/valueType :db.type/long :db/cardinality :db.cardinality/many}
                {:db/ident :r/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity :db/index true}
                {:db/ident :r/s :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/fulltext true :db/index true}]"#,
        )
        .expect("schema");
    let a = *store
        .transact(r#"[{:db/id "a" :r/n 1 :r/m 1 :r/id "A" :r/s "x"}]"#)
        .expect("a")
        .tempids
        .get("a")
        .unwrap();
    store.cache(&kw!(:r/n), CacheDirection::Forward).unwrap();
    store.cache(&kw!(:r/id), CacheDirection::Both).unwrap();

    let schema = store.conn().current_schema();
    let attr = |k| schema.get_entid(&k).map(|e| e.0).unwrap();
    let (r_n, r_m) = (attr(kw!(:r/n)), attr(kw!(:r/m)));
    let attrs: BTreeSet<_> = [r_n, r_m, attr(kw!(:r/id)), attr(kw!(:r/s))].into();
    let (send, heard) = mpsc::channel();
    let send = Mutex::new(send);
    store.register_observer(
        "watch".to_string(),
        Arc::new(TxObserver::new(attrs, move |_key, batch| {
            for (tx, changes) in batch {
                send.lock().unwrap().send((*tx, changes.clone())).unwrap();
            }
        })),
    );

    let r = store
        .transact(&format!(
            "[[:db/retract {a} :r/n 7] [:db/retract {a} :r/m 7] [:db/retract {a} :r/id \"B\"]
              [:db/retract {a} :r/s \"y\"]]"
        ))
        .expect("retract absent");
    assert!(r.is_empty());
    let n = |store: &Store| {
        store
            .q_once(&format!("[:find ?v . :where [{a} :r/n ?v]]"), None)
            .into_scalar_result()
            .unwrap()
    };
    assert_eq!(n(&store), Some(TypedValue::Long(1).into()));
    let ip = store.begin_read().unwrap();
    assert_eq!(
        ip.lookup_value_for_attribute(a, &kw!(:r/n)).unwrap(),
        Some(TypedValue::Long(1))
    );
    assert_eq!(
        ip.lookup_value_for_attribute(a, &kw!(:r/id)).unwrap(),
        Some(TypedValue::typed_string("A"))
    );
    drop(ip);
    assert!(
        heard
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "observer notified of a transaction that changed nothing"
    );

    // A real retraction still updates the cache and notifies.
    let r = store
        .transact(&format!(
            "[[:db/retract {a} :r/n 1] [:db/retract {a} :r/m 7]]"
        ))
        .expect("retract present");
    assert_eq!((r.datoms_asserted, r.datoms_retracted), (0, 1));
    assert_eq!(n(&store), None);
    let (tx, changes) = heard
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("notified");
    assert_eq!(tx, r.tx_id);
    assert!(changes.contains(&r_n));
    assert!(!changes.contains(&r_m), "{changes:?}");
}

// #152: only a :db.unique/identity attribute makes `[:attr v]` in a cardinality-many ref's
// value a lookup ref (DataScript's rule). Two idents of other attributes stay two refs.
#[test]
fn test_vector_of_idents_on_a_ref_list() {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :user/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/value :db/index true}
                {:db/ident :user/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity :db/index true}
                {:db/ident :user/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :form/fields :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}]"#,
        )
        .expect("schema");
    let fields = |store: &mut Store, tx: &str| -> Vec<String> {
        let f = *store
            .transact(tx)
            .unwrap_or_else(|e| panic!("{tx}: {e}"))
            .tempids
            .get("f")
            .unwrap();
        let mut v: Vec<String> = store
            .q_once(
                &format!("[:find [?i ...] :where [{f} :form/fields ?r] [?r :db/ident ?i]]"),
                None,
            )
            .into_coll_result()
            .unwrap()
            .into_iter()
            .map(|b| b.into_kw().expect("keyword").to_string())
            .collect();
        v.sort();
        v
    };
    // :user/email is :db.unique/value: two refs.
    assert_eq!(
        fields(
            &mut store,
            r#"[{:db/id "f" :form/fields [:user/email :user/name]}]"#
        ),
        [":user/email", ":user/name"]
    );
    // :db/ident is :db.unique/identity, so this is a lookup ref to :user/name ...
    assert_eq!(
        fields(
            &mut store,
            r#"[{:db/id "f" :form/fields [:db/ident :user/name]}]"#
        ),
        [":user/name"]
    );
    // ... and the way to write the two refs is to make the first a lookup ref.
    assert_eq!(
        fields(
            &mut store,
            r#"[{:db/id "f" :form/fields [[:db/ident :user/id] :user/name]}]"#
        ),
        [":user/id", ":user/name"]
    );
}
