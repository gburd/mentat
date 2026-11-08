// Two `Store`s on one file: each must see what the other committed.
//
// A `Store` used to read its partition map (next entid, next tx id) and
// schema once at open. Another connection's commits then went unnoticed:
// the next transaction reused a tx id and an entid the other connection had
// already used (so one write silently replaced the other), and attributes
// the other connection installed were unknown (writes failed, queries
// returned nothing). Reported from Improv, 2026-11-07.

use mentat::{kw, Binding, Queryable, Store, TypedValue};

fn names(s: &Store) -> Vec<(i64, String)> {
    let r = s
        .q_once("[:find ?e ?n :where [?e :t/name ?n]]", None)
        .unwrap()
        .results;
    let mut out: Vec<(i64, String)> = r
        .into_rel()
        .unwrap()
        .into_iter()
        .map(|row| {
            let e = match &row[0] {
                Binding::Scalar(TypedValue::Ref(e)) => *e,
                b => panic!("{b:?}"),
            };
            let n = match &row[1] {
                Binding::Scalar(TypedValue::String(n)) => n.to_string(),
                b => panic!("{b:?}"),
            };
            (e, n)
        })
        .collect();
    out.sort();
    out
}

/// The tx entities with more than one `:db/txInstant` ever asserted: a tx id
/// two transactions both used.
fn shared_tx_ids(s: &Store) -> i64 {
    s.sqlite_ref()
        .query_row(
            "SELECT count(*) FROM (SELECT e FROM timelined_transactions \
             WHERE a = 3 AND added = 1 GROUP BY e HAVING count(*) > 1)",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

fn fresh(path: &str) {
    let mut s = Store::open(path).unwrap();
    s.transact(
        "[{:db/ident :t/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
    )
    .unwrap();
}

#[test]
fn two_stores_on_one_file_get_distinct_ids() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap();
    fresh(path);

    let mut a = Store::open(path).unwrap();
    let mut b = Store::open(path).unwrap();
    let rb = b.transact(r#"[{:db/id "x" :t/name "from B"}]"#).unwrap();
    let ra = a.transact(r#"[{:db/id "x" :t/name "from A"}]"#).unwrap();
    assert_ne!(ra.tx_id, rb.tx_id, "one tx id for two transactions");
    assert_ne!(
        ra.tempids["x"], rb.tempids["x"],
        "one entid for two entities"
    );
    assert_eq!(a.last_tx_id(), ra.tx_id);

    let c = Store::open(path).unwrap();
    assert_eq!(
        names(&c),
        vec![
            (rb.tempids["x"], "from B".to_string()),
            (ra.tempids["x"], "from A".to_string())
        ],
        "both writes survive"
    );
    assert_eq!(shared_tx_ids(&c), 0, "no tx entity holds two txInstants");
    // B, which wrote first, sees A's write too.
    assert_eq!(names(&b).len(), 2);
}

#[test]
fn an_attribute_another_store_installed_is_usable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap();
    fresh(path);

    let mut a = Store::open(path).unwrap();
    let mut b = Store::open(path).unwrap();
    b.transact(
        "[{:db/ident :t/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
    )
    .unwrap();
    // A query first: it used to find nothing, without an error.
    b.transact(r#"[{:db/id "z" :t/age 3}]"#).unwrap();
    let found = a
        .q_once("[:find ?e :where [?e :t/age _]]", None)
        .unwrap()
        .results
        .into_rel()
        .unwrap()
        .row_count();
    assert_eq!(found, 1, "A's query sees B's attribute and datom");
    // Then a write: it used to fail with "no entid found for ident".
    let r = a.transact(r#"[{:db/id "y" :t/age 7}]"#).unwrap();
    assert!(r.tempids.contains_key("y"));
    let ages = a
        .q_once("[:find ?a :where [_ :t/age ?a]]", None)
        .unwrap()
        .results
        .into_rel()
        .unwrap()
        .row_count();
    assert_eq!(ages, 2);
}

#[test]
fn a_cached_attribute_sees_another_stores_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap();
    fresh(path);

    let x = {
        let mut s = Store::open(path).unwrap();
        s.transact(r#"[{:db/id "x" :t/name "before"}]"#)
            .unwrap()
            .tempids["x"]
    };
    let mut a = Store::open(path).unwrap();
    a.cache(&kw!(:t/name), mentat::CacheDirection::Forward)
        .unwrap();
    assert_eq!(names(&a), vec![(x, "before".to_string())]);
    // Another handle, opened after the first write, replaces the value.
    let mut b = Store::open(path).unwrap();
    b.transact(&format!(r#"[[:db/add {x} :t/name "after"]]"#))
        .unwrap();
    // A's cached value must follow; lookup reads the cache directly.
    let v = a.lookup_value_for_attribute(x, &kw!(:t/name)).unwrap();
    assert_eq!(v, Some(TypedValue::String("after".to_string().into())));
    assert_eq!(names(&a), vec![(x, "after".to_string())]);
}

/// The second writer in another process, as in the field: the test binary
/// runs itself with MENTAT_STALE_CHILD set.
#[test]
fn a_writer_in_another_process() {
    if let Ok(path) = std::env::var("MENTAT_STALE_CHILD") {
        let mut b = Store::open(&path).unwrap();
        let r = b
            .transact(r#"[{:db/id "x" :t/name "from the child"}]"#)
            .unwrap();
        println!("CHILD tx {} entid {}", r.tx_id, r.tempids["x"]);
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap().to_string();
    fresh(&path);

    let mut a = Store::open(&path).unwrap();
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["a_writer_in_another_process", "--exact", "--nocapture"])
        .env("MENTAT_STALE_CHILD", &path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let ra = a
        .transact(r#"[{:db/id "x" :t/name "from the parent"}]"#)
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    let child = stdout
        .lines()
        .find(|l| l.starts_with("CHILD"))
        .expect("child line");
    assert!(
        !child.contains(&format!("tx {} ", ra.tx_id)),
        "same tx id: {child} / parent {}",
        ra.tx_id
    );
    let c = Store::open(&path).unwrap();
    assert_eq!(names(&c).len(), 2, "both writes survive");
    assert_eq!(shared_tx_ids(&c), 0);
}
