//! Integration test for the optional mino scripting layer, backed by the real
//! SQLite-backed `mentat::Store`. Only exercised with `--features mino`;
//! without the feature the file compiles to nothing.
#![cfg(feature = "mino")]

use mentat::script::Interpreter;

/// The full round-trip through the scripting layer to real SQLite:
/// open an in-memory store, transact a schema, transact data, query it back.
/// This uses Mentat's ACTUAL schema/tx/query syntax (not mino.store's
/// schemaless `:alice`-style sugar, which does not apply to Mentat).
#[test]
fn mentat_store_round_trips_on_sqlite() {
    let mut it = Interpreter::new();

    // Open an in-memory SQLite store (empty path -> :memory:). The handle is
    // an opaque integer.
    it.eval("(def c (mentat.store/open))").unwrap();
    assert_eq!(it.eval_to_string("c").unwrap(), "1");

    // Transact a schema: define :person/name as a one-cardinality string attr.
    // The returned tx report is a map with :mentat.store/tx-id.
    let schema_report = it
        .eval_to_string(
            "(mentat.store/transact c \
               [{:db/ident :person/name \
                 :db/valueType :db.type/string \
                 :db/cardinality :db.cardinality/one}])",
        )
        .expect("schema transact failed");
    assert!(
        schema_report.contains(":mentat.store/tx-id"),
        "expected a tx report map, got: {schema_report}"
    );

    // Transact data against that schema.
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Alice\"}])")
        .expect("data transact failed");

    // Query it back. A :find of one var over a relation yields a set of
    // one-element tuple vectors: #{["Alice"]}.
    let got = it
        .eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
        .expect("query failed");
    assert_eq!(got, "#{[\"Alice\"]}");

    // A scalar find yields the value directly.
    let scalar = it
        .eval_to_string("(mentat.store/q c '[:find ?n . :where [?e :person/name ?n]])")
        .expect("scalar query failed");
    assert_eq!(scalar, "\"Alice\"");

    // db is identity-on-conn for the SQLite backing.
    assert_eq!(
        it.eval_to_string("(mentat.store/db c)").unwrap(),
        it.eval_to_string("c").unwrap()
    );

    // A query through (mentat.store/db c) works the same as through the conn.
    let via_db = it
        .eval_to_string("(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])")
        .expect("query via db failed");
    assert_eq!(via_db, "#{[\"Alice\"]}");

    // close drops the store; querying a closed handle errors honestly.
    assert_eq!(it.eval_to_string("(mentat.store/close c)").unwrap(), "nil");
    let err = match it.eval("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])") {
        Err(e) => e,
        Ok(_) => panic!("query on closed store should error"),
    };
    assert!(err.contains("no open store"), "got: {err}");
}

/// The prims Mentat's schema-driven model cannot honestly back are stubbed
/// with a clear error, not faked.
#[test]
fn unsupported_prims_error_honestly() {
    let mut it = Interpreter::new();
    it.eval("(def c (mentat.store/open))").unwrap();
    for prim in ["read", "entity", "entities", "datoms", "pull"] {
        let err = match it.eval(&format!("(mentat.store/{prim} c)")) {
            Err(e) => e,
            Ok(_) => panic!("prim {prim} should give a not-supported error"),
        };
        assert!(
            err.contains("not supported by the SQLite backing"),
            "prim {prim} gave: {err}"
        );
    }
}
