//! mentat_duckdb_store end to end, on a real DuckDB (bundled client library):
//! datoms land in DuckDB tables, and the engine's transact / query / pull /
//! history / as-of / cas / retractEntity behave as on SQLite.

use duckdb::Connection;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use mentat_query_algebrizer::{QueryInputs, TemporalBound};
use mentat_query_projector::QueryResults;

use core_traits::{Binding, TypedValue};

fn rows(out: QueryResults) -> Vec<Vec<String>> {
    fn cell(b: &Binding) -> String {
        match b {
            Binding::Scalar(TypedValue::String(s)) => s.to_string(),
            Binding::Scalar(TypedValue::Long(l)) => l.to_string(),
            Binding::Scalar(TypedValue::Ref(r)) => r.to_string(),
            Binding::Scalar(TypedValue::Double(d)) => d.to_string(),
            Binding::Scalar(TypedValue::Boolean(b)) => b.to_string(),
            Binding::Scalar(TypedValue::Keyword(k)) => k.to_string(),
            other => format!("{other:?}"),
        }
    }
    let mut v: Vec<Vec<String>> = match out {
        QueryResults::Scalar(s) => s.iter().map(|b| vec![cell(b)]).collect(),
        QueryResults::Coll(c) => c.iter().map(|b| vec![cell(b)]).collect(),
        QueryResults::Tuple(t) => t.iter().map(|r| r.iter().map(cell).collect()).collect(),
        QueryResults::Rel(r) => r.rows().map(|r| r.iter().map(cell).collect()).collect(),
    };
    v.sort();
    v
}

fn q(store: &DuckStore, query: &str) -> Vec<Vec<String>> {
    rows(store.q(query, None, None).expect(query).results)
}

const SCHEMA: &str = r#"[
  {:db/ident :person/name  :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true}
  {:db/ident :person/age   :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
  {:db/ident :person/score :db/valueType :db.type/double :db/cardinality :db.cardinality/one}
  {:db/ident :person/tag   :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}
  {:db/ident :person/friend :db/valueType :db.type/ref   :db/cardinality :db.cardinality/many}
]"#;

fn seeded(c: &Connection) -> DuckStore<'_> {
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn(c)));
    let s = DuckStore::new(conn, "default");
    s.transact(SCHEMA).expect("schema");
    s.transact(
        r#"[{:db/id "a" :person/name "Alice" :person/age 30 :person/score 9.5 :person/tag [:t/x :t/y]}
            {:db/id "b" :person/name "Bob"   :person/age 25 :person/score 5.0 :person/friend "a"}]"#,
    )
    .expect("data");
    s
}

#[test]
fn datoms_live_in_duckdb_tables() {
    let c = Connection::open_in_memory().unwrap();
    let _s = seeded(&c);
    let n: i64 = c
        .query_row(
            "SELECT count(*) FROM mentat.datoms WHERE a > 65535",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(n >= 9, "user datoms in mentat.datoms: {n}");
    let names: Vec<String> = c
        .prepare("SELECT union_extract(d.v, 's') FROM mentat.datoms d JOIN mentat.idents i ON i.e = d.a \
                  WHERE union_extract(i.v, 's') = ':person/name' ORDER BY 1")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(names, vec!["Alice".to_string(), "Bob".to_string()]);
}

#[test]
fn query_patterns_joins_and_predicates() {
    let c = Connection::open_in_memory().unwrap();
    let s = seeded(&c);
    assert_eq!(
        q(
            &s,
            "[:find ?n ?a :where [?e :person/name ?n] [?e :person/age ?a]]"
        ),
        vec![
            vec!["Alice".to_string(), "30".into()],
            vec!["Bob".into(), "25".into()]
        ]
    );
    assert_eq!(
        q(
            &s,
            "[:find ?n :where [?e :person/age ?a] [(> ?a 26)] [?e :person/name ?n]]"
        ),
        vec![vec!["Alice".to_string()]]
    );
    // Numeric predicate on a double attribute with an integer constant (SQLite compares across INTEGER/REAL).
    assert_eq!(
        q(
            &s,
            "[:find ?n :where [?e :person/score ?x] [(< ?x 6)] [?e :person/name ?n]]"
        ),
        vec![vec!["Bob".to_string()]]
    );
    // Ref join.
    assert_eq!(
        q(&s, "[:find ?fn :where [?b :person/name \"Bob\"] [?b :person/friend ?f] [?f :person/name ?fn]]"),
        vec![vec!["Alice".to_string()]]
    );
    // Cardinality many + keyword values. (Keyword values must be namespaced:
    // the shared codec, SQLite included, rejects a bare `:x`.)
    assert_eq!(
        q(&s, "[:find ?t :where [_ :person/tag ?t]]"),
        vec![vec![":t/x".to_string()], vec![":t/y".into()]]
    );
    // Aggregates.
    assert_eq!(
        q(&s, "[:find (count ?e) . :where [?e :person/name _]]"),
        vec![vec!["2".to_string()]]
    );
    assert_eq!(
        q(&s, "[:find (sum ?a) . :where [_ :person/age ?a]]"),
        vec![vec!["55".to_string()]]
    );
    assert_eq!(
        q(&s, "[:find (max ?x) . :where [_ :person/score ?x]]"),
        vec![vec!["9.5".to_string()]]
    );
    // not / or.
    assert_eq!(
        q(
            &s,
            "[:find ?n :where [?e :person/name ?n] (not [?e :person/friend _])]"
        ),
        vec![vec!["Alice".to_string()]]
    );
}

#[test]
fn inputs_upsert_cardinality_one_and_tx_fns() {
    let c = Connection::open_in_memory().unwrap();
    let s = seeded(&c);
    let out = s
        .q(
            "[:find ?a . :in ?n :where [?e :person/name ?n] [?e :person/age ?a]]",
            Some(QueryInputs::with_value_sequence(vec![(
                edn::query::Variable::from_valid_name("?n"),
                TypedValue::typed_string("Bob"),
            )])),
            None,
        )
        .unwrap();
    assert_eq!(rows(out.results), vec![vec!["25".to_string()]]);

    // Upsert by :db.unique/identity + cardinality-one replacement.
    s.transact(r#"[{:person/name "Alice" :person/age 31}]"#)
        .unwrap();
    assert_eq!(
        q(
            &s,
            "[:find ?a . :where [?e :person/name \"Alice\"] [?e :person/age ?a]]"
        ),
        vec![vec!["31".to_string()]]
    );
    assert_eq!(
        q(&s, "[:find (count ?e) . :where [?e :person/name _]]"),
        vec![vec!["2".to_string()]]
    );

    // :db.fn/cas, and a failing cas leaves the store unchanged (atomic).
    let alice = q(&s, "[:find ?e . :where [?e :person/name \"Alice\"]]")[0][0].clone();
    s.transact(&format!("[[:db.fn/cas {alice} :person/age 31 32]]"))
        .unwrap();
    assert!(s
        .transact(&format!(
            "[[:db.fn/cas {alice} :person/age 99 1] [:db/add {alice} :person/age 50]]"
        ))
        .is_err());
    assert_eq!(
        q(
            &s,
            "[:find ?a . :where [?e :person/name \"Alice\"] [?e :person/age ?a]]"
        ),
        vec![vec!["32".to_string()]]
    );

    // retractEntity.
    let bob = q(&s, "[:find ?e . :where [?e :person/name \"Bob\"]]")[0][0].clone();
    s.transact(&format!("[[:db/retractEntity {bob}]]")).unwrap();
    assert_eq!(
        q(&s, "[:find ?n :where [_ :person/name ?n]]"),
        vec![vec!["Alice".to_string()]]
    );
}

#[test]
fn history_as_of_since() {
    let c = Connection::open_in_memory().unwrap();
    let s = seeded(&c);
    let t1 = s.last_tx().unwrap();
    s.transact(r#"[{:person/name "Alice" :person/age 40}]"#)
        .unwrap();
    let as_of = s
        .q(
            "[:find ?a . :where [?e :person/name \"Alice\"] [?e :person/age ?a]]",
            None,
            Some(TemporalBound::AsOf(t1)),
        )
        .unwrap();
    assert_eq!(rows(as_of.results), vec![vec!["30".to_string()]]);
    // `since` bounds history patterns (as on SQLite): only the datoms of the
    // later transaction, i.e. the retraction of 30 and the assertion of 40.
    let since = s
        .q(
            "[:find ?a ?added :where [_ :person/age ?a ?tx ?added]]",
            None,
            Some(TemporalBound::Since(t1)),
        )
        .unwrap();
    assert_eq!(
        rows(since.results),
        vec![
            vec!["30".to_string(), "false".into()],
            vec!["40".to_string(), "true".into()]
        ]
    );
    // A 5-place history pattern sees the retraction of 30.
    let hist = q(
        &s,
        "[:find ?a ?added :where [?e :person/name \"Alice\"] [?e :person/age ?a ?tx ?added]]",
    );
    assert!(
        hist.contains(&vec!["30".to_string(), "false".into()]),
        "{hist:?}"
    );
    assert!(
        hist.contains(&vec!["40".to_string(), "true".into()]),
        "{hist:?}"
    );
}

#[test]
fn pull_and_named_stores() {
    let c = Connection::open_in_memory().unwrap();
    let s = seeded(&c);
    let out = s
        .q(
            "[:find (pull ?e [:person/name :person/age]) . :where [?e :person/name \"Bob\"]]",
            None,
            None,
        )
        .unwrap();
    let QueryResults::Scalar(Some(Binding::Map(m))) = out.results else {
        panic!("{:?}", out.results)
    };
    assert_eq!(m.0.len(), 2, "{m:?}");

    // A second, path-named store is a separate DuckDB schema.
    let conn2: &dyn SqlConn = Box::leak(Box::new(DuckConn(&c)));
    let other = DuckStore::new(conn2, "/tmp/demo.mentat");
    other.transact(SCHEMA).unwrap();
    other.transact(r#"[{:person/name "Zed"}]"#).unwrap();
    assert_eq!(
        q(&other, "[:find ?n :where [_ :person/name ?n]]"),
        vec![vec!["Zed".to_string()]]
    );
    assert_eq!(
        q(&s, "[:find (count ?e) . :where [?e :person/name _]]"),
        vec![vec!["2".to_string()]]
    );
    assert!(
        other.schema_name().starts_with("mentat_tmp_demo_mentat_"),
        "{}",
        other.schema_name()
    );
}

#[test]
fn persists_in_a_duckdb_file() {
    let dir = std::env::temp_dir().join(format!("mentat-duck-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("p.duckdb");
    {
        let c = Connection::open(&path).unwrap();
        let _s = seeded(&c);
    }
    let c = Connection::open(&path).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM mentat.transactions", [], |r| r.get(0))
        .unwrap();
    assert!(n > 0);
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn(&c)));
    let s = DuckStore::new(conn, "default");
    assert_eq!(
        q(&s, "[:find (count ?e) . :where [?e :person/name _]]"),
        vec![vec!["2".to_string()]]
    );
    let _ = std::fs::remove_dir_all(dir);
}
