//! Not a correctness test: where a point lookup's time goes (run with
//! --ignored --nocapture).
use duckdb::Connection;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use std::time::Instant;

#[test]
#[ignore]
fn point_lookup_breakdown() {
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn(&c);
    let s = DuckStore::new(&dc as &dyn SqlConn, "default");
    s.transact("[{:db/ident :u/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true}]").unwrap();
    let mut tx = String::from("[");
    for i in 0..20000 {
        tx.push_str(&format!("{{:u/email \"u{i}@x\"}} "));
    }
    tx.push(']');
    s.transact(&tx).unwrap();
    let q = "[:find ?e . :where [?e :u/email \"u77@x\"]]";
    let n = 300;
    let t = |f: &dyn Fn()| {
        f();
        let t0 = Instant::now();
        for _ in 0..n {
            f();
        }
        t0.elapsed().as_secs_f64() * 1e3 / n as f64
    };
    println!(
        "open()          {:.3} ms",
        t(&|| {
            s.open().unwrap();
        })
    );
    println!(
        "q()             {:.3} ms",
        t(&|| {
            s.q(q, None, None).unwrap();
        })
    );
    println!(
        "SELECT 1        {:.3} ms",
        t(&|| {
            dc.execute_batch("SELECT 1").unwrap();
        })
    );
    println!(
        "generation read {:.3} ms",
        t(&|| {
            dc.execute(
                "SELECT value FROM mentat.meta WHERE key = 'generation'",
                &[],
            )
            .unwrap();
        })
    );
    println!(
        "raw lookup      {:.3} ms",
        t(&|| {
            dc.execute("SELECT e FROM mentat.datoms WHERE a = 65536 AND v = 'u77@x'::UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)", &[]).unwrap();
        })
    );
}
