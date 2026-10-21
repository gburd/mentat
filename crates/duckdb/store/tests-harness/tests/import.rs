//! `DuckStore::import_sqlite`: a SQLite-format store (what the DuckDB
//! extension wrote before 1.11, written here by the SQLite engine) imported
//! into DuckDB answers the differential test's queries exactly as the SQLite
//! store does, now, as of and since every transaction, and keeps its tx ids,
//! tx instants, entids and partition high-water marks.

use duckdb::Connection;
use mentat::Queryable;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use mentat_query_algebrizer::TemporalBound;

#[path = "differential.rs"]
#[allow(dead_code)]
mod differential;
use differential::{norm, QUERIES, TXS};

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

#[test]
fn imported_store_matches_the_sqlite_store() {
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
    let duck = DuckStore::new(&dc as &dyn SqlConn, "imported");
    let imported = duck.import_sqlite(path_s).expect("import");
    assert_eq!(imported.last_tx, *tx_ids.last().unwrap());
    // Every transaction plus the bootstrap one.
    assert_eq!(imported.transactions as usize, txs.len() + 1);

    let mut failures = Vec::new();
    let mut check = |label: &str, q: &str, temporal: Option<TemporalBound>| {
        let a = match temporal {
            Some(TemporalBound::AsOf(t)) => sqlite.q_once_as_of(q, None, t),
            Some(TemporalBound::Since(t)) => sqlite.q_once_since(q, None, t),
            None => sqlite.q_once(q, None),
        };
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
    let dir = tempfile::TempDir::new().unwrap();
    let good = dir.path().join("good.mentat");
    mentat::Store::open(good.to_str().unwrap())
        .unwrap()
        .transact(TXS[0])
        .unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let conn = &dc as &dyn SqlConn;

    // Not a file.
    let e = DuckStore::new(conn, "a")
        .import_sqlite(dir.path().join("missing.mentat").to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("mentat_import"), "{e}");

    // A SQLite file that isn't a mentat store.
    let other = dir.path().join("other.db");
    c.execute_batch(&format!(
        "ATTACH '{}' AS o (TYPE sqlite); CREATE TABLE o.t (x INTEGER); DETACH o;",
        other.display()
    ))
    .unwrap();
    let e = DuckStore::new(conn, "b")
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
        .import_sqlite(v1.to_str().unwrap())
        .unwrap_err();
    assert!(e.to_string().contains("version-1 mentat store"), "{e}");
    assert!(e.to_string().starts_with("mentat_import"), "{e}");
    drop(mentat::Store::open(v1.to_str().unwrap()).unwrap());
    DuckStore::new(conn, "v1")
        .import_sqlite(v1.to_str().unwrap())
        .unwrap();

    // A store that already has data: refused, and left as it was.
    let s = DuckStore::new(conn, "c");
    s.transact(TXS[0]).unwrap();
    let before = s.last_tx().unwrap();
    let e = s.import_sqlite(good.to_str().unwrap()).unwrap_err();
    assert!(e.to_string().contains("already has transactions"), "{e}");
    assert_eq!(s.last_tx().unwrap(), before);

    // Importing twice into a fresh store name works once.
    let s = DuckStore::new(conn, "d");
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
