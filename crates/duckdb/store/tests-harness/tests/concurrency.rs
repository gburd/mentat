//! Concurrent writers and readers on one store, each thread on its own
//! connection to one database (as the extension's connection pool runs
//! calls): no transaction is lost or fails, every entid and tx id is
//! allocated once, and a reader never sees part of a transaction.

use duckdb::Connection;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use mentat_query_projector::QueryResults;
use std::sync::atomic::{AtomicBool, Ordering};

use core_traits::{Binding, TypedValue};

const WRITERS: usize = 8;
const READERS: usize = 4;
const TXS: usize = 20; // per writer
const BATCH: usize = 4; // entities per transaction

fn long(out: QueryResults) -> i64 {
    match out {
        QueryResults::Scalar(Some(Binding::Scalar(TypedValue::Long(n)))) => n,
        QueryResults::Scalar(None) => 0,
        other => panic!("{other:?}"),
    }
}

fn count(s: &DuckStore) -> i64 {
    long(
        s.q("[:find (count ?e) . :where [?e :w/n _]]", None, None)
            .unwrap()
            .results,
    )
}

#[test]
fn concurrent_writers_and_readers_on_one_store() {
    let c = Connection::open_in_memory().unwrap();
    {
        let dc = DuckConn::new(&c);
        DuckStore::new(&dc as &dyn SqlConn, "conc").unwrap()
            .transact(
                "[{:db/ident :w/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
                  {:db/ident :w/writer :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
            )
            .unwrap();
    }
    let done = AtomicBool::new(false);
    let allocated: Vec<i64> = std::thread::scope(|sc| {
        let readers: Vec<_> = (0..READERS)
            .map(|_| {
                let c = c.try_clone().unwrap();
                let done = &done;
                sc.spawn(move || {
                    let dc = DuckConn::new(&c);
                    let s = DuckStore::new(&dc as &dyn SqlConn, "conc").unwrap();
                    let (mut last, mut reads) = (0, 0);
                    while !done.load(Ordering::Relaxed) {
                        let n = count(&s);
                        assert_eq!(n % BATCH as i64, 0, "saw part of a transaction: {n}");
                        assert!(n >= last, "count went backwards: {last} -> {n}");
                        (last, reads) = (n, reads + 1);
                    }
                    reads
                })
            })
            .collect();
        let writers: Vec<_> = (0..WRITERS)
            .map(|w| {
                let c = c.try_clone().unwrap();
                sc.spawn(move || {
                    let dc = DuckConn::new(&c);
                    let s = DuckStore::new(&dc as &dyn SqlConn, "conc").unwrap();
                    let mut ids = vec![];
                    for t in 0..TXS {
                        let tx: String = (0..BATCH)
                            .map(|i| {
                                format!("{{:db/id \"e{i}\" :w/n {} :w/writer {w}}}", t * BATCH + i)
                            })
                            .collect();
                        let r = s
                            .transact(&format!("[{tx}]"))
                            .unwrap_or_else(|e| panic!("writer {w} tx {t}: {e}"));
                        ids.push(r.tx_id);
                        ids.extend(r.tempids.values());
                    }
                    ids
                })
            })
            .collect();
        // Join every writer before failing, so the readers always stop.
        let results: Vec<_> = writers.into_iter().map(|h| h.join()).collect();
        done.store(true, Ordering::Relaxed);
        for r in readers {
            assert!(r.join().unwrap() > 0);
        }
        results
            .into_iter()
            .flat_map(|r| r.expect("a writer failed"))
            .collect()
    });

    let mut unique = allocated.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        allocated.len(),
        "an entid or tx id was allocated twice"
    );
    assert_eq!(allocated.len(), WRITERS * TXS * (BATCH + 1));

    let dc = DuckConn::new(&c);
    let s = DuckStore::new(&dc as &dyn SqlConn, "conc").unwrap();
    assert_eq!(count(&s), (WRITERS * TXS * BATCH) as i64);
    for w in 0..WRITERS {
        let q = format!("[:find (count ?e) . :where [?e :w/writer {w}]]");
        assert_eq!(
            long(s.q(&q, None, None).unwrap().results),
            (TXS * BATCH) as i64
        );
    }
    // The partition map kept up: the next transaction's entid is new.
    let r = s.transact("[{:db/id \"x\" :w/n -1}]").unwrap();
    assert!(!unique.contains(&r.tempids["x"]) && !unique.contains(&r.tx_id));
}
