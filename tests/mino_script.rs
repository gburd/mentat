//! Integration test for the optional mino scripting layer. Only exercised with
//! `--features mino`; without the feature the file compiles to nothing.
#![cfg(feature = "mino")]

use mentat::script::Interpreter;

#[test]
fn mentat_store_round_trips() {
    let mut it = Interpreter::new();
    // open -> transact -> read round-trip through the aliased mentat.store ns.
    let got = it
        .eval_to_string(
            "(def c (mentat.store/open)) \
             (mentat.store/transact c {:alice {:name \"Alice\" :age 30}}) \
             (mentat.store/read (mentat.store/db c) :alice :age)",
        )
        .expect("script eval failed");
    assert_eq!(got, "30");

    assert_eq!(
        it.eval_to_string("(mentat.store/store? (mentat.store/open))")
            .unwrap(),
        "true"
    );
}
