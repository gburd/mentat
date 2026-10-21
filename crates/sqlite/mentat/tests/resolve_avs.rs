// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Upsert and lookup-ref resolution (`MentatStoring::resolve_avs`): correct
//! on both arms (plain and :db/fulltext values), and O(pairs x log store), not
//! O(store) per statement (reported by the Improv team against 1.17.1).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mentat::{Binding, HasSchema, Keyword, Queryable, Store, TypedValue};
use mentat_db::db::MentatStoring;
use mentat_db::types::AVPair;

fn scalar(store: &Store, q: &str) -> Option<Binding> {
    store
        .q_once(q, None)
        .expect("q")
        .into_scalar()
        .expect("scalar")
}

fn long(store: &Store, q: &str) -> i64 {
    match scalar(store, q) {
        Some(Binding::Scalar(TypedValue::Long(n))) => n,
        r => panic!("{q}: {r:?}"),
    }
}

fn entid(store: &Store, q: &str) -> i64 {
    match scalar(store, q) {
        Some(Binding::Scalar(TypedValue::Ref(e))) => e,
        r => panic!("{q}: {r:?}"),
    }
}

/// A `:db.unique/identity` attribute that is also `:db/fulltext`: an upsert of
/// an existing title and a lookup-ref by title resolve to the existing entity.
#[test]
fn test_fulltext_identity_upsert_and_lookup_ref() {
    let mut s = Store::open("").unwrap();
    s.transact(
        r#"[{:db/ident :doc/title :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true :db/fulltext true}
            {:db/ident :doc/body :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/index true :db/fulltext true}
            {:db/ident :doc/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
            {:db/ident :ref/doc :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]"#,
    )
    .unwrap();
    // "gamma" is also another attribute's fulltext text: same fulltext_values row.
    s.transact(
        r#"[{:doc/title "alpha beta" :doc/n 1} {:doc/title "gamma" :doc/n 2} {:doc/body "gamma"}]"#,
    )
    .unwrap();
    let alpha = entid(&s, r#"[:find ?e . :where [?e :doc/title "alpha beta"]]"#);
    let gamma = entid(&s, r#"[:find ?e . :where [?e :doc/title "gamma"]]"#);

    // Upsert: same entity.
    let r = s
        .transact(r#"[{:db/id "a" :doc/title "alpha beta" :doc/n 10}]"#)
        .unwrap();
    assert_eq!(r.tempids["a"], alpha);
    assert_eq!(long(&s, "[:find (count ?e) . :where [?e :doc/n _]]"), 2);
    assert_eq!(
        long(
            &s,
            r#"[:find ?n . :where [?e :doc/title "alpha beta"] [?e :doc/n ?n]]"#
        ),
        10
    );

    // Lookup-ref by title.
    s.transact(r#"[{:ref/doc (lookup-ref :doc/title "gamma")}]"#)
        .unwrap();
    assert_eq!(entid(&s, "[:find ?e . :where [_ :ref/doc ?e]]"), gamma);
    assert_eq!(
        long(&s, "[:find ?n . :where [?x :ref/doc ?e] [?e :doc/n ?n]]"),
        2
    );

    // A text no title has does not resolve, even though fulltext_values has it.
    s.transact(r#"[{:doc/body "only a body"}]"#).unwrap();
    let e = s
        .transact(r#"[{:ref/doc (lookup-ref :doc/title "only a body")}]"#)
        .expect_err("no such title");
    assert!(e.to_string().contains("lookup"), "{e}");
}

/// The plain arm: an identity upsert and a lookup-ref on a non-fulltext
/// attribute, in a store that also has fulltext values.
#[test]
fn test_identity_upsert_and_lookup_ref() {
    let mut s = Store::open("").unwrap();
    s.transact(
        r#"[{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true}
            {:db/ident :u/id :db/valueType :db.type/long :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true}
            {:db/ident :u/bio :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/index true :db/fulltext true}
            {:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
            {:db/ident :u/friend :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}]"#,
    )
    .unwrap();
    let r = s
        .transact(r#"[{:db/id "x" :u/email "x@e" :u/id 7 :u/bio "x@e"}]"#)
        .unwrap();
    let x = r.tempids["x"];

    let r = s
        .transact(r#"[{:db/id "x" :u/email "x@e" :u/n 1}]"#)
        .unwrap();
    assert_eq!(r.tempids["x"], x);
    let r = s.transact(r#"[{:db/id "x" :u/id 7 :u/n 2}]"#).unwrap();
    assert_eq!(r.tempids["x"], x);
    // The long 7 does not match a string "7" or a ref 7.
    let r = s.transact(r#"[{:db/id "y" :u/email "7"}]"#).unwrap();
    assert_ne!(r.tempids["y"], x);

    s.transact(r#"[{:u/email "z@e" :u/friend (lookup-ref :u/email "x@e")}]"#)
        .unwrap();
    s.transact(r#"[[:db/add (lookup-ref :u/id 7) :u/n 3]]"#)
        .unwrap();
    assert_eq!(
        entid(
            &s,
            r#"[:find ?f . :where [?z :u/email "z@e"] [?z :u/friend ?f]]"#
        ),
        x
    );
    assert_eq!(
        long(&s, r#"[:find ?n . :where [?e :u/id 7] [?e :u/n ?n]]"#),
        3
    );
    assert_eq!(long(&s, "[:find (count ?e) . :where [?e :u/email _]]"), 3);
}

/// Repeated (a, v) pairs in one batch: through the transactor, and straight
/// into `resolve_avs`, whose map must equal the deduplicated lookups.
#[test]
fn test_duplicate_pairs_in_one_batch() {
    let mut s = Store::open("").unwrap();
    s.transact(
        r#"[{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true}
            {:db/ident :u/title :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true :db/fulltext true}
            {:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
            {:db/ident :u/m :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]"#,
    )
    .unwrap();
    let r = s
        .transact(r#"[{:db/id "a" :u/email "a@e" :u/title "t a"} {:db/id "b" :u/email "b@e"}]"#)
        .unwrap();
    let (a, b) = (r.tempids["a"], r.tempids["b"]);

    // Two tempids upserting through the same pairs, plus repeated lookup-refs.
    let r = s
        .transact(
            r#"[{:db/id "p" :u/email "a@e" :u/title "t a"} {:db/id "q" :u/email "a@e" :u/n 1}
                [:db/add (lookup-ref :u/email "b@e") :u/n 2]
                [:db/add (lookup-ref :u/email "b@e") :u/m 3]
                [:db/add (lookup-ref :u/title "t a") :u/m 4]]"#,
        )
        .unwrap();
    assert_eq!((r.tempids["p"], r.tempids["q"]), (a, a));
    assert_eq!(
        long(
            &s,
            r#"[:find ?n . :where [?e :u/email "a@e"] [?e :u/n ?n]]"#
        ),
        1
    );
    assert_eq!(
        long(
            &s,
            r#"[:find ?n . :where [?e :u/email "b@e"] [?e :u/n ?n]]"#
        ),
        2
    );
    assert_eq!(
        long(
            &s,
            r#"[:find ?n . :where [?e :u/email "b@e"] [?e :u/m ?n]]"#
        ),
        3
    );
    assert_eq!(
        long(
            &s,
            r#"[:find ?n . :where [?e :u/email "a@e"] [?e :u/m ?n]]"#
        ),
        4
    );

    let email = s
        .conn()
        .current_schema()
        .get_entid(&Keyword::namespaced("u", "email"))
        .unwrap()
        .0;
    let title = s
        .conn()
        .current_schema()
        .get_entid(&Keyword::namespaced("u", "title"))
        .unwrap()
        .0;
    let pa: AVPair = (email, TypedValue::typed_string("a@e"));
    let pa2: AVPair = (email, TypedValue::typed_string("a@e"));
    let pb: AVPair = (email, TypedValue::typed_string("b@e"));
    let pt: AVPair = (title, TypedValue::typed_string("t a"));
    let none: AVPair = (email, TypedValue::typed_string("nobody"));
    let avs = [&pa, &pb, &pa2, &none, &pt, &pb, &pt];
    let m = s.sqlite_ref().resolve_avs(&avs).unwrap();
    assert_eq!(m.len(), 3);
    assert_eq!((m[&pa], m[&pa2], m[&pb], m[&pt]), (a, a, b, a));
    assert!(!m.contains_key(&none));
}

/// Fill the store to `n` `:cell/key` datoms and `n / 10` fulltext datoms,
/// shaped as the transactor writes them, fast (raw SQL).
fn grow(s: &Store, key: i64, title: i64, from: usize, n: usize) {
    s.sqlite_ref()
        .execute_batch(&format!(
            "WITH RECURSIVE i(x) AS (SELECT {from} UNION ALL SELECT x + 1 FROM i WHERE x + 1 < {n})
             INSERT INTO datoms (e, a, v, tx, value_type_tag, index_avet, unique_value)
             SELECT 1000000 + x, {key}, 'cell ' || x, 268435457, 10, 1, 1 FROM i;
             WITH RECURSIVE i(x) AS (SELECT {f} UNION ALL SELECT x + 1 FROM i WHERE x + 1 < {t})
             INSERT INTO fulltext_values (text) SELECT 'title ' || x FROM i;
             INSERT INTO datoms (e, a, v, tx, value_type_tag, index_avet, index_fulltext, unique_value)
             SELECT 3000000 + rowid, {title}, rowid, 268435457, 10, 1, 1, 1 FROM fulltext_values
              WHERE rowid NOT IN (SELECT v FROM datoms WHERE a = {title});",
            f = from / 10,
            t = n / 10,
        ))
        .unwrap();
}

/// SQLite VM steps (progress-handler calls at N = 1) for one `resolve_avs` of
/// `pairs`, and its time, for information only. A first call prepares and
/// caches the statement, so only execution counts.
fn resolve_ops(s: &Store, pairs: &[AVPair]) -> (u64, Duration) {
    let refs: Vec<&AVPair> = pairs.iter().collect();
    let c = s.sqlite_ref();
    assert_eq!(c.resolve_avs(&refs).unwrap().len(), pairs.len());
    let ops = Arc::new(AtomicU64::new(0));
    let o = Arc::clone(&ops);
    c.progress_handler(
        1,
        Some(move || {
            o.fetch_add(1, Ordering::Relaxed);
            false
        }),
    )
    .unwrap();
    let start = Instant::now();
    let m = c.resolve_avs(&refs).unwrap();
    let t = start.elapsed();
    c.progress_handler(0, None::<fn() -> bool>).unwrap();
    assert_eq!(m.len(), pairs.len());
    (ops.load(Ordering::Relaxed), t)
}

/// VM steps for the pairs `pairs(key, title)` builds, at ~11k datoms (1k
/// fulltext values) and at ~110k (10k).
fn ops_small_big(pairs: impl Fn(i64, i64) -> Vec<AVPair>) -> (u64, u64) {
    let mut s = Store::open("").unwrap();
    s.transact(
        r#"[{:db/ident :cell/key :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true}
            {:db/ident :doc/title :db/valueType :db.type/string :db/cardinality :db.cardinality/one
             :db/unique :db.unique/identity :db/index true :db/fulltext true}]"#,
    )
    .unwrap();
    let schema = s.conn().current_schema();
    let key = schema
        .get_entid(&Keyword::namespaced("cell", "key"))
        .unwrap()
        .0;
    let title = schema
        .get_entid(&Keyword::namespaced("doc", "title"))
        .unwrap()
        .0;
    let pairs = pairs(key, title);

    grow(&s, key, title, 0, 10_000);
    let (small, ts) = resolve_ops(&s, &pairs);
    grow(&s, key, title, 10_000, 100_000);
    let n: i64 = s
        .sqlite_ref()
        .query_row("SELECT count(*) FROM datoms", [], |r| r.get(0))
        .unwrap();
    assert!(n > 100_000, "{n} datoms");
    let (big, tb) = resolve_ops(&s, &pairs);
    println!(
        "{} lookups: {small} VM steps ({ts:?}) at ~11k datoms, {big} ({tb:?}) at {n}",
        pairs.len()
    );
    (small, big)
}

/// 500 existing keys resolve with the same work at ~11k and ~110k datoms.
/// Counted in VM steps, not time, so a loaded machine can't fail it. Before
/// the fix SQLite scanned the AVET index (and probed FTS per datom of the
/// attribute) for every statement: 9.6x here.
#[test]
fn test_resolve_avs_does_not_scale_with_store() {
    // Keys present at both sizes, spread over the first 10k.
    let (small, big) = ops_small_big(|key, _| {
        (0..500)
            .map(|i| (key, TypedValue::typed_string(format!("cell {}", i * 20))))
            .collect()
    });
    assert!(2 * big < 3 * small, "{small} -> {big} VM steps");
}
