// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Regressions for the engine bugs the scale benchmark found
//! (benchmarks/results/scale-2026-09-27T010840Z/findings.md).

use std::time::Duration;

use mentat::{IntoResult, Queryable, Store};

fn schema(store: &mut Store) {
    store
        .transact(
            r#"[
        {:db/ident :s/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
        {:db/ident :s/n    :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
        {:db/ident :s/tag  :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
    ]"#,
        )
        .expect("schema");
}

fn count(store: &Store, q: &str) -> i64 {
    store
        .q_once(q, None)
        .into_scalar_result()
        .expect("q")
        .and_then(|b| b.into_long())
        .expect("a count")
}

/// Bug 1: `insert_non_fts_searches` chunks by `max_vars / 6` but asserted
/// `6 * n < max_vars`, so the first full chunk (5461 datoms at the default
/// 32766 variables) panicked. One tx of 20,000 datoms must succeed.
#[test]
fn test_huge_transaction() {
    let mut store = Store::open("").expect("open");
    schema(&mut store);
    // 10,000 entities x 2 cardinality-one attributes = 20,000 datoms (Inexact search path).
    let mut tx = String::from("[");
    for i in 0..10_000 {
        tx.push_str(&format!(r#"{{:s/name "n{i}" :s/n {i}}}"#));
    }
    tx.push(']');
    store.transact(&tx).expect("20k-datom tx");
    assert_eq!(
        count(&store, "[:find (count ?e) . :where [?e :s/n _]]"),
        10_000
    );
    assert_eq!(
        count(&store, "[:find (count ?e) . :where [?e :s/name _]]"),
        10_000
    );

    // And the cardinality-many (Exact search) path: 6,000 values on one entity.
    let mut tx = String::from(r#"[{:db/id "x" :s/tag ["#);
    for i in 0..6_000 {
        tx.push_str(&format!(r#""t{i}" "#));
    }
    tx.push_str("]}]");
    store.transact(&tx).expect("6k-value card-many tx");
    assert_eq!(
        count(&store, "[:find (count ?t) . :where [_ :s/tag ?t]]"),
        6_000
    );
}

/// Bug 1, the lookup-ref path (`resolve_avs`, 4 bindings per row): more than
/// max_vars/4 lookup refs in one tx.
#[test]
fn test_many_lookup_refs_in_one_tx() {
    let mut store = Store::open("").expect("open");
    store
        .transact(
            r#"[{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one
                 :db/unique :db.unique/identity :db/index true}
                {:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]"#,
        )
        .expect("schema");
    let n = 9_000;
    let mut tx = String::from("[");
    for i in 0..n {
        tx.push_str(&format!(r#"{{:u/email "e{i}"}}"#));
    }
    tx.push(']');
    store.transact(&tx).expect("users");
    let mut tx = String::from("[");
    for i in 0..n {
        tx.push_str(&format!(
            r#"[:db/add (lookup-ref :u/email "e{i}") :u/n {i}]"#
        ));
    }
    tx.push(']');
    store.transact(&tx).expect("9k lookup refs");
    assert_eq!(count(&store, "[:find (count ?e) . :where [?e :u/n _]]"), n);
}

/// Bug 2: an interrupted query used to panic in the projector
/// (`rows.next().unwrap()`) and poison the Store's metadata mutex, so every
/// later call failed. It must return an error and leave the Store usable.
#[test]
fn test_interrupted_query_leaves_store_usable() {
    let mut store = Store::open("").expect("open");
    schema(&mut store);
    let mut tx = String::from("[");
    for i in 0..2_000 {
        tx.push_str(&format!(r#"{{:s/n {i}}}"#));
    }
    tx.push(']');
    store.transact(&tx).expect("data");

    // A 2000^3 cross join: runs for minutes unless interrupted.
    let slow = "[:find ?a ?b ?c :where [?a :s/n _] [?b :s/n _] [?c :s/n _]]";
    let h = store.sqlite_ref().get_interrupt_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        h.interrupt();
    });
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.q_once(slow, None)));
    t.join().unwrap();
    let r = r.expect("an interrupted query must not panic");
    let e = r.expect_err("an interrupted query must fail");
    assert!(e.to_string().contains("interrupt"), "error: {e}");

    // The same Store keeps working: queries, pull and transact.
    assert_eq!(
        count(&store, "[:find (count ?e) . :where [?e :s/n _]]"),
        2_000
    );
    store
        .transact(r#"[{:s/n 5000}]"#)
        .expect("tx after interrupt");
    assert_eq!(
        count(&store, "[:find (count ?e) . :where [?e :s/n _]]"),
        2_001
    );
}
