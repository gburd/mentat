//! Enum idents on the embedded engine (#416). A keyword in a ref attribute's
//! value place is an ident naming an entity, Datomic's enum idiom:
//! `[?e :c/color :color/red]`. pg_mentat's enum_ident_tests.rs runs the same
//! queries on the same data.

extern crate mentat;

use mentat::{Keyword, QueryInputs, Queryable, Store, TypedValue, Variable};

fn setup() -> Store {
    let mut s = Store::open("").expect("opened");
    s.transact(
        r#"[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
            {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
            {:db/ident :c/kw :db/valueType :db.type/keyword :db/cardinality :db.cardinality/one}]"#,
    )
    .expect("schema");
    s.transact("[{:db/ident :color/red} {:db/ident :color/blue}]")
        .expect("enums");
    s.transact(
        r#"[{:c/name "car" :c/color :color/red}
            {:c/name "sky" :c/color :color/blue}
            {:db/id "t" :c/kw :color/red}]"#,
    )
    .expect("data");
    s
}

fn coll(s: &Store, q: &str) -> Vec<String> {
    coll_in(s, q, None)
}

fn coll_in(s: &Store, q: &str, inputs: Option<QueryInputs>) -> Vec<String> {
    let mut v: Vec<String> = s
        .q_once(q, inputs)
        .unwrap_or_else(|e| panic!("{q}: {e:?}"))
        .results
        .into_coll()
        .expect("coll")
        .into_iter()
        .map(|b| match b.clone().into_string() {
            Some(s) => s.to_string(),
            None => format!("{b:?}"),
        })
        .collect();
    v.sort();
    v
}

#[test]
fn enum_ident_in_ref_value_place() {
    let s = setup();
    let q = "[:find [?n ...] :where [?e :c/color :color/red] [?e :c/name ?n]]";
    assert_eq!(coll(&s, q), ["car"]);
    // An unknown ident matches nothing, not an error.
    let q = "[:find [?n ...] :where [?e :c/color :color/green] [?e :c/name ?n]]";
    assert!(coll(&s, q).is_empty());
    // The same keyword on a keyword attribute is a keyword value.
    assert_eq!(
        coll(&s, "[:find [?e ...] :where [?e :c/kw :color/red]]").len(),
        1
    );
    // not / or / rule bodies.
    let q = "[:find [?n ...] :where [?e :c/name ?n] (not [?e :c/color :color/red])]";
    assert_eq!(coll(&s, q), ["sky"]);
    let q = "[:find [?n ...] :where [?e :c/name ?n] \
             (or [?e :c/color :color/red] [?e :c/color :color/blue])]";
    assert_eq!(coll(&s, q), ["car", "sky"]);
    let q = "[:find [?n ...] :rules [[(red ?e) [?e :c/color :color/red]]] \
             :where (red ?e) [?e :c/name ?n]]";
    assert_eq!(coll(&s, q), ["car"]);
    let q = "[:find [?n ...] :where [?e :c/name ?n] (not-join [?e] [?e :c/color :color/blue])]";
    assert_eq!(coll(&s, q), ["car"]);
}

/// With the attribute unknown (`?a`), the keyword is a keyword value only:
/// the :db/ident datom of :color/red and the :c/kw value, not the car's ref.
#[test]
fn enum_ident_untyped_value_place_is_keyword() {
    let s = setup();
    let car = coll(&s, "[:find [?e ...] :where [?e :c/name \"car\"]]");
    let hits = coll(&s, "[:find [?e ...] :where [?e ?a :color/red]]");
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert!(!hits.contains(&car[0]), "{hits:?}");
}

fn red() -> TypedValue {
    TypedValue::Keyword(Keyword::namespaced("color", "red").into())
}

// The three cases below return nothing on the embedded engine today (pg_mentat
// resolves them; issue #420): a keyword :in value or ground is typed Keyword
// before the pattern makes the variable a ref.
#[test]
#[ignore = "embedded :in keyword into a ref position, issue #420"]
fn enum_ident_scalar_input() {
    let s = setup();
    let q = "[:find [?n ...] :in ?c :where [?e :c/color ?c] [?e :c/name ?n]]";
    let inputs = QueryInputs::with_value_sequence(vec![(Variable::from_valid_name("?c"), red())]);
    assert_eq!(coll_in(&s, q, Some(inputs)), ["car"]);
}

#[test]
#[ignore = "embedded :in keyword into a ref position, issue #420"]
fn enum_ident_collection_input() {
    let s = setup();
    let q = "[:find [?n ...] :in [?c ...] :where [?e :c/color ?c] [?e :c/name ?n]]";
    let blue = TypedValue::Keyword(Keyword::namespaced("color", "blue").into());
    let inputs = QueryInputs::with_collection(Variable::from_valid_name("?c"), vec![red(), blue]);
    assert_eq!(coll_in(&s, q, Some(inputs)), ["car", "sky"]);
}

#[test]
#[ignore = "embedded ground keyword into a ref position, issue #420"]
fn enum_ident_ground() {
    let s = setup();
    let q = "[:find [?n ...] :where [(ground :color/red) ?c] [?e :c/color ?c] [?e :c/name ?n]]";
    assert_eq!(coll(&s, q), ["car"]);
}
