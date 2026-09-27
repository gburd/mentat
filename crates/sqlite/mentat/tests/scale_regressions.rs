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

use mentat::{
    IntoResult, QueryExplanation, QueryInputs, Queryable, Store, TemporalBound, TypedValue,
    Variable,
};

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

fn plan(store: &Store, q: &str, inputs: Option<QueryInputs>, t: Option<TemporalBound>) -> String {
    match store.q_explain_temporal(q, inputs, t).expect("explain") {
        QueryExplanation::ExecutionPlan { steps, .. } => steps
            .iter()
            .map(|s| s.detail.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => panic!("expected a plan"),
    }
}

/// Bug 3: as-of's correlated NOT EXISTS (and since's `tx > T`) had only a
/// `(timeline)` index on timelined_transactions, so every as-of was a scan of
/// all history per outer row. Both must use the covering history index.
#[test]
fn test_as_of_and_since_use_history_index() {
    let mut store = Store::open("").expect("open");
    schema(&mut store);
    let e = *store
        .transact(r#"[{:db/id "e" :s/name "a" :s/n 1}]"#)
        .expect("tx")
        .tempids
        .get("e")
        .unwrap();
    let t = store.last_tx_id();
    store
        .transact(&format!(r#"[[:db/add {e} :s/name "b"]]"#))
        .expect("update");

    let q = "[:find ?v :in ?e :where [?e :s/name ?v]]";
    let inputs = || {
        QueryInputs::with_value_sequence(vec![(
            Variable::from_valid_name("?e"),
            TypedValue::Ref(e),
        )])
    };
    for (q, i) in [
        (q, Some(inputs())),
        ("[:find ?e ?v :where [?e :s/name ?v]]", None),
    ] {
        let as_of = plan(&store, q, i, Some(TemporalBound::AsOf(t)));
        // The outer pattern and the NOT EXISTS probe both search the index.
        assert_eq!(
            as_of
                .matches("USING COVERING INDEX idx_transactions_aevt")
                .count(),
            2,
            "as_of plan should use the history index twice:\n{as_of}"
        );
        assert!(
            !as_of.contains("SCAN "),
            "as_of must not scan history:\n{as_of}"
        );
        assert!(
            !as_of.contains("timeline=?"),
            "as_of must not use the timeline index:\n{as_of}"
        );
    }
    let since = plan(
        &store,
        "[:find ?e :where [?e :s/name _]]",
        None,
        Some(TemporalBound::Since(t)),
    );
    assert!(
        since.contains("USING COVERING INDEX idx_transactions_aevt (a=?"),
        "since plan should use the history index:\n{since}"
    );
    // Results stay right.
    let v = store
        .q_once_as_of(q, inputs(), t)
        .into_rel_result()
        .expect("as_of");
    assert_eq!(v.row_count(), 1);
    assert_eq!(
        v.rows().next().unwrap()[0]
            .clone()
            .into_string()
            .unwrap()
            .as_str(),
        "a"
    );
}

fn temp_store(name: &str) -> String {
    let p = std::env::temp_dir().join(format!("mentat-{name}-{}.db", std::process::id()));
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
    }
    p.to_str().unwrap().to_string()
}

fn raw(path: &str, sql: &str) {
    rusqlite::Connection::open(path)
        .expect("raw open")
        .execute_batch(sql)
        .expect(sql);
}

/// Bug 5: `Store::open` derived the partition map by GROUP BY over the whole
/// log (the `parts` view), O(history). The marks now live in `known_parts`.
#[test]
fn test_open_does_not_scan_log() {
    let path = temp_store("open");
    let (next_e, last_tx) = {
        let mut store = Store::open(&path).expect("create");
        schema(&mut store);
        store.transact(r#"[{:s/n 1} {:s/n 2}]"#).expect("tx");
        let e = store.transact(r#"[{:db/id "x" :s/n 3}]"#).expect("tx");
        (*e.tempids.get("x").unwrap() + 1, store.last_tx_id())
    };
    // If open touched the log-derived view, it would now fail.
    raw(&path, "DROP VIEW parts");
    let mut store = Store::open(&path).expect("open without the parts view");
    assert_eq!(store.last_tx_id(), last_tx);
    let r = store
        .transact(r#"[{:db/id "y" :s/n 4}]"#)
        .expect("tx after reopen");
    assert_eq!(*r.tempids.get("y").unwrap(), next_e, "no entid reuse");
    assert_eq!(r.tx_id, last_tx + 1);
}

/// A version-1 store (no history index, no persisted marks) is upgraded on
/// open, with the marks derived once from its log.
#[test]
fn test_v1_store_is_upgraded_on_open() {
    let path = temp_store("v1");
    let (next_e, last_tx) = {
        let mut store = Store::open(&path).expect("create");
        schema(&mut store);
        let e = store.transact(r#"[{:db/id "x" :s/n 3}]"#).expect("tx");
        (*e.tempids.get("x").unwrap() + 1, store.last_tx_id())
    };
    raw(
        &path,
        "DROP INDEX idx_transactions_aevt; ALTER TABLE known_parts DROP COLUMN idx; PRAGMA user_version = 1;",
    );
    let mut store = Store::open(&path).expect("upgrade");
    let c = store.sqlite_ref();
    let v: i64 = c
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(v, mentat_db::db::CURRENT_VERSION as i64);
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'idx_transactions_aevt'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(store.last_tx_id(), last_tx);
    let r = store
        .transact(r#"[{:db/id "y" :s/n 4}]"#)
        .expect("tx after upgrade");
    assert_eq!(*r.tempids.get("y").unwrap(), next_e);
}

/// Bug 4: concurrent readers collapsed (8 threads on a 1M-datom store: 20
/// ops/s). The bundled SQLite's SQLITE_ENABLE_MEMORY_MANAGEMENT puts every
/// connection in one page-cache group behind a global mutex. The workspace
/// undoes it (.cargo/config.toml LIBSQLITE3_FLAGS), and every connection maps
/// the file (mmap_size) for builds that don't.
#[test]
fn test_no_shared_page_cache_contention() {
    let path = temp_store("mmap");
    let store = Store::open(&path).expect("open");
    let c = store.sqlite_ref();
    let opts: Vec<String> = c
        .prepare("PRAGMA compile_options")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert!(
        !opts.iter().any(|o| o == "ENABLE_MEMORY_MANAGEMENT"),
        "bundled SQLite still has ENABLE_MEMORY_MANAGEMENT: {opts:?}"
    );
    assert!(opts.iter().any(|o| o == "DEFAULT_MEMSTATUS=0"), "{opts:?}");
    let mmap: i64 = c.query_row("PRAGMA mmap_size", [], |r| r.get(0)).unwrap();
    assert_eq!(mmap, 1 << 30);
}

/// Large sorts (an aggregate's GROUP BY) on in-memory temp storage got slower
/// each time one connection repeated them; temp b-trees are file-backed now.
#[test]
fn test_temp_store_is_file_backed() {
    let store = Store::open("").expect("open");
    let t: i64 = store
        .sqlite_ref()
        .query_row("PRAGMA temp_store", [], |r| r.get(0))
        .unwrap();
    assert_eq!(t, 1);
}
