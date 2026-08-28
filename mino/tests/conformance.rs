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
