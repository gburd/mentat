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

/// Fulltext attributes and values, transacted after `TXS`.
const FULLTEXT_TXS: &[&str] = &[
    // Fulltext: SQLite FTS4 vs DuckDB's tokenized match.
    r#"[{:db/ident :p/bio  :db/valueType :db.type/string :db/cardinality :db.cardinality/one  :db/fulltext true :db/index true}
        {:db/ident :p/tags :db/valueType :db.type/string :db/cardinality :db.cardinality/many :db/fulltext true :db/index true}]"#,
    r#"[{:p/name "Alice" :p/bio "Hello darkness, my old friend" :p/tags ["sustainable food" "Ally's pick"]}
        {:p/name "Bob"   :p/bio "I've come to talk with you again" :p/tags ["food"]}
        {:p/name "Carol" :p/bio "Hello-World foo_bar ÉCOLE café 3.14 Straße"}
        {:p/name "Dan"   :p/bio "hello darkness"}
        {:p/name "Eve"   :p/bio "Hello darkness, my old friend" :p/tags ["nightlife"]}]"#,
    // Replacing a fulltext value: the old text no longer matches for Bob.
    r#"[{:p/name "Bob" :p/bio "talk is cheap"}]"#,
    // FTS4 syntax corners: texts that tell a phrase from an AND of its
    // words, and SQLite's unicode61 tokens and case folds.
    r#"[{:db/ident :p/note :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true :db/index true}]"#,
    r#"[{:p/name "n1"  :p/note "hello world"}
        {:p/name "n2"  :p/note "world says hello"}
        {:p/name "n3"  :p/note "don t"}
        {:p/name "n4"  :p/note "t don"}
        {:p/name "n5"  :p/note "b a"}
        {:p/name "n6"  :p/note "a bc"}
        {:p/name "n7"  :p/note "not a"}
        {:p/name "n8"  :p/note "or a"}
        {:p/name "n9"  :p/note "hello or world"}
        {:p/name "n10" :p/note "c or java"}
        {:p/name "n11" :p/note "a b"}
        {:p/name "n12" :p/note "́abc x́y"}
        {:p/name "n13" :p/note "हिंदी"}
        {:p/name "n14" :p/note "İstanbul"}
        {:p/name "n15" :p/note "ΣΊΣΥΦΟΣ"}
        {:p/name "n16" :p/note "σίσυφοσ"}
        {:p/name "n17" :p/note "Ⓐ foo_bar ﬁle"}
        {:p/name "n18" :p/note "a c"}]"#,
];

/// `(fulltext ...)` queries; each must work on both engines.
const FULLTEXT: &[&str] = &[
    "[:find ?e ?v ?tx ?s :where [(fulltext $ :p/bio \"hello\") [[?e ?v ?tx ?s]]]]",
    "[:find ?n :where [(fulltext $ :p/bio \"darkness\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"DARK*\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"dark\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"\\\"hello darkness\\\"\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"\\\"darkness hello\\\"\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"\\\"hel* dark*\\\"\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"hello friend\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"hello OR talk\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"hello NOT friend\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"(hello OR talk) AND cheap\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"hello OR talk NOT cheap\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"again\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"hello-world\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"foo\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"école\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"Café\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"cafe\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"straße\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"14\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"hello ---\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"---\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/bio \"talk\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n ?t :where [(fulltext $ :p/tags \"food\") [[?e ?t]]] [?e :p/name ?n]]",
    "[:find ?n ?t :where [(fulltext $ :p/tags \"ally\") [[?e ?t]]] [?e :p/name ?n]]",
    // Not a fulltext attribute: known empty.
    "[:find ?e :where [(fulltext $ :p/name \"Alice\") [[?e]]]]",
    // A search term bound by an earlier pattern (Alice's nicks: Al, Ally).
    "[:find ?e ?v :where [?a :p/name \"Alice\"] [?a :p/nick ?k] [(fulltext $ :p/tags ?k) [[?e ?v]]]]",
    // Plain patterns over a fulltext attribute.
    "[:find ?n ?b :where [?e :p/bio ?b] [?e :p/name ?n]]",
    "[:find ?n :where [?e :p/bio \"hello darkness\"] [?e :p/name ?n]]",
    "[:find ?t :where [_ :p/tags ?t]]",
    // (Pull has no order for cardinality-many values; tags are compared above.)
    "[:find (pull ?e [:p/name :p/bio]) . :where [?e :p/name \"Alice\"]]",
    "[:find ?n :where [(fulltext $ :p/bio \"darkness\") [[?e]]] [?e :p/name ?n] [?e :p/age 25]]",
    "[:find ?n :where [(fulltext $ :p/note \"hello-world\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"don't\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"a*b\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"\\\"a b\\\"*\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"--- NOT a\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"--- OR a\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"hello. OR world\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"C++ OR java\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"a \\\"\\\"\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"\\\"\\\" OR a\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"a NOT \\\"\\\"\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"a NOT b NEAR c\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"^a\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"^b\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"b ^a\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"text:hello\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"searchid:1\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"abc\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"x\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"द\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"ह\") [[?e]]] [?e :p/name ?n]]",
    // (`istanbul` would differ: DuckDB folds İ to i, SQLite keeps it.)
    "[:find ?n :where [(fulltext $ :p/note \"İstanbul\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"σίσυφος\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"ς\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"ⓐ\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"foo\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"file\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"ﬁle\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"a NEAR b\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"(a b) c\") [[?e]]] [?e :p/name ?n]]",
    "[:find ?n :where [(fulltext $ :p/note \"a (b c)\") [[?e]]] [?e :p/name ?n]]",
];

/// Malformed FTS4 queries: both engines must reject them.
const FULLTEXT_BAD: &[&str] = &[
    "hello OR",
    "NOT hello",
    "(hello",
    "hello)",
    "\\\"hello",
    "AND",
    "hello NOT ---",
    "() OR a",
    "(a OR b) NEAR c",
    "a NEAR (b OR c)",
    "a (NOT b)",
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
    let dc = DuckConn::new(&c);
    let duck = DuckStore::new(&dc as &dyn SqlConn, "default").unwrap();

    let mut tx_ids = Vec::new();
    let mut counts = Vec::new();
    for (i, tx) in TXS.iter().chain(FULLTEXT_TXS).enumerate() {
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
        counts.push((a.datoms_asserted.unwrap(), a.datoms_retracted.unwrap()));
    }

    // TXS's last four: a no-op, then a replacement and a new value, a
    // keyword, then three new refs.
    assert_eq!(counts[TXS.len() - 4..TXS.len()], [(0, 0), (2, 1), (1, 0), (3, 0)]);

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
                    "99)]]",
                    "(fulltext",
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
    for q in FULLTEXT {
        check("fulltext", q, None, false);
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
    for bad in FULLTEXT_BAD {
        use mentat::Queryable;
        let q = format!("[:find ?e :where [(fulltext $ :p/bio \"{bad}\") [[?e]]]]");
        let (a, b) = (sqlite.q_once(&q, None), duck.q(&q, None, None));
        if a.is_ok() || b.is_ok() {
            failures.push(format!(
                "bad fulltext {q}: sqlite ok={} duckdb ok={}",
                a.is_ok(),
                b.is_ok()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
