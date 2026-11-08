// Two `Store`s on one file: each must see what the other committed.
//
// A `Store` used to read its partition map (next entid, next tx id) and
// schema once at open. Another connection's commits then went unnoticed:
// the next transaction reused a tx id and an entid the other connection had
// already used (so one write silently replaced the other), and attributes
// the other connection installed were unknown (writes failed, queries
// returned nothing). Reported from Improv, 2026-11-07.

use mentat::{kw, Binding, HasSchema, Queryable, Store, TypedValue};

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

/// Calls that read a Store's metadata without running a query: they must see
/// another Store's schema and transactions too.
#[test]
fn schema_and_tx_accessors_see_other_stores() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap();
    fresh(path);

    let mut a = Store::open(path).unwrap();
    let mut b = Store::open(path).unwrap();
    let r = b
        .transact(
            "[{:db/ident :t/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
        )
        .unwrap();
    assert_eq!(a.last_tx_id(), r.tx_id);
    assert!(a
        .current_schema()
        .unwrap()
        .get_entid(&kw!(:t/age))
        .is_some());
    // Store::cache resolves the attribute through the schema.
    a.cache(&kw!(:t/age), mentat::CacheDirection::Forward)
        .unwrap();
    // QueryBuilder resolves a keyword input through the schema.
    let out = mentat::QueryBuilder::new(&mut a, "[:find ?e . :in ?a :where [?e :db/ident ?a]]")
        .bind_ref_from_kw("?a", kw!(:t/age))
        .map(|_| ())
        .err();
    assert!(out.is_none(), "{out:?}");
}

/// One Conn used with two SQLite connections to the file: each connection's
/// data_version counts separately, so a version recorded on one says
/// nothing about the other. Conn must reload when the connection changes.
#[test]
fn one_conn_over_two_sqlite_connections() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap();
    fresh(path);

    let mut c1 = mentat::new_connection(path).unwrap();
    let c2 = mentat::new_connection(path).unwrap();
    let conn = mentat::Conn::connect(&mut c1).unwrap();
    // Another Store moves c1's and c2's versions by different amounts.
    let mut w = Store::open(path).unwrap();
    for i in 0..3 {
        w.transact(&format!(r#"[{{:t/name "w{i}"}}]"#)).unwrap();
        // c2 reads in between, so its count runs behind c1's.
        if i == 0 {
            conn.q_once(&c2, "[:find ?e :where [?e :t/name _]]", None)
                .unwrap();
        }
    }
    w.transact(
        "[{:db/ident :t/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
    )
    .unwrap();
    w.transact(r#"[{:t/age 1}]"#).unwrap();
    let n = conn
        .q_once(&c1, "[:find ?e :where [?e :t/age _]]", None)
        .unwrap()
        .results
        .into_rel()
        .unwrap()
        .row_count();
    assert_eq!(n, 1);
}

/// Another connection commits in the instant after A's COMMIT, before A
/// does anything else. If A read its data_version after COMMIT, it would take
/// that commit as already seen and reuse its ids on the next write.
#[test]
fn a_commit_right_after_ours_is_not_missed() {
    use std::sync::Mutex;
    // trace_v2 takes a plain fn, so its state lives in a static: the path to
    // write to, and whether A's COMMIT has started.
    static INJECT: Mutex<Option<(String, bool)>> = Mutex::new(None);
    fn on_stmt(e: rusqlite::trace::TraceEvent<'_>) {
        let rusqlite::trace::TraceEvent::Stmt(_, sql) = e else {
            return;
        };
        let mut g = INJECT.lock().unwrap();
        match g.as_mut() {
            Some((_, seen)) if !*seen => *seen = sql.trim().eq_ignore_ascii_case("COMMIT"),
            Some(_) => {
                // The first statement A runs after COMMIT: the lock is free.
                let (path, _) = g.take().unwrap();
                let mut b = Store::open(&path).unwrap();
                b.transact(r#"[{:db/id "b" :t/name "from B"}]"#).unwrap();
            }
            None => {}
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.db");
    let path = path.to_str().unwrap();
    fresh(path);
    let mut a = Store::open(path).unwrap();
    a.transact(r#"[{:t/name "warm"}]"#).unwrap();

    a.sqlite_mut().trace_v2(
        rusqlite::trace::TraceEventCodes::SQLITE_TRACE_STMT,
        Some(on_stmt),
    );
    *INJECT.lock().unwrap() = Some((path.to_string(), false));
    a.transact(r#"[{:db/id "a" :t/name "from A"}]"#).unwrap();
    // B runs at A's next statement at the latest: the next transact's BEGIN.
    a.transact(r#"[{:db/id "c" :t/name "from A again"}]"#)
        .unwrap();
    a.sqlite_mut()
        .trace_v2(rusqlite::trace::TraceEventCodes::empty(), None);
    assert!(INJECT.lock().unwrap().is_none(), "B never ran");

    let c = Store::open(path).unwrap();
    assert_eq!(shared_tx_ids(&c), 0, "A's next write reused B's tx id");
    assert_eq!(names(&c).len(), 4, "a write was lost");
}
