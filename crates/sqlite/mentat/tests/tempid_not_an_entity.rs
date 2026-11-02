// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! #384: a tempid used only as a ref value, never as the entity of a datom (and not
//! upserting), fails the transaction with Datomic's `:db.error/tempid-not-an-entity`.

use mentat::{IntoResult, Queryable, Store};

fn setup() -> Store {
    let mut store = Store::open("").expect("opened");
    store
        .transact(
            r#"[{:db/ident :v/id :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity :db/index true}
                {:db/ident :v/uv :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/value :db/index true}
                {:db/ident :v/likes :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
                {:db/ident :v/part :db/valueType :db.type/ref :db/cardinality :db.cardinality/one
                 :db/isComponent true}
                {:db/ident :v/tx :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]"#,
        )
        .expect("schema");
    store.transact(r#"[{:v/id "a" :v/uv "ua"}]"#).expect("data");
    store
}

fn refs(store: &Store) -> usize {
    store
        .q_once("[:find ?r :where [_ :v/likes ?r]]", None)
        .into_rel_result()
        .expect("query")
        .row_count()
}

fn rejects(store: &mut Store, tx: &str, tempid: &str) {
    let err = store.transact(tx).expect_err(tx).to_string();
    assert!(
        err.contains(":db.error/tempid-not-an-entity") && err.contains(&format!("\"{tempid}\"")),
        "{tx}: {err}"
    );
}

#[test]
fn value_only_tempids_are_rejected() {
    let mut store = setup();
    let before = refs(&store);
    // The issue's three forms: map form with a list, map form with `[:v/uv "ua"]` (a
    // list of a ref to :v/uv and a tempid "ua", by DataScript's identity-only rule),
    // and list form.
    rejects(&mut store, r#"[{:v/id "d8" :v/likes ["t1" "t2"]}]"#, "t1");
    rejects(&mut store, r#"[{:v/id "e3" :v/likes [:v/uv "ua"]}]"#, "ua");
    rejects(
        &mut store,
        r#"[[:db/add "l" :v/id "l"] [:db/add "l" :v/likes "t3"]]"#,
        "t3",
    );
    // Only the value of a component attribute: still an entity with no datoms.
    rejects(&mut store, r#"[{:v/id "c" :v/part "p"}]"#, "p");
    assert_eq!(refs(&store), before, "nothing committed");
}

#[test]
fn tempids_that_name_entities_are_fine() {
    let mut store = setup();
    // Used as a value and as an entity.
    let r = store
        .transact(r#"[{:v/id "d" :v/likes ["t1"]} {:db/id "t1" :v/id "t1"}]"#)
        .expect("value and entity");
    assert!(r.tempids.contains_key("t1"));
    // Upserts to an existing entity (:v/id "a") and is used as a value.
    let a = store
        .transact(r#"[[:db/add "u" :v/id "a"] [:db/add "e" :v/id "e"] [:db/add "e" :v/likes "u"]]"#)
        .expect("upsert");
    let existing = store
        .q_once(r#"[:find ?e . :where [?e :v/id "a"]]"#, None)
        .into_scalar_result()
        .expect("query")
        .expect("a")
        .into_entid()
        .expect("entid");
    assert_eq!(a.tempids["u"], existing);
    // The current transaction as a value.
    store
        .transact(r#"[[:db/add "x" :v/id "x"] [:db/add "x" :v/tx (transaction-tx)]]"#)
        .expect("transaction-tx");
    // List form `[:db/add e a [:v/uv "ua"]]` is a lookup ref, not a tempid.
    store
        .transact(r#"[[:db/add "y" :v/id "y"] [:db/add "y" :v/likes [:v/uv "ua"]]]"#)
        .expect("lookup ref");
}

/// A tx-function's expansion is checked like any other tx-data.
#[cfg(feature = "mino")]
#[test]
fn tx_fn_expansion_is_checked() {
    let mut store = setup();
    store
        .transact(
            r#"[{:db/ident :v/like-new :db/fn "(fn [db who] [[:db/add [:v/id who] :v/likes \"n\"]])"}
                {:db/ident :v/like-made :db/fn "(fn [db who] [[:db/add [:v/id who] :v/likes \"n\"] {:db/id \"n\" :v/id \"n\"}])"}]"#,
        )
        .expect("install");
    rejects(&mut store, r#"[[:v/like-new "a"]]"#, "n");
    store
        .transact(r#"[[:v/like-made "a"]]"#)
        .expect("n has a datom");
}
