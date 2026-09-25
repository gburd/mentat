//! Integration test for the optional mino scripting layer, backed by the real
//! SQLite-backed `mentat::Store`. Only exercised with `--features mino`;
//! without the feature the file compiles to nothing.
//!
//! These tests prove the Datomic-a-like model: `db` yields an immutable
//! database *value* (a map carrying its basis tx), reads take a db value,
//! `with` is a pure speculative `db -> db'` that does not commit, and
//! `as-of`/`since` produce db values whose `datoms` reflect the basis.
#![cfg(feature = "mino")]

use mentat::script::Interpreter;

/// Open an in-memory store, define `:person/name`, and insert Alice + Bob.
/// Returns the interpreter with `c` bound to the conn handle.
fn seeded() -> Interpreter {
    let mut it = Interpreter::new();
    it.eval("(def c (mentat.store/open))").unwrap();
    it.eval_to_string(
        "(mentat.store/transact c \
           [{:db/ident :person/name \
             :db/valueType :db.type/string \
             :db/cardinality :db.cardinality/one}])",
    )
    .expect("schema transact failed");
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Alice\"}])")
        .expect("data transact failed");
    it
}

/// The eid of the person named `name`, as a scalar-query result string.
fn eid_of(it: &mut Interpreter, name: &str) -> String {
    it.eval_to_string(&format!(
        "(mentat.store/q (mentat.store/db c) '[:find ?e . :where [?e :person/name \"{name}\"]])"
    ))
    .expect("eid query failed")
}

#[test]
fn db_is_an_immutable_value_not_the_conn() {
    let mut it = seeded();

    // `open` yields an opaque integer conn handle.
    assert_eq!(it.eval_to_string("c").unwrap(), "1");

    // `db` yields a db VALUE: a map, not the conn integer. It carries the
    // basis-tx and the db tag, and identifies its conn.
    let db = it.eval_to_string("(mentat.store/db c)").unwrap();
    assert!(db.contains(":mentat.store/db true"), "db value: {db}");
    assert!(db.contains(":mentat.store/basis-tx"), "db value: {db}");
    assert!(db.contains(":mentat.store/conn 1"), "db value: {db}");
    // It is NOT just the conn handle.
    assert_ne!(db, "1");

    // The basis-tx is a positive tx id (two committed txns: schema + data).
    let basis = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap();
    assert!(
        basis.parse::<i64>().unwrap() > 0x1000_0000,
        "basis-tx should be a real tx id, got {basis}"
    );
}

#[test]
fn q_takes_a_db_value() {
    let mut it = seeded();
    let got = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])",
        )
        .expect("query failed");
    assert_eq!(got, "#{[\"Alice\"]}");

    // A bare conn is accepted for ergonomics (treated as current db).
    let via_conn = it
        .eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
        .expect("query via conn failed");
    assert_eq!(via_conn, "#{[\"Alice\"]}");
}

#[test]
fn pull_returns_a_map() {
    let mut it = seeded();
    let eid = eid_of(&mut it, "Alice");
    let pulled = it
        .eval_to_string(&format!(
            "(mentat.store/pull (mentat.store/db c) {eid} [:person/name])"
        ))
        .expect("pull failed");
    assert_eq!(pulled, "{:person/name \"Alice\"}");
}

#[test]
fn entity_returns_an_entity_map() {
    let mut it = seeded();
    let eid = eid_of(&mut it, "Alice");
    let ent = it
        .eval_to_string(&format!("(mentat.store/entity (mentat.store/db c) {eid})"))
        .expect("entity failed");
    // Entity map carries :db/id and the attribute.
    assert!(ent.contains(&format!(":db/id {eid}")), "entity: {ent}");
    assert!(ent.contains(":person/name \"Alice\""), "entity: {ent}");
}

#[test]
fn read_returns_the_scalar() {
    let mut it = seeded();
    let eid = eid_of(&mut it, "Alice");
    let v = it
        .eval_to_string(&format!(
            "(mentat.store/read (mentat.store/db c) {eid} :person/name)"
        ))
        .expect("read failed");
    assert_eq!(v, "\"Alice\"");
}

#[test]
fn datoms_returns_tuples() {
    let mut it = seeded();
    let ds = it
        .eval_to_string("(mentat.store/datoms (mentat.store/db c))")
        .expect("datoms failed");
    // A vector of [e a v] tuples that includes the Alice name assertion.
    assert!(ds.starts_with('['), "datoms: {ds}");
    assert!(ds.contains(":person/name \"Alice\""), "datoms: {ds}");
}

#[test]
fn with_is_speculative_and_does_not_commit() {
    let mut it = seeded();

    // Count Alice-or-Bob names on the current db.
    let before = it
        .eval_to_string("(mentat.store/q c '[:find ?n :where [?e :person/name ?n]])")
        .unwrap();
    assert_eq!(before, "#{[\"Alice\"]}");

    // Speculatively add Bob via `with`. It returns a map with a db-after and a
    // tx-report at a NEW basis.
    let with_result = it
        .eval_to_string("(mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])")
        .expect("with failed");
    assert!(
        with_result.contains(":mentat.store/db-after"),
        "with result: {with_result}"
    );
    assert!(
        with_result.contains(":mentat.store/tx-report"),
        "with result: {with_result}"
    );

    // The db-after basis-tx is greater than the current basis: a new tx ran.
    let basis_now: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    let db_after_basis: i64 = it
        .eval_to_string(
            "(:mentat.store/basis-tx \
               (:mentat.store/db-after \
                 (mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])))",
        )
        .unwrap()
        .parse()
        .unwrap();
    assert!(
        db_after_basis > basis_now,
        "db-after basis {db_after_basis} should exceed current {basis_now}"
    );

    // CRUCIAL: the real store is UNCHANGED. A fresh db off the conn still sees
    // only Alice — Bob was rolled back, never committed.
    let after = it
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [?e :person/name ?n]])",
        )
        .unwrap();
    assert_eq!(after, "#{[\"Alice\"]}", "with must not commit");
}

#[test]
fn as_of_and_since_reflect_the_basis() {
    let mut it = seeded();

    // Basis after the first (Alice) data tx.
    let basis_alice: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();

    // Commit a second person, Bob, advancing the basis.
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Bob\"}])")
        .expect("Bob transact failed");
    let basis_bob: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    assert!(basis_bob > basis_alice);

    // `as-of basis_alice`: reconstructed datoms include Alice, NOT Bob.
    let as_of_alice = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis_alice}))"
        ))
        .expect("as-of datoms failed");
    assert!(as_of_alice.contains("Alice"), "as-of Alice: {as_of_alice}");
    assert!(
        !as_of_alice.contains("Bob"),
        "as-of should exclude Bob: {as_of_alice}"
    );

    // `as-of basis_bob`: datoms include both.
    let as_of_bob = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis_bob}))"
        ))
        .expect("as-of bob datoms failed");
    assert!(
        as_of_bob.contains("Alice") && as_of_bob.contains("Bob"),
        "as-of Bob: {as_of_bob}"
    );

    // `since basis_alice`: only datoms asserted AFTER Alice's tx — so Bob, not Alice.
    let since_alice = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/since (mentat.store/db c) {basis_alice}))"
        ))
        .expect("since datoms failed");
    assert!(
        since_alice.contains("Bob"),
        "since should include Bob: {since_alice}"
    );
    assert!(
        !since_alice.contains("Alice"),
        "since should exclude Alice: {since_alice}"
    );

    // as-of / since return db values.
    let as_of_db = it
        .eval_to_string(&format!(
            "(mentat.store/as-of (mentat.store/db c) {basis_alice})"
        ))
        .unwrap();
    assert!(
        as_of_db.contains(&format!(":mentat.store/as-of {basis_alice}")),
        "as-of db: {as_of_db}"
    );
    let since_db = it
        .eval_to_string(&format!(
            "(mentat.store/since (mentat.store/db c) {basis_alice})"
        ))
        .unwrap();
    assert!(
        since_db.contains(&format!(":mentat.store/since {basis_alice}")),
        "since db: {since_db}"
    );
}

#[test]
fn q_against_non_current_basis_errors_honestly() {
    let mut it = seeded();
    let basis: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    // Arbitrary Datalog against an as-of basis is not supported; it must error,
    // not silently run against the current basis.
    let err = match it.eval(&format!(
        "(mentat.store/q (mentat.store/as-of (mentat.store/db c) {basis}) \
           '[:find ?n :where [?e :person/name ?n]])"
    )) {
        Err(e) => e,
        Ok(_) => panic!("q against as-of basis should error"),
    };
    assert!(err.contains("not supported"), "got: {err}");
}

/// The inst/uuid representation the layer emits round-trips through the mino
/// reader/printer: `read_one(pr-str v) == v`. This is the honest round-trip
/// this mino-rs port supports (it has no `#uuid`/`read-string`; a `#inst`
/// literal reads to a calendar map, and `#uuid` cannot eval at all). The layer
/// emits the exact cons form the reader itself produces from each literal, so
/// the printed form re-reads identically.
#[test]
fn inst_and_uuid_representations_round_trip_through_the_reader() {
    use mino_rs::printer::print_str;
    use mino_rs::reader::read_one;
    for lit in [
        r#"#inst "2017-01-01T00:00:00Z""#,
        r#"#uuid "12345678-1234-5678-1234-567812345678""#,
    ] {
        let (v, _) = read_one(lit).expect("read literal");
        let printed = print_str(&v);
        let (v2, _) = read_one(&printed).expect("re-read printed form");
        assert_eq!(
            printed,
            print_str(&v2),
            "pr-str/read-string round-trip must be stable for {lit}"
        );
    }
    // And the layer prints exactly these forms (matching read_one's output).
    let (inst, _) = read_one(r#"#inst "2017-01-01T00:00:00Z""#).unwrap();
    assert_eq!(
        print_str(&inst),
        "(clojure.instant/read-instant-date \"2017-01-01T00:00:00Z\")"
    );
    let (uuid, _) = read_one(r#"#uuid "12345678-1234-5678-1234-567812345678""#).unwrap();
    assert_eq!(
        print_str(&uuid),
        "(parse-uuid \"12345678-1234-5678-1234-567812345678\")"
    );
}

/// The instant OUTPUT path is real: an as-of `datoms` reconstruction replays
/// the `transactions` table, which carries the per-tx `:db/txInstant`
/// (instant-typed) datom. That instant is emitted as the tagged reader form,
/// proving `typed_value`/`edn_value` map `Instant` to `(clojure.instant/...)`.
#[test]
fn instant_output_path_emits_tagged_form() {
    let mut it = seeded();
    let basis: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    let ds = it
        .eval_to_string(&format!(
            "(mentat.store/datoms (mentat.store/as-of (mentat.store/db c) {basis}))"
        ))
        .expect("as-of datoms failed");
    // The reconstructed set includes :db/txInstant datoms whose value is an
    // instant, emitted as the tagged reader form.
    assert!(
        ds.contains("(clojure.instant/read-instant-date"),
        "as-of datoms should carry a tagged instant (:db/txInstant): {ds}"
    );
}
