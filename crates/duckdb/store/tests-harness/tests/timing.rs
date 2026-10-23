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
    let dc = DuckConn::new(&c);
    let s = DuckStore::new(&dc as &dyn SqlConn, "default").unwrap();
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

/// `(fulltext ...)` over 100k texts of one attribute, with another fulltext
/// attribute holding 100k more (run with --ignored --nocapture).
#[test]
#[ignore]
fn fulltext_100k() {
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn::new(&c);
    let s = DuckStore::new(&dc as &dyn SqlConn, "default").unwrap();
    s.transact(
        "[{:db/ident :u/bio :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}
          {:db/ident :u/note :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}]",
    )
    .unwrap();
    for chunk in 0..10 {
        let mut tx = String::from("[");
        for i in chunk * 10_000..(chunk + 1) * 10_000 {
            let bio = if i % 100 == 0 {
                "Hello darkness my old friend"
            } else {
                "The quick brown fox jumps over the lazy dog"
            };
            tx.push_str(&format!(
                "{{:u/bio \"{bio} {i}\" :u/note \"note number {i} lorem ipsum\"}} "
            ));
        }
        tx.push(']');
        s.transact(&tx).unwrap();
    }
    let words = |n: usize, op: &str| (0..n).map(|i| format!("w{i}")).collect::<Vec<_>>().join(op);
    let big = std::env::var("MENTAT_TIMING_BIG").is_ok();
    let mut queries = vec![
        "darkness".to_string(),
        "\\\"hello dark*\\\" NOT fox".to_string(),
    ];
    if big {
        queries.extend([
            "the quick brown fox jumps over lazy dog".to_string(),
            words(4095, " "),
            words(4095, " OR "),
        ]);
    }
    for ft in queries {
        let q = format!("[:find (count ?e) . :where [(fulltext $ :u/bio \"{ft}\") [[?e]]]]");
        let q = q.as_str();
        s.q(q, None, None).unwrap();
        let n = 5;
        let t0 = Instant::now();
        let mut out = String::new();
        for _ in 0..n {
            out = format!("{:?}", s.q(q, None, None).unwrap().results);
        }
        println!(
            "{:.1} ms  {} -> {out}",
            t0.elapsed().as_secs_f64() * 1e3 / n as f64,
            &q[..q.len().min(110)]
        );
    }
}

/// `DuckStore::new` + `open` per call, for a store in the home database and
/// one in an attached database, with 50 other schemas around (run with
/// --ignored --nocapture).
#[test]
#[ignore]
fn store_name_resolution() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch("ATTACH ':memory:' AS side").unwrap();
    for i in 0..50 {
        c.execute_batch(&format!(
            "CREATE SCHEMA s{i}; CREATE TABLE s{i}.t1(x INT); CREATE TABLE s{i}.t2(x INT); \
             CREATE TABLE side.main.u{i}(x INT)"
        ))
        .unwrap();
    }
    let dc = DuckConn::new(&c);
    let conn = &dc as &dyn SqlConn;
    for name in ["x", "side.x"] {
        DuckStore::new(conn, name)
            .unwrap()
            .transact("[{:db/ident :u/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]")
            .unwrap();
        let n = 2000;
        let t0 = Instant::now();
        for _ in 0..n {
            DuckStore::new(conn, name).unwrap().open().unwrap();
        }
        let open = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
        let t0 = Instant::now();
        for _ in 0..n {
            DuckStore::new(conn, name).unwrap();
        }
        println!(
            "{name}: new+open {open:.1} us/call, new alone {:.1} us",
            t0.elapsed().as_secs_f64() * 1e6 / n as f64
        );
    }
}
