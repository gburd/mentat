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

/// Task 3.2 gate: clj_control_test.clj. Now that core.clj is loaded (Task
/// 4.2), nearly every control-macro deftest passes: when/when-not/cond/condp/
/// case/if-let/when-let/and/or/not and the simple for-comprehensions all work.
/// Only two deftests remain skipped, each gated on a later phase:
const CONTROL_SKIP: &[&str] = &[
    // `dotimes` body uses atom/swap!/deref for its counter (Phase 5.3 atoms).
    "clj-dotimes",
    // `for` with chained :let + :when over multiple bindings expands to nested
    // mapcat/lazy-seq comprehension the eager `for` stand-in can't compose
    // (Phase 5 lazy seqs).
    "clj-for-let",
];

#[test]
fn clj_control_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/clj_control_test.clj"),
        CONTROL_SKIP,
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} clj_control_test assertions failed");
}

/// Task 4.2 gate: clj_predicates_test.clj. core.clj is now loaded, so the
/// predicate fns (true?/false?/some?/coll?/integer?/...) all resolve. Only
/// three deftests carry a single `(lazy-seq ...)` assertion each — lazy seqs
/// are Phase 5 — so those three are skipped; every other predicate deftest
/// (216 assertions) passes.
const PREDICATES_SKIP: &[&str] = &[
    // one `(list? (lazy-seq ...))` assertion; lazy seqs are Phase 5.
    "clj-list?",
    // one `(seq? (lazy-seq ...))` assertion; lazy seqs are Phase 5.
    "clj-seq?",
    // one `(coll? (lazy-seq ...))` assertion; lazy seqs are Phase 5.
    "clj-coll?",
];

#[test]
fn clj_predicates_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/clj_predicates_test.clj"),
        PREDICATES_SKIP,
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} clj_predicates_test assertions failed");
}

/// Task 5.1 gate: clojure_string_test.clj. The C string prims (subs/char-at/
/// upper-case/lower-case/trim/starts-with?/ends-with?/includes?/join/split/
/// replace/replace-first) plus the bundled `lib/clojure/string.clj` (loaded in
/// `Interp::new`) provide blank?/capitalize/escape/triml/trimr/reverse/
/// index-of/last-index-of/re-quote-replacement/trim-newline. The `str` alias
/// resolves `str/X` -> `clojure.string/X`.
///
/// Skipped deftests all need REGEX patterns (`#"..."`), which is Task 5.2:
/// the Phase-5.1 reader can't parse regex literals, so a kept deftest
/// containing one fails to read. `split-lines` is likewise regex-backed
/// (`#"\r?\n"`) so its defn is skipped at load and its deftest is skipped here.
const STRING_SKIP: &[&str] = &[
    // string+regex split forms; regex literals `#","` etc. (Task 5.2).
    "str-split-limit",
    "str-split-zero-width",
    // split-lines is defined with `#"\r?\n"` (Task 5.2), so it is unbound.
    "str-split-lines-crlf",
    // regex-match replace / replace-first (Task 5.2 regex engine).
    "str-replace-regex-string",
    "str-replace-regex-backref",
    "str-replace-first-regex",
    "str-replace-regex-quote",
    "str-replace-regex-fn",
];

#[test]
fn clojure_string_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/clojure_string_test.clj"),
        STRING_SKIP,
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} clojure_string_test assertions failed");
}

/// Task 4.2 gate: clj_higher_order_test.clj. core.clj + the Clojure supplement
/// (comp/partial/complement/juxt/zipmap/empty/find/some/every?) plus keyword/
/// symbol/map/vector-as-fn callability make every deftest pass with NO skips.
#[test]
fn clj_higher_order_corpus_passes() {
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/clj_higher_order_test.clj"),
        &[],
    );
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} clj_higher_order_test assertions failed");
}
