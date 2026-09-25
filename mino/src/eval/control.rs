//! `try`/`catch`/`finally`. Ports `eval/control.c` (`eval_try`,
//! `partition_try_clauses`, `normalize_exception`).
//!
//! mino's `catch` takes NO exception type/class — `(catch binding handler...)`
//! catches ALL exceptions unconditionally (documented DEVIATION in control.c:
//! mino has a single exception kind, a diagnostic map, not a class hierarchy).
//!
//! Contract (verified against the mino binary):
//!   * body evaluates; its value is the try's value when nothing throws;
//!   * on a throw, the (single) catch clause runs with `binding` bound to the
//!     *normalized* diagnostic map, and its value becomes the try's value;
//!   * `finally` runs on EVERY path (normal, caught, re-thrown) and its value
//!     is discarded — the try/catch result or the propagating throw stands;
//!   * a re-throw from the catch handler propagates after `finally` runs.

use crate::env::Env;
use crate::error::{normalize_exception, throw_classified, Throw};
use crate::eval::Interp;
use crate::value::Value;

/// The partitioned shape of a `(try body... [catch e handler...]
/// [finally cleanup...])` form.
struct TryClauses {
    body: Vec<Value>,
    catch_var: Option<crate::symbol::Symbol>,
    catch_body: Vec<Value>,
    finally_body: Vec<Value>,
    has_catch: bool,
    has_finally: bool,
}

/// Walk the args once, classifying each top-level form as body / catch /
/// finally. Ports `partition_try_clauses`.
fn partition(args: &[Value]) -> Result<TryClauses, Throw> {
    let mut c = TryClauses {
        body: Vec::new(),
        catch_var: None,
        catch_body: Vec::new(),
        finally_body: Vec::new(),
        has_catch: false,
        has_finally: false,
    };
    for clause in args {
        match clause_head(clause) {
            Some("catch") => {
                // (catch binding handler...) — no type argument.
                let elems = list_elems(clause);
                let var = match elems.get(1) {
                    Some(Value::Sym(s)) => s.clone(),
                    Some(_) => {
                        return Err(throw_classified(
                            "syntax",
                            "MSY001",
                            "catch binding must be a symbol",
                        ))
                    }
                    None => {
                        return Err(throw_classified(
                            "syntax",
                            "MSY001",
                            "catch requires a binding symbol",
                        ))
                    }
                };
                c.catch_var = Some(var);
                c.catch_body = elems.into_iter().skip(2).collect();
                c.has_catch = true;
            }
            Some("finally") => {
                c.finally_body = list_elems(clause).into_iter().skip(1).collect();
                c.has_finally = true;
            }
            _ => c.body.push(clause.clone()),
        }
    }
    Ok(c)
}

/// `(try ...)`: evaluate the body inside a catch/finally frame.
pub fn eval_try(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let clauses = partition(args)?;

    // 1. Body.
    let mut result = no_recur(it.eval_implicit_do(&clauses.body, env));

    // A tripped resource limit is uncatchable: skip catch AND finally (any
    // eval would re-trip immediately) and propagate it as-is.
    if let Some(p) = &it.tripped {
        return Err(Throw(p.clone()));
    }

    // 2. Catch: run the handler if the body threw.
    if clauses.has_catch {
        if let Err(Throw(raw)) = result {
            let ex = normalize_exception(&raw);
            let local = env.child();
            if let Some(var) = &clauses.catch_var {
                local.set(var.clone(), ex);
            }
            // A re-throw here propagates (after finally, below).
            result = no_recur(it.eval_implicit_do(&clauses.catch_body, &local));
        }
    }

    if let Some(p) = &it.tripped {
        return Err(Throw(p.clone()));
    }

    // 3. Finally: run unconditionally; value discarded, errors in it propagate.
    if clauses.has_finally {
        it.eval_implicit_do(&clauses.finally_body, env)?;
    }

    // 4. The try/catch result (or propagating throw) stands.
    result
}

/// A `recur` signal cannot cross a try frame (the unwind machinery would be
/// skipped); mino raises "cannot recur across try". Convert any `Recur` result
/// into that error.
fn no_recur(r: Result<Value, Throw>) -> Result<Value, Throw> {
    match r {
        Ok(Value::Recur(_)) => Err(throw_classified(
            "syntax",
            "MSY001",
            "cannot recur across try",
        )),
        other => other,
    }
}

/// Head symbol name of a `(sym ...)` clause, or None for a body form.
fn clause_head(v: &Value) -> Option<&'static str> {
    if let Value::Cons(cell) = v {
        if let Value::Sym(s) = &cell.0 {
            if s.ns.is_none() {
                return match &*s.name {
                    "catch" => Some("catch"),
                    "finally" => Some("finally"),
                    _ => None,
                };
            }
        }
    }
    None
}

/// Collect a proper cons list's elements.
fn list_elems(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = v;
    while let Value::Cons(cell) = cur {
        out.push(cell.0.clone());
        cur = &cell.1;
    }
    out
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    /// Every expected value below is copied verbatim from the mino binary
    /// (`mino -e '(EXPR)'`).
    fn ev(it: &mut Interp, s: &str) -> String {
        print_str(&it.eval_str(s).unwrap())
    }

    #[test]
    fn try_normal_returns_body() {
        let mut it = Interp::new();
        assert_eq!(ev(&mut it, "(try 7)"), "7");
        assert_eq!(ev(&mut it, "(try 1 2 3)"), "3");
        // finally runs but its value is discarded.
        assert_eq!(ev(&mut it, "(try 1 (finally 99))"), "1");
    }

    #[test]
    fn catch_catches_and_binds() {
        let mut it = Interp::new();
        // catch binds the exception; handler value becomes the try value.
        assert_eq!(ev(&mut it, "(try (throw \"x\") (catch e 42))"), "42");
        // A thrown keyword is normalized: caught as a diagnostic map whose
        // :mino/data is the keyword and :mino/message is "uncaught...".
        assert_eq!(
            ev(&mut it, "(try (throw :boom) (catch e (ex-data e)))"),
            ":boom"
        );
        // A thrown string: message is the string.
        assert_eq!(
            ev(&mut it, "(try (throw \"boom\") (catch e (ex-message e)))"),
            "\"boom\""
        );
    }

    #[test]
    fn ex_info_roundtrip() {
        let mut it = Interp::new();
        // ex-info builds {:message :data}.
        assert_eq!(
            ev(&mut it, "(ex-info \"boom\" {:a 1})"),
            "{:message \"boom\", :data {:a 1}}"
        );
        // Thrown + caught: ex-message/ex-data unwrap through the diagnostic.
        assert_eq!(
            ev(
                &mut it,
                "(try (throw (ex-info \"boom\" {:a 1})) (catch e (ex-message e)))"
            ),
            "\"boom\""
        );
        assert_eq!(
            ev(
                &mut it,
                "(try (throw (ex-info \"boom\" {:a 1})) (catch e (ex-data e)))"
            ),
            "{:a 1}"
        );
        // Directly (not thrown) on the ex-info map.
        assert_eq!(ev(&mut it, "(ex-message (ex-info \"hi\" {}))"), "\"hi\"");
        assert_eq!(ev(&mut it, "(ex-data (ex-info \"hi\" {:k 1}))"), "{:k 1}");
    }

    #[test]
    fn finally_runs_on_both_paths() {
        // No atoms yet: prove finally runs (and ordering) via def'd side effects
        // observed after the try. `def` mutates the root env; we read it back.
        let mut it = Interp::new();
        // Normal path: finally sets marker to :ran.
        it.eval_str("(def m1 :init)").unwrap();
        ev(&mut it, "(try 1 (finally (def m1 :ran)))");
        assert_eq!(ev(&mut it, "m1"), ":ran");
        // Throw+catch path: finally still runs.
        it.eval_str("(def m2 :init)").unwrap();
        ev(
            &mut it,
            "(try (throw \"x\") (catch e :caught) (finally (def m2 :ran)))",
        );
        assert_eq!(ev(&mut it, "m2"), ":ran");
    }

    #[test]
    fn nested_and_rethrow() {
        let mut it = Interp::new();
        // Nested try: inner catch handles, outer never fires.
        assert_eq!(
            ev(
                &mut it,
                "(try (try (throw \"inner\") (catch e (ex-message e))) (catch e2 \"outer\"))"
            ),
            "\"inner\""
        );
        // Rethrow from a catch propagates to the enclosing try.
        assert_eq!(
            ev(
                &mut it,
                "(try (try (throw \"a\") (catch e (throw \"b\"))) (catch e2 (ex-message e2)))"
            ),
            "\"b\""
        );
    }

    #[test]
    fn runtime_errors_are_catchable() {
        let mut it = Interp::new();
        // division by zero.
        assert_eq!(
            ev(&mut it, "(try (/ 1 0) (catch e (ex-message e)))"),
            "\"division by zero\""
        );
        // unbound symbol.
        assert_eq!(
            ev(&mut it, "(try undefined-thing (catch e (ex-message e)))"),
            "\"unbound symbol: undefined-thing\""
        );
        // rethrow when no catch: propagates out as an Err.
        assert!(it.eval_str("(try (throw \"x\") (finally 1))").is_err());
    }
}
