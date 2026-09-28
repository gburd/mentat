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
            "INSERT INTO mentat_managed_indexes VALUES ('idx_datoms_aevt', 1, 0, 0, 'adaptive'),
                                                       ('idx_datoms_eavt', 1, 0, 0, 'schema')",
            [],
        )
        .unwrap();
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    assert!(indexes(&s).contains(&"idx_datoms_aevt".to_string()));
    assert!(indexes(&s).contains(&"idx_datoms_eavt".to_string()));
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

// ---------------------------------------------------------------------------
// Schema value indexes: :db/index / :db/unique get a usable index by default.
// ---------------------------------------------------------------------------

fn email_plan(s: &Store) -> String {
    let q = "[:find ?e . :in ?m :where [?e :u/email ?m]]";
    let i = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?m"),
        TypedValue::typed_string("a@x"),
    )]);
    match s.q_explain(q, i).expect("explain") {
        QueryExplanation::ExecutionPlan { steps, .. } => steps
            .iter()
            .map(|s| s.detail.clone())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => panic!("expected a plan"),
    }
}

fn users(s: &mut Store, attr: &str) -> i64 {
    s.transact(&format!(
        r#"[{{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one {attr}}}
            {{:db/ident :u/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}}]"#
    ))
    .expect("schema");
    s.transact(r#"[{:u/email "a@x" :u/name "A"} {:u/email "b@x" :u/name "B"}]"#)
        .expect("users");
    mentat::HasSchema::get_entid(
        &*s.conn().current_schema(),
        &mentat::Keyword::namespaced("u", "email"),
    )
    .unwrap()
    .0
}

fn kind(s: &Store, a: i64) -> Option<String> {
    s.sqlite_ref()
        .query_row(
            "SELECT kind FROM mentat_managed_indexes WHERE a = ?",
            [a],
            |r| r.get(0),
        )
        .ok()
}

#[test]
fn test_unique_attribute_gets_a_usable_index_in_default_mode() {
    let mut s = Store::open("").unwrap();
    let a = users(&mut s, ":db/unique :db.unique/identity :db/index true");
    let idx = format!("idx_auto_avet_{a}");
    assert!(indexes(&s).contains(&idx), "{:?}", indexes(&s));
    assert_eq!(kind(&s, a).as_deref(), Some("schema"));
    let p = email_plan(&s);
    assert!(
        p.contains(&format!("USING COVERING INDEX {idx} (a=? AND v=?)")),
        "{p}"
    );
    // :db.unique/value too. Plain attributes (:u/name) get nothing.
    let mut s = Store::open("").unwrap();
    let a = users(&mut s, ":db/unique :db.unique/value :db/index true");
    assert_eq!(kind(&s, a).as_deref(), Some("schema"));
    let auto: Vec<_> = indexes(&s)
        .into_iter()
        .filter(|i| i.starts_with("idx_auto_avet_"))
        .collect();
    assert_eq!(auto, vec![format!("idx_auto_avet_{a}")]);
}

#[test]
fn test_schema_index_follows_the_flag_and_survives_adaptive() {
    let mut s = Store::open("").unwrap();
    let a = users(&mut s, ":db/unique :db.unique/identity :db/index true");
    let idx = format!("idx_auto_avet_{a}");
    assert!(indexes(&s).contains(&idx));
    // Adaptive vestigial drops leave a schema index alone while the flag is set.
    s.set_auto_index(AutoIndex::Adaptive);
    s.set_index_tuning(1, Duration::from_secs(0));
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    assert!(indexes(&s).contains(&idx));
    // So does Schema mode's cleanup of adaptive indexes.
    s.set_auto_index(AutoIndex::Schema);
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    assert!(indexes(&s).contains(&idx));
    // Retracting :db/unique drops it.
    s.transact(r#"[[:db/retract :u/email :db/unique :db.unique/identity]]"#)
        .expect("retract unique");
    assert!(!indexes(&s).contains(&idx), "{:?}", indexes(&s));
    assert_eq!(kind(&s, a), None);
    // And back.
    s.transact(r#"[[:db/add :u/email :db/unique :db.unique/value]]"#)
        .expect("unique again");
    assert!(indexes(&s).contains(&idx));
}

fn owners(s: &mut Store, extra: &str) -> (i64, i64) {
    s.transact(&format!(
        r#"[{{:db/ident :t/owner :db/valueType :db.type/ref :db/cardinality :db.cardinality/one {extra}}}
            {{:db/ident :t/state :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one :db/index true}}]"#
    ))
    .unwrap();
    let schema = s.conn().current_schema();
    let id = |n: &str| {
        mentat::HasSchema::get_entid(&*schema, &mentat::Keyword::namespaced("t", n))
            .unwrap()
            .0
    };
    (id("owner"), id("state"))
}

#[test]
fn test_ref_index_follows_db_index() {
    let mut s = Store::open("").unwrap();
    let (owner, state) = owners(&mut s, ":db/index true");
    // A ref with :db/index gets one; a scalar enum doesn't (see wants_schema_index).
    assert_eq!(kind(&s, owner).as_deref(), Some("schema"));
    assert_eq!(kind(&s, state), None);
    // Removing :db/index drops it.
    s.transact(r#"[[:db/add :t/owner :db/index false]]"#)
        .expect("alter");
    assert_eq!(kind(&s, owner), None);
    assert!(!indexes(&s).contains(&format!("idx_auto_avet_{owner}")));
}

#[test]
fn test_adaptive_index_is_adopted_when_the_flag_is_set() {
    let mut s = Store::open("").unwrap();
    let (a, _) = owners(&mut s, "");
    let mut tx = String::from("[");
    for i in 0..100 {
        tx.push_str(&format!(r#"{{:db/id "o{i}" :t/owner "o{i}"}}"#));
    }
    tx.push(']');
    let r = s.transact(&tx).unwrap(); // selective enough to keep an adaptive index
    let o = r.tempids["o7"];
    s.set_auto_index(AutoIndex::Adaptive);
    s.set_index_tuning(1, Duration::from_secs(0));
    s.q_once(&format!("[:find ?e . :where [?e :t/owner {o}]]"), None)
        .unwrap(); // K = 1: an adaptive index
    assert_eq!(kind(&s, a).as_deref(), Some("adaptive"));
    s.transact(r#"[[:db/add :t/owner :db/index true]]"#)
        .unwrap();
    assert_eq!(kind(&s, a).as_deref(), Some("schema"));
    // Now idle, but not dropped: the flag keeps it.
    assert_eq!(s.tune_indexes(false).unwrap(), vec![]);
    assert!(indexes(&s).contains(&format!("idx_auto_avet_{a}")));
}

#[test]
fn test_upgraded_store_gets_schema_indexes() {
    let dir = std::env::temp_dir().join(format!("mentat-schema-idx-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    for from in [1, 2] {
        let path = dir.join(format!("v{from}.db"));
        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
        }
        let path = path.to_str().unwrap().to_string();
        let a = {
            let mut s = Store::open(&path).unwrap();
            users(&mut s, ":db/unique :db.unique/identity :db/index true")
        };
        // Make it look like an old store: no registry, no schema index.
        let sql = if from == 1 {
            "DROP INDEX idx_transactions_aevt; ALTER TABLE known_parts DROP COLUMN idx;"
        } else {
            ""
        };
        rusqlite::Connection::open(&path)
            .unwrap()
            .execute_batch(&format!(
                "{sql} DROP INDEX idx_auto_avet_{a}; DROP TABLE mentat_managed_indexes;
                 PRAGMA user_version = {from};"
            ))
            .unwrap();
        let s = Store::open(&path).expect("upgrade");
        assert!(
            indexes(&s).contains(&format!("idx_auto_avet_{a}")),
            "v{from}"
        );
        assert_eq!(kind(&s, a).as_deref(), Some("schema"));
        let p = email_plan(&s);
        assert!(p.contains(&format!("idx_auto_avet_{a}")), "v{from}: {p}");
    }
}

/// A schema index created with its attribute (before any data) is ANALYZEd
/// as empty; stale "0 0 0 0" statistics made the planner start joins at it.
/// Opening (or tuning) the store refreshes them once there is data.
#[test]
fn test_empty_schema_index_stats_are_refreshed() {
    let dir = std::env::temp_dir().join(format!("mentat-stale-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("s.db");
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", path.display()));
    }
    let path = path.to_str().unwrap().to_string();
    let stat = |s: &Store, a: i64| -> String {
        s.sqlite_ref()
            .query_row(
                "SELECT stat FROM sqlite_stat1 WHERE idx = ?",
                [format!("idx_auto_avet_{a}")],
                |r| r.get(0),
            )
            .unwrap()
    };
    let a = {
        let mut s = Store::open(&path).unwrap();
        s.transact(
            r#"[{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true}]"#,
        )
        .unwrap();
        let a = mentat::HasSchema::get_entid(
            &*s.conn().current_schema(),
            &mentat::Keyword::namespaced("u", "email"),
        )
        .unwrap()
        .0;
        assert!(stat(&s, a).starts_with("0 "), "{}", stat(&s, a));
        s.transact(r#"[{:u/email "a"} {:u/email "b"} {:u/email "c"}]"#)
            .unwrap();
        a
    };
    let mut s = Store::open(&path).unwrap();
    assert_eq!(stat(&s, a), "3 3 1 1");
    // And via tune_indexes on a live store.
    s.transact(r#"[{:db/ident :u/code :db/valueType :db.type/long :db/cardinality :db.cardinality/one :db/unique :db.unique/value :db/index true}]"#)
        .unwrap();
    s.transact(r#"[{:u/code 1} {:u/code 2}]"#).unwrap();
    let c = mentat::HasSchema::get_entid(
        &*s.conn().current_schema(),
        &mentat::Keyword::namespaced("u", "code"),
    )
    .unwrap()
    .0;
    s.tune_indexes(false).unwrap();
    assert_eq!(stat(&s, c), "2 2 1 1");
}
