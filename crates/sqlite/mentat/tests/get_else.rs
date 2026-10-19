//! `get-else` and `missing?` on the embedded SQLite engine (issue #335). The
//! same cases as pg_mentat's get_else_tests: several patterns, the entity
//! bound by a middle pattern or as a ref value, two get-else in one query,
//! predicates on the result, cardinality-many rejected, and missing?.

extern crate mentat;

use mentat::{Queryable, Store};

fn setup() -> Store {
    let mut s = Store::open("").expect("opened");
    s.transact(
        r#"[
        {:db/ident :t/kind    :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one :db/index true}
        {:db/ident :t/value   :db/valueType :db.type/string  :db/cardinality :db.cardinality/one}
        {:db/ident :t/private :db/valueType :db.type/boolean :db/cardinality :db.cardinality/one}
        {:db/ident :t/rank    :db/valueType :db.type/long    :db/cardinality :db.cardinality/one}
        {:db/ident :t/tag     :db/valueType :db.type/string  :db/cardinality :db.cardinality/many}
        {:db/ident :t/person  :db/valueType :db.type/ref     :db/cardinality :db.cardinality/one}
        {:db/ident :p/email   :db/valueType :db.type/string  :db/cardinality :db.cardinality/one
         :db/unique :db.unique/identity :db/index true}
    ]"#,
    )
    .expect("schema");
    s.transact(
        r#"[{:db/id "x" :p/email "x@example.com"}
            {:t/kind :k/chat :t/value "a" :t/person "x" :t/rank 3}
            {:t/kind :k/chat :t/value "b" :t/person "x" :t/private true}
            {:t/kind :k/mail :t/value "c" :t/private false}]"#,
    )
    .expect("data");
    s
}

/// Rows as sorted debug strings, so set order doesn't matter.
fn rows(s: &Store, q: &str) -> Vec<String> {
    let out = s.q_once(q, None).unwrap_or_else(|e| panic!("{q}: {e:?}"));
    let mut v: Vec<String> = out
        .results
        .into_rel()
        .expect("rel")
        .into_iter()
        .map(|row| {
            let cells: Vec<String> = row
                .iter()
                .map(|b| match b.clone().into_scalar() {
                    Some(v) => format!("{v:?}"),
                    None => format!("{b:?}"),
                })
                .collect();
            format!("[{}]", cells.join(", "))
        })
        .collect();
    v.sort();
    v
}

fn expect(s: &Store, q: &str, want: &[&str]) {
    assert_eq!(
        rows(s, q),
        want.iter().map(|w| w.to_string()).collect::<Vec<_>>(),
        "{q}"
    );
}

#[test]
fn get_else_one_and_several_patterns() {
    let s = setup();
    expect(
        &s,
        "[:find ?v ?p :where [?c :t/value ?v] [(get-else $ ?c :t/private false) ?p]]",
        &[
            r#"[String("a"), Boolean(false)]"#,
            r#"[String("b"), Boolean(true)]"#,
            r#"[String("c"), Boolean(false)]"#,
        ],
    );
    // The 2026-10-18 report's shape: entity bound by the first pattern.
    expect(
        &s,
        "[:find ?v ?p :where [?c :t/kind :k/chat] [?c :t/value ?v] \
         [(get-else $ ?c :t/private false) ?p]]",
        &[
            r#"[String("a"), Boolean(false)]"#,
            r#"[String("b"), Boolean(true)]"#,
        ],
    );
    // Entity bound by a middle pattern, via a lookup on a ref.
    expect(
        &s,
        "[:find ?k ?v ?priv :where [?p :p/email \"x@example.com\"] [?c :t/person ?p] \
         [?c :t/kind ?k] [?c :t/value ?v] [(get-else $ ?c :t/private false) ?priv]]",
        &[
            r#"[Keyword(Keyword(NamespaceableName { namespace: Some("k"), name: "chat" })), String("a"), Boolean(false)]"#,
            r#"[Keyword(Keyword(NamespaceableName { namespace: Some("k"), name: "chat" })), String("b"), Boolean(true)]"#,
        ],
    );
}

#[test]
fn get_else_twice_and_on_a_ref_value() {
    let s = setup();
    expect(
        &s,
        "[:find ?v ?priv ?rank ?mail :where [?c :t/kind :k/chat] [?c :t/value ?v] [?c :t/person ?p] \
         [(get-else $ ?c :t/private false) ?priv] [(get-else $ ?c :t/rank 0) ?rank] \
         [(get-else $ ?p :p/email \"none\") ?mail]]",
        &[
            r#"[String("a"), Boolean(false), Long(3), String("x@example.com")]"#,
            r#"[String("b"), Boolean(true), Long(0), String("x@example.com")]"#,
        ],
    );
}

#[test]
fn predicate_on_a_get_else_result() {
    let s = setup();
    expect(
        &s,
        "[:find ?v :where [?c :t/kind :k/chat] [?c :t/value ?v] \
         [(get-else $ ?c :t/rank 0) ?r] [(> ?r 1)]]",
        &[r#"[String("a")]"#],
    );
}

#[test]
fn get_else_on_cardinality_many_is_an_error() {
    let s = setup();
    let err = s
        .q_once(
            "[:find ?v ?t :where [?c :t/value ?v] [(get-else $ ?c :t/tag \"none\") ?t]]",
            None,
        )
        .expect_err("card-many get-else must be rejected");
    assert!(format!("{err:?}").contains("cardinality-many"), "{err:?}");
}

#[test]
fn missing_alone_and_with_get_else() {
    let s = setup();
    expect(
        &s,
        "[:find ?v :where [?c :t/value ?v] [(missing? $ ?c :t/private)]]",
        &[r#"[String("a")]"#],
    );
    expect(
        &s,
        "[:find ?v ?r :where [?c :t/kind :k/chat] [?c :t/value ?v] \
         [(missing? $ ?c :t/private)] [(get-else $ ?c :t/rank 0) ?r]]",
        &[r#"[String("a"), Long(3)]"#],
    );
}
