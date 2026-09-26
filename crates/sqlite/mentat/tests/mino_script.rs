//! SQLite-backend-specific scripting behaviors that are NOT part of the shared
//! Datomic-model suite (`mentat_script::model_tests`, run in
//! `tests/script_model.rs`): Mentat's honest refusal to run arbitrary Datalog
//! against a historical basis (it has no as-of query rewrite), and the instant
//! OUTPUT path through the as-of `datoms` reconstruction. The portable model
//! behaviors live in the shared suite so both backends prove them once.
//!
//! Only exercised with `--features mino`.
#![cfg(feature = "mino")]

use mentat::script::Interpreter;

/// Open an in-memory store, define `:person/name`, and insert Alice.
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

/// SQLite's `datoms` renders the attribute place as a KEYWORD (pg renders it
/// as an entid). This backend-specific tuple shape lives here; the shared model
/// suite asserts only the portable facts (starts with `[`, contains the value).
#[test]
fn datoms_render_the_attribute_as_a_keyword() {
    let mut it = seeded();
    let ds = it
        .eval_to_string("(mentat.store/datoms (mentat.store/db c))")
        .expect("datoms failed");
    assert!(ds.starts_with('['), "datoms: {ds}");
    assert!(ds.contains(":person/name \"Alice\""), "datoms: {ds}");
}

#[test]
fn q_against_non_current_basis_errors_honestly() {
    let mut it = seeded();
    let basis: i64 = it
        .eval_to_string("(:mentat.store/basis-tx (mentat.store/db c))")
        .unwrap()
        .parse()
        .unwrap();
    // Arbitrary Datalog against an as-of basis is not supported on SQLite; it
    // must error, not silently run against the current basis.
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
/// reader/printer: `read_one(pr-str v) == v`. #uuid reads to a real UUID value
/// and prints back as `#uuid "…"`; #inst reads to the
/// `clojure.instant/read-instant-date` constructor form (which re-reads
/// identically). Both round-trip stably.
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
    let (inst, _) = read_one(r#"#inst "2017-01-01T00:00:00Z""#).unwrap();
    assert_eq!(
        print_str(&inst),
        "(clojure.instant/read-instant-date \"2017-01-01T00:00:00Z\")"
    );
    let (uuid, _) = read_one(r#"#uuid "12345678-1234-5678-1234-567812345678""#).unwrap();
    assert_eq!(
        print_str(&uuid),
        "#uuid \"12345678-1234-5678-1234-567812345678\""
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
    assert!(
        ds.contains("(clojure.instant/read-instant-date"),
        "as-of datoms should carry a tagged instant (:db/txInstant): {ds}"
    );
}

/// `with_default_path`: a no-arg `(mentat.store/open)` opens the host-supplied
/// file, so writes persist to it and are visible to a plain `Store::open`.
#[test]
fn with_default_path_makes_no_arg_open_use_that_file() {
    let dir = std::env::temp_dir().join(format!("mentat_default_path_{}", std::process::id()));
    let path = dir.to_str().unwrap().to_string();
    let _ = std::fs::remove_file(&path);

    let mut it = Interpreter::with_default_path(&path);
    it.eval("(def c (mentat.store/open))").unwrap();
    it.eval_to_string(
        "(mentat.store/transact c [{:db/ident :person/name \
           :db/valueType :db.type/string :db/cardinality :db.cardinality/one}])",
    )
    .unwrap();
    it.eval_to_string("(mentat.store/transact c [{:person/name \"Zed\"}])")
        .unwrap();
    drop(it);

    use mentat::{IntoResult, Queryable};
    let store = mentat::Store::open(&path).unwrap();
    let name = store
        .q_once("[:find ?n . :where [_ :person/name ?n]]", None)
        .into_scalar_result()
        .unwrap()
        .and_then(|b| b.into_string());
    assert_eq!(name.as_deref().map(|s| s.as_str()), Some("Zed"));

    // Plain `new()` is still in-memory: a no-arg open does not see Zed.
    let mut fresh = Interpreter::new();
    fresh.eval("(def c (mentat.store/open))").unwrap();
    let r = fresh
        .eval_to_string(
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [_ :person/name ?n]])",
        )
        .unwrap_or_default();
    assert!(!r.contains("Zed"), "in-memory store leaked: {r}");
    let _ = std::fs::remove_file(&path);
}
