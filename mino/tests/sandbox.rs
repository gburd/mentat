//! Sandbox + resource-limit tests for the host embedding API. A separate test
//! binary (= separate process), so a pre-fix stack overflow only aborts this.

use mino_rs::embed::Limits;
use mino_rs::Interpreter;
use std::time::{Duration, Instant};

fn err_of(it: &mut Interpreter, src: &str) -> String {
    match it.eval_to_string(src) {
        Ok(v) => panic!("expected an error from {src}, got {v}"),
        Err(e) => e,
    }
}

fn assert_limit(e: &str, which: &str) {
    assert!(e.contains(":eval/limit"), "not a limit error: {e}");
    assert!(e.contains(which), "expected {which} in: {e}");
}

const MB: u64 = 1024 * 1024;

#[test]
fn sandboxed_has_no_filesystem() {
    let mut it = Interpreter::sandboxed();
    for src in [
        "(slurp \"/etc/passwd\")",
        "(spit \"/tmp/mino-sandbox-spit\" \"x\")",
        "(rm-rf \"/tmp/mino-sandbox-nope\")",
        "(mkdir-p \"/tmp/mino-sandbox-dir\")",
        "(file-exists? \"/\")",
    ] {
        let e = err_of(&mut it, src);
        assert!(e.contains("unbound symbol"), "{src} => {e}");
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("mino-sandbox-{}-{nanos}", std::process::id()));
    let p = path.display().to_string();
    let e = err_of(&mut it, &format!("(mino.store/open \"{p}\")"));
    assert!(e.contains("sandbox"), "{e}");
    assert!(!path.exists());
    assert!(!std::path::Path::new(&format!("{p}.wal")).exists());
    assert_eq!(it.eval_to_string("(+ 1 2)").unwrap(), "3");
    assert!(it.eval("(mino.store/open)").is_ok());
}

#[test]
fn sandboxed_captures_print() {
    let mut it = Interpreter::sandboxed();
    it.eval("(println \"hi\")").unwrap();
    assert_eq!(it.take_output(), "hi\n");
    assert_eq!(it.take_output(), "");
}

#[test]
fn step_limit_stops_infinite_loop() {
    let mut it = Interpreter::sandboxed();
    it.set_limits(Limits { steps: Some(100_000), ..Default::default() });
    assert_limit(&err_of(&mut it, "(loop [] (recur))"), ":steps");
}

#[test]
fn limit_cannot_be_caught() {
    let mut it = Interpreter::sandboxed();
    it.set_limits(Limits { steps: Some(100_000), ..Default::default() });
    let e = err_of(&mut it, "(loop [] (try (loop [] (recur)) (catch e nil)) (recur))");
    assert_limit(&e, ":steps");
    let e = err_of(&mut it, "(try (loop [] (recur)) (catch e :caught))");
    assert!(!e.contains(":caught"));
    assert_limit(&e, ":steps");
}

#[test]
fn limits_reset_per_top_level_eval() {
    let mut it = Interpreter::sandboxed();
    // Calibrate: count how many steps a fixed loop takes, then budget so each
    // run uses ~60% of it.
    let src = "(loop [i 0] (if (< i 2000) (recur (inc i)) i))";
    it.set_limits(Limits { steps: Some(u64::MAX), ..Default::default() });
    it.eval(src).unwrap();
    let used = it.interp().steps;
    it.set_limits(Limits { steps: Some(used * 10 / 6), ..Default::default() });
    assert_eq!(it.eval_to_string(src).unwrap(), "2000");
    assert_eq!(it.eval_to_string(src).unwrap(), "2000");
}

#[test]
fn check_hook_can_abort() {
    use std::cell::Cell;
    use std::rc::Rc;
    let calls = Rc::new(Cell::new(0u32));
    let c = calls.clone();
    let mut it = Interpreter::sandboxed();
    it.set_check_hook(Box::new(move || {
        c.set(c.get() + 1);
        if c.get() > 3 {
            Err(mino_rs::error::throw_str("cancelled by host"))
        } else {
            Ok(())
        }
    }));
    let e = err_of(&mut it, "(loop [i 0] (if (< i 10000000) (recur (inc i)) i))");
    assert!(e.contains("cancelled by host"), "{e}");
    calls.set(0);
    let e = err_of(&mut it, "(try (loop [] (recur)) (catch e :caught))");
    assert!(e.contains("cancelled by host"), "{e}");
}

#[test]
fn heap_limit_stops_one_step_allocation() {
    let mut it = Interpreter::sandboxed();
    it.set_limits(Limits { heap_bytes: Some(8 * MB), ..Default::default() });
    for src in [
        "(range 100000000000)",
        "(count (vec (range 100000000)))",
        "(apply str (repeat 100000000 \"x\"))",
    ] {
        let t = Instant::now();
        let e = err_of(&mut it, src);
        assert_limit(&e, ":heap");
        assert!(t.elapsed() < Duration::from_secs(2), "{src} took {:?}", t.elapsed());
    }
}

#[test]
fn depth_limit_turns_eval_recursion_into_an_error() {
    let h = std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let mut it = Interpreter::sandboxed();
            it.set_limits(Limits { depth: Some(500), ..Default::default() });
            it.eval_to_string("(defn f [n] (if (zero? n) 0 (inc (f (dec n))))) (f 1000000)")
        })
        .unwrap();
    let r = h.join().expect("thread overflowed or panicked");
    let e = r.expect_err("expected a depth error");
    assert_limit(&e, ":depth");
}

#[test]
fn limits_leave_ordinary_scripts_alone() {
    let mut it = Interpreter::sandboxed();
    it.set_limits(Limits { steps: Some(100_000), heap_bytes: Some(8 * MB), depth: Some(500) });
    assert_eq!(it.eval_to_string("(reduce + (range 1000))").unwrap(), "499500");
    assert_eq!(
        it.eval_to_string("(loop [i 0] (if (< i 10000) (recur (inc i)) i))").unwrap(),
        "10000"
    );
    assert_eq!(
        it.eval_to_string("(defn g [n] (if (zero? n) 0 (inc (g (dec n))))) (g 100)").unwrap(),
        "100"
    );
}

#[test]
fn new_still_has_host_access() {
    let mut it = Interpreter::new();
    assert!(it.eval("(file-exists? \"/\")").is_ok());
}

#[test]
fn new_has_no_limits_by_default() {
    let mut it = Interpreter::new();
    assert_eq!(
        it.eval_to_string("(loop [i 0] (if (< i 5000000) (recur (inc i)) i))").unwrap(),
        "5000000"
    );
}
