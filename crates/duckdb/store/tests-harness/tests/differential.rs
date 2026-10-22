//! Differential test: the same transactions and queries against mentat's
//! SQLite store and against mentat_duckdb_store, compared result for result.
//! Any query where the engines disagree is a DuckDB-dialect bug.

use duckdb::Connection;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_duckdb_store_tests::fixtures::{norm, QUERIES, TXS};
use mentat_duckdb_store_tests::DuckConn;
use mentat_query_algebrizer::TemporalBound;
use mentat_query_projector::QueryResults;

/// An ordered query's rows must come back in the same order; keep it.
fn norm_ordered(r: QueryResults) -> String {
    match r {
        QueryResults::Coll(c) => format!(
            "coll {:?}",
            c.iter().map(|x| format!("{x:?}")).collect::<Vec<_>>()
        ),
        QueryResults::Rel(r) => format!(
            "rel {:?}",
            r.rows()
                .map(|row| row
                    .iter()
                    .map(|x| format!("{x:?}"))
                    .collect::<Vec<_>>()
                    .join(" | "))
                .collect::<Vec<_>>()
        ),
        other => norm(other),
    }
}

const ORDERED: &[&str] = &[
    "[:find ?n ?a :order (asc ?a) (asc ?n) :where [?e :p/name ?n] [?e :p/age ?a]]",
    "[:find ?n ?h :order (desc ?h) :where [?e :p/name ?n] [?e :p/height ?h]]",
    "[:find ?n :order ?n :limit 2 :where [_ :p/name ?n]]",
];

#[test]
fn duckdb_matches_sqlite() {
    let mut sqlite = mentat::Store::open("").unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let duck = DuckStore::new(&dc as &dyn SqlConn, "default");

    let mut tx_ids = Vec::new();
    for (i, tx) in TXS.iter().enumerate() {
        let a = sqlite
            .transact(*tx)
            .unwrap_or_else(|e| panic!("sqlite tx {i}: {e}"));
        let b = duck
            .transact(tx)
            .unwrap_or_else(|e| panic!("duckdb tx {i}: {e}"));
        assert_eq!(a.tx_id, b.tx_id, "tx {i}: tx ids differ");
        assert_eq!(a.tempids, b.tempids, "tx {i}: tempids differ");
        tx_ids.push(a.tx_id);
    }

    let mut failures = Vec::new();
    let mut check = |label: &str, q: &str, temporal: Option<TemporalBound>, ordered: bool| {
        use mentat::Queryable;
        let a = match temporal {
            Some(TemporalBound::AsOf(t)) => sqlite.q_once_as_of(q, None, t),
            Some(TemporalBound::Since(t)) => sqlite.q_once_since(q, None, t),
            None => sqlite.q_once(q, None),
        };
        let b = duck.q(q, None, temporal);
        let f = if ordered { norm_ordered } else { norm };
        match (a, b) {
            (Ok(a), Ok(b)) => {
                let (a, b) = (f(a.results), f(b.results));
                if a != b {
                    failures.push(format!("{label} {q}\n  sqlite: {a}\n  duckdb: {b}"));
                }
            }
            (Err(a), Ok(b)) => failures.push(format!(
                "{label} {q}\n  sqlite ERR {a}\n  duckdb: {}",
                f(b.results)
            )),
            (Ok(a), Err(b)) => failures.push(format!(
                "{label} {q}\n  sqlite: {}\n  duckdb ERR {b}",
                f(a.results)
            )),
            // Both failing agrees, except for queries that must work: the
            // get-else / missing? cases were silently "agreeing" this way
            // while neither engine implemented them (#335).
            (Err(a), Err(_)) if q.contains("get-else") || q.contains("missing?") => {
                failures.push(format!("{label} {q}\n  both engines ERR: {a}"))
            }
            (Err(_), Err(_)) => {}
        }
    };
    for q in QUERIES {
        check("now", q, None, false);
    }
    for q in ORDERED {
        check("ordered", q, None, true);
    }
    // As-of every transaction, since every transaction.
    for &t in &tx_ids {
        for q in &QUERIES[..12] {
            check(&format!("asOf {t}"), q, Some(TemporalBound::AsOf(t)), false);
            check(
                &format!("since {t}"),
                q,
                Some(TemporalBound::Since(t)),
                false,
            );
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
