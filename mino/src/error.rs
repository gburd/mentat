//! Thrown values + the structured error payload model.
//!
//! Ports the throw/error payload model from `runtime/error.c` +
//! `eval/control.c`. Eval returns `Result<Value, Throw>`; `Throw` carries the
//! *raw* thrown Clojure value (mino stores it in `try_stack[].exception`).
//! When a `catch` clause runs, [`normalize_exception`] reshapes it into the
//! diagnostic map a catch binding sees — mirroring control.c's
//! `normalize_exception`.
//!
//! Diagnostic map shape (matches the mino binary):
//! ```clojure
//! {:mino/kind KIND, :mino/code CODE, :mino/phase :eval,
//!  :mino/message MSG, :mino/data DATA}
//! ```
//! `KIND` is a keyword (`:user`, `:name`, `:eval/type`, `:eval/arity`, ...),
//! `CODE` a string (`"MNS001"`, `"MTY001"`, ...). We omit `:mino/location`:
//! the reader does not yet carry source spans (Phase 4), and no Phase-3
//! conformance test asserts on it.

use crate::collections::map::PMap;
use crate::printer::print_str;
use crate::symbol::Symbol;
use crate::value::Value;
use gc::Gc;
use std::fmt;

/// A thrown Clojure value. Holds the raw payload; `normalize_exception`
/// reshapes it for the catch binding.
pub struct Throw(pub Value);

/// Wrap a string message as a thrown `Str` value. Kept for internal callers;
/// a bare string, when caught, is normalized to a `:user`/`MUS001` diagnostic
/// whose `:mino/message` is the string (matching `(throw "msg")` in mino).
pub fn throw_str(msg: &str) -> Throw {
    Throw(Value::Str(Gc::new(msg.to_string())))
}

/// A keyword `Value` for `:name`-style namespaced/plain keys.
fn kw(name: &str) -> Value {
    Value::Keyword(Symbol::plain(name))
}

fn kw_ns(ns: &str, name: &str) -> Value {
    Value::Keyword(Symbol::namespaced(ns, name))
}

fn str_val(s: &str) -> Value {
    Value::Str(Gc::new(s.to_string()))
}

/// Build a diagnostic map `{:mino/kind KIND :mino/code CODE :mino/phase :eval
/// :mino/message MSG :mino/data DATA}`. `kind` is a keyword name, possibly
/// namespaced ("eval/type" -> `:eval/type`).
fn diag_map(kind: &str, code: &str, msg: &str, data: Value) -> Value {
    let kind_kw = match kind.split_once('/') {
        Some((ns, n)) => kw_ns(ns, n),
        None => kw(kind),
    };
    let m = PMap::empty()
        .assoc(kw_ns("mino", "kind"), kind_kw)
        .assoc(kw_ns("mino", "code"), str_val(code))
        .assoc(kw_ns("mino", "phase"), kw("eval"))
        .assoc(kw_ns("mino", "message"), str_val(msg))
        .assoc(kw_ns("mino", "data"), data);
    Value::Map(Gc::new(m))
}

/// A classified runtime error, thrown as a structured diagnostic map so a
/// `catch` binding + `ex-message`/`ex-data` can inspect it. Mirrors mino's
/// `prim_throw_classified` / `set_eval_diag`. `data` defaults to `nil`.
pub fn throw_classified(kind: &str, code: &str, msg: &str) -> Throw {
    Throw(diag_map(kind, code, msg, Value::Nil))
}

/// A `:eval/limit` throw for data nested deeper than
/// [`crate::depth::MAX_DATA_DEPTH`] while printing. Data is `{:limit :nesting}`
/// so a `catch`/`ex-data` can tell it apart from the step/heap/depth budgets.
pub fn throw_nesting_limit() -> Throw {
    let data = PMap::empty().assoc(kw("limit"), kw("nesting"));
    Throw(diag_map(
        "eval/limit",
        "MLM001",
        "print: data nested too deep",
        Value::Map(Gc::new(data)),
    ))
}

/// The payload of a tripped resource limit (steps/heap/depth, or a host
/// check-hook abort): `{:mino/kind :eval/limit :mino/code "MLM001"
/// :mino/message MSG :mino/data {:limit :WHICH :value N}}`. Matches upstream
/// mino's diag kind "limit" / code MLM001.
pub fn limit_diag(msg: &str, which: &str, value: u64) -> Value {
    let data = PMap::empty()
        .assoc(kw("limit"), kw(which))
        .assoc(kw("value"), Value::Int(value.min(i64::MAX as u64) as i64));
    diag_map("eval/limit", "MLM001", msg, Value::Map(Gc::new(data)))
}

/// The `:mino/message` a catch clause would see for this thrown value.
pub fn message_of(ex: &Value) -> String {
    match &normalize_exception(ex) {
        Value::Map(m) => match m.get(&kw_ns("mino", "message")) {
            Some(Value::Str(s)) => (**s).clone(),
            _ => print_str(ex),
        },
        _ => print_str(ex),
    }
}

/// Normalize a raw thrown value into the diagnostic map a `catch` binding
/// sees. Ports `normalize_exception` in eval/control.c:
///   * a map already carrying `:mino/kind` passes through unchanged;
///   * a string -> `:user`/`MUS001`, message = the string, data = the string;
///   * a map without `:mino/kind` (e.g. ex-info's `{:message :data}`) ->
///     `:user`/`MUS001`, message = its `:message` if a string else
///     "uncaught exception", data = the whole map;
///   * any other value -> message "uncaught exception: <pr-str>", data = it;
///   * nil -> message "uncaught exception: nil".
pub fn normalize_exception(ex: &Value) -> Value {
    let kind_key = kw_ns("mino", "kind");
    if let Value::Map(m) = ex {
        if m.contains(&kind_key) {
            return ex.clone();
        }
    }
    let (msg, data) = match ex {
        Value::Str(s) => ((**s).clone(), ex.clone()),
        Value::Map(m) => {
            let msg = match m.get(&kw("message")) {
                Some(Value::Str(s)) => (**s).clone(),
                _ => "uncaught exception".to_string(),
            };
            (msg, ex.clone())
        }
        Value::Nil => ("uncaught exception: nil".to_string(), Value::Nil),
        other => (
            format!("uncaught exception: {}", print_str(other)),
            other.clone(),
        ),
    };
    diag_map("user", "MUS001", &msg, data)
}

impl fmt::Display for Throw {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&print_str(&self.0))
    }
}

impl fmt::Debug for Throw {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Throw({})", print_str(&self.0))
    }
}
