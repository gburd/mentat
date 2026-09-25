//! General tail calls + flat-list teardown. A separate test binary (= separate
//! process), so a pre-fix stack overflow only aborts this.

use mino_rs::embed::Limits;
use mino_rs::Interpreter;

const LIMITS: Limits = Limits {
    steps: Some(50_000_000),
    heap_bytes: None,
    depth: Some(300),
};

/// Run `src` in a fresh sandboxed interpreter with `limits` on a thread with
/// a `stack`-byte stack.
fn run(stack: usize, limits: Limits, src: &'static str) -> Result<String, String> {
    std::thread::Builder::new()
        .stack_size(stack)
        .spawn(move || {
            let mut it = Interpreter::sandboxed();
            it.set_limits(limits);
            it.eval_to_string(src)
        })
        .unwrap()
        .join()
        .expect("thread overflowed or panicked")
}

fn ok(src: &'static str) -> String {
    run(1024 * 1024, LIMITS, src).unwrap_or_else(|e| panic!("{src} => {e}"))
}

fn assert_limit(e: &str, which: &str) {
    assert!(e.contains(":eval/limit"), "not a limit error: {e}");
    assert!(e.contains(which), "expected {which} in: {e}");
}

#[test]
fn dotimes_is_constant_stack() {
    assert_eq!(
        ok("(let [a (atom 0)] (dotimes [i 1000000] (swap! a inc)) @a)"),
        "1000000"
    );
}

#[test]
fn while_is_constant_stack() {
    assert_eq!(
        ok("(let [a (atom 0)] (while (< @a 100000) (swap! a inc)) @a)"),
        "100000"
    );
}

#[test]
fn self_tail_call() {
    assert_eq!(
        ok("(defn cnt [n acc] (if (zero? n) acc (cnt (dec n) (inc acc)))) (cnt 1000000 0)"),
        "1000000"
    );
}

#[test]
fn mutual_tail_calls() {
    assert_eq!(
        ok("(declare my-odd?)
            (defn my-even? [n] (if (zero? n) true (my-odd? (dec n))))
            (defn my-odd? [n] (if (zero? n) false (my-even? (dec n))))
            (my-even? 100000)"),
        "true"
    );
}

#[test]
fn non_tail_still_limited() {
    let e = run(
        1024 * 1024,
        LIMITS,
        "(defn f [n] (if (zero? n) 0 (inc (f (dec n))))) (f 1000000)",
    )
    .expect_err("expected a depth error");
    assert_limit(&e, ":depth");
}

#[test]
fn tail_call_inside_try_is_caught() {
    assert_eq!(
        ok("(defn boom [] (throw (ex-info \"x\" {}))) (try (boom) (catch e :caught))"),
        ":caught"
    );
}

#[test]
fn tail_call_value_never_leaks() {
    assert_eq!(ok("(pr-str ((fn [] ((fn [] 42)))))"), "\"42\"");
    assert_eq!(ok("(def x ((fn [] (identity 5)))) x"), "5");
    assert_eq!(ok("(eval '((fn [] ((fn [] 7)))))"), "7");
    assert_eq!(ok("(def y ((fn [] ((fn [] 6))))) y"), "6");
    assert_eq!(ok("[((fn [] ((fn [] 8))))]"), "[8]");
}

#[test]
fn step_limit_still_trips_on_infinite_tail_loop() {
    let limits = Limits {
        steps: Some(100_000),
        ..LIMITS
    };
    let e = run(1024 * 1024, limits, "(defn spin [] (spin)) (spin)")
        .expect_err("expected a step error");
    assert_limit(&e, ":steps");
}

#[test]
fn long_flat_list_does_not_crash_debug() {
    let r = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            let mut it = Interpreter::new();
            (
                it.eval_to_string("(count (range 200000))"),
                it.eval_to_string("(reduce + (range 200000))"),
            )
        })
        .unwrap()
        .join()
        .expect("thread overflowed or panicked");
    assert_eq!(r.0.unwrap(), "200000");
    assert_eq!(r.1.unwrap(), "19999900000");
}
