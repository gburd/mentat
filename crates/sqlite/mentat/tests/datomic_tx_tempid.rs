// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! #414: Datomic's reserved tempid "datomic.tx" names the transaction being committed.

use mentat::{Binding, IntoResult, QueryInputs, Queryable, Store, TypedValue, Variable};

fn setup() -> Store {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :v/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity :db/index true}
                {:db/ident :v/uv :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/value :db/index true}
                {:db/ident :v/src :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                {:db/ident :v/likes :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
                {:db/ident :v/tx :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]"#,
        )
        .expect("schema");
    store.transact(r#"[{:v/id "a" :v/uv "ua"}]"#).expect("data");
    store
}

fn scalar(store: &Store, q: &str) -> Option<Binding> {
    store.q_once(q, None).into_scalar_result().expect(q)
}

fn src_of_tx(store: &Store, tx: i64) -> Option<Binding> {
    store
        .q_once(
            "[:find ?s . :in ?tx :where [?tx :v/src ?s]]",
            QueryInputs::with_value_sequence(vec![(
                Variable::from_valid_name("?tx"),
                TypedValue::Ref(tx),
            )]),
        )
        .into_scalar_result()
        .expect("query")
}

#[test]
fn annotates_the_transaction_in_map_and_list_form() {
    let mut store = setup();
    let r = store
        .transact(r#"[{:db/id "datomic.tx" :v/src "import"} {:v/id "b"}]"#)
        .expect("map form");
    assert_eq!(r.tempids.get("datomic.tx"), Some(&r.tx_id));
    assert_eq!(src_of_tx(&store, r.tx_id), Some("import".into()));
    // Read back joined to the data the transaction wrote.
    assert_eq!(
        scalar(
            &store,
            r#"[:find ?s . :where [?e :v/id "b" ?tx] [?tx :v/src ?s]]"#
        ),
        Some("import".into())
    );

    let r = store
        .transact(r#"[[:db/add "datomic.tx" :v/src "list"] [:db/add "c" :v/id "c"]]"#)
        .expect("list form");
    assert_eq!(r.tempids.get("datomic.tx"), Some(&r.tx_id));
    assert_eq!(src_of_tx(&store, r.tx_id), Some("list".into()));
    // Through tx-data.
    let out = store
        .q_once(
            "[:find ?v . :in ?t :where [(tx-data $ ?t) [[?e ?a ?v]]] [?a :db/ident :v/src]]",
            QueryInputs::with_value_sequence(vec![(
                Variable::from_valid_name("?t"),
                TypedValue::Ref(r.tx_id),
            )]),
        )
        .into_scalar_result()
        .expect("tx-data");
    assert_eq!(out, Some("list".into()));

    // Not used: not reported.
    let r = store.transact(r#"[{:v/id "d"}]"#).expect("plain");
    assert!(!r.tempids.contains_key("datomic.tx"), "{:?}", r.tempids);
}

#[test]
fn as_a_ref_value_and_as_of() {
    let mut store = setup();
    let before = store.transact(r#"[{:v/id "x"}]"#).expect("x").tx_id;
    let r = store
        .transact(r#"[{:v/id "x" :v/tx "datomic.tx"} {:db/id "datomic.tx" :v/src "s"}]"#)
        .expect("ref value");
    assert_eq!(
        scalar(
            &store,
            r#"[:find ?t . :where [?e :v/id "x"] [?e :v/tx ?t]]"#
        ),
        Some(Binding::Scalar(TypedValue::Ref(r.tx_id)))
    );
    // Used only as a value, it is still the transaction (it has a :db/txInstant).
    let r2 = store
        .transact(r#"[{:v/id "y" :v/tx "datomic.tx"}]"#)
        .expect("value only");
    assert_eq!(r2.tempids.get("datomic.tx"), Some(&r2.tx_id));

    let q = r#"[:find ?s . :where [?e :v/id "x"] [?e :v/tx ?t] [?t :v/src ?s]]"#;
    let as_of = |t| {
        store
            .q_once_as_of(q, None, t)
            .into_scalar_result()
            .expect("as-of")
    };
    assert_eq!(as_of(before), None);
    assert_eq!(as_of(r.tx_id), Some("s".into()));
}

#[test]
fn a_value_only_tempid_beside_it_still_fails() {
    let mut store = setup();
    let err = store
        .transact(r#"[{:db/id "datomic.tx" :v/likes ["t9"]}]"#)
        .expect_err("t9 is value only")
        .to_string();
    assert!(
        err.contains(":db.error/tempid-not-an-entity") && err.contains("\"t9\""),
        "{err}"
    );
}

#[test]
fn explicit_tx_instant() {
    let mut store = setup();
    let r = store
        .transact(
            r#"[{:db/id "datomic.tx" :db/txInstant #inst "2017-06-16T00:56:41.257Z"} {:v/id "i"}]"#,
        )
        .expect("txInstant");
    assert_eq!(r.tx_instant.to_rfc3339(), "2017-06-16T00:56:41.257+00:00");
    assert_eq!(
        store
            .q_once(
                "[:find ?i . :in ?tx :where [?tx :db/txInstant ?i]]",
                QueryInputs::with_value_sequence(vec![(
                    Variable::from_valid_name("?tx"),
                    TypedValue::Ref(r.tx_id),
                )]),
            )
            .into_scalar_result()
            .expect("query"),
        Some(Binding::Scalar(TypedValue::Instant(r.tx_instant)))
    );
}

#[test]
fn a_unique_value_another_entity_holds_is_a_conflict() {
    let mut store = setup();
    for (tx, attr, value) in [
        (r#"[{:db/id "datomic.tx" :v/id "a"}]"#, ":v/id", "\"a\""),
        (r#"[{:db/id "datomic.tx" :v/uv "ua"}]"#, ":v/uv", "\"ua\""),
        // (transaction-tx) is the same entity.
        (
            r#"[[:db/add (transaction-tx) :v/uv "ua"]]"#,
            ":v/uv",
            "\"ua\"",
        ),
        // Another entity asserts it in the same transaction.
        (
            r#"[{:db/id "datomic.tx" :v/uv "n"} {:v/id "p" :v/uv "n"}]"#,
            ":v/uv",
            "\"n\"",
        ),
        (
            r#"[{:db/id "datomic.tx" :v/id "n"} {:db/id "p" :v/id "n"}]"#,
            ":v/id",
            "\"n\"",
        ),
    ] {
        let err = store.transact(tx).expect_err(tx).to_string();
        assert!(
            err.contains(":db.error/unique-conflict")
                && err.contains(&format!("'{attr}'"))
                && err.contains(value),
            "{tx}: {err}"
        );
    }
    assert_eq!(
        scalar(&store, "[:find (count ?e) . :where [?e :v/uv _]]"),
        Some(Binding::Scalar(TypedValue::Long(1)))
    );
    // A value nobody holds is fine, and so is one its holder gives up here.
    store
        .transact(r#"[{:db/id "datomic.tx" :v/uv "free"}]"#)
        .expect("free");
    store
        .transact(r#"[{:v/id "a" :v/uv "ub"} {:db/id "datomic.tx" :v/id "t" :v/uv "ua"}]"#)
        .expect("released");
}

/// A tx-function may annotate the transaction too.
#[cfg(feature = "mino")]
#[test]
fn from_a_tx_fn() {
    let mut store = setup();
    store
        .transact(
            r#"[{:db/ident :v/stamp :db/fn "(fn [db who] [{:db/id \"datomic.tx\" :v/src who}])"}]"#,
        )
        .expect("install");
    let r = store
        .transact(r#"[[:v/stamp "bob"] {:v/id "f"}]"#)
        .expect("tx-fn");
    assert_eq!(src_of_tx(&store, r.tx_id), Some("bob".into()));
    assert_eq!(r.tempids.get("datomic.tx"), Some(&r.tx_id));
}
