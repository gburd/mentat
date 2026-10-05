//! Differential test: the same transactions and queries against mentat's
//! SQLite store and against mentat_duckdb_store, compared result for result.
//! Any query where the engines disagree is a DuckDB-dialect bug.

use duckdb::Connection;
use mentat_duckdb_store::{DuckStore, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use mentat_query_algebrizer::TemporalBound;
use mentat_query_projector::QueryResults;

use core_traits::Binding;

fn norm(r: QueryResults) -> String {
    fn b(x: &Binding) -> String {
        format!("{x:?}")
    }
    match r {
        QueryResults::Scalar(s) => format!("scalar {:?}", s.as_ref().map(b)),
        QueryResults::Coll(c) => {
            let mut v: Vec<String> = c.iter().map(b).collect();
            v.sort();
            format!("coll {v:?}")
        }
        QueryResults::Tuple(t) => {
            format!("tuple {:?}", t.map(|t| t.iter().map(b).collect::<Vec<_>>()))
        }
        QueryResults::Rel(r) => {
            let mut v: Vec<String> = r
                .rows()
                .map(|row| row.iter().map(b).collect::<Vec<_>>().join(" | "))
                .collect();
            v.sort();
            format!("rel {v:?}")
        }
    }
}

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

const TXS: &[&str] = &[
    r#"[{:db/ident :p/name   :db/valueType :db.type/string  :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true}
        {:db/ident :p/age    :db/valueType :db.type/long    :db/cardinality :db.cardinality/one :db/index true}
        {:db/ident :p/height :db/valueType :db.type/double  :db/cardinality :db.cardinality/one}
        {:db/ident :p/alive  :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
        {:db/ident :p/born   :db/valueType :db.type/instant :db/cardinality :db.cardinality/one}
        {:db/ident :p/id     :db/valueType :db.type/uuid    :db/cardinality :db.cardinality/one :db/unique :db.unique/value :db/index true}
        {:db/ident :p/role   :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}
        {:db/ident :p/likes  :db/valueType :db.type/ref     :db/cardinality :db.cardinality/many}
        {:db/ident :p/boss   :db/valueType :db.type/ref     :db/cardinality :db.cardinality/one}
        {:db/ident :p/nick   :db/valueType :db.type/string  :db/cardinality :db.cardinality/many}
        {:db/ident :p/blob   :db/valueType :db.type/bytes   :db/cardinality :db.cardinality/one}]"#,
    r#"[{:db/id "a" :p/name "Alice" :p/age 30 :p/height 1.65 :p/alive true  :p/born #inst "1990-01-02T03:04:05.000Z"
         :p/id #uuid "11111111-1111-4111-8111-111111111111" :p/role [:r/admin :r/dev] :p/nick ["Al" "Ally"]}
        {:db/id "b" :p/name "Bob"   :p/age 25 :p/height 1.80 :p/alive true  :p/born #inst "1995-06-07T08:09:10.000Z"
         :p/id #uuid "22222222-2222-4222-8222-222222222222" :p/role [:r/dev] :p/boss "a" :p/likes ["a"]}
        {:db/id "c" :p/name "Carol" :p/age 41 :p/height 1.70 :p/alive false :p/born #inst "1980-11-12T13:14:15.000Z"
         :p/role [:r/ops] :p/boss "a" :p/likes ["a" "b"]}
        {:db/id "d" :p/name "Dan"   :p/age 25 :p/height 5.0}]"#,
    r#"[{:p/name "Alice" :p/age 31}]"#,
    r#"[{:p/name "Dan" :p/height 1.9}]"#,
    r#"[{:p/name "Eve" :p/age 25 :p/blob #bytes 010203}]"#,
];

const QUERIES: &[&str] = &[
    "[:find ?n :where [_ :p/name ?n]]",
    "[:find ?n ?a :where [?e :p/name ?n] [?e :p/age ?a]]",
    "[:find ?e . :where [?e :p/name \"Bob\"]]",
    "[:find [?n ...] :where [?e :p/age 25] [?e :p/name ?n]]",
    "[:find [?n ?a] :where [?e :p/name \"Carol\"] [?e :p/name ?n] [?e :p/age ?a]]",
    // Numeric predicates, integer and double constants, both attribute types.
    "[:find ?n :where [?e :p/age ?a] [(> ?a 26)] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/age ?a] [(<= ?a 25)] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/height ?h] [(< ?h 2)] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/height ?h] [(>= ?h 1.7)] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/age ?a] [(> ?a 30.5)] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/age ?a] [(!= ?a 25)] [?e :p/name ?n]]",
    // Value equality for every type.
    "[:find ?n :where [?e :p/alive false] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/role :r/dev] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/id #uuid \"22222222-2222-4222-8222-222222222222\"] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/born ?t] [(< ?t #inst \"1992-01-01T00:00:00.000Z\")] [?e :p/name ?n]]",
    "[:find ?b :where [_ :p/blob ?b]]",
    "[:find ?n :where [?e :p/nick \"Ally\"] [?e :p/name ?n]]",
    // Refs, joins, reverse.
    "[:find ?bn ?n :where [?e :p/boss ?b] [?b :p/name ?bn] [?e :p/name ?n]]",
    "[:find ?n :where [?a :p/name \"Alice\"] [?e :p/likes ?a] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/name ?n] (not [?e :p/boss _])]",
    "[:find ?n :where [?e :p/name ?n] (or [?e :p/age 25] [?e :p/alive false])]",
    "[:find ?n :where [?e :p/name ?n] (not-join [?e] [?e :p/role :r/dev])]",
    // Aggregates.
    "[:find (count ?e) . :where [?e :p/name _]]",
    "[:find (sum ?a) . :where [_ :p/age ?a]]",
    "[:find (avg ?a) . :where [_ :p/age ?a]]",
    "[:find (max ?a) . :where [_ :p/age ?a]]",
    "[:find (min ?h) . :where [_ :p/height ?h]]",
    "[:find (max ?n) . :where [_ :p/name ?n]]",
    "[:find ?a (count ?e) :where [?e :p/age ?a]]",
    "[:find (sum ?a) . :with ?e :where [?e :p/age ?a]]",
    "[:find (count-distinct ?a) . :where [_ :p/age ?a]]",
    "[:find (max ?t) . :where [_ :p/born ?t]]",
    // Strings: ordering and equality compare text, not numbers.
    "[:find ?n :where [_ :p/name ?n] [(< ?n \"Bz\")]]",
    "[:find ?n :where [?e :p/name ?n] [?e :p/nick ?k] [(!= ?k \"Al\")]]",
    // Unbound value type: [?e ?a ?v] with a numeric predicate across types.
    "[:find ?e ?v :where [?e :p/age ?v] [(< ?v 26)]]",
    // Types at the edges.
    "[:find ?n :where [?e :p/height 1.65] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/age 25.0] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/alive true] [?e :p/name ?n]]",
    // Functions and bindings.
    "[:find ?n ?x :where [?e :p/age ?a] [(* ?a 2) ?x] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/name ?n] [(get-else $ ?e :p/height 0.0) ?h] [(> ?h 1.75)]]",
    "[:find ?n :where [?e :p/name ?n] [(missing? $ ?e :p/boss)]]",
    "[:find ?n :where [(ground [\"Bob\" \"Zed\"]) [?n ...]] [_ :p/name ?n]]",
    "[:find ?e ?a ?v :where [?e :p/name \"Alice\"] [?e ?a ?v]]",
    // Lookup refs and idents as values.
    "[:find ?n :where [?e :p/boss [:p/name \"Alice\"]] [?e :p/name ?n]]",
    "[:find ?a :where [:db.part/db :db.install/attribute ?a] [?a :db/ident :p/age]]",
    // History (5-place) and tx.
    "[:find ?a ?added :where [?e :p/name \"Alice\"] [?e :p/age ?a ?tx ?added]]",
    "[:find ?h ?added :where [?e :p/name \"Dan\"] [?e :p/height ?h ?tx ?added]]",
    "[:find (pull ?e [:p/name :p/age {:p/boss [:p/name]}]) . :where [?e :p/name \"Bob\"]]",
    "[:find (pull ?e [*]) . :where [?e :p/name \"Carol\"]]",
    "[:find (pull ?e [:p/name :p/_boss]) . :where [?e :p/name \"Alice\"]]",
];

const ORDERED: &[&str] = &[
    "[:find ?n ?a :order (asc ?a) (asc ?n) :where [?e :p/name ?n] [?e :p/age ?a]]",
    "[:find ?n ?h :order (desc ?h) :where [?e :p/name ?n] [?e :p/height ?h]]",
    "[:find ?n :order ?n :limit 2 :where [_ :p/name ?n]]",
];

#[test]
fn duckdb_matches_sqlite() {
    let mut sqlite = mentat::Store::open("").unwrap();
    let c = Connection::open_in_memory().unwrap();
    let dc = DuckConn(&c);
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
