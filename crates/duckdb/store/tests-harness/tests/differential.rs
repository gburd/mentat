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
    let mut counts = Vec::new();
    for (i, tx) in TXS.iter().enumerate() {
        let a = sqlite
            .transact(*tx)
            .unwrap_or_else(|e| panic!("sqlite tx {i}: {e}"));
        let b = duck
            .transact(tx)
            .unwrap_or_else(|e| panic!("duckdb tx {i}: {e}"));
        assert_eq!(a.tx_id, b.tx_id, "tx {i}: tx ids differ");
        assert_eq!(a.tempids, b.tempids, "tx {i}: tempids differ");
        // #281: the datom counts come from each store's own SQL.
        assert_eq!(
            (a.datoms_asserted, a.datoms_retracted),
            (b.datoms_asserted, b.datoms_retracted),
            "tx {i}: datom counts differ"
        );
        tx_ids.push(a.tx_id);
        counts.push((a.datoms_asserted, a.datoms_retracted));
    }

    // The last two transactions: a no-op, then a replacement and a new value.
    assert_eq!(counts[counts.len() - 2..], [(0, 0), (2, 1)]);

    let mut failures = Vec::new();
    let mut check = |label: &str, q: &str, temporal: Option<TemporalBound>, ordered: bool| {
        use mentat::Queryable;
        let a = sqlite.q_once_temporal(q, None, temporal);
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
            (Err(a), Err(_))
                if [
                    "get-else",
                    "missing?",
                    "like",
                    "-with?",
                    "includes?",
                    "?tx)",
                    "(max ?l)",
                ]
                .iter()
                .any(|f| q.contains(f)) =>
            {
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
    // As-of every transaction, since every transaction. The pulls too: a
    // find-pull reads the query's basis (before 1.18.0 it read current state).
    let pulls = QUERIES.iter().filter(|q| q.contains("(pull "));
    let temporal: Vec<&str> = QUERIES[..12]
        .iter()
        .copied()
        .chain(pulls.copied())
        .collect();
    // And both bounds: since the previous tx, as of this one.
    for (i, &t) in tx_ids.iter().enumerate() {
        let window = i.checked_sub(1).map(|p| TemporalBound::AsOfSince {
            as_of: t,
            since: tx_ids[p],
        });
        for q in &temporal {
            check(&format!("asOf {t}"), q, Some(TemporalBound::AsOf(t)), false);
            check(
                &format!("since {t}"),
                q,
                Some(TemporalBound::Since(t)),
                false,
            );
            if let Some(w) = window {
                check(&format!("{w:?}"), q, Some(w), false);
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
