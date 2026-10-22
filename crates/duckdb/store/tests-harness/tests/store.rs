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
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(c)));
    let s = DuckStore::new(conn, "default").unwrap();
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
    let conn2: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let other = DuckStore::new(conn2, "/tmp/demo.mentat").unwrap();
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
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.duckdb");
    {
        let c = Connection::open(&path).unwrap();
        let _s = seeded(&c);
    }
    let c = Connection::open(&path).unwrap();
    let n: i64 = c
        .query_row("SELECT count(*) FROM mentat.transactions", [], |r| r.get(0))
        .unwrap();
    assert!(n > 0);
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "default").unwrap();
    assert_eq!(
        q(&s, "[:find (count ?e) . :where [?e :person/name _]]"),
        vec![vec!["2".to_string()]]
    );
}

/// A store reopens from its WAL: the database closed without a checkpoint
/// (as when the DuckDB process exits after `edn_t`). A named `CREATE TYPE`
/// in the store's schema broke WAL replay on DuckDB 1.5.6.
#[test]
fn reopens_from_wal_without_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("w.duckdb");
    {
        let c = Connection::open(&path).unwrap();
        c.execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        let _s = seeded(&c);
    }
    assert!(
        dir.path().join("w.duckdb.wal").exists(),
        "expected an un-checkpointed WAL"
    );
    let c = Connection::open(&path).expect("WAL replays");
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "default").unwrap();
    assert_eq!(
        q(&s, "[:find (count ?e) . :where [?e :person/name _]]"),
        vec![vec!["2".to_string()]]
    );
}

/// Stores from 1.11.0-1.14.0 carry ART indexes that duckdb/duckdb#26828
/// corrupts on WAL replay; opening such a store drops them, and a store
/// created now has none.
#[test]
fn legacy_secondary_indexes_are_dropped_on_open() {
    let index_count = |c: &Connection| -> i64 {
        c.query_row(
            "SELECT count(*) FROM duckdb_indexes() WHERE schema_name = 'mentat'",
            [],
            |r| r.get(0),
        )
        .unwrap()
    };
    let c = Connection::open_in_memory().unwrap();
    let _s = seeded(&c);
    assert_eq!(index_count(&c), 0, "new stores have no secondary indexes");

    // Recreate the 1.14.0 layout's indexes, as an old store has them.
    c.execute_batch(
        "CREATE INDEX idx_datoms_ea ON mentat.datoms (e, a); \
         CREATE INDEX idx_datoms_a ON mentat.datoms (a); \
         CREATE INDEX idx_tt_tx ON mentat.timelined_transactions (tx); \
         UPDATE mentat.meta SET value = '1' WHERE key = 'generation';",
    )
    .unwrap();
    assert_eq!(index_count(&c), 3);
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "default").unwrap();
    let names = q(&s, "[:find ?n :where [_ :person/name ?n]]");
    assert_eq!(names.len(), 2, "the store still answers: {names:?}");
    assert_eq!(index_count(&c), 0, "legacy indexes dropped on open");
}

/// The same query, run before and after each write, on two stores sharing
/// one connection, sees every write. On DuckDB 1.5.x a reused prepared plan
/// keeps the statistics it was planned with (duckdb/duckdb#24696): one
/// planned while an attribute had no datoms kept returning nothing.
#[test]
fn repeated_queries_see_new_datoms() {
    let c = Connection::open_in_memory().unwrap();
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let (one, two) = (
        DuckStore::new(conn, "one").unwrap(),
        DuckStore::new(conn, "two").unwrap(),
    );
    let count = "[:find (count ?e) . :where [?e :person/age _]]";
    let pull = "[:find (pull ?e [:person/age]) . :where [?e :person/name \"P\"]]";
    for s in [&one, &two] {
        s.transact(SCHEMA).unwrap();
        assert_eq!(q(s, count), vec![vec!["0".to_string()]]);
    }
    for (i, w) in ["one", "one", "two", "one", "two"].iter().enumerate() {
        let s = if *w == "one" { &one } else { &two };
        s.transact(&format!("[{{:person/name \"p{i}\" :person/age {i}}}]"))
            .unwrap();
        let (n1, n2) = (q(&one, count), q(&two, count));
        let want = |store: &str| {
            let n = ["one", "one", "two", "one", "two"][..=i]
                .iter()
                .filter(|x| **x == store)
                .count();
            vec![vec![n.to_string()]]
        };
        assert_eq!((n1, n2), (want("one"), want("two")), "after write {i}");
    }
    // A pull before and after its entity gains an attribute.
    one.transact("[{:person/name \"P\"}]").unwrap();
    let before = format!("{:?}", one.q(pull, None, None).unwrap().results);
    one.transact("[{:person/name \"P\" :person/age 77}]")
        .unwrap();
    let after = format!("{:?}", one.q(pull, None, None).unwrap().results);
    assert!(
        !before.contains("77") && after.contains("77"),
        "{before} / {after}"
    );
}

/// A query run before an attribute (and its entid) exists sees it after.
#[test]
fn query_before_a_new_attribute_sees_it_after() {
    let c = Connection::open_in_memory().unwrap();
    let s = seeded(&c);
    let ids = "[:find (count ?e) . :where [?e :person/email _]]";
    let by_value = "[:find ?e . :where [?e :person/email \"z@x\"]]";
    // Before the attribute exists: no rows (not an error).
    let before = format!("{:?}", s.q(ids, None, None).unwrap().results);
    assert!(!before.contains("Long(1)"), "{before}");
    s.transact(
        "[{:db/ident :person/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
    )
    .unwrap();
    assert_eq!(q(&s, ids), vec![vec!["0".to_string()]]);
    assert_eq!(q(&s, by_value), Vec::<Vec<String>>::new());
    let e = s
        .transact("[{:db/id \"z\" :person/email \"z@x\"}]")
        .unwrap()
        .tempids["z"];
    assert_eq!(q(&s, ids), vec![vec!["1".to_string()]]);
    assert_eq!(q(&s, by_value), vec![vec![e.to_string()]]);
}

/// A store from before 1.18 has an (empty, unused) `fulltext_values` table;
/// opening it replaces the table with the view `(fulltext ...)` joins, and
/// fulltext values transacted before then match.
#[test]
fn pre_1_18_fulltext_table_is_upgraded_on_open() {
    let c = Connection::open_in_memory().unwrap();
    let s = seeded(&c);
    s.transact(
        "[{:db/ident :person/bio :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}]",
    )
    .unwrap();
    s.transact(r#"[{:person/name "Alice" :person/bio "Hello darkness"}]"#)
        .unwrap();
    c.execute_batch(
        "DROP VIEW mentat.fulltext_values; \
         CREATE TABLE mentat.fulltext_values (rowid_ BIGINT, text VARCHAR NOT NULL, searchid BIGINT); \
         UPDATE mentat.meta SET value = '1' WHERE key = 'generation';",
    )
    .unwrap();
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "default").unwrap();
    assert_eq!(
        q(
            &s,
            "[:find ?n :where [(fulltext $ :person/bio \"DARKNESS\") [[?e]]] [?e :person/name ?n]]"
        ),
        vec![vec!["Alice".to_string()]]
    );
    let views: i64 = c
        .query_row(
            "SELECT count(*) FROM duckdb_views() WHERE schema_name = 'mentat' AND view_name = 'fulltext_values'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(views, 1);
}

/// A pre-1.18 store (an old `fulltext_values` table and legacy indexes) opened
/// read-only -- the whole file, or ATTACHed READ_ONLY -- answers plain
/// queries: `open` doesn't try to upgrade it. Its `(fulltext ...)` queries
/// find nothing until it's opened writable once.
#[test]
fn pre_1_18_store_opens_read_only() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("old.duckdb");
    let names = "[:find ?n :where [_ :person/name ?n]]";
    let fts = "[:find ?n :where [(fulltext $ :person/bio \"darkness\") [[?e]]] [?e :person/name ?n]]";
    {
        let c = Connection::open(&path).unwrap();
        let s = seeded(&c);
        s.transact(
            "[{:db/ident :person/bio :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}]",
        )
        .unwrap();
        s.transact(r#"[{:person/name "Alice" :person/bio "Hello darkness"}]"#)
            .unwrap();
        // The 1.15-1.17 layout.
        c.execute_batch(
            "DROP VIEW mentat.fulltext_values; \
             CREATE TABLE mentat.fulltext_values (rowid_ BIGINT, text VARCHAR NOT NULL, searchid BIGINT); \
             CREATE INDEX idx_datoms_a ON mentat.datoms (a); \
             UPDATE mentat.meta SET value = '1' WHERE key = 'generation';",
        )
        .unwrap();
    }
    let want = vec![vec!["Alice".to_string()], vec!["Bob".to_string()]];
    {
        let c = Connection::open_with_flags(
            &path,
            duckdb::Config::default()
                .access_mode(duckdb::AccessMode::ReadOnly)
                .unwrap(),
        )
        .unwrap();
        let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
        let s = DuckStore::new(conn, "default").unwrap();
        assert_eq!(q(&s, names), want);
        assert_eq!(q(&s, fts), Vec::<Vec<String>>::new());
    }
    {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch(&format!("ATTACH '{}' AS old (READ_ONLY)", path.display()))
            .unwrap();
        let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
        let s = DuckStore::new(conn, "old.").unwrap();
        assert_eq!(q(&s, names), want);
    }
    // Writable once: upgraded, and fulltext finds the old text. (A new
    // generation, so this thread's cached schema doesn't skip the upgrade.)
    let c = Connection::open(&path).unwrap();
    c.execute_batch("UPDATE mentat.meta SET value = '2' WHERE key = 'generation'")
        .unwrap();
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "default").unwrap();
    assert_eq!(q(&s, fts), vec![vec!["Alice".to_string()]]);
}

/// `'db.name'` addresses store `name` in an ATTACHed database: every row
/// lands in that file, not in the main one; the store reopens from the file
/// (also from an un-checkpointed WAL), under another ATTACH name, and when the
/// file is opened directly.
#[test]
fn stores_in_an_attached_database() {
    let dir = std::env::temp_dir().join(format!("mentat-duck-attach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let (main, other) = (dir.join("home.duckdb"), dir.join("other.duckdb"));
    let count = "[:find (count ?e) . :where [?e :person/name _]]";
    let fts = "[:find ?n :where [(fulltext $ :person/bio \"dark*\") [[?e]]] [?e :person/name ?n]]";
    let attach = |c: &Connection, alias: &str| {
        c.execute_batch(&format!("ATTACH '{}' AS {alias}", other.display()))
            .unwrap();
    };
    {
        let c = Connection::open(&main).unwrap();
        attach(&c, "otherdb");
        let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
        let s = DuckStore::new(conn, "otherdb.people").unwrap();
        assert_eq!(s.catalog_name(), "otherdb");
        s.transact(SCHEMA).unwrap();
        s.transact(
            "[{:db/ident :person/bio :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}]",
        )
        .unwrap();
        s.transact(r#"[{:person/name "Ann" :person/bio "Hello darkness"} {:person/name "Ben"}]"#)
            .unwrap();
        assert_eq!(q(&s, count), vec![vec!["2".to_string()]]);
        assert_eq!(q(&s, fts), vec![vec!["Ann".to_string()]]);
        // `'otherdb.'` is otherdb's default store; the main database's
        // stores are separate, and the main file holds no store at all.
        let d = DuckStore::new(conn, "otherdb.").unwrap();
        assert_eq!((d.catalog_name(), d.schema_name()), ("otherdb", "mentat"));
        d.transact(SCHEMA).unwrap();
        let m = DuckStore::new(conn, "people").unwrap();
        assert_eq!(m.catalog_name(), "home");
        m.transact(SCHEMA).unwrap();
        assert_eq!(q(&m, count), vec![vec!["0".to_string()]]);
        let where_: Vec<(String, String)> = c
            .prepare(
                "SELECT database_name, schema_name FROM duckdb_tables() \
                 WHERE table_name = 'datoms' ORDER BY ALL",
            )
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            where_,
            vec![
                ("home".to_string(), m.schema_name().to_string()),
                ("otherdb".to_string(), "mentat".to_string()),
                ("otherdb".to_string(), s.schema_name().to_string()),
            ]
        );
        // Close without a checkpoint: the attached file reopens from its WAL.
        c.execute_batch("PRAGMA disable_checkpoint_on_shutdown")
            .unwrap();
        s.transact(r#"[{:person/name "Cy"}]"#).unwrap();
    }
    {
        // Attached under another name.
        let c = Connection::open(&main).unwrap();
        attach(&c, "Again");
        let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
        let s = DuckStore::new(conn, "again.people").unwrap();
        assert_eq!(s.catalog_name(), "Again");
        assert_eq!(q(&s, count), vec![vec!["3".to_string()]]);
        assert_eq!(q(&s, fts), vec![vec!["Ann".to_string()]]);
        s.transact(r#"[{:person/name "Di"}]"#).unwrap();
    }
    // The file on its own, as the main database: the same store, by name.
    let c = Connection::open(&other).unwrap();
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "people").unwrap();
    assert_eq!(q(&s, count), vec![vec!["4".to_string()]]);
    assert_eq!(q(&s, fts), vec![vec!["Ann".to_string()]]);
    let _ = std::fs::remove_dir_all(dir);
}

/// A name whose prefix is not an attached database is a store in the home
/// database, as before; so is a dotted name already in use there.
#[test]
fn dotted_names_without_an_attached_database() {
    let c = Connection::open_in_memory().unwrap();
    let conn: &dyn SqlConn = Box::leak(Box::new(DuckConn::new(&c)));
    let s = DuckStore::new(conn, "nodb.people").unwrap();
    assert_eq!(s.catalog_name(), "memory");
    assert!(s.schema_name().starts_with("mentat_nodb_people_"));
    s.transact(SCHEMA).unwrap();
    c.execute_batch("ATTACH ':memory:' AS nodb").unwrap();
    let again = DuckStore::new(conn, "nodb.people").unwrap();
    assert_eq!(
        again.catalog_name(),
        "memory",
        "an existing store keeps its name"
    );
    let fresh = DuckStore::new(conn, "nodb.fresh").unwrap();
    assert_eq!(fresh.catalog_name(), "nodb");
}
