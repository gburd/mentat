// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Adaptive per-attribute value indexes (`mentat::auto_index`).

use std::time::Duration;

use mentat::{
    AutoIndex, IndexAction, QueryExplanation, QueryInputs, Queryable, Store, TypedValue, Variable,
};

const Q: &str = "[:find ?e :in ?n :where [?e :p/name ?n]]";

fn store() -> Store {
    let mut s = Store::open("").expect("open");
    s.transact(
        r#"[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
            {:db/ident :p/age  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}]"#,
    )
    .expect("schema");
    s.transact(r#"[{:p/name "a" :p/age 1} {:p/name "b" :p/age 2}]"#)
        .expect("data");
    // Enough distinct names that an index on them is selective.
    let mut tx = String::from("[");
    for i in 0..100 {
        tx.push_str(&format!(r#"{{:p/name "n{i}"}}"#));
    }
    tx.push(']');
    s.transact(&tx).expect("names");
    s
}

fn by_name(s: &Store, n: &str) -> usize {
    let i = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?n"),
        TypedValue::typed_string(n),
    )]);
    s.q_once(Q, i).expect("q").results.len()
}

fn plan(s: &Store) -> String {
    let i = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?n"),
        TypedValue::typed_string("a"),
    )]);
    match s.q_explain(Q, i).expect("explain") {
        QueryExplanation::ExecutionPlan { steps, .. } => steps
            .iter()
            .map(|s| s.detail.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => panic!("expected a plan"),
    }
}

fn indexes(s: &Store) -> Vec<String> {
    let mut st = s
        .sqlite_ref()
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'datoms' ORDER BY name")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

#[test]
fn test_adaptive_creates_after_k_and_planner_uses_it() {
    let mut s = store();
    s.set_auto_index(AutoIndex::Adaptive);
    s.set_index_tuning(3, Duration::from_secs(3600));
    let core = indexes(&s);
    assert!(!plan(&s).contains("idx_auto_avet_"));

    assert_eq!(by_name(&s, "a"), 1);
    assert_eq!(by_name(&s, "b"), 1);
    // Unfiltered and placeholder-valued patterns don't count.
    s.q_once("[:find ?e ?n :where [?e :p/name ?n]]", None)
        .unwrap();
    assert_eq!(
        s.tune_indexes(true).unwrap(),
        vec![],
        "below K: nothing to do"
    );
    assert_eq!(by_name(&s, "zz"), 0); // the 3rd: tuning runs automatically

    let auto: Vec<_> = indexes(&s)
        .into_iter()
        .filter(|i| !core.contains(i))
        .collect();
    assert_eq!(auto.len(), 1, "{auto:?}");
    assert!(auto[0].starts_with("idx_auto_avet_"));
    let p = plan(&s);
    assert!(
        p.contains(&format!("USING COVERING INDEX {} (a=? AND v=?)", auto[0])),
        "{p}"
    );
    // Results are unchanged.
    assert_eq!(by_name(&s, "a"), 1);
    let n: i64 = s
        .sqlite_ref()
        .query_row("SELECT count(*) FROM mentat_managed_indexes", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(n, 1);
    // It has planner statistics, but no STAT4 samples (they slow every prepare).
    let c = s.sqlite_ref();
    let stat1: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_stat1 WHERE idx = ?",
            [&auto[0]],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(stat1, 1);
    let stat4: i64 = c
        .query_row(
            "SELECT count(*) FROM sqlite_stat4 WHERE idx = ?",
            [&auto[0]],
            |r| r.get(0),
        )
        .unwrap_or(0);
    assert_eq!(stat4, 0);
}

#[test]
fn test_drop_after_idle_window_only_mentat_indexes() {
    let mut s = store();
    s.set_auto_index(AutoIndex::Adaptive);
    s.set_index_tuning(1, Duration::from_secs(0));
    s.sqlite_ref()
        .execute_batch("CREATE INDEX my_own_idx ON datoms (tx)")
        .unwrap();
    by_name(&s, "a"); // K = 1: created
    let core = indexes(&s);
    assert!(core.iter().any(|i| i.starts_with("idx_auto_avet_")));

    // A period with no use and window 0: dropped. Nothing else is.
    let acts = s.tune_indexes(false).unwrap();
    assert!(matches!(&acts[..], [IndexAction::Drop { .. }]), "{acts:?}");
    let after = indexes(&s);
    assert!(!after.iter().any(|i| i.starts_with("idx_auto_avet_")));
    for i in [
        "idx_datoms_eavt",
        "idx_datoms_aevt",
        "idx_datoms_avet",
        "my_own_idx",
    ] {
        assert!(after.contains(&i.to_string()), "{i} dropped: {after:?}");
    }
    // A forged registry row naming a core index is ignored.
    s.sqlite_ref()
        .execute(
            "INSERT INTO mentat_managed_indexes VALUES ('idx_datoms_aevt', 1, 0, 0)",
            [],
        )
        .unwrap();
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    assert!(indexes(&s).contains(&"idx_datoms_aevt".to_string()));
}

#[test]
fn test_hysteresis_used_index_is_kept() {
    let mut s = store();
    s.set_auto_index(AutoIndex::Adaptive);
    s.set_index_tuning(2, Duration::from_secs(0));
    by_name(&s, "a");
    by_name(&s, "a"); // K = 2: created, new period
    by_name(&s, "b"); // used once in the new period (below K: no auto run)
    assert_eq!(
        s.tune_indexes(false).unwrap(),
        vec![],
        "used this period: kept"
    );
    // A long idle window: an unused period alone doesn't drop it.
    s.set_index_tuning(2, Duration::from_secs(3600));
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    // Window 0 and an unused period: dropped.
    s.set_index_tuning(2, Duration::from_secs(0));
    assert_eq!(s.tune_indexes(false).unwrap().len(), 1);
}

#[test]
fn test_unselective_index_is_rejected() {
    let mut s = store();
    s.set_auto_index(AutoIndex::Adaptive);
    s.set_index_tuning(1, Duration::from_secs(3600));
    // Every entity has age 1 or 2: 50% of the attribute per value.
    let mut tx = String::from("[");
    for i in 0..200 {
        tx.push_str(&format!("{{:p/age {}}}", 1 + i % 2));
    }
    tx.push(']');
    s.transact(&tx).unwrap();
    let q = "[:find ?e :where [?e :p/age 1]]";
    assert_eq!(s.q_once(q, None).unwrap().results.len(), 101);
    assert!(!indexes(&s).iter().any(|i| i.starts_with("idx_auto_avet_")));
    // Not retried.
    s.q_once(q, None).unwrap();
    assert_eq!(s.tune_indexes(true).unwrap(), vec![]);
}

#[test]
fn test_modes_off_and_schema() {
    let mut s = store();
    // Default is Schema: queries never create indexes.
    s.set_index_tuning(1, Duration::from_secs(3600));
    by_name(&s, "a");
    assert!(!indexes(&s).iter().any(|i| i.starts_with("idx_auto_avet_")));

    s.set_auto_index(AutoIndex::Adaptive);
    by_name(&s, "a");
    assert!(indexes(&s).iter().any(|i| i.starts_with("idx_auto_avet_")));
    // Off: tuning is a no-op.
    s.set_auto_index(AutoIndex::Off);
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    // Schema: drops what Adaptive created (dry run first).
    s.set_auto_index(AutoIndex::Schema);
    let dry = s.tune_indexes(true).unwrap();
    assert_eq!(dry.len(), 1);
    assert!(indexes(&s).iter().any(|i| i.starts_with("idx_auto_avet_")));
    assert_eq!(s.tune_indexes(false).unwrap(), dry);
    assert!(!indexes(&s).iter().any(|i| i.starts_with("idx_auto_avet_")));
}
