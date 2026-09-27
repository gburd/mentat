//! The Datomic-model test suite for the `mentat.store/*` scripting surface,
//! written once and run against every [`ScriptBackend`] (plan § 1.19).
//!
//! Each function takes a `&mut mino_rs::Interpreter` that already has the store
//! prims installed (via [`crate::install`]) over some backend. The `mentat`
//! crate and `pg_mentat` each build such an interpreter over their real
//! storage and call these; `crates/script/tests/model.rs` runs them against an
//! in-memory fake. One suite, three backends — so the two real backends stop
//! drifting.
//!
//! This module is compiled into the library (not gated) so downstream test
//! crates can depend on it without a dev-dependency dance; it pulls in nothing
//! but the interpreter it is handed.

use mino_rs::Interpreter;

/// Seed a store-installed interpreter with the schema and Alice, binding `c` to
/// the conn handle. Backend-agnostic: it only evals the shared script.
pub fn seed(it: &mut Interpreter) {
    it.eval("(def c (mentat.store/open))").expect("open");
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :person/name :db/valueType :db.type/string \
             :db/cardinality :db.cardinality/one}])",
    )
    .expect("schema transact");
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Alice\"}])")
        .expect("data transact");
}

/// The eid of the person named `name`, as a scalar-query result string.
pub fn eid_of(it: &mut Interpreter, name: &str) -> String {
    it.eval_to_string(&format!(
        "(mentat.store/q (mentat.store/db c) '[:find ?e . :where [?e :person/name \"{name}\"]])"
    ))
    .expect("eid query")
}

/// The first synthetic tx id (Mentat's `TX0`); real basis ids exceed it.
pub const TX0: i64 = 0x1000_0000;

pub fn db_is_an_immutable_value_not_the_conn(it: &mut Interpreter) {
    seed(it);
    assert_eq!(it.eval_to_string("c").unwrap(), "1");
    let db = it.eval_to_string("(mentat.store/db c)").unwrap();
    assert!(db.contains(":mentat.store/db true"), "{db}");
    assert!(db.contains(":mentat.store/basis-tx"), "{db}");
    assert!(db.contains(":mentat.store/conn 1"), "{db}");
    assert_ne!(db, "1");
    let basis = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap();
    assert!(basis.parse::<i64>().unwrap() > TX0, "basis {basis}");
}

pub fn q_takes_a_db_value(it: &mut Interpreter) {
    seed(it);
    assert_eq!(
        it.eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])"
        )
        .unwrap(),
        "#{[\"Alice\"]}"
    );
    assert_eq!(
        it.eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
            .unwrap(),
        "#{[\"Alice\"]}"
    );
}

pub fn pull_returns_a_map(it: &mut Interpreter) {
    seed(it);
    let eid = eid_of(it, "Alice");
    let pulled = it
        .eval_to_string(&format!(
            "(mentat.store/pull (mentat.store/db c) {eid} [:person/name])"
        ))
        .unwrap();
    // A keyword-keyed map carrying the pulled value. (The exact map shape is
    // backend-specific — pg may include extra keys — so assert the content.)
    assert!(pulled.starts_with('{'), "pull -> map: {pulled}");
    assert!(pulled.contains(":person/name \"Alice\""), "pull: {pulled}");
}

pub fn entity_returns_an_entity_map(it: &mut Interpreter) {
    seed(it);
    let eid = eid_of(it, "Alice");
    let ent = it
        .eval_to_string(&format!("(mentat.store/entity (mentat.store/db c) {eid})"))
        .unwrap();
    assert!(ent.contains(&format!(":db/id {eid}")), "{ent}");
    assert!(ent.contains(":person/name \"Alice\""), "{ent}");
}

pub fn read_returns_the_scalar(it: &mut Interpreter) {
    seed(it);
    let eid = eid_of(it, "Alice");
    assert_eq!(
        it.eval_to_string(&format!(
            "(mentat.store/read (mentat.store/db c) {eid} :person/name)"
        ))
        .unwrap(),
        "\"Alice\""
    );
}

pub fn datoms_returns_tuples(it: &mut Interpreter) {
    seed(it);
    let ds = it
        .eval_to_string("(mentat.store/datoms (mentat.store/db c))")
        .unwrap();
    // A vector of tuples that includes the Alice name assertion's value. (The
    // attribute place is a keyword on SQLite and an entid on pg — a real
    // backend difference — so assert only the portable facts.)
    assert!(ds.starts_with('['), "datoms: {ds}");
    assert!(
        ds.contains("\"Alice\""),
        "datoms should include Alice: {ds}"
    );
}

pub fn with_is_speculative_and_does_not_commit(it: &mut Interpreter) {
    seed(it);
    let before = it
        .eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
        .unwrap();
    assert_eq!(before, "#{[\"Alice\"]}");
    let with_result = it
        .eval_to_string("(mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])")
        .unwrap();
    assert!(
        with_result.contains(":mentat.store/db-after"),
        "{with_result}"
    );
    assert!(
        with_result.contains(":mentat.store/tx-report"),
        "{with_result}"
    );
    let basis_now: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    let db_after_basis: i64 = it
        .eval_to_string(
            "(:mentat.store/basis-tx (:mentat.store/db-after \
               (mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])))",
        )
        .unwrap()
        .parse()
        .unwrap();
    assert!(db_after_basis > basis_now, "{db_after_basis} > {basis_now}");
    let after = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])",
        )
        .unwrap();
    assert_eq!(after, "#{[\"Alice\"]}", "with must not commit");
}

pub fn as_of_and_since_reflect_the_basis(it: &mut Interpreter) {
    seed(it);
    let basis_alice: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Bob\"}])")
        .unwrap();
    let basis_bob: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    assert!(basis_bob > basis_alice);

    let as_of_alice = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis_alice}))"
        ))
        .unwrap();
    assert!(as_of_alice.contains("Alice"), "{as_of_alice}");
    assert!(!as_of_alice.contains("Bob"), "{as_of_alice}");

    let as_of_bob = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis_bob}))"
        ))
        .unwrap();
    assert!(
        as_of_bob.contains("Alice") && as_of_bob.contains("Bob"),
        "{as_of_bob}"
    );

    let since_alice = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/since (mentat.store/db c) {basis_alice}))"
        ))
        .unwrap();
    assert!(since_alice.contains("Bob"), "{since_alice}");
    assert!(!since_alice.contains("Alice"), "{since_alice}");

    let as_of_db = it
        .eval_to_string(&format!(
            "(mentat.store/as-of (mentat.store/db c) {basis_alice})"
        ))
        .unwrap();
    assert!(
        as_of_db.contains(&format!(":mentat.store/as-of {basis_alice}")),
        "{as_of_db}"
    );
    let since_db = it
        .eval_to_string(&format!(
            "(mentat.store/since (mentat.store/db c) {basis_alice})"
        ))
        .unwrap();
    assert!(
        since_db.contains(&format!(":mentat.store/since {basis_alice}")),
        "{since_db}"
    );
}

pub fn tx_report_has_the_datomic_shape(it: &mut Interpreter) {
    seed(it);
    let report = it
        .eval_to_string("(mentat.store/transact c [{:person/name \"Carol\"}])")
        .unwrap();
    assert!(report.contains(":mentat.store/tx-id"), "{report}");
    assert!(report.contains(":mentat.store/tempids"), "{report}");
    assert!(report.contains(":mentat.store/db-after"), "{report}");
}

/// The inst/uuid VALUE BUILDERS emit values that round-trip through the mino
/// reader. Backend-independent (no interpreter needed).
pub fn inst_and_uuid_builders_round_trip_through_the_reader() {
    use mino_rs::printer::print_str;
    use mino_rs::reader::read_one;
    let uuid = crate::values::uuid_value("12345678-1234-5678-1234-567812345678");
    assert_eq!(
        print_str(&uuid),
        "#uuid \"12345678-1234-5678-1234-567812345678\""
    );
    let inst = crate::values::inst_value("2017-01-01T00:00:00Z");
    assert_eq!(
        print_str(&inst),
        "(clojure.instant/read-instant-date \"2017-01-01T00:00:00Z\")"
    );
    for v in [uuid, inst] {
        let (v2, _) = read_one(&print_str(&v)).unwrap();
        assert_eq!(print_str(&v), print_str(&v2));
    }
}

// ---------------------------------------------------------------------------
// :in inputs, history, cas / retractEntity (Datomic-like layer, 1.10).
// ---------------------------------------------------------------------------

/// `(q db query arg ...)`: Datomic's `:in $ ?x` with the args after the query.
/// A scalar input filters the value place. Runs on every backend (the fake
/// understands `:in $ ?x` and `:in $ [?x ...]` on one clause).
pub fn q_takes_in_inputs(it: &mut Interpreter) {
    seed(it);
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Bob\"}])")
        .unwrap();
    assert_eq!(
        it.eval_to_string(
            "(mentat.store/q (mentat.store/db c) \
               '[:find ?n :in $ ?n :where [?e :person/name ?n]] \"Bob\")"
        )
        .unwrap(),
        "#{[\"Bob\"]}"
    );
    // A collection input binds any of its values.
    let both = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) \
               '[:find ?n :in $ [?n ...] :where [?e :person/name ?n]] [\"Alice\" \"Bob\" \"Zed\"])",
        )
        .unwrap();
    assert!(
        both.contains("\"Alice\"") && both.contains("\"Bob\"") && !both.contains("Zed"),
        "{both}"
    );
    // A missing input is an error, not a silently unbound variable.
    assert!(it
        .eval(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :in $ ?n :where [?e :person/name ?n]])"
        )
        .is_err());
}

/// Seed people with ages, bound to `c`. Real engines only (the fake is a toy).
fn seed_people(it: &mut Interpreter) {
    it.eval("(def c (mentat.store/open))").expect("open");
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :person/name :db/valueType :db.type/string \
             :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true} \
            {:db/ident :person/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one} \
            {:db/ident :person/friend :db/valueType :db.type/ref :db/cardinality :db.cardinality/many}])",
    )
    .expect("schema");
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/id \"a\" :person/name \"Alice\" :person/age 30 :person/friend \"b\"} \
            {:db/id \"b\" :person/name \"Bob\" :person/age 40}])",
    )
    .expect("people");
}

/// Mixed inputs on a real engine: a ref-typed scalar, a collection, and a
/// relation, bound by position; an entity-position integer is a ref.
pub fn q_mixed_inputs(it: &mut Interpreter) {
    seed_people(it);
    let bob = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?e . :where [?e :person/name \"Bob\"]])",
        )
        .unwrap();
    assert_eq!(
        it.eval_to_string(&format!(
            "(mentat.store/q (mentat.store/db c) \
               '[:find ?n . :in $ ?f [?a ...] :where [?e :person/friend ?f] [?e :person/age ?a] [?e :person/name ?n]] \
               {bob} [30 31])"
        ))
        .unwrap(),
        "\"Alice\""
    );
    let rel = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) \
               '[:find ?n :in $ [[?n ?a]] :where [?e :person/name ?n] [?e :person/age ?a]] \
               [[\"Alice\" 30] [\"Bob\" 41]])",
        )
        .unwrap();
    assert_eq!(rel, "#{[\"Alice\"]}");
}

/// The eid of a seeded person, as a number string.
fn person(it: &mut Interpreter, name: &str) -> String {
    it.eval_to_string(&format!(
        "(mentat.store/q (mentat.store/db c) '[:find ?e . :where [?e :person/name \"{name}\"]])"
    ))
    .unwrap()
}

/// History patterns: `[?e ?a ?v ?tx ?added]` sees retractions (`false`) and
/// assertions (`true`) across transactions.
pub fn history_patterns_see_added(it: &mut Interpreter) {
    seed_people(it);
    let alice = person(it, "Alice");
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {alice} :person/age 31]])"
    ))
    .unwrap();
    let h = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) \
               '[:find ?v ?added :where [?e :person/age ?v ?tx ?added] [?e :person/name \"Alice\"]])",
        )
        .unwrap();
    for want in ["[30 true]", "[30 false]", "[31 true]"] {
        assert!(h.contains(want), "history should contain {want}: {h}");
    }
}

/// `:db/cas` swaps only from the expected old value; `:db/retractEntity`
/// removes every datom of the entity. Both take an entid: unlike Datomic,
/// neither backend resolves a lookup ref in a tx fn.
pub fn cas_and_retract_entity(it: &mut Interpreter) {
    seed_people(it);
    let (alice, bob) = (person(it, "Alice"), person(it, "Bob"));
    let age = "(mentat.store/q (mentat.store/db c) \
               '[:find ?a . :where [?e :person/name \"Alice\"] [?e :person/age ?a]])";
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/cas {alice} :person/age 30 31]])"
    ))
    .expect("cas from the right value");
    assert_eq!(it.eval_to_string(age).unwrap(), "31");
    assert!(
        it.eval(&format!(
            "(mentat.store/transact c [[:db/cas {alice} :person/age 30 32]])"
        ))
        .is_err(),
        "cas from a stale value must fail"
    );
    assert_eq!(it.eval_to_string(age).unwrap(), "31");

    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/retractEntity {bob}]])"
    ))
    .expect("retractEntity");
    assert_eq!(person(it, "Bob"), "nil");
    // Datomic also retracts refs TO the entity; neither backend does yet (the
    // shared suite pins only what both do). Bob's own datoms are all gone:
    assert_eq!(
        it.eval_to_string(&format!(
            "(mentat.store/q (mentat.store/db c) '[:find ?a . :where [{bob} :person/age ?a]])"
        ))
        .unwrap(),
        "nil"
    );
}
