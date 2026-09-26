//! The shared Datomic-model scripting suite (`mentat_script::model_tests`) run
//! against the REAL pg_mentat engine backend as `#[pg_test]`s. Together with
//! the SQLite run (`crates/sqlite/mentat/tests/script_model.rs`) and the fake
//! run (`crates/script/tests/model.rs`), this is the one suite that keeps the
//! two backends from drifting (plan § 1.19).
//!
//! Registered from `lib.rs` behind `#[cfg(feature = "script")]`. Each test
//! builds a sandboxed interpreter over the pg engine (via `build_interpreter`,
//! which preserves the Task-1c sandbox + GUC limits + check hook) and seeds it
//! through SPI in the pg_test's transaction.
//!
//! The historical-`q` behavior is exercised implicitly: pg_mentat's `q`
//! forwards the temporal bound (§ 1.20), so unlike SQLite there is no as-of-q
//! honest-error gate here.

#[cfg(any(test, feature = "pg_test"))]
#[pgrx::pg_schema]
mod tests {
    use crate::functions::script::build_interpreter;
    use mentat_script::model_tests as m;

    macro_rules! model_pg_test {
        ($name:ident) => {
            #[pgrx::pg_test]
            fn $name() {
                let mut it = build_interpreter();
                m::$name(&mut it);
            }
        };
    }

    model_pg_test!(db_is_an_immutable_value_not_the_conn);
    model_pg_test!(q_takes_a_db_value);
    model_pg_test!(pull_returns_a_map);
    model_pg_test!(entity_returns_an_entity_map);
    model_pg_test!(read_returns_the_scalar);
    model_pg_test!(datoms_returns_tuples);
    model_pg_test!(with_is_speculative_and_does_not_commit);
    model_pg_test!(as_of_and_since_reflect_the_basis);
    model_pg_test!(tx_report_has_the_datomic_shape);

    #[pgrx::pg_test]
    fn inst_and_uuid_builders_round_trip_through_the_reader() {
        m::inst_and_uuid_builders_round_trip_through_the_reader();
    }
}
