//! The shared Datomic-model scripting suite (`mentat_script::model_tests`) run
//! against the REAL SQLite-backed `Store` backend. Proves the extracted
//! `mentat_script` layer behaves identically on live storage, and — with the
//! fake-backend run in `crates/script` and the pg_mentat `#[pg_test]` run — is
//! the one suite that keeps the two backends from drifting (plan § 1.19).
//!
//! Only compiled with `--features mino`.
#![cfg(feature = "mino")]

use mentat::script::Interpreter;
use mentat_script::model_tests as m;

/// Run a model-suite body against a fresh store-installed interpreter.
macro_rules! model_test {
    ($name:ident) => {
        #[test]
        fn $name() {
            let mut interp = Interpreter::new();
            m::$name(interp.inner());
        }
    };
}

model_test!(db_is_an_immutable_value_not_the_conn);
model_test!(q_takes_a_db_value);
model_test!(pull_returns_a_map);
model_test!(entity_returns_an_entity_map);
model_test!(read_returns_the_scalar);
model_test!(datoms_returns_tuples);
model_test!(with_is_speculative_and_does_not_commit);
model_test!(as_of_and_since_reflect_the_basis);
model_test!(tx_report_has_the_datomic_shape);
model_test!(q_takes_in_inputs);
model_test!(q_mixed_inputs);
model_test!(history_patterns_see_added);
model_test!(cas_and_retract_entity);

#[test]
fn inst_and_uuid_builders_round_trip_through_the_reader() {
    m::inst_and_uuid_builders_round_trip_through_the_reader();
}
