//! Conformance harness: run mino's own `.clj` corpus against the Rust port and
//! assert zero failures over the deftests the current phase supports.
//!
//! Phase 1 supports only self-eval + `+ - * / = < > <= >=` and
//! `if/do/quote/fn/def/apply` — no macros, no core.clj. So the gate here covers
//! the arithmetic file's pure-arith deftests and skips every deftest that needs
//! a not-yet-ported primitive, predicate, or reader literal. Each skipped name
//! is annotated with the phase that turns it on (see docs/plan/mino-rs-port.md).

use mino_rs::corpus::run_corpus_file;

/// Deftests skipped in Phase 1, with the phase that enables each. The reader
/// also can't yet parse ratio (`5/2`), `##NaN`, `0x..` hex, or `N`-suffixed
/// bigint literals that live inside several of these, which is the *other*
/// reason they must be skipped rather than read: the harness never reads a
/// skipped deftest's body, so those unparseable literals don't abort the file.
const SKIP: &[&str] = &[
    // ratio literal `5/2` (Phase 5.5 bignum/ratio) + `double` coercion (Phase 5).
    "division",
    // `testing`+`not` predicates (Phase 4 core.clj) + `==` cross-tier eq (Phase 5).
    "comparisons",
    // `mod`/`rem`/`quot` primitives (Phase 5 numeric).
    "mod-rem-quot",
    // `bit-and/or/xor/not/shift-*` primitives (Phase 5 numeric).
    "bitwise",
    // bit-shift prims + `##`/hex literals + `thrown?` bounds (Phase 5 numeric).
    "bit-shift-boundary",
    // bit-shift prims + `0x` hex literals (Phase 5 numeric).
    "bit-shift-right-preserves-sign",
    // `second`/`ffirst` seq prims (Phase 2) + `inc/dec/zero?/pos?/...` predicates
    // and `abs/max/min` (Phase 4 core.clj).
    "trivial-compositions",
    // `int`/`float`/`double` coercion + `float?`/`NaN?` predicates + `##NaN`
    // literals (Phase 5 numeric coercion + Phase 4 predicates).
    "numeric-coercion",
    // long-overflow throw + `+'`/`*'` N-promotion + `N` literals (Phase 5.5 bignum).
    "integer-overflow-strict-and-primed",
    // `unchecked-int/long/byte/...` narrowing casts (Phase 5 numeric).
    "unchecked-narrowing-casts",
    // `unchecked-add-int`/etc. wraparound arithmetic (Phase 5 numeric).
    "unchecked-int-arithmetic",
    // `bit-shift-left` + `+'`/`-'` + `type` + `let` (Phase 3 let, Phase 5.5 bignum).
    "tagged-int-boundary",
    // `float?` predicate (Phase 4 core.clj) + scientific-notation edge cases.
    "scientific-notation-signed-exponents",
];

#[test]
fn arithmetic_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/arithmetic_test.clj"),
        SKIP,
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} arithmetic assertions failed");
}

/// Task 2.3 gate: are_test.clj. The `are`/`is`/`thrown?` deftests exercise
/// `=`, `inc`, `number?`, `nth`, and `thrown?` — all now implemented.
///
/// The three `is-*`/`binding`-based deftests need `let`, `binding`, `atom`,
/// `try`/`catch`, and `*report-counters*` (Phase 3 let/try, Phase 5 atoms).
/// They are skipped with rationale; the four `are-*` deftests must pass.
const ARE_SKIP: &[&str] = &[
    // needs let + atom + binding + try/catch + *report-counters* (Phase 3/5).
    "is-eq-continues-after-throw-in-value",
    "is-truthy-continues-after-throw-in-value",
];

#[test]
fn are_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/are_test.clj"),
        ARE_SKIP,
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} are_test assertions failed");
}

/// Task 3.1 gate: binding_test.clj. Only the deftests that need nothing beyond
/// def / let / loop / recur / destructuring are kept. Everything gated on
/// later phases is skipped with the phase noted:
///   - dynamic vars + `binding` + bound?/thread-bound?/with-bindings*/
///     push+pop-thread-bindings + `*ns*` thread-binding: Phase 4 (dynamic
///     vars, namespaces, `ns`/`in-ns`/`alias`).
///   - `try`/`catch`/`throw`/`finally` + `eval`: Task 3.2 (control) / Phase 4.
/// The kept deftests (def-then-read, def-redefine, let-binding,
/// var-redef-closure) exercise def + let + sequential/testing only.
const BINDING_SKIP: &[&str] = &[
    // dynamic-var `binding` + `re-find`/`eval`/`try` (Phase 4 + Task 3.2).
    "binding-on-dynamic-var-rebinds",
    "binding-on-non-dynamic-var-throws",
    // bound?/thread-bound?/with-bindings*/push+pop + dynamic vars (Phase 4).
    "bound?-checks-root-or-thread",
    "thread-bound?-checks-only-thread",
    "with-bindings-installs-and-pops",
    "push-pop-thread-bindings-pair",
    "with-bindings-snapshot-via-get-thread-bindings",
    "binding-frame-unwinds-on-throw",
    // var-identity binding across ns/alias spellings (Phase 4 namespaces).
    "qualified-binding-visible-to-unqualified-reader",
    "alias-binding-visible-to-unqualified-reader",
    "qualified-binding-visible-via-qualified-read",
    "nested-qualified-bindings-stack-and-restore",
    "core-var-qualified-binding-bare-read",
    "core-var-bare-binding-qualified-read",
    "qualified-binding-frame-unwinds-on-throw",
    // *ns* thread-binding save/restore across in-ns (Phase 4 namespaces).
    "binding-ns-restore-after-in-ns",
    "binding-ns-body-reflects-in-ns-then-restores",
    "binding-ns-nested-restore",
    "binding-ns-restores-on-throw",
];

#[test]
fn binding_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/binding_test.clj"),
        BINDING_SKIP,
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} binding_test assertions failed");
}
