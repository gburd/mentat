//! Re-asserting an existing `:db/ident` (#415). `:db/ident` is
//! `:db.unique/identity`, so a map that names an existing ident upserts into
//! the entity that has it: the ident never moves to a new entity. pg_mentat's
//! ident_upsert_tests.rs runs the same cases.

extern crate mentat;

use mentat::{HasSchema, Keyword, Queryable, Store};

fn entid(s: &Store, ns: &str, name: &str) -> i64 {
    s.conn()
        .current_schema()
        .get_entid(&Keyword::namespaced(ns, name))
        .expect("ident")
        .0
}

fn scalar(s: &Store, q: &str) -> String {
    let r = s
        .q_once(q, None)
        .unwrap_or_else(|e| panic!("{q}: {e:?}"))
        .results
        .into_scalar()
        .expect("scalar");
    format!("{r:?}")
}

fn count(s: &Store, q: &str) -> usize {
    s.q_once(q, None)
        .unwrap_or_else(|e| panic!("{q}: {e:?}"))
        .results
        .into_coll()
        .expect("coll")
        .len()
}

#[test]
fn reassert_attribute_ident_with_doc() {
    let mut s = Store::open("").expect("opened");
    s.transact(
        "[{:db/ident :tf/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]",
    )
    .expect("schema");
    let n = entid(&s, "tf", "n");
    s.transact("[{:tf/n 7}]").expect("data");
    let r = s
        .transact(r#"[{:db/ident :tf/n :db/doc "the n"}]"#)
        .expect("doc");
    assert!(r.tempids.is_empty(), "{:?}", r.tempids);
    assert_eq!(entid(&s, "tf", "n"), n, "the ident did not move");
    let doc = scalar(
        &s,
        "[:find ?d . :where [?a :db/ident :tf/n] [?a :db/doc ?d]]",
    );
    assert!(doc.contains("the n"), "{doc}");
    assert_eq!(count(&s, "[:find [?e ...] :where [?e :tf/n 7]]"), 1);
    s.transact(r#"[{:db/ident :tf/n :db/doc "again"}]"#)
        .expect("doc 2");
    assert_eq!(entid(&s, "tf", "n"), n);
}

#[test]
fn reassert_enum_ident() {
    let mut s = Store::open("").expect("opened");
    s.transact(
        "[{:db/ident :c/color :db/valueType :db.type/ref :db/cardinality :db.cardinality/one}
          {:db/ident :c/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]",
    )
    .expect("schema");
    s.transact("[{:db/ident :color/red}]").expect("enum");
    let red = entid(&s, "color", "red");
    s.transact(r#"[{:c/name "car" :c/color :color/red}]"#)
        .expect("before");
    s.transact(r#"[{:db/ident :color/red :db/doc "red"}]"#)
        .expect("re-assert");
    assert_eq!(entid(&s, "color", "red"), red);
    s.transact(r#"[{:c/name "rose" :c/color :color/red}]"#)
        .expect("after");
    let q = "[:find [?n ...] :where [?c :db/ident :color/red] [?e :c/color ?c] [?e :c/name ?n]]";
    assert_eq!(count(&s, q), 2);
    let q = "[:find [?n ...] :where [?e :c/color :color/red] [?e :c/name ?n]]";
    assert_eq!(count(&s, q), 2);
    // One existing and one new enum ident in one transaction.
    s.transact(r#"[{:db/ident :color/red :db/doc "still red"} {:db/ident :color/blue}]"#)
        .expect("mixed");
    assert_eq!(entid(&s, "color", "red"), red);
    assert_ne!(entid(&s, "color", "blue"), red);
    assert_eq!(
        count(&s, "[:find [?e ...] :where [?e :db/ident :color/red]]"),
        1
    );
}

#[test]
fn idempotent_schema_install_twice() {
    let schema = "[{:db/ident :s/a :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/doc \"a\"}
                  {:db/ident :s/b :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}
                  {:db/ident :s.e/x} {:db/ident :s.e/y :db/doc \"y\"}]";
    let mut s = Store::open("").expect("opened");
    s.transact(schema).expect("first");
    let before: Vec<i64> = [("s", "a"), ("s", "b"), ("s.e", "x"), ("s.e", "y")]
        .iter()
        .map(|(ns, n)| entid(&s, ns, n))
        .collect();
    s.transact(schema).expect("second");
    s.transact(schema).expect("third");
    let after: Vec<i64> = [("s", "a"), ("s", "b"), ("s.e", "x"), ("s.e", "y")]
        .iter()
        .map(|(ns, n)| entid(&s, ns, n))
        .collect();
    assert_eq!(before, after);
}

#[test]
fn rename_ident() {
    let mut s = Store::open("").expect("opened");
    s.transact("[{:db/ident :r/old}]").expect("ident");
    let e = entid(&s, "r", "old");
    // Datomic's rename: assert the new ident on the same entity.
    s.transact(&format!("[[:db/add {e} :db/ident :r/new]]"))
        .expect("rename");
    assert_eq!(entid(&s, "r", "new"), e);
    assert!(s
        .conn()
        .current_schema()
        .get_entid(&Keyword::namespaced("r", "old"))
        .is_none());
}
