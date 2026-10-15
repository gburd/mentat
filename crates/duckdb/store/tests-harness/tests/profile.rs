//! Not a correctness test: where one call's time goes, per SQL statement, on
//! a real store. Run on a COPY of a benchmark database (it transacts):
//!   MENTAT_PROFILE_DB=/tmp/xs.duckdb MENTAT_PROFILE_STORE=/home/admin/stores/xs \
//!   cargo test --release --test profile -- --ignored --nocapture
use duckdb::Connection;
use mentat_duckdb_store::{DuckStore, SqlConn, SqlError, SqlRow, SqlValue};
use mentat_duckdb_store_tests::DuckConn;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::time::Instant;

/// A `SqlConn` that times every statement (and, for queries, the prepare
/// alone, on a separate prepare of the same text).
struct Trace<'a> {
    inner: DuckConn<'a>,
    log: RefCell<Vec<(String, f64)>>,
}

impl Trace<'_> {
    fn rec<T>(&self, sql: &str, f: impl FnOnce() -> T) -> T {
        let t = Instant::now();
        let r = f();
        let key: String = sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let width = if std::env::var("MENTAT_PROFILE_FULL").is_ok() {
            usize::MAX
        } else {
            90
        };
        let key: String = key.chars().take(width).collect();
        self.log
            .borrow_mut()
            .push((key, t.elapsed().as_secs_f64() * 1e3));
        r
    }
}

impl SqlConn for Trace<'_> {
    fn dialect(&self) -> mentat_duckdb_store::Dialect {
        self.inner.dialect()
    }
    fn execute_batch(&self, sql: &str) -> Result<(), SqlError> {
        self.rec(sql, || self.inner.execute_batch(sql))
    }
    fn execute(&self, sql: &str, p: &[SqlValue]) -> Result<usize, SqlError> {
        self.rec(sql, || self.inner.execute(sql, p))
    }
    fn query(
        &self,
        sql: &str,
        p: &[SqlValue],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        self.rec(sql, || self.inner.query(sql, p, f))
    }
    fn query_named(
        &self,
        sql: &str,
        p: &[(&str, SqlValue)],
        f: &mut dyn FnMut(&dyn SqlRow) -> Result<(), SqlError>,
    ) -> Result<(), SqlError> {
        self.rec(&format!("[prepare only] {sql}"), || {
            self.inner.0.prepare(sql).map(|_| ())
        })
        .unwrap();
        self.rec(sql, || self.inner.query_named(sql, p, f))
    }
    fn max_variables(&self) -> usize {
        self.inner.max_variables()
    }
    fn column_count(&self, sql: &str) -> Result<usize, SqlError> {
        self.inner.column_count(sql)
    }
}

fn profile(name: &str, n: usize, tr: &Trace, f: &dyn Fn()) {
    for _ in 0..20 {
        f();
    }
    tr.log.borrow_mut().clear();
    let t0 = Instant::now();
    for _ in 0..n {
        f();
    }
    let total = t0.elapsed().as_secs_f64() * 1e3 / n as f64;
    let mut by: BTreeMap<String, (usize, f64)> = BTreeMap::new();
    let mut order = vec![];
    for (k, ms) in tr.log.borrow().iter() {
        if !by.contains_key(k) {
            order.push(k.clone());
        }
        let e = by.entry(k.clone()).or_default();
        e.0 += 1;
        e.1 += ms;
    }
    let sql: f64 = by
        .iter()
        .filter(|(k, _)| !k.starts_with("[prepare only]"))
        .map(|(_, (_, ms))| ms)
        .sum::<f64>()
        / n as f64;
    println!(
        "\n== {name}: {total:.3} ms/call, of which SQL {sql:.3} ms, other {:.3} ms",
        total - sql
    );
    for k in order {
        let (c, ms) = by[&k];
        println!("  {:7.3} ms  x{:<3} {k}", ms / n as f64, c / n);
    }
}

#[test]
#[ignore]
fn call_breakdown() {
    let db = std::env::var("MENTAT_PROFILE_DB").expect("MENTAT_PROFILE_DB");
    let store = std::env::var("MENTAT_PROFILE_STORE").expect("MENTAT_PROFILE_STORE");
    let c = Connection::open(db).unwrap();
    let tr = Trace {
        inner: DuckConn(&c),
        log: RefCell::new(vec![]),
    };
    let s = DuckStore::new(&tr as &dyn SqlConn, &store);
    let q1 = "[:find ?e ?name :in $ ?email :where [?e :user/email ?email] [?e :user/name ?name]]";
    let opts = serde_json::json!({"inputs": ["user7@example.com"]});
    let n = 200;
    profile("open()", n, &tr, &|| {
        s.open().unwrap();
    });
    profile("point_lookup q_json", n, &tr, &|| {
        assert_eq!(s.q_json(q1, &opts).unwrap().results.len(), 1);
    });
    let e: i64 = {
        let mut e = 0;
        c.query_row(
            &format!(
                "SELECT max(e) FROM {}.datoms WHERE a = (SELECT e FROM {0}.idents WHERE v.s = ':issue/title')",
                s.schema_name()
            ),
            [],
            |r| {
                e = r.get(0)?;
                Ok(())
            },
        )
        .unwrap();
        e
    };
    let pull = format!("[:find (pull ?e [*]) . :in ?e :where [?e _ _]]");
    profile("pull [*]", n, &tr, &|| {
        let inputs = mentat_query_algebrizer::QueryInputs::with_value_sequence(vec![(
            edn::query::Variable::from_valid_name("?e"),
            core_traits::TypedValue::Ref(e),
        )]);
        s.q(&pull, Some(inputs), None).unwrap();
    });
    profile("transact 1 datom", 50, &tr, &|| {
        s.transact(&format!("[[:db/add {e} :issue/state :state/open]]"))
            .unwrap();
    });
}

/// Fixed DuckDB per-statement costs on the same database: fresh prepare vs a
/// cached prepared statement, for a trivial statement, the generation read
/// and the point-lookup query.
#[test]
#[ignore]
fn statement_costs() {
    let db = std::env::var("MENTAT_PROFILE_DB").expect("MENTAT_PROFILE_DB");
    let store = std::env::var("MENTAT_PROFILE_STORE").expect("MENTAT_PROFILE_STORE");
    let c = Connection::open(db).unwrap();
    let s = mentat_duckdb_store::DuckStore::new(&DuckConn(&c) as &dyn SqlConn, &store)
        .schema_name()
        .to_string();
    let n = 500;
    let t = |name: &str, f: &dyn Fn()| {
        for _ in 0..20 {
            f();
        }
        let t0 = Instant::now();
        for _ in 0..n {
            f();
        }
        println!(
            "{:8.3} ms  {name}",
            t0.elapsed().as_secs_f64() * 1e3 / n as f64
        );
    };
    let gen = format!("SELECT value::BIGINT FROM {s}.meta WHERE key = 'generation'");
    let q = format!("SELECT DISTINCT d0.e, d1.v FROM {s}.datoms d0, {s}.datoms d1 WHERE d0.a = 65536 AND d0.v_s = $1 AND d1.a = 65537 AND d0.e = d1.e");
    for threads in [0, 1] {
        if threads == 1 {
            c.execute_batch("SET threads = 1").unwrap();
            println!("-- threads = 1");
        }
        t("SELECT 1 (prepare+run)", &|| {
            c.prepare("SELECT 1")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap();
        });
        t("SELECT 1 (cached)", &|| {
            c.prepare_cached("SELECT 1")
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap();
        });
        t("SET search_path", &|| {
            c.execute_batch(&format!("SET search_path = '{s}'"))
                .unwrap();
        });
        t("generation (prepare+run)", &|| {
            c.prepare(&gen).unwrap().query([]).unwrap().next().unwrap();
        });
        t("generation (cached)", &|| {
            c.prepare_cached(&gen)
                .unwrap()
                .query([])
                .unwrap()
                .next()
                .unwrap();
        });
        t("point query (prepare+run)", &|| {
            c.prepare(&q)
                .unwrap()
                .query(["user7@example.com"])
                .unwrap()
                .next()
                .unwrap();
        });
        t("point query (cached)", &|| {
            c.prepare_cached(&q)
                .unwrap()
                .query(["user7@example.com"])
                .unwrap()
                .next()
                .unwrap();
        });
    }
}
