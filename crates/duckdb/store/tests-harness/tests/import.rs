//! `DuckStore::import_sqlite`: a SQLite-format store (what the DuckDB
//! extension wrote before 1.11, written here by the SQLite engine) imported
//! into DuckDB answers the differential test's queries exactly as the SQLite
//! store does, now, as of and since every transaction, and keeps its tx ids,
//! tx instants, entids and partition high-water marks.

use duckdb::Connection;
use mentat::Queryable;
use mentat_duckdb_store::{failpoints, DuckStore, SqlConn};
use mentat_duckdb_store_tests::fixtures::{norm, QUERIES, TXS};
use mentat_duckdb_store_tests::DuckConn;
use mentat_query_algebrizer::TemporalBound;

/// Values at the edges of what the import must carry bit for bit.
const EDGES: &str = r#"[{:db/ident :x/d :db/valueType :db.type/double :db/cardinality :db.cardinality/many}
  {:db/ident :x/ft :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}
  {:db/ident :x/s :db/valueType :db.type/string :db/cardinality :db.cardinality/many}
  {:db/ident :x/l :db/valueType :db.type/long :db/cardinality :db.cardinality/many}]"#;

fn edge_data() -> String {
    let doubles = [
        "0.1",
        "0.0",
        "1.65",
        "4.9E-324",
        "-4.9E-324",
        "2.2250738585072014E-308",
        "2.225073858507201E-308",
        "1.7976931348623157E308",
        "-1.7976931348623157E308",
        "0.30000000000000004",
        "123456789.12345678",
        "1.0E-310",
        "6.02214076E23",
        "#f +Infinity",
        "#f -Infinity",
        "1.9999999999999998",
        "0.9999999999999999",
    ];
    format!(
        r#"[{{:db/id "e" :x/d [{}] :x/ft "fulltext text, 'quoted'"
             :x/s ["it's" "" "  spaced  " "ünïcödé ✓" "line\nbreak" "1.5" "42"]
             :x/l [9223372036854775807 -9223372036854775808 0]}}
            {{:db/id "f" :x/ft "fulltext text, 'quoted'"}}]"#,
        doubles.join(" ")
    )
}

/// Whether DuckDB's sqlite extension (which every test here needs) loads:
/// installed from extensions.duckdb.org on first use. Offline, the tests
/// skip with a message, unless `MENTAT_REQUIRE_SQLITE_EXT=1` (CI sets it, so
/// CI never skips silently).
fn sqlite_ext() -> bool {
    static OK: std::sync::OnceLock<Result<(), String>> = std::sync::OnceLock::new();
    let ok = OK.get_or_init(|| {
        Connection::open_in_memory()
            .and_then(|c| c.execute_batch("INSTALL sqlite; LOAD sqlite"))
            .map_err(|e| e.to_string())
    });
    match ok {
        Ok(()) => true,
        Err(e) if std::env::var("MENTAT_REQUIRE_SQLITE_EXT").as_deref() == Ok("1") => {
            panic!("DuckDB's sqlite extension won't load (MENTAT_REQUIRE_SQLITE_EXT=1): {e}")
        }
        Err(e) => {
            // Straight to stderr: libtest captures eprintln! of a passing test.
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr(),
                "SKIPPED (set MENTAT_REQUIRE_SQLITE_EXT=1 to fail instead): \
                 DuckDB's sqlite extension won't load: {e}"
            );
            false
        }
    }
}

#[test]
fn imported_store_matches_the_sqlite_store() {
    if !sqlite_ext() {
        return;
    }
    // Every SQLite file a test opens is made in a TempDir, never in the tree.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("old.mentat");
    let path_s = path.to_str().unwrap();
    let mut sqlite = mentat::Store::open(path_s).unwrap();
    let mut tx_ids = Vec::new();
    let edges = edge_data();
    // Edge values after the differential's transactions, then a
    // replacement of a fulltext value (SQLite keeps an FTS4 rowid in both
    // `datoms` and the log; the import must turn both into text).
    let mut txs: Vec<String> = TXS.iter().map(|t| t.to_string()).collect();
    txs.extend([EDGES.to_string(), edges]);
    for tx in &txs {
        tx_ids.push(
            sqlite
                .transact(tx.as_str())
                .unwrap_or_else(|e| panic!("{tx}: {e}"))
                .tx_id,
        );
    }
    let e = sqlite
        .q_once("[:find ?e . :where [?e :x/ft _] [?e :x/d _]]", None)
        .unwrap()
        .results;
    let mentat::QueryResults::Scalar(Some(mentat::Binding::Scalar(mentat::TypedValue::Ref(e)))) = e
    else {
        panic!("{e:?}")
    };
    let replace = format!("[[:db/add {e} :x/ft \"a second text\"]]");
    tx_ids.push(sqlite.transact(replace.as_str()).unwrap().tx_id);
    txs.push(replace);

    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let duck = DuckStore::new(&dc as &dyn SqlConn, "imported").unwrap();
    let imported = duck.import_sqlite(path_s).expect("import");
    assert_eq!(imported.last_tx, *tx_ids.last().unwrap());
    // Every transaction plus the bootstrap one.
    assert_eq!(imported.transactions as usize, txs.len() + 1);

    let mut failures = Vec::new();
    let mut check = |label: &str, q: &str, temporal: Option<TemporalBound>| {
        let a = sqlite.q_once_temporal(q, None, temporal);
        let b = duck.q(q, None, temporal);
        match (a, b) {
            (Ok(a), Ok(b)) => {
                let (a, b) = (norm(a.results), norm(b.results));
                if a != b {
                    failures.push(format!("{label} {q}\n  sqlite: {a}\n  duckdb: {b}"));
                }
            }
            (Err(_), Err(_)) => {}
            (a, b) => failures.push(format!(
                "{label} {q}\n  sqlite ok={} duckdb ok={}: {:?} / {:?}",
                a.is_ok(),
                b.is_ok(),
                a.err().map(|e| e.to_string()),
                b.err().map(|e| e.to_string())
            )),
        }
    };
    let edge_queries = [
        "[:find ?d :where [_ :x/d ?d]]",
        "[:find ?s :where [_ :x/s ?s]]",
        "[:find ?l :where [_ :x/l ?l]]",
        "[:find ?tx ?when :where [?tx :db/txInstant ?when]]",
        "[:find ?e ?i :where [?e :db/ident ?i]]",
        "[:find ?e ?t :where [?e :x/ft ?t]]",
    ];
    for q in QUERIES.iter().chain(edge_queries.iter()) {
        check("now", q, None);
    }
    // (Not the fulltext query: the SQLite engine's as-of/since/history SQL
    // returns the FTS rowid, not the text, and fails decoding it.)
    for &t in &tx_ids {
        for q in QUERIES[..12].iter().chain(edge_queries[..3].iter()) {
            check(&format!("asOf {t}"), q, Some(TemporalBound::AsOf(t)));
            check(&format!("since {t}"), q, Some(TemporalBound::Since(t)));
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );

    // Fulltext history and as-of on the import: the replaced text, as text.
    let texts = |q: &str, t: Option<TemporalBound>| -> Vec<String> {
        let mut v: Vec<String> = match duck.q(q, None, t).unwrap().results {
            mentat::QueryResults::Rel(r) => r
                .rows()
                .map(|row| {
                    row.iter()
                        .map(|b| match b {
                            mentat::Binding::Scalar(mentat::TypedValue::String(s)) => s.to_string(),
                            other => format!("{other:?}"),
                        })
                        .collect::<Vec<_>>()
                        .join(" | ")
                })
                .collect(),
            other => vec![format!("{other:?}")],
        };
        v.sort();
        v
    };
    assert_eq!(
        texts("[:find ?v ?added :where [_ :x/ft ?v _ ?added]]", None),
        vec![
            "a second text | Scalar(Boolean(true))",
            "fulltext text, 'quoted' | Scalar(Boolean(false))",
            "fulltext text, 'quoted' | Scalar(Boolean(true))",
        ]
    );
    assert_eq!(
        texts(
            &format!("[:find ?t :where [{e} :x/ft ?t]]"),
            Some(TemporalBound::AsOf(tx_ids[tx_ids.len() - 2]))
        ),
        vec!["fulltext text, 'quoted'"]
    );

    // Doubles bit for bit (the query results above compare Debug text).
    let bits = |rows: Vec<f64>| -> Vec<u64> {
        let mut v: Vec<u64> = rows.into_iter().map(f64::to_bits).collect();
        v.sort();
        v
    };
    let duck_d: Vec<f64> = c
        .prepare(&format!(
            "SELECT v_d FROM {}.datoms WHERE v_d IS NOT NULL",
            duck.schema_name()
        ))
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let sq = rusqlite_like_doubles(&sqlite);
    assert_eq!(bits(duck_d), bits(sq), "doubles differ");

    // The next transaction gets the same tx id and entid on both. (The
    // tx-instant query above already compared every imported instant.)
    let next = r#"[{:db/id "n" :p/name "Next"}]"#;
    let a = sqlite.transact(next).unwrap();
    let b = duck.transact(next).unwrap();
    assert_eq!(a.tx_id, b.tx_id, "tx id after import");
    assert_eq!(a.tempids, b.tempids, "entid after import");
}

/// Every double in the SQLite store's datoms, via the engine.
fn rusqlite_like_doubles(sqlite: &mentat::Store) -> Vec<f64> {
    let mut out = Vec::new();
    for q in [
        "[:find ?e ?d :where [?e :x/d ?d]]",
        "[:find ?e ?d :where [?e :p/height ?d]]",
    ] {
        if let mentat::QueryResults::Rel(rel) = sqlite.q_once(q, None).unwrap().results {
            for row in rel.rows() {
                if let mentat::Binding::Scalar(mentat::TypedValue::Double(d)) = &row[1] {
                    out.push(d.into_inner());
                }
            }
        }
    }
    out
}

#[test]
fn import_refuses_a_store_with_data_and_non_mentat_files() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let good = dir.path().join("good.mentat");
    mentat::Store::open(good.to_str().unwrap())
        .unwrap()
        .transact(TXS[0])
        .unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let conn = &dc as &dyn SqlConn;

    // Not a file: refused, and not created (a read-write attach would
    // create an empty one).
    let missing = dir.path().join("missing.mentat");
    let e = DuckStore::new(conn, "a")
        .unwrap()
        .import_sqlite(missing.to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("mentat_import: can't open"), "{e}");
    assert!(
        !missing.exists(),
        "the import created {}",
        missing.display()
    );

    // A SQLite file that isn't a mentat store.
    let other = dir.path().join("other.db");
    c.execute_batch(&format!(
        "ATTACH '{}' AS o (TYPE sqlite); CREATE TABLE o.t (x INTEGER); DETACH o;",
        other.display()
    ))
    .unwrap();
    let e = DuckStore::new(conn, "b")
        .unwrap()
        .import_sqlite(other.to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("not a mentat store"), "{e}");

    // A version-1 store (mentat before 1.10: no partition high-water marks;
    // made here by undoing the engine's v1 -> v3 upgrade): refused with how
    // to upgrade it, and importable once the engine has opened it.
    let v1 = dir.path().join("v1.mentat");
    let mut old = mentat::Store::open(v1.to_str().unwrap()).unwrap();
    old.transact(TXS[0]).unwrap();
    old.sqlite_mut()
        .execute_batch(
            "DROP INDEX idx_transactions_aevt; ALTER TABLE known_parts DROP COLUMN idx; \
             DROP TABLE mentat_managed_indexes; PRAGMA user_version = 1;",
        )
        .unwrap();
    drop(old);
    let e = DuckStore::new(conn, "v1")
        .unwrap()
        .import_sqlite(v1.to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("version-1 mentat store"), "{e}");
    assert!(e.to_string().starts_with("mentat_import"), "{e}");
    drop(mentat::Store::open(v1.to_str().unwrap()).unwrap());
    DuckStore::new(conn, "v1")
        .unwrap()
        .import_sqlite(v1.to_str().unwrap())
        .unwrap();

    // A store that already has data: refused, and left as it was.
    let s = DuckStore::new(conn, "c").unwrap();
    s.transact(TXS[0]).unwrap();
    let before = s.last_tx().unwrap();
    let e = s.import_sqlite(good.to_str().unwrap()).unwrap_err();
    assert!(e.to_string().contains("already has transactions"), "{e}");
    assert_eq!(s.last_tx().unwrap(), before);

    // Importing twice into a fresh store name works once.
    let s = DuckStore::new(conn, "d").unwrap();
    s.import_sqlite(good.to_str().unwrap()).unwrap();
    assert!(s.import_sqlite(good.to_str().unwrap()).is_err());
    // Nothing stays attached.
    let attached: i64 = c
        .query_row(
            "SELECT count(*) FROM duckdb_databases() WHERE database_name LIKE '%import_src'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(attached, 0);
}

/// While a SQLite mentat file is ATTACHed (by an import, or by hand),
/// DuckDB's `duckdb_indexes()` fails ("syntax error at or near "0"": the
/// sqlite scanner hands DuckDB SQLite's partial-index SQL). Opening stores
/// must not depend on it: a new store, a store from before the
/// `legacy_indexes_dropped` meta row, and an import of the attached file
/// itself all work, and the legacy indexes are dropped (by name) while it is
/// still attached.
#[test]
fn stores_open_while_a_sqlite_mentat_file_is_attached() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let old = dir.path().join("old.mentat");
    mentat::Store::open(old.to_str().unwrap())
        .unwrap()
        .transact(TXS[0])
        .unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let conn = &dc as &dyn SqlConn;
    // A store as 1.15.0-1.17.x left it: no meta row, maybe 1.14.0's indexes.
    let legacy = DuckStore::new(conn, "legacy").unwrap();
    legacy.transact(TXS[0]).unwrap();
    let s = legacy.schema_name().to_string();
    c.execute_batch(&format!(
        "DELETE FROM {s}.meta WHERE key = 'legacy_indexes_dropped'; \
         CREATE INDEX idx_datoms_ea ON {s}.datoms (e, a); \
         CREATE INDEX idx_tt_tx ON {s}.timelined_transactions (tx); \
         UPDATE {s}.meta SET value = '1' WHERE key = 'generation';"
    ))
    .unwrap();

    c.execute_batch(&format!(
        "ATTACH '{}' AS old (TYPE sqlite, READ_ONLY)",
        old.display()
    ))
    .unwrap();
    // What this guards against, logged rather than asserted (an upstream
    // fix would change it): duckdb_indexes() and information_schema.tables
    // fail. `open` uses neither (nor any other catalog function).
    for sql in [
        "SELECT count(*) FROM duckdb_indexes()",
        "SELECT count(*) FROM information_schema.tables",
    ] {
        let r = c.query_row(sql, [], |r| r.get::<_, i64>(0));
        eprintln!("with a mentat SQLite file attached: {sql}: {r:?}");
    }
    // A new store, the legacy store (cache stale: generation changed), and
    // the attached file imported.
    let fresh = DuckStore::new(conn, "fresh").unwrap();
    fresh.transact(TXS[0]).unwrap();
    fresh.transact(TXS[1]).unwrap();
    assert!(fresh.q(QUERIES[0], None, None).is_ok());
    legacy.transact(TXS[1]).unwrap();
    assert!(legacy.q(QUERIES[0], None, None).is_ok());
    DuckStore::new(conn, "people")
        .unwrap()
        .import_sqlite(old.to_str().unwrap())
        .unwrap();
    let n = |sql: &str| -> i64 { c.query_row(sql, [], |r| r.get(0)).unwrap() };
    // Recorded while the file is attached.
    assert_eq!(
        n(&format!(
            "SELECT count(*) FROM {s}.meta WHERE key = 'legacy_indexes_dropped'"
        )),
        1
    );
    c.execute_batch("DETACH old").unwrap();
    assert_eq!(
        n(&format!(
            "SELECT count(*) FROM duckdb_indexes() WHERE schema_name = '{s}'"
        )),
        0
    );
    assert_eq!(
        n(&format!(
            "SELECT count(*) FROM {s}.meta WHERE key = 'legacy_indexes_dropped'"
        )),
        1
    );
}

/// Imports on one connection while another connection creates, writes and
/// queries other stores: each import's ATTACH must not fail them.
#[test]
fn other_stores_work_during_an_import() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let old = dir.path().join("old.mentat");
    let mut sqlite = mentat::Store::open(old.to_str().unwrap()).unwrap();
    for tx in TXS {
        sqlite.transact(*tx).unwrap();
    }
    drop(sqlite);
    let c = Connection::open_in_memory().unwrap();
    DuckStore::new(&DuckConn::new(&c) as &dyn SqlConn, "warm")
        .unwrap()
        .transact(TXS[0])
        .unwrap();
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|sc| {
        let c1 = c.try_clone().unwrap();
        let (old, done) = (&old, &done);
        let importer = sc.spawn(move || {
            let dc = DuckConn::new(&c1);
            for i in 0..8 {
                DuckStore::new(&dc as &dyn SqlConn, &format!("imp{i}"))
                    .unwrap()
                    .import_sqlite(old.to_str().unwrap())
                    .unwrap();
            }
            done.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let c2 = c.try_clone().unwrap();
        let dc = DuckConn::new(&c2);
        let conn = &dc as &dyn SqlConn;
        let mut i = 0;
        // At least once, however fast the imports are.
        while i == 0 || !done.load(std::sync::atomic::Ordering::Relaxed) {
            let s = DuckStore::new(conn, &format!("other{i}")).unwrap();
            s.transact(TXS[0])
                .unwrap_or_else(|e| panic!("new store {i} during import: {e}"));
            s.q(QUERIES[0], None, None).unwrap();
            DuckStore::new(conn, "warm")
                .unwrap()
                .transact(r#"[{:p/name "W"}]"#)
                .unwrap_or_else(|e| panic!("warm store during import: {e}"));
            i += 1;
        }
        importer.join().unwrap();
    });
}

/// Imports on one connection (each ATTACHes and DETACHes its SQLite source)
/// while clones query a store in an attached database ('side.x'). On
/// DuckDB 1.5.6 a `duckdb_tables()` racing a SQLite DETACH segfaults in the
/// sqlite scanner (`SQLiteSchemaEntry::Scan`), and 1.18.0's first cut ran
/// one on every 'db.name' call. Bounded: 3 s of churn, 4 querying threads.
#[test]
fn attached_store_queries_survive_import_churn() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let old = dir.path().join("old.mentat");
    mentat::Store::open(old.to_str().unwrap())
        .unwrap()
        .transact(TXS[0])
        .unwrap();
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("ATTACH ':memory:' AS side").unwrap();
    {
        let dc = DuckConn::new(&c);
        let s = DuckStore::new(&dc as &dyn SqlConn, "side.x").unwrap();
        s.transact(TXS[0]).unwrap();
        s.transact(TXS[1]).unwrap();
    }
    let done = std::sync::atomic::AtomicBool::new(false);
    let (imports, queries) = std::thread::scope(|sc| {
        let c1 = c.try_clone().unwrap();
        let (old, done) = (&old, &done);
        let importer = sc.spawn(move || {
            let dc = DuckConn::new(&c1);
            let mut i = 0;
            while !done.load(std::sync::atomic::Ordering::Relaxed) {
                DuckStore::new(&dc as &dyn SqlConn, &format!("imp{i}"))
                    .unwrap()
                    .import_sqlite(old.to_str().unwrap())
                    .unwrap();
                i += 1;
            }
            i
        });
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let c2 = c.try_clone().unwrap();
                sc.spawn(move || {
                    let dc = DuckConn::new(&c2);
                    let t0 = std::time::Instant::now();
                    let mut n = 0;
                    while t0.elapsed() < std::time::Duration::from_secs(3) {
                        let s = DuckStore::new(&dc as &dyn SqlConn, "side.x").unwrap();
                        assert_eq!(s.catalog_name(), "side");
                        s.q(QUERIES[0], None, None).unwrap();
                        n += 1;
                    }
                    n
                })
            })
            .collect();
        let queries: usize = readers.into_iter().map(|r| r.join().unwrap()).sum();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        (importer.join().unwrap(), queries)
    });
    assert!(
        imports > 0 && queries > 0,
        "{imports} imports, {queries} queries"
    );
}

/// Stores in two databases whose names differ only in punctuation ("my-db",
/// "my_db") are different stores, imported under different locks, so each
/// import's attached source needs its own name: one import must not DETACH
/// or replace the other's. B runs while A's source is attached.
#[test]
fn imports_into_like_named_databases_keep_their_own_source() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let (la, lb) = (dir.path().join("a.mentat"), dir.path().join("b.mentat"));
    for (path, tx) in [(&la, &TXS[..1]), (&lb, &TXS[..2])] {
        let mut s = mentat::Store::open(path.to_str().unwrap()).unwrap();
        for t in tx {
            s.transact(*t).unwrap();
        }
    }
    let a: &'static Connection = Box::leak(Box::new(Connection::open_in_memory().unwrap()));
    a.execute_batch("ATTACH ':memory:' AS \"my-db\"; ATTACH ':memory:' AS my_db")
        .unwrap();
    let b = a.try_clone().unwrap();
    let lb2 = lb.to_str().unwrap().to_string();
    let out = std::rc::Rc::new(std::cell::RefCell::new(None));
    let out2 = out.clone();
    failpoints::run_at("import", move || {
        // Another thread, as the extension's pool would run it.
        let r = std::thread::spawn(move || {
            let dc = DuckConn::new(&b);
            DuckStore::new(&dc as &dyn SqlConn, "my_db.y")
                .unwrap()
                .import_sqlite(&lb2)
                .map(|i| i.transactions)
                .map_err(|e| e.to_string())
        })
        .join()
        .unwrap();
        let sources: i64 = a
            .query_row(
                "SELECT count(*) FROM duckdb_databases() WHERE database_name LIKE '%import_src'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        *out2.borrow_mut() = Some((r, sources));
    });
    let dc = DuckConn::new(a);
    let ra = DuckStore::new(&dc as &dyn SqlConn, "my-db.y")
        .unwrap()
        .import_sqlite(la.to_str().unwrap())
        .map(|i| i.transactions)
        .map_err(|e| e.to_string());
    let (rb, sources) = out.borrow_mut().take().expect("the hook didn't run");
    // a.mentat has the bootstrap + 1 transaction, b.mentat one more; after
    // B, A's source is still attached (B detached only its own).
    assert_eq!((ra, rb, sources), (Ok(2), Ok(3), 1));
}

/// A crafted file whose partition counter is below its partition's start
/// (Partition::new asserts against it) fails with an error, not a panic; a
/// panic mid-import (injected) unwinds cleanly. Either way nothing stays
/// attached, no transaction stays open, and the same store then imports a
/// good file and takes transactions.
#[test]
fn a_failed_or_panicking_import_leaves_nothing_behind() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let (good, bad) = (
        dir.path().join("good.mentat"),
        dir.path().join("bad.mentat"),
    );
    for (p, n) in [(&good, 2), (&bad, 1)] {
        let mut st = mentat::Store::open(p.to_str().unwrap()).unwrap();
        for tx in &TXS[..n] {
            st.transact(*tx).unwrap();
        }
    }
    mentat::Store::open(bad.to_str().unwrap())
        .unwrap()
        .sqlite_mut()
        .execute_batch("UPDATE known_parts SET idx = start - 1 WHERE part = ':db.part/user'")
        .unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let conn = &dc as &dyn SqlConn;
    let s = DuckStore::new(conn, "victim").unwrap();
    let clean = |what: &str| {
        let attached: i64 = c
            .query_row(
                "SELECT count(*) FROM duckdb_databases() WHERE database_name LIKE '%import_src'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(attached, 0, "{what}: the source is still attached");
        c.execute_batch("BEGIN; ROLLBACK")
            .unwrap_or_else(|e| panic!("{what}: a transaction is still open: {e}"));
        DuckStore::new(conn, "other")
            .unwrap()
            .transact(TXS[0])
            .unwrap_or_else(|e| panic!("{what}: another store can't transact: {e}"));
    };

    let e = s.import_sqlite(bad.to_str().unwrap()).unwrap_err();
    assert!(e.to_string().contains("bad known_parts row"), "{e}");
    assert!(e.to_string().starts_with("mentat_import"), "{e}");
    clean("bad partition");

    failpoints::arm("import");
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        s.import_sqlite(good.to_str().unwrap())
    }));
    assert!(r.is_err(), "the failpoint didn't fire");
    clean("panic");

    let imported = s.import_sqlite(good.to_str().unwrap()).unwrap();
    assert_eq!(imported.transactions, 3);
    s.transact(r#"[{:p/name "After"}]"#).unwrap();
}

/// A fulltext attribute whose definition was later retracted in full
/// (ident, valueType, cardinality, fulltext): it's gone from `schema`, but
/// its log rows still hold FTS rowids. The import must turn those into the
/// text, not copy the rowid as if it were the string.
#[test]
fn a_retracted_fulltext_attribute_keeps_its_history_text() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("ft.mentat");
    let mut sqlite = mentat::Store::open(path.to_str().unwrap()).unwrap();
    let def = "{:db/ident :x/ft :db/valueType :db.type/string \
               :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}";
    sqlite.transact(format!("[{def}]").as_str()).unwrap();
    let e = sqlite
        .transact(r#"[{:db/id "e" :x/ft "hello world"}]"#)
        .unwrap()
        .tempids["e"];
    let attr: i64 = sqlite
        .sqlite_mut()
        .query_row("SELECT e FROM idents WHERE v = ':x/ft'", [], |r| r.get(0))
        .unwrap();
    sqlite
        .transact(format!(r#"[[:db/retract {e} :x/ft "hello world"]]"#).as_str())
        .unwrap();
    sqlite
        .transact(
            format!(
                "[[:db/retract {attr} :db/ident :x/ft] \
                  [:db/retract {attr} :db/valueType :db.type/string] \
                  [:db/retract {attr} :db/cardinality :db.cardinality/one] \
                  [:db/retract {attr} :db/fulltext true] [:db/retract {attr} :db/index true]]"
            )
            .as_str(),
        )
        .unwrap();
    // The shape this is about: no schema rows, rowids in the log.
    let (schema_rows, log_rowids): (i64, i64) = sqlite
        .sqlite_mut()
        .query_row(
            &format!(
                "SELECT (SELECT count(*) FROM schema WHERE e = {attr}), \
                 (SELECT count(*) FROM timelined_transactions WHERE a = {attr} AND typeof(v) = 'integer')"
            ),
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((schema_rows, log_rowids), (0, 2));
    drop(sqlite);

    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let duck = DuckStore::new(&dc as &dyn SqlConn, "ft").unwrap();
    duck.import_sqlite(path.to_str().unwrap()).unwrap();
    let mut log: Vec<(String, i64, bool)> = c
        .prepare(&format!(
            "SELECT union_tag(v)::VARCHAR || ':' || v::VARCHAR, value_type_tag, added \
             FROM {}.timelined_transactions WHERE a = {attr}",
            duck.schema_name()
        ))
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    log.sort();
    assert_eq!(
        log,
        [
            ("s:hello world".to_string(), 10, false),
            ("s:hello world".to_string(), 10, true)
        ]
    );
}

/// The source is attached READ_ONLY: mid-import, a write through the
/// import's attach fails, and afterwards the file is byte for byte as it was,
/// with no files beside it.
#[test]
fn the_source_is_read_only() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("old.mentat");
    let mut st = mentat::Store::open(path.to_str().unwrap()).unwrap();
    st.transact(TXS[0]).unwrap();
    // Checkpointed and closed: one file, no -wal/-shm.
    st.sqlite_mut()
        .execute_batch("PRAGMA journal_mode = DELETE")
        .unwrap();
    drop(st);
    let files = || -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    };
    assert_eq!(files(), ["old.mentat"]);
    let before = std::fs::read(&path).unwrap();

    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let s = DuckStore::new(&dc as &dyn SqlConn, "ro").unwrap();
    // From another connection (attaches are database-wide), while the
    // import holds its attach.
    let other: &'static Connection = Box::leak(Box::new(c.try_clone().unwrap()));
    let seen = std::rc::Rc::new(std::cell::RefCell::new(None));
    let seen2 = seen.clone();
    failpoints::run_at("import", move || {
        let src: String = other
            .query_row(
                "SELECT database_name FROM duckdb_databases() WHERE database_name LIKE '%import_src'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let w = other
            .prepare(&format!(
                "SELECT count(*) FROM sqlite_query('{src}', 'DELETE FROM known_parts RETURNING part')"
            ))
            .and_then(|mut st| st.query_row([], |r| r.get::<_, i64>(0)));
        *seen2.borrow_mut() = Some(w.map_err(|e| e.to_string()));
    });
    s.import_sqlite(path.to_str().unwrap()).unwrap();
    let w = seen.borrow_mut().take().expect("the hook didn't run");
    assert!(
        w.as_ref().is_err_and(|e| e.contains("readonly")),
        "a write through the import's attach: {w:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before, "the source changed");
    assert_eq!(files(), ["old.mentat"]);
}

/// An untrusted file: a VIEW named like one of mentat's tables is refused
/// before anything reads it. (The scanner's SQLite runs a view's SQL; an
/// unbounded recursive one used to run forever holding the store's write
/// lock.) And a WAL-mode file in a read-only directory is refused with what
/// to do about it.
#[test]
fn views_and_unreadable_wal_files_are_refused_up_front() {
    if !sqlite_ext() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let conn = &dc as &dyn SqlConn;

    // A real store, then `datoms` replaced by an endless view.
    let view = dir.path().join("view.mentat");
    let mut st = mentat::Store::open(view.to_str().unwrap()).unwrap();
    st.transact(TXS[0]).unwrap();
    st.sqlite_mut()
        .execute_batch(
            "DROP TABLE datoms; \
             CREATE VIEW datoms AS WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) \
             SELECT n AS e, 1 AS a, 1 AS v, 1 AS tx, 0 AS value_type_tag, 0 AS index_avet, \
             0 AS index_vaet, 0 AS index_fulltext, 0 AS unique_value FROM r",
        )
        .unwrap();
    drop(st);
    let t0 = std::time::Instant::now();
    let e = DuckStore::new(conn, "view")
        .unwrap()
        .import_sqlite(view.to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("not a mentat store"), "{e}");
    assert!(t0.elapsed().as_secs() < 10, "took {:?}", t0.elapsed());

    // fulltext_values replaced by an endless view with a `rowid` column, and
    // one datom marked fulltext so the import would read it.
    let ftv = dir.path().join("ftview.mentat");
    let mut st = mentat::Store::open(ftv.to_str().unwrap()).unwrap();
    st.transact(TXS[0]).unwrap();
    st.sqlite_mut()
        .execute_batch(
            "DROP TABLE fulltext_values; \
             CREATE VIEW fulltext_values AS WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r) \
             SELECT n AS rowid, 'x' AS text FROM r; \
             UPDATE datoms SET index_fulltext = 1, value_type_tag = 10, v = 1 WHERE rowid = (SELECT min(rowid) FROM datoms)",
        )
        .unwrap();
    drop(st);
    let t0 = std::time::Instant::now();
    let e = DuckStore::new(conn, "ftview")
        .unwrap()
        .import_sqlite(ftv.to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("not a mentat store"), "{e}");
    assert!(t0.elapsed().as_secs() < 10, "took {:?}", t0.elapsed());

    // WAL mode (mentat's default) in a directory we can't write: SQLite
    // can't create the -shm. Skipped as root, which can write anyway.
    let ro = dir.path().join("ro");
    std::fs::create_dir(&ro).unwrap();
    let wal = ro.join("old.mentat");
    let mut st = mentat::Store::open(wal.to_str().unwrap()).unwrap();
    st.transact(TXS[0]).unwrap();
    drop(st);
    for f in ["old.mentat-wal", "old.mentat-shm"] {
        let _ = std::fs::remove_file(ro.join(f));
    }
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(ro.join("probe"), b"").is_err() {
        let e = DuckStore::new(conn, "wal")
            .unwrap()
            .import_sqlite(wal.to_str().unwrap())
            .unwrap_err();
        assert!(
            e.to_string().contains("WAL-mode") && e.to_string().contains("writable"),
            "{e}"
        );
    }
    std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
}
