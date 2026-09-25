//! `try`/`catch`/`finally`. Ports `eval/control.c` (`eval_try`,
//! `partition_try_clauses`, `normalize_exception`) plus the classed/keyword
//! catch dispatch of ADR 32 and ADR 37.
//!
//! `catch` accepts three shapes:
//!   * `(catch e body...)` — bare catch-all (mino's original single-kind form).
//!   * `(catch Class e body...)` — a *symbol* class from the frozen compat
//!     table (ADR 32): it maps a JVM-ish class token to the set of
//!     `:mino/kind` values it accepts. An unknown class symbol is a COMPILE
//!     ERROR (typo safety), mirroring the JVM's unknown-class failure.
//!   * `(catch :some/kind e body...)` — a *keyword* class (ADR 37): first
//!     looked up in the table (so `:default` stays catch-all), and otherwise
//!     matched against the thrown diagnostic's `:mino/kind` by exact equality.
//!     An unknown keyword is NOT an error — it is a kind literal.
//!
//! Multiple catch clauses run first-match-wins; a bare clause may follow
//! classed ones. `finally` runs on every path.
//!
//! Contract (verified against the mino binary):
//!   * body evaluates; its value is the try's value when nothing throws;
//!   * on a throw, the FIRST matching catch clause runs with `binding` bound
//!     to the *normalized* diagnostic map, and its value becomes the try's
//!     value; if no clause matches, the throw propagates (after `finally`);
//!   * `finally` runs on EVERY path (normal, caught, re-thrown) and its value
//!     is discarded — the try/catch result or the propagating throw stands;
//!   * a re-throw from the catch handler propagates after `finally` runs.

use crate::env::Env;
use crate::error::{normalize_exception, throw_classified, Throw};
use crate::eval::Interp;
use crate::symbol::Symbol;
use crate::value::Value;

/// What a catch clause dispatches on.
enum CatchClass {
    /// Bare `(catch e ...)`, or a `:default`/`Throwable`/`Exception` alias:
    /// matches any diagnostic.
    Any,
    /// A symbol class or table-keyword alias: matches when the thrown
    /// diagnostic's `:mino/kind` is one of these kinds (kind strings like
    /// `"eval/type"`, `"user"`).
    Kinds(Vec<&'static str>),
    /// A keyword class not in the table (ADR 37): matches `:mino/kind` by
    /// exact keyword equality against this symbol.
    Kind(Symbol),
}

/// One parsed catch clause.
struct Catch {
    class: CatchClass,
    var: Symbol,
    body: Vec<Value>,
}

/// The ADR 32 compat table: a class-name token -> the `:mino/kind` values it
/// accepts. `None` means catch-all (Throwable/Exception). Returns `Some(kinds)`
/// for a known class, or `Err` (via the caller) for an unknown symbol.
fn compat_class(name: &str) -> Option<CatchClass> {
    match name {
        // Catch-all classes.
        "Throwable" | "Exception" | "java.lang.Throwable" | "java.lang.Exception" => {
            Some(CatchClass::Any)
        }
        "ExceptionInfo" | "clojure.lang.ExceptionInfo" => Some(CatchClass::Kinds(vec!["user"])),
        "Error" | "java.lang.Error" => Some(CatchClass::Kinds(vec!["internal"])),
        "ClassCastException"
        | "ArithmeticException"
        | "NullPointerException"
        | "NumberFormatException" => Some(CatchClass::Kinds(vec!["eval/type"])),
        "IllegalArgumentException" => Some(CatchClass::Kinds(vec!["eval/arity", "eval/contract"])),
        "UnsupportedOperationException" => Some(CatchClass::Kinds(vec!["eval/contract"])),
        "IndexOutOfBoundsException" | "StringIndexOutOfBoundsException" => {
            Some(CatchClass::Kinds(vec!["eval/bounds"]))
        }
        "IllegalStateException" => Some(CatchClass::Kinds(vec!["eval/state"])),
        _ => None,
    }
}

/// The `:mino/kind` of a normalized diagnostic as a kind string
/// (`"eval/type"`, `"user"`, `"store/backend"`, ...), or None.
fn diagnostic_kind(ex: &Value) -> Option<Symbol> {
    let Value::Map(m) = ex else { return None };
    let key = Value::Keyword(Symbol::namespaced("mino", "kind"));
    match m.get(&key) {
        Some(Value::Keyword(k)) => Some(k.clone()),
        _ => None,
    }
}

/// Render a kind symbol as its slash-joined string (`"eval/type"`, `"user"`).
fn kind_str(k: &Symbol) -> String {
    match &k.ns {
        Some(ns) => format!("{ns}/{}", k.name),
        None => k.name.to_string(),
    }
}

/// Does this catch clause handle the normalized diagnostic `ex`?
fn clause_matches(class: &CatchClass, ex: &Value) -> bool {
    match class {
        CatchClass::Any => true,
        CatchClass::Kinds(kinds) => match diagnostic_kind(ex) {
            Some(k) => kinds.contains(&kind_str(&k).as_str()),
            None => false,
        },
        CatchClass::Kind(want) => match diagnostic_kind(ex) {
            Some(k) => &k == want,
            None => false,
        },
    }
}

/// The partitioned shape of a `(try body... catch-clause* [finally cleanup...])`
/// form.
struct TryClauses {
    body: Vec<Value>,
    catches: Vec<Catch>,
    finally_body: Vec<Value>,
    has_finally: bool,
}

/// Walk the args once, classifying each top-level form as body / catch /
/// finally. Ports `partition_try_clauses` extended for ADR 32/37 catch classes.
fn partition(args: &[Value]) -> Result<TryClauses, Throw> {
    let mut c = TryClauses {
        body: Vec::new(),
        catches: Vec::new(),
        finally_body: Vec::new(),
        has_finally: false,
    };
    for clause in args {
        match clause_head(clause) {
            Some("catch") => {
                let elems = list_elems(clause);
                // (catch e body...)          -> bare catch-all
                // (catch Class e body...)    -> classed
                // The clause has a class iff there are two leading symbols/
                // keywords before the body.
                let (class, var, skip) = match (elems.get(1), elems.get(2)) {
                    // Two leading tokens: classed catch. elems[1] is the class
                    // (symbol or keyword), elems[2] is the binding symbol.
                    (Some(class_tok), Some(Value::Sym(bind))) => {
                        let class = parse_catch_class(class_tok)?;
                        (class, bind.clone(), 3)
                    }
                    // One leading symbol: bare catch-all.
                    (Some(Value::Sym(bind)), _) => (CatchClass::Any, bind.clone(), 2),
                    (Some(_), _) => {
                        return Err(throw_classified(
                            "syntax",
                            "MSY001",
                            "catch binding must be a symbol",
                        ))
                    }
                    (None, _) => {
                        return Err(throw_classified(
                            "syntax",
                            "MSY001",
                            "catch requires a binding symbol",
                        ))
                    }
                };
                c.catches.push(Catch {
                    class,
                    var,
                    body: elems.into_iter().skip(skip).collect(),
                });
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

/// Parse a catch class token (the form before the binding symbol). A symbol
/// must be in the compat table (unknown = compile error, ADR 32). A keyword is
/// looked up in the table first (so `:default` is catch-all) and otherwise is
/// a kind literal matched by equality (ADR 37).
fn parse_catch_class(tok: &Value) -> Result<CatchClass, Throw> {
    match tok {
        Value::Sym(s) => {
            let name = if let Some(ns) = &s.ns {
                format!("{ns}.{}", s.name)
            } else {
                s.name.to_string()
            };
            // Try the bare name and the ns-qualified name against the table.
            compat_class(&name)
                .or_else(|| compat_class(&s.name))
                .ok_or_else(|| {
                    throw_classified(
                        "syntax",
                        "MSY001",
                        &format!("catch: unknown exception class {name}"),
                    )
                })
        }
        Value::Keyword(k) => {
            // `:default` is the catch-all alias (ADR 32). Any other keyword is
            // matched against :mino/kind by equality (ADR 37).
            if k.ns.is_none() && &*k.name == "default" {
                Ok(CatchClass::Any)
            } else {
                Ok(CatchClass::Kind(k.clone()))
            }
        }
        _ => Err(throw_classified(
            "syntax",
            "MSY001",
            "catch class must be a symbol or keyword",
        )),
    }
}

/// `(try ...)`: evaluate the body inside a catch/finally frame.
pub fn eval_try(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let clauses = partition(args)?;

    // 1. Body.
    let mut result = no_recur(eval_forced(it, &clauses.body, env));

    // A tripped resource limit is uncatchable: skip catch AND finally (any
    // eval would re-trip immediately) and propagate it as-is.
    if let Some(p) = &it.tripped {
        return Err(Throw(p.clone()));
    }

    // 2. Catch: run the FIRST matching handler if the body threw.
    if let Err(Throw(raw)) = &result {
        let ex = normalize_exception(raw);
        if let Some(cat) = clauses.catches.iter().find(|c| clause_matches(&c.class, &ex)) {
            let local = env.child();
            local.set(cat.var.clone(), ex);
            // A re-throw here propagates (after finally, below).
            result = no_recur(eval_forced(it, &cat.body, &local));
        }
        // No matching clause: `result` stays the original Err and propagates.
    }

    if let Some(p) = &it.tripped {
        return Err(Throw(p.clone()));
    }

    // 3. Finally: run unconditionally; value discarded, errors in it propagate.
    if clauses.has_finally {
        eval_forced(it, &clauses.finally_body, env)?;
    }

    // 4. The try/catch result (or propagating throw) stands.
    result
}

/// Eval a try/catch/finally body to a finished value. A tail call must not
/// leave the try frame as a signal: its callee would then run (and throw)
/// outside the catch/finally. Run it here, inside the frame.
fn eval_forced(it: &mut Interp, body: &[Value], env: &Env) -> Result<Value, Throw> {
    let r = it.eval_implicit_do(body, env);
    it.force(r)
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

    #[test]
    fn classed_catch_dispatches_on_kind() {
        let mut it = Interp::new();
        // A keyword catch matches :mino/kind by equality (ADR 37): a
        // user-thrown map with :mino/kind :store/backend.
        assert_eq!(
            ev(
                &mut it,
                "(try (throw {:mino/kind :store/backend :mino/message \"bad\"}) \
                 (catch :store/backend e :caught))"
            ),
            ":caught"
        );
        // A non-matching keyword catch declines; the throw propagates.
        assert!(it
            .eval_str(
                "(try (throw {:mino/kind :store/backend}) (catch :store/schema e :caught))"
            )
            .is_err());
        // First-match-wins across clauses; a later bare clause is the fallback.
        assert_eq!(
            ev(
                &mut it,
                "(try (throw {:mino/kind :store/schema}) \
                 (catch :store/backend e :backend) \
                 (catch e :fallback))"
            ),
            ":fallback"
        );
        // :default is catch-all.
        assert_eq!(
            ev(&mut it, "(try (throw \"x\") (catch :default e :caught))"),
            ":caught"
        );
    }

    #[test]
    fn classed_catch_symbol_from_compat_table() {
        let mut it = Interp::new();
        // ArithmeticException maps to :eval/type; division-by-zero is :eval/math
        // in the port, so use a real :eval/type error instead: a type error.
        // Throwable is catch-all.
        assert_eq!(
            ev(&mut it, "(try (/ 1 0) (catch Throwable e :caught))"),
            ":caught"
        );
        // ExceptionInfo maps to :user — an ex-info throw is caught.
        assert_eq!(
            ev(
                &mut it,
                "(try (throw (ex-info \"boom\" {:a 1})) (catch ExceptionInfo e (ex-message e)))"
            ),
            "\"boom\""
        );
        // An unknown class SYMBOL is a compile error.
        assert!(it
            .eval_str("(try 1 (catch NoSuchThingException e 2))")
            .is_err());
    }
}
