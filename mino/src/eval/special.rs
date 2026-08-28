//! Special-form handlers beyond the core three (`if`/`do`/`quote`).
//! Ports `src/eval/defs.c` (`def`) and `src/eval/fn.c` (`fn`).

use crate::env::Env;
use crate::error::{throw_str, Throw};
use crate::eval::func;
use crate::eval::Interp;
use crate::symbol::Symbol;
use crate::value::Value;

/// `(def name)` / `(def name value)`. Binds `name` in the root/current-ns env
/// and returns the var (`#'user/name`), matching mino's Clojure semantics.
/// (Namespaces arrive in Phase 4; until then the current ns is `user`.)
pub fn eval_def(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let name = match args.first() {
        Some(Value::Sym(s)) if s.ns.is_none() => s.clone(),
        Some(_) => return Err(throw_str("def name must be a symbol")),
        None => return Err(throw_str("def requires a name")),
    };
    // (def name value): eval and bind. (def name): declaration only.
    if let Some(value_form) = args.get(1) {
        let value = it.eval(value_form, env)?;
        it.root.set(name.clone(), value);
    }
    Ok(Value::Var(Symbol::namespaced("user", &name.name)))
}

/// `(fn ...)` / `(fn* ...)`: build a closure over the calling env.
pub fn eval_fn(_it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    func::make_fn(args, env)
}

/// `(defmacro name [params] body...)` / multi-arity. Ports `eval_defmacro`
/// (defs.c): builds a fn flagged as a macro and binds it under `name`.
/// Optional docstring and attr-map between the name and the params are
/// skipped (the port does not track var metadata yet).
pub fn eval_defmacro(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let name = match args.first() {
        Some(Value::Sym(s)) if s.ns.is_none() => s.clone(),
        Some(_) => return Err(throw_str("defmacro name must be a symbol")),
        None => return Err(throw_str("defmacro requires a name, parameters, and body")),
    };
    let mut rest = &args[1..];
    // Optional docstring, then optional attr-map (skip both).
    if matches!(rest.first(), Some(Value::Str(_))) && rest.len() > 1 {
        rest = &rest[1..];
    }
    if matches!(rest.first(), Some(Value::Map(_))) && rest.len() > 1 {
        rest = &rest[1..];
    }
    if rest.is_empty() {
        return Err(throw_str("defmacro requires a name, parameters, and body"));
    }
    // Reuse fn parsing: `rest` is either `[params] body...` (single arity) or
    // `([params] body...) ...` (multi-arity), exactly what make_fn accepts.
    let mac = match func::make_fn(rest, env)? {
        Value::Fn(ref closure) => {
            let c = &**closure;
            Value::Fn(gc::Gc::new(func::Closure {
                arities: c.arities.clone(),
                env: c.env.clone(),
                name: Some(name.clone()),
                is_macro: true,
            }))
        }
        _ => unreachable!("make_fn returns a Fn"),
    };
    it.root.set(name, mac.clone());
    Ok(mac)
}
