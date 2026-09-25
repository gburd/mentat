// Copyright 2016-2018 Mozilla
//
// The query-grammar contract shared by the SQLite and PostgreSQL backends.
// Every item here must parse identically in both repos' copies of `edn`.

use edn::parse;
use edn::query::{PatternNonValuePlace, WhereClause};

#[test]
fn in_sources_and_bindings() {
    // `$` is a source (in_sources), `[?a ...]` a binding (in_bindings).
    let q = parse::parse_query("[:find ?x :in $ [?a ...] :where [?x :foo/bar ?a]]").unwrap();
    assert_eq!((q.in_bindings.len(), q.in_sources.len()), (1, 1));
}

#[test]
fn in_scalar_derives_in_vars() {
    let q = parse::parse_query("[:find ?x :in ?y :where [?x :foo/bar ?y]]").unwrap();
    assert_eq!(q.in_vars.len(), 1);
    assert_eq!(q.in_bindings.len(), 1);
}

#[test]
fn five_place_pattern_parses_with_added() {
    let q = parse::parse_query("[:find ?e :where [?e ?a ?v ?tx ?added]]").unwrap();
    match &q.where_clauses[0] {
        WhereClause::Pattern(p) => assert!(matches!(p.added, PatternNonValuePlace::Variable(_))),
        _ => panic!("expected a pattern"),
    }
}

#[test]
fn four_place_pattern_has_placeholder_added() {
    let q = parse::parse_query("[:find ?e :where [?e :foo/bar ?v]]").unwrap();
    match &q.where_clauses[0] {
        WhereClause::Pattern(p) => assert_eq!(p.added, PatternNonValuePlace::Placeholder),
        _ => panic!("expected a pattern"),
    }
}

#[test]
fn rules_under_both_rules_and_with() {
    for q in [
        "[:find ?p :rules [[(adult ?p) [?p :person/age ?a]]] :where (adult ?p)]",
        "[:find ?p :with [[(adult ?p) [?p :person/age ?a]]] :where (adult ?p)]",
    ] {
        assert_eq!(parse::parse_query(q).unwrap().rules.len(), 1, "{q}");
    }
}

#[test]
fn with_vars_still_means_with_vars() {
    let q = parse::parse_query("[:find (count ?x) :with ?y :where [?x :a/b ?y]]").unwrap();
    assert_eq!(q.with.len(), 1);
    assert_eq!(q.rules.len(), 0);
}

#[test]
fn plain_keyword_value_parses() {
    assert!(parse::parse_query("[:find ?e :where [?e :task/status :done]]").is_ok());
}

#[test]
fn reverse_pull_parses() {
    assert!(
        parse::parse_query("[:find (pull ?e [:person/_friends]) :where [?e :person/name _]]")
            .is_ok()
    );
}

#[test]
fn builtin_tx_fn_exists() {
    let _ = edn::entities::BuiltinTxFn::Cas;
    let _ = edn::entities::BuiltinTxFn::RetractEntity;
}
