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

/// Issue #328: `q` against an as-of / since db value runs the query on that
/// basis (temporal Datalog), on every backend, instead of erroring.
pub fn q_runs_against_as_of_and_since(it: &mut Interpreter) {
    seed(it);
    let basis_alice: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Bob\"}])")
        .unwrap();

    let names = "'[:find ?n :where [?e :person/name ?n]]";
    let now = it
        .eval_to_string(&format!("(mentat.store/q (mentat.store/db c) {names})"))
        .unwrap();
    assert!(now.contains("Alice") && now.contains("Bob"), "{now}");

    let then = it
        .eval_to_string(&format!(
            "(mentat.store/q (mentat.store/as-of (mentat.store/db c) {basis_alice}) {names})"
        ))
        .unwrap();
    assert_eq!(then, "#{[\"Alice\"]}", "as-of must hide later facts");

    // q with :in inputs works on a historical basis too.
    let with_in = it
        .eval_to_string(&format!(
            "(mentat.store/q (mentat.store/as-of (mentat.store/db c) {basis_alice}) \
             '[:find ?e :in $ ?n :where [?e :person/name ?n]] \"Bob\")"
        ))
        .unwrap();
    assert_eq!(
        with_in, "#{}",
        "Bob did not exist as of Alice's basis: {with_in}"
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
/// removes every datom of the entity AND every ref pointing at it (Datomic's
/// semantics; issue #326). Both accept a lookup ref `[:unique/attr v]` in the
/// entity place (issue #327).
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

    // A lookup ref names Alice in the entity place of cas.
    it.eval_to_string(
        "(mentat.store/transact c [[:db/cas [:person/name \"Alice\"] :person/age 31 32]])",
    )
    .expect("cas via a lookup ref");
    assert_eq!(it.eval_to_string(age).unwrap(), "32");

    // Alice -> :person/friend -> Bob is an inbound ref to Bob.
    let friends = "(mentat.store/q (mentat.store/db c) \
                   '[:find ?f . :where [?e :person/name \"Alice\"] [?e :person/friend ?f]])";
    assert_eq!(it.eval_to_string(friends).unwrap(), bob);

    // retractEntity via a lookup ref.
    it.eval_to_string("(mentat.store/transact c [[:db/retractEntity [:person/name \"Bob\"]]])")
        .expect("retractEntity via a lookup ref");
    assert_eq!(person(it, "Bob"), "nil");
    // Bob's own datoms are all gone:
    assert_eq!(
        it.eval_to_string(&format!(
            "(mentat.store/q (mentat.store/db c) '[:find ?a . :where [{bob} :person/age ?a]])"
        ))
        .unwrap(),
        "nil"
    );
    // ...and so is the ref TO him, while Alice herself remains.
    assert_eq!(
        it.eval_to_string(friends).unwrap(),
        "nil",
        "inbound ref retracted"
    );
    assert_eq!(person(it, "Alice"), alice);
}

/// User-defined transaction functions (issues #325, #333): an ident carrying
/// `:db/fn` (mino source of `(fn [db & args] ...)` returning tx-data) is
/// invoked as `[:my/fn arg ...]` inside a transaction, on every backend.
pub fn transaction_functions(it: &mut Interpreter) {
    seed_people(it);
    let age = |it: &mut Interpreter, who: &str| {
        it.eval_to_string(&format!(
            "(mentat.store/q (mentat.store/db c) \
               '[:find ?a . :where [?e :person/name \"{who}\"] [?e :person/age ?a]])"
        ))
        .unwrap()
    };

    // Install: a fn that reads the db (as of the start of the tx) with q and
    // returns a lookup-ref cas, so a stale call aborts.
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :person/birthday \
             :db/fn \"(fn [db who] \
                        (let [a (ffirst (mentat.store/q db \
                                  '[:find ?a :in $ ?n :where [?e :person/name ?n] [?e :person/age ?a]] \
                                  who))] \
                          [[:db/cas [:person/name who] :person/age a (inc a)]]))\"}])",
    )
    .expect("install :person/birthday");

    it.eval_to_string("(mentat.store/transact c [[:person/birthday \"Alice\"]])")
        .expect("invoke");
    assert_eq!(age(it, "Alice"), "31");

    // Spliced with other tx-data in the same transaction, and nested: a fn
    // that returns a call to another fn.
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :person/two-birthdays \
             :db/fn \"(fn [db who] [[:person/birthday who]])\"}])",
    )
    .expect("install nested fn");
    it.eval_to_string(
        "(mentat.store/transact c [[:person/two-birthdays \"Bob\"] {:person/name \"Carol\" :person/age 7}])",
    )
    .expect("nested invoke");
    assert_eq!(age(it, "Bob"), "41");
    assert_eq!(age(it, "Carol"), "7");

    // A fn that throws aborts the whole transaction, with the fn named.
    it.eval_to_string(
        "(mentat.store/transact c [{:db/ident :person/boom :db/fn \"(fn [db] (throw (ex-info \\\"nope\\\" {})))\"}])",
    )
    .expect("install :person/boom");
    let err = it
        .eval_to_string(
            "(mentat.store/transact c [[:person/boom] {:person/name \"Dan\" :person/age 1}])",
        )
        .expect_err("a throwing tx-fn aborts the tx");
    assert!(
        err.contains("txfn-error") && err.contains(":person/boom"),
        "{err}"
    );
    assert_eq!(age(it, "Dan"), "nil", "nothing from the aborted tx");

    // Self-recursion is bounded.
    it.eval_to_string(
        "(mentat.store/transact c [{:db/ident :person/forever :db/fn \"(fn [db] [[:person/forever]])\"}])",
    )
    .expect("install :person/forever");
    let err = it
        .eval_to_string("(mentat.store/transact c [[:person/forever]])")
        .expect_err("unbounded recursion");
    assert!(err.contains("txfn-depth"), "{err}");

    // Sandboxed: no host filesystem.
    it.eval_to_string(
        "(mentat.store/transact c [{:db/ident :person/escape :db/fn \"(fn [db] (slurp \\\"/etc/passwd\\\") [])\"}])",
    )
    .expect("install :person/escape");
    let err = it
        .eval_to_string("(mentat.store/transact c [[:person/escape]])")
        .expect_err("slurp must be unavailable");
    assert!(err.contains("txfn-error"), "{err}");
}

/// `pull`, `entities` and a find-pull `q` on an as-of / since db read that
/// basis. Alice's age changes after T: as-of T must show the old value, since
/// T only the new one. Before 1.18.0 pg and DuckDB returned the current value
/// here and the embedded engine refused.
pub fn pull_reads_the_as_of_and_since_basis(it: &mut Interpreter) {
    seed_people(it);
    let alice = person(it, "Alice");
    let t: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {alice} :person/age 31] {{:person/name \"Carol\"}}])"
    ))
    .unwrap();
    let as_of = format!("(mentat.store/as-of (mentat.store/db c) {t})");
    let since = format!("(mentat.store/since (mentat.store/db c) {t})");

    let now = it
        .eval_to_string(&format!(
            "(:person/age (mentat.store/pull (mentat.store/db c) {alice} '[:person/age]))"
        ))
        .unwrap();
    assert_eq!(now, "31");
    let then = it
        .eval_to_string(&format!(
            "(:person/age (mentat.store/pull {as_of} {alice} '[:person/name :person/age]))"
        ))
        .unwrap();
    assert_eq!(then, "30", "as-of T pulls the value before the change");
    let then_name = it
        .eval_to_string(&format!(
            "(:person/name (mentat.store/pull {as_of} {alice} '[:person/name]))"
        ))
        .unwrap();
    assert_eq!(then_name, "\"Alice\"");

    // since T: only what was asserted after T, so the new age and no name.
    let later = it
        .eval_to_string(&format!(
            "(mentat.store/pull {since} {alice} '[:person/name :person/age])"
        ))
        .unwrap();
    assert!(later.contains(":person/age 31"), "{later}");
    assert!(!later.contains("Alice"), "{later}");

    // entity on an as-of db shows the old value too.
    let ent = it
        .eval_to_string(&format!(
            "(:person/age (mentat.store/entity {as_of} {alice}))"
        ))
        .unwrap();
    assert_eq!(ent, "30", "entity as of T");

    // A find-pull inside q reads the db's basis too.
    let q_then = it
        .eval_to_string(&format!(
            "(:person/age (mentat.store/q {as_of} \
               '[:find (pull ?e [:person/age]) . :where [?e :person/name \"Alice\"]]))"
        ))
        .unwrap();
    assert_eq!(q_then, "30", "find-pull under as-of");

    // entities: Carol did not exist as of T.
    let ents = |it: &mut Interpreter, db: &str| {
        it.eval_to_string(&format!(
            "(count (mentat.store/entities {db} :person/name))"
        ))
        .unwrap()
    };
    assert_eq!(ents(it, "(mentat.store/db c)"), "3");
    assert_eq!(ents(it, &as_of), "2");
    assert_eq!(ents(it, &since), "1");
}

/// Map specs in a pull pattern, `{:attr [sub-pattern]}`, forward
/// (cardinality-one and -many) and reverse, nested, on every backend. Each
/// nested map carries `:db/id`; on an as-of db the nesting reads that basis.
pub fn pull_nested_map_specs(it: &mut Interpreter) {
    seed_people(it);
    it.eval_to_string(
        "(mentat.store/transact c [{:db/ident :person/best :db/valueType :db.type/ref \
           :db/cardinality :db.cardinality/one}])",
    )
    .unwrap();
    let (alice, bob) = (person(it, "Alice"), person(it, "Bob"));
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {bob} :person/best {alice}]])"
    ))
    .unwrap();
    let t: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {alice} :person/age 31]])"
    ))
    .unwrap();
    let pull = |it: &mut Interpreter, db: &str, e: &str, pat: &str, path: &str| {
        it.eval_to_string(&format!(
            "(get-in (mentat.store/pull {db} {e} '{pat}) {path})"
        ))
        .unwrap()
    };
    let now = "(mentat.store/db c)";
    let then = format!("(mentat.store/as-of (mentat.store/db c) {t})");

    // Cardinality-many forward: a vector of maps.
    let pat = "[:person/name {:person/friend [:person/name :person/age]}]";
    assert_eq!(
        pull(it, now, &alice, pat, "[:person/friend 0 :person/name]"),
        "\"Bob\""
    );
    assert_eq!(pull(it, now, &alice, pat, "[:person/friend 0 :db/id]"), bob);
    assert_eq!(pull(it, now, &alice, pat, "[:person/name]"), "\"Alice\"");
    // Cardinality-one forward: one map, nested two deep.
    let pat = "[{:person/best [:person/age {:person/friend [:person/name]}]}]";
    assert_eq!(pull(it, now, &bob, pat, "[:person/best :db/id]"), alice);
    assert_eq!(pull(it, now, &bob, pat, "[:person/best :person/age]"), "31");
    assert_eq!(
        pull(
            it,
            now,
            &bob,
            pat,
            "[:person/best :person/friend 0 :person/name]"
        ),
        "\"Bob\""
    );
    // ...and on an as-of db: Alice's age before the change.
    assert_eq!(
        pull(it, &then, &bob, pat, "[:person/best :person/age]"),
        "30"
    );
    // Reverse: who has Bob as a friend.
    let pat = "[{:person/_friend [:person/name]}]";
    assert_eq!(
        pull(it, now, &bob, pat, "[:person/_friend 0 :person/name]"),
        "\"Alice\""
    );
    assert_eq!(
        pull(it, now, &bob, pat, "[:person/_friend 0 :db/id]"),
        alice
    );
    // No ref: the key is absent.
    assert_eq!(
        pull(
            it,
            now,
            &alice,
            "[{:person/best [:person/name]}]",
            "[:person/best]"
        ),
        "nil"
    );
    // A find-pull with a map spec.
    assert_eq!(
        it.eval_to_string(
            "(get-in (mentat.store/q (mentat.store/db c) \
               '[:find (pull ?e [{:person/best [:person/name]}]) . :where [?e :person/name \"Bob\"]]) \
               [:person/best :person/name])"
        )
        .unwrap(),
        "\"Alice\""
    );
}

/// t1: n 1, tags p q, s "a"; t2: n 2, tag p retracted, s retracted;
/// t3: n 3, tag r. Returns (x, t1, t2, t3).
fn seed_t364(it: &mut Interpreter) -> (String, i64, i64, i64) {
    it.eval("(def c (mentat.store/open))").expect("open");
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :t364/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one} \
            {:db/ident :t364/tag :db/valueType :db.type/string :db/cardinality :db.cardinality/many} \
            {:db/ident :t364/s :db/valueType :db.type/string :db/cardinality :db.cardinality/one}])",
    )
    .expect("schema");
    let basis = |it: &mut Interpreter| -> i64 {
        it.eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
            .unwrap()
            .parse()
            .unwrap()
    };
    it.eval_to_string(
        "(mentat.store/transact c [{:db/id \"x\" :t364/n 1 :t364/s \"a\"} \
           [:db/add \"x\" :t364/tag \"p\"] [:db/add \"x\" :t364/tag \"q\"]])",
    )
    .unwrap();
    let t1 = basis(it);
    let x = it
        .eval_to_string("(mentat.store/q (mentat.store/db c) '[:find ?e . :where [?e :t364/n _]])")
        .unwrap();
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {x} :t364/n 2] [:db/retract {x} :t364/tag \"p\"] \
           [:db/retract {x} :t364/s \"a\"]])"
    ))
    .unwrap();
    let t2 = basis(it);
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {x} :t364/n 3] [:db/add {x} :t364/tag \"r\"]])"
    ))
    .unwrap();
    (x, t1, t2, basis(it))
}

/// `(as-of (since db S) T)` etc.: `None` leaves that bound off.
fn db_on(as_of: Option<i64>, since: Option<i64>) -> String {
    let mut d = "(mentat.store/db c)".to_string();
    if let Some(t) = as_of {
        d = format!("(mentat.store/as-of {d} {t})");
    }
    if let Some(s) = since {
        d = format!("(mentat.store/since {d} {s})");
    }
    d
}

/// `q` as a sorted vector, so expected values are exact.
fn q_sorted(it: &mut Interpreter, d: &str, query: &str) -> String {
    it.eval_to_string(&format!("(vec (sort (mentat.store/q {d} '{query})))"))
        .unwrap_or_else(|e| panic!("{d} {query}: {e}"))
}

const Q364_N: &str = "[:find [?v ...] :where [_ :t364/n ?v]]";
const Q364_TAG: &str = "[:find [?v ...] :where [_ :t364/tag ?v]]";
const Q364_S: &str = "[:find [?v ...] :where [_ :t364/s ?v]]";
const Q364_HIST: &str = "[:find ?v ?added :where [_ :t364/n ?v _ ?added]]";

fn check_q364(it: &mut Interpreter, d: &str, n: &str, tag: &str, s: &str) {
    assert_eq!(q_sorted(it, d, Q364_N), n, "{d} :t364/n");
    assert_eq!(q_sorted(it, d, Q364_TAG), tag, "{d} :t364/tag");
    assert_eq!(q_sorted(it, d, Q364_S), s, "{d} :t364/s");
}

/// As of T hides a value retracted at or before T, cardinality-one and
/// -many, replaced or plainly retracted (Datomic's as-of). Before 1.18.0
/// pg's edn_q showed retracted values unless a newer cardinality-one value
/// replaced them.
pub fn as_of_hides_retracted_values(it: &mut Interpreter) {
    let (_, t1, t2, t3) = seed_t364(it);
    check_q364(
        it,
        &db_on(Some(t1), None),
        "[1]",
        "[\"p\" \"q\"]",
        "[\"a\"]",
    );
    check_q364(it, &db_on(Some(t2), None), "[2]", "[\"q\"]", "[]");
    check_q364(it, &db_on(Some(t3), None), "[3]", "[\"q\" \"r\"]", "[]");
    // A history pattern as of T is the log up to T, retractions included.
    assert_eq!(
        q_sorted(it, &db_on(Some(t2), None), Q364_HIST),
        "[[1 false] [1 true] [2 true]]"
    );
}

/// A value retracted and asserted again later: hidden as of the retraction,
/// visible again as of the re-assertion, and `since` the retraction shows it
/// (the live assertion is the later one).
pub fn as_of_sees_a_value_reasserted_after_retraction(it: &mut Interpreter) {
    let (x, t1, t2, t3) = seed_t364(it);
    // t4: tag "p" (retracted at t2) is asserted again.
    it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/add {x} :t364/tag \"p\"]])"
    ))
    .unwrap();
    let t4: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    let tags = |it: &mut Interpreter, d: &str| q_sorted(it, d, Q364_TAG);
    assert_eq!(tags(it, &db_on(Some(t1), None)), "[\"p\" \"q\"]");
    assert_eq!(tags(it, &db_on(Some(t2), None)), "[\"q\"]");
    assert_eq!(tags(it, &db_on(Some(t3), None)), "[\"q\" \"r\"]");
    assert_eq!(tags(it, &db_on(Some(t4), None)), "[\"p\" \"q\" \"r\"]");
    assert_eq!(tags(it, &db_on(None, Some(t3))), "[\"p\"]");
    assert_eq!(tags(it, &db_on(None, Some(t1))), "[\"p\" \"r\"]");
    assert_eq!(tags(it, &db_on(Some(t4), Some(t2))), "[\"p\" \"r\"]");
    assert_eq!(tags(it, &db_on(Some(t3), Some(t1))), "[\"r\"]");

    // Retract and re-assert "q" in one transaction. Datomic and the embedded
    // transactor refuse it (add/retract conflict); pg accepts it and the
    // retraction wins (a transactor difference, not a read-rule one). Either
    // way, as of that transaction reads what current state reads.
    let same = it.eval_to_string(&format!(
        "(mentat.store/transact c [[:db/retract {x} :t364/tag \"q\"] [:db/add {x} :t364/tag \"q\"]])"
    ));
    if same.is_ok() {
        let now = "(mentat.store/db c)";
        let t5: i64 = it
            .eval_to_string(&format!("(:mentat.store/basis-tx {now})"))
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(tags(it, &db_on(Some(t5), None)), tags(it, now));
    }
}

/// `missing?` and `get-else` read the db's basis like any pattern: as of t1
/// :t364/s is "a"; it was retracted at t2.
pub fn missing_and_get_else_read_the_basis(it: &mut Interpreter) {
    let (x, t1, t2, _) = seed_t364(it);
    let missing = "[:find [?e ...] :where [?e :t364/n _] [(missing? $ ?e :t364/s)]]";
    let get_else = "[:find [?s ...] :where [?e :t364/n _] [(get-else $ ?e :t364/s \"none\") ?s]]";
    assert_eq!(q_sorted(it, &db_on(Some(t1), None), missing), "[]");
    assert_eq!(
        q_sorted(it, &db_on(Some(t2), None), missing),
        format!("[{x}]")
    );
    assert_eq!(q_sorted(it, &db_on(Some(t1), None), get_else), "[\"a\"]");
    assert_eq!(q_sorted(it, &db_on(Some(t2), None), get_else), "[\"none\"]");
    // since t1: :t364/n's live value (3) is from t3, :t364/s none.
    assert_eq!(q_sorted(it, &db_on(None, Some(t1)), get_else), "[\"none\"]");
}

/// Datomic's `since S` (issue #364): the current datoms whose tx is after S,
/// so a value added after S and since replaced is gone. Before 1.18.0
/// `since t1` below returned [1 2 3] on SQLite and DuckDB (retraction rows
/// counted) and [2 3] on pg (replaced values counted). A 5-place history
/// pattern under since still reads the log after S, retractions included.
pub fn since_reads_current_datoms_added_after_t(it: &mut Interpreter) {
    let (x, t1, t2, t3) = seed_t364(it);
    check_q364(it, &db_on(None, Some(t1)), "[3]", "[\"r\"]", "[]");
    check_q364(it, &db_on(None, Some(t2)), "[3]", "[\"r\"]", "[]");
    check_q364(it, &db_on(None, Some(t3)), "[]", "[]", "[]");
    assert_eq!(
        q_sorted(it, &db_on(None, Some(t1)), Q364_HIST),
        "[[1 false] [2 false] [2 true] [3 true]]"
    );
    // read and pull agree with q.
    let since = db_on(None, Some(t1));
    assert_eq!(
        it.eval_to_string(&format!("(mentat.store/read {since} {x} :t364/n)"))
            .unwrap(),
        "3"
    );
    assert_eq!(
        it.eval_to_string(&format!(
            "(let [m (mentat.store/pull {since} {x} '[:t364/n :t364/tag :t364/s])] \
               [(:t364/n m) (vec (sort (:t364/tag m))) (:t364/s m)])"
        ))
        .unwrap(),
        "[3 [\"r\"] nil]"
    );
}

/// `(since (as-of db T) S)`: the assertions with S < tx <= T not retracted by
/// T, in either order, for q, pull, read and entities; a history pattern sees
/// the log with S < tx <= T. Before 1.18.0 pg raised "mutually exclusive" and
/// SQLite and DuckDB dropped `since`.
pub fn as_of_and_since_together(it: &mut Interpreter) {
    let (x, t1, t2, t3) = seed_t364(it);
    check_q364(it, &db_on(Some(t2), Some(t1)), "[2]", "[]", "[]");
    check_q364(it, &db_on(Some(t3), Some(t1)), "[3]", "[\"r\"]", "[]");
    check_q364(it, &db_on(Some(t3), Some(t2)), "[3]", "[\"r\"]", "[]");
    check_q364(it, &db_on(Some(t1), Some(t1)), "[]", "[]", "[]");
    // Order doesn't matter.
    let swapped =
        format!("(mentat.store/as-of (mentat.store/since (mentat.store/db c) {t1}) {t2})");
    assert_eq!(q_sorted(it, &swapped, Q364_N), "[2]");
    assert_eq!(
        q_sorted(it, &db_on(Some(t2), Some(t1)), Q364_HIST),
        "[[1 false] [2 true]]"
    );

    let pull = |it: &mut Interpreter, d: &str| {
        it.eval_to_string(&format!(
            "(let [m (mentat.store/pull {d} {x} '[:t364/n :t364/tag :t364/s])] \
               [(:t364/n m) (vec (sort (:t364/tag m))) (:t364/s m)])"
        ))
        .unwrap_or_else(|e| panic!("pull {d}: {e}"))
    };
    assert_eq!(pull(it, &db_on(Some(t2), Some(t1))), "[2 [] nil]");
    assert_eq!(pull(it, &db_on(Some(t3), Some(t1))), "[3 [\"r\"] nil]");
    let ents = |it: &mut Interpreter, d: &str| {
        it.eval_to_string(&format!("(count (mentat.store/entities {d} :t364/tag))"))
            .unwrap()
    };
    assert_eq!(ents(it, &db_on(Some(t2), Some(t1))), "0");
    assert_eq!(ents(it, &db_on(Some(t3), Some(t1))), "1");
    let read = |it: &mut Interpreter, d: &str| {
        it.eval_to_string(&format!("(mentat.store/read {d} {x} :t364/n)"))
            .unwrap()
    };
    assert_eq!(read(it, &db_on(Some(t2), Some(t1))), "2");
    assert_eq!(read(it, &db_on(Some(t1), Some(t1))), "nil");
    let entity = it
        .eval_to_string(&format!(
            "(:t364/n (mentat.store/entity {} {x}))",
            db_on(Some(t2), Some(t1))
        ))
        .unwrap();
    assert_eq!(entity, "2");
}
