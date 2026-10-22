//! The shared Datomic-model scripting suite (`mentat_script::model_tests`),
//! run against DuckDB-backed stores: the same suite the SQLite, pg_mentat
//! and fake backends run.

use duckdb::Connection;
use mentat_duckdb_store::{script, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use mentat_script::model_tests as m;

fn interp() -> mino_rs::Interpreter {
    let c: &'static Connection = Box::leak(Box::new(Connection::open_in_memory().unwrap()));
    let conn: &'static dyn SqlConn = Box::leak(Box::new(DuckConn::new(c)));
    script::interpreter(conn, "default")
}

macro_rules! model_test {
    ($name:ident) => {
        #[test]
        fn $name() {
            m::$name(&mut interp());
        }
    };
}

model_test!(db_is_an_immutable_value_not_the_conn);
model_test!(q_takes_a_db_value);
model_test!(pull_returns_a_map);
model_test!(entity_returns_an_entity_map);
model_test!(read_returns_the_scalar);
model_test!(datoms_returns_tuples);
model_test!(as_of_and_since_reflect_the_basis);
model_test!(q_runs_against_as_of_and_since);
model_test!(pull_reads_the_as_of_and_since_basis);
model_test!(pull_nested_map_specs);
model_test!(as_of_hides_retracted_values);
model_test!(as_of_sees_a_value_reasserted_after_retraction);
model_test!(since_reads_current_datoms_added_after_t);
model_test!(as_of_and_since_together);
model_test!(missing_and_get_else_read_the_basis);
model_test!(or_reads_the_basis);
model_test!(not_or_reads_the_basis);
model_test!(pull_of_an_unknown_attribute_is_empty);
model_test!(tx_report_has_the_datomic_shape);
model_test!(q_takes_in_inputs);
model_test!(q_mixed_inputs);
model_test!(history_patterns_see_added);
model_test!(cas_and_retract_entity);
model_test!(transaction_functions);

model_test!(with_is_speculative_and_does_not_commit);

fn eval(it: &mut mino_rs::Interpreter, src: &str) -> String {
    it.eval_to_string(src)
        .unwrap_or_else(|e| panic!("{src}: {e}"))
}

/// `with` on DuckDB rolls its DuckDB transaction back: the speculative tx id
/// is handed out again by the next real transact (the partition counters
/// rolled back), another connection sees nothing, a failing `with` leaves no
/// trace and no open transaction, and a speculative schema change doesn't
/// leak into the cached schema.
#[test]
fn with_rolls_back_everything() {
    let c: &'static Connection = Box::leak(Box::new(Connection::open_in_memory().unwrap()));
    let conn: &'static dyn SqlConn = Box::leak(Box::new(DuckConn::new(c)));
    let mut it = script::interpreter(conn, "default");
    m::seed(&mut it);
    let other = c.try_clone().unwrap();
    let snapshot = |c: &Connection| -> (i64, i64, i64) {
        c.query_row(
            "SELECT (SELECT count(*) FROM mentat.datoms), \
                    (SELECT count(*) FROM mentat.timelined_transactions), \
                    (SELECT sum(idx) FROM mentat.known_parts)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    };
    let before = snapshot(&other);

    let spec_tx: i64 = eval(
        &mut it,
        "(:mentat.store/tx-id (:mentat.store/tx-report \
           (mentat.store/with (mentat.store/db c) [{:db/id \"b\" :person/name \"Bob\"}])))",
    )
    .parse()
    .unwrap();
    assert_eq!(snapshot(&other), before, "with left rows behind");
    assert_eq!(snapshot(c), before);

    // A failing `with` (unknown attribute): an error, nothing left, and the
    // connection has no transaction left open (the next transact commits).
    let err = it
        .eval_to_string("(mentat.store/with (mentat.store/db c) [{:person/name \"X\" :no/such 1}])")
        .unwrap_err();
    assert!(err.contains("no/such"), "{err}");
    assert_eq!(snapshot(&other), before);

    // A speculative schema change is not visible afterwards.
    eval(
        &mut it,
        "(mentat.store/with (mentat.store/db c) \
           [{:db/ident :person/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one}])",
    );
    assert!(it
        .eval("(mentat.store/transact c [{:person/email \"z@x\"}])")
        .is_err());

    // Two schema-changing tx-data in one `with` (a tx-fn's first install
    // also defines :db/fn): both apply, then both roll back.
    eval(
        &mut it,
        "(mentat.store/with (mentat.store/db c) \
           [{:db/ident :my/inc :db/fn \"(fn [db] [])\"}])",
    );
    assert_eq!(snapshot(&other), before);
    assert!(
        !conn_has_ident(c, ":db/fn"),
        "with committed the :db/fn attribute"
    );

    let real_tx: i64 = eval(
        &mut it,
        "(:mentat.store/tx-id (mentat.store/transact c [{:person/name \"Bob\"}]))",
    )
    .parse()
    .unwrap();
    assert_eq!(real_tx, spec_tx, "the tx counter was rolled back");
    assert_eq!(
        eval(
            &mut it,
            "(count (mentat.store/q (mentat.store/db c) '[:find ?n :where [_ :person/name ?n]]))"
        ),
        "2"
    );
}

fn conn_has_ident(c: &Connection, ident: &str) -> bool {
    c.query_row(
        "SELECT count(*) FROM mentat.idents WHERE union_extract(v, 's') = ?",
        [ident],
        |r| r.get::<_, i64>(0),
    )
    .unwrap()
        > 0
}

/// `with` runs transaction functions (expanded before the rolled-back
/// transaction) and reports their tx-data's tempids.
#[test]
fn with_expands_transaction_functions() {
    let mut it = interp();
    m::seed(&mut it);
    eval(
        &mut it,
        r#"(mentat.store/transact c [{:db/ident :my/person
             :db/fn "(fn [db n] [{:db/id \"p\" :person/name n}])"}])"#,
    );
    let r = eval(
        &mut it,
        r#"(:mentat.store/tempids (:mentat.store/tx-report
             (mentat.store/with (mentat.store/db c) [[:my/person "Zed"]])))"#,
    );
    assert!(r.contains("\"p\""), "{r}");
    assert_eq!(
        eval(
            &mut it,
            "(mentat.store/q (mentat.store/db c) '[:find ?n :where [_ :person/name ?n]])"
        ),
        "#{[\"Alice\"]}"
    );
}
