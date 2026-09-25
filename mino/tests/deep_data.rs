//! Deep-nesting regression tests: printing, hashing, equality and comparison
//! of data nested thousands of levels deep must NOT overflow the Rust stack.
//! Its own process (separate test binary), so a pre-fix stack overflow only
//! aborts this binary. Run on a 2 MB-ish stack thread (the default `cargo
//! test` main thread, or `RUST_MIN_STACK=1048576` for spawned threads).
//!
//! Each "deep" value is built in-language by a tail-recursive loop that wraps
//! the accumulator one vector deep per step:
//!   (def x (loop [x nil i 0] (if (< i N) (recur [x] (inc i)) x)))
//! giving a value nested N deep. Building it uses constant stack (tail calls);
//! only the later print/hash/eq/compare walks used to recurse on the Rust
//! stack and overflow.

use mino_rs::Interpreter;

const N: usize = 100_000;

fn deep_prelude() -> String {
    format!("(def x (loop [x nil i 0] (if (< i {N}) (recur [x] (inc i)) x)))")
}

/// Fresh interpreter with the deep value `x` already defined.
fn with_deep() -> Interpreter {
    let mut it = Interpreter::new();
    it.eval(&deep_prelude()).expect("building deep value");
    it
}

#[test]
fn deep_source_is_a_read_error() {
    // 100000 nested `[` (unterminated) must be rejected by the reader's depth
    // cap as an error, not overflow the stack while reading.
    let src: String = "[".repeat(N);
    let mut it = Interpreter::new();
    let r = it.eval(&src);
    assert!(r.is_err(), "reading {N} nested `[` should be an error");
}

#[test]
fn deep_pr_str_errors() {
    let mut it = with_deep();
    let e = it
        .eval_to_string("(pr-str x)")
        .expect_err("pr-str of deep data should error");
    assert!(
        e.contains(":eval/limit") || e.contains("too deep") || e.contains("nesting"),
        "pr-str error not a nesting limit: {e}"
    );

    let e = it
        .eval_to_string("(str x)")
        .expect_err("str of deep data should error");
    assert!(
        e.contains(":eval/limit") || e.contains("too deep") || e.contains("nesting"),
        "str error not a nesting limit: {e}"
    );
}

#[test]
fn deep_hash_no_crash() {
    let mut it = with_deep();
    let s = it.eval_to_string("(hash x)").expect("hash of deep data");
    // Just needs to be an integer literal (no crash, no error).
    assert!(s.parse::<i64>().is_ok(), "hash did not return an int: {s}");
}

#[test]
fn deep_eq_no_crash() {
    let mut it = with_deep();
    assert_eq!(it.eval_to_string("(= x x)").expect("= deep"), "true");
    // Shallow structural equality is unaffected.
    assert_eq!(
        it.eval_to_string("(= [1 [2]] [1 [2]])").expect("= shallow"),
        "true"
    );
    assert_eq!(
        it.eval_to_string("(= [1 [2]] [1 [3]])")
            .expect("!= shallow"),
        "false"
    );
}

#[test]
fn deep_compare() {
    let mut it = with_deep();
    // Either "0" (equal) or a nesting error is acceptable; the point is no
    // stack overflow.
    match it.eval_to_string("(compare x x)") {
        Ok(s) => assert_eq!(s, "0", "compare of equal deep data should be 0"),
        Err(e) => assert!(
            e.contains(":eval/limit") || e.contains("too deep") || e.contains("nesting"),
            "compare error not a nesting limit: {e}"
        ),
    }
}

#[test]
fn self_ref_atom_prints() {
    let mut it = Interpreter::new();
    it.eval("(def a (atom nil))").expect("def atom");
    it.eval("(reset! a a)").expect("reset! self-ref");
    // Printing a self-referencing atom must terminate with a cycle marker,
    // not overflow the stack. An error is also acceptable (no overflow).
    match it.eval_to_string("(pr-str a)") {
        Ok(s) => assert!(s.contains("#<cycle>"), "no cycle marker in: {s}"),
        Err(_) => {} // an error is fine too — just no overflow
    }
}

#[test]
fn ordinary_data_unchanged() {
    let mut it = Interpreter::new();
    // pr-str of ordinary small data is byte-for-byte what it was before.
    assert_eq!(
        it.eval_to_string("(pr-str {:a [1 2]})").unwrap(),
        "\"{:a [1 2]}\""
    );
    assert_eq!(
        it.eval_to_string("(pr-str [1 2 3])").unwrap(),
        "\"[1 2 3]\""
    );
    assert_eq!(
        it.eval_to_string("(pr-str '(1 2 3))").unwrap(),
        "\"(1 2 3)\""
    );
    assert_eq!(it.eval_to_string("(pr-str #{1})").unwrap(), "\"#{1}\"");
    // hash is stable and equal for equal values.
    assert_eq!(
        it.eval_to_string("(= (hash [1 2]) (hash [1 2]))").unwrap(),
        "true"
    );
    // eq on ordinary maps/sets/lists.
    assert_eq!(
        it.eval_to_string("(= {:a 1 :b 2} {:b 2 :a 1})").unwrap(),
        "true"
    );
    assert_eq!(it.eval_to_string("(= #{1 2 3} #{3 2 1})").unwrap(), "true");
    assert_eq!(it.eval_to_string("(= '(1 2) [1 2])").unwrap(), "true");
    // compare on ordinary vectors/numbers.
    assert_eq!(it.eval_to_string("(compare 1 2)").unwrap(), "-1");
    assert_eq!(it.eval_to_string("(compare [1 2] [1 3])").unwrap(), "-1");
    assert_eq!(it.eval_to_string("(compare [1 2] [1 2])").unwrap(), "0");
}
