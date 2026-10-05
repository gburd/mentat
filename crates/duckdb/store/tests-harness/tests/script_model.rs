//! The shared Datomic-model scripting suite (`mentat_script::model_tests`),
//! run against DuckDB-backed stores: the same suite the SQLite, pg_mentat
//! and fake backends run.

use duckdb::Connection;
use mentat_duckdb_store::{script, SqlConn};
use mentat_duckdb_store_tests::DuckConn;
use mentat_script::model_tests as m;

fn interp() -> mino_rs::Interpreter {
    let c: &'static Connection = Box::leak(Box::new(Connection::open_in_memory().unwrap()));
    let conn: &'static dyn SqlConn = Box::leak(Box::new(DuckConn(c)));
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
model_test!(tx_report_has_the_datomic_shape);
model_test!(q_takes_in_inputs);
model_test!(q_mixed_inputs);
model_test!(history_patterns_see_added);
model_test!(cas_and_retract_entity);

/// `with` needs a savepoint inside DuckDB's transaction, which DuckDB lacks;
/// the DuckDB backend refuses it with a clear error rather than committing.
#[test]
fn with_is_refused_not_committed() {
    let mut it = interp();
    m::seed(&mut it);
    let Err(err) = it.eval("(mentat.store/with (mentat.store/db c) [{:person/name \"Bob\"}])")
    else {
        panic!("with must be refused");
    };
    assert!(err.contains("not supported"), "{err}");
    let n = it
        .eval_to_string(
            "(count (mentat.store/q (mentat.store/db c) '[:find ?n :where [_ :person/name ?n]]))",
        )
        .unwrap();
    assert_eq!(n, "1");
}
