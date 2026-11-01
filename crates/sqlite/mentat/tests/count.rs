// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! `(count ?x)` compiles to the cheapest SQL that keeps Datalog set semantics:
//! plain `count` when the projected variables are a key of the join,
//! `count(DISTINCT ?x)` when only the counted variable can repeat, and an inner
//! `SELECT DISTINCT` otherwise (`:with`).

use mentat::{Binding, QueryExplanation, QueryResults, Queryable, Store, TypedValue};

fn store() -> Store {
    let mut s = Store::open("").expect("open");
    s.transact(
        r#"[{:db/ident :m/name  :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
            {:db/ident :m/heads :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
            {:db/ident :m/weapon :db/valueType :db.type/string :db/cardinality :db.cardinality/many}]"#,
    )
    .unwrap();
    s.transact(
        r#"[{:m/name "Medusa"   :m/heads 1 :m/weapon "Stony gaze"}
            {:m/name "Cyclops"  :m/heads 1 :m/weapon ["Large club" "Mighty arms" "Stompy feet"]}
            {:m/name "Chimera"  :m/heads 1 :m/weapon "Stony gaze"}
            {:m/name "Cerberus" :m/heads 3 :m/weapon ["Large club" "Deadly drool"]}]"#,
    )
    .unwrap();
    s
}

fn sql(s: &Store, q: &str) -> String {
    match s.q_explain(q, None).unwrap() {
        QueryExplanation::ExecutionPlan { query, .. } => query.sql,
        _ => panic!("expected a plan"),
    }
}

fn rows(s: &Store, q: &str) -> Vec<Vec<TypedValue>> {
    match s.q_once(q, None).unwrap().results {
        QueryResults::Rel(r) => r
            .into_iter()
            .map(|row| row.into_iter().map(|b| b.into_scalar().unwrap()).collect())
            .collect(),
        QueryResults::Scalar(Some(Binding::Scalar(v))) => vec![vec![v]],
        r => panic!("{r:?}"),
    }
}

fn long(i: i64) -> TypedValue {
    TypedValue::Long(i)
}

#[test]
fn test_count_key_is_plain_count() {
    let s = store();
    // ?m is a key of a card-one pattern: no DISTINCT anywhere.
    let q = "[:find ?h (count ?m) :order ?h :where [?m :m/heads ?h]]";
    assert!(!sql(&s, q).contains("DISTINCT"), "{}", sql(&s, q));
    assert_eq!(
        rows(&s, q),
        vec![vec![long(1), long(3)], vec![long(3), long(1)]]
    );
    let q = "[:find (count ?m) . :where [?m :m/name _]]";
    assert!(!sql(&s, q).contains("DISTINCT"), "{}", sql(&s, q));
    assert_eq!(rows(&s, q), vec![vec![long(4)]]);
    // Two card-one patterns on ?m: still a key.
    let q = "[:find ?h (count ?m) :order ?h :where [?m :m/heads ?h] [?m :m/name _]]";
    assert!(!sql(&s, q).contains("DISTINCT"), "{}", sql(&s, q));
    assert_eq!(
        rows(&s, q),
        vec![vec![long(1), long(3)], vec![long(3), long(1)]]
    );
}

#[test]
fn test_count_over_counting_cases_stay_set_counts() {
    let s = store();
    // The value of a card-many attribute repeats across entities: a set count
    // (5 distinct weapons, not 7 datoms).
    let q = "[:find (count ?w) . :where [_ :m/weapon ?w]]";
    assert!(sql(&s, q).contains("count(DISTINCT"), "{}", sql(&s, q));
    assert_eq!(rows(&s, q), vec![vec![long(5)]]);
    // An extra card-many join multiplies rows per ?m: still one per monster.
    let q = "[:find ?h (count ?m) :order ?h :where [?m :m/heads ?h] [?m :m/weapon _]]";
    assert!(sql(&s, q).contains("count(DISTINCT"), "{}", sql(&s, q));
    assert_eq!(
        rows(&s, q),
        vec![vec![long(1), long(3)], vec![long(3), long(1)]]
    );
    // Heads: 1, 1, 1, 3 -> the set {1, 3}.
    let q = "[:find (count ?h) . :where [_ :m/heads ?h]]";
    assert_eq!(rows(&s, q), vec![vec![long(2)]]);
    // :with keeps the inner DISTINCT and counts per (?h, ?m): 4.
    let q = "[:find (count ?h) . :with ?m :where [?m :m/heads ?h]]";
    assert_eq!(rows(&s, q), vec![vec![long(4)]]);
    // Weapons per monster name, :with the monster.
    let q = "[:find ?n (count ?w) :with ?m :order ?n :where [?m :m/name ?n] [?m :m/weapon ?w]]";
    assert_eq!(
        rows(&s, q),
        vec![
            vec!["Cerberus".into(), long(2)],
            vec!["Chimera".into(), long(1)],
            vec!["Cyclops".into(), long(3)],
            vec!["Medusa".into(), long(1)],
        ]
    );
}

// #394: `(count-distinct ?x)` counts distinct values whatever else the query
// projects, as one key of value and type: it keeps a long apart from an equal
// double, a boolean or a ref, and two doubles that differ in the last bit.
#[test]
fn test_count_distinct() {
    let s = store();
    let one = |q: &str| rows(&s, q);
    // Medusa, Cyclops, Chimera 1 head; Cerberus 3: two distinct head counts.
    assert_eq!(
        one("[:find (count-distinct ?h) . :where [_ :m/heads ?h]]"),
        [[long(2)]]
    );
    // :with doesn't change it (count does: one per monster).
    assert_eq!(
        one("[:find (count-distinct ?h) (count ?h) :with ?m :where [?m :m/heads ?h]]"),
        [[long(2), long(4)]]
    );
    // Grouped: weapons per head count. 1 head: Stony gaze, Large club, Mighty
    // arms, Stompy feet; 3 heads: Large club, Deadly drool.
    let mut g = one("[:find ?h (count-distinct ?w) :where [?m :m/heads ?h] [?m :m/weapon ?w]]");
    g.sort_by_key(|r| format!("{r:?}"));
    assert_eq!(g, [[long(1), long(4)], [long(3), long(2)]]);
    // Over no rows: 0.
    assert_eq!(
        one("[:find (count-distinct ?h) . :where [_ :m/heads ?h] [(> ?h 9)]]"),
        [[long(0)]]
    );

    let mut s = Store::open("").expect("open");
    s.transact(
        r#"[{:db/ident :k/l :db/valueType :db.type/long :db/cardinality :db.cardinality/many}
            {:db/ident :k/d :db/valueType :db.type/double :db/cardinality :db.cardinality/many}
            {:db/ident :k/b :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}]"#,
    )
    .unwrap();
    let e = s
        .transact(r#"[{:db/id "x" :k/l [1 5] :k/d [5.0 0.3 0.30000000000000004] :k/b true}]"#)
        .unwrap()
        .tempids["x"];
    let one = |q: &str| rows(&s, q);
    // Every value of x: longs 1 5, doubles 5.0 0.3 0.30000000000000004,
    // boolean true (stored as 1): six.
    assert_eq!(
        one(&format!("[:find (count-distinct ?v) . :where [{e} ?a ?v]]")),
        [[long(6)]]
    );
    assert_eq!(
        one("[:find (count-distinct ?v) . :where (or [_ :k/l ?v] [_ :k/d ?v])]"),
        [[long(5)]]
    );
    assert_eq!(
        one("[:find (count-distinct ?v) . :where [_ :k/d ?v]]"),
        [[long(3)]]
    );
}
