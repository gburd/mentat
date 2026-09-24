//! `fn` closures, multi-arity dispatch, and application.
//! Ports `src/eval/fn.c` (`make_fn`, `build_multi_arity_clauses`,
//! `find_arity_clause`, `apply_callable`).

use crate::env::Env;
use crate::error::{throw_str, Throw};
use crate::eval::Interp;
use crate::symbol::Symbol;
use crate::value::Value;
use gc::{Finalize, Gc, Trace};

/// One arity clause: the raw parameter vector (so full destructuring works),
/// its fixed-arg count, whether it is variadic (`& rest`), and the body.
#[derive(Trace, Finalize, Clone)]
pub struct Arity {
    pub params: Value,
    pub fixed: usize,
    pub variadic: bool,
    pub body: Vec<Value>,
}

/// A user function: one or more arity clauses closed over `env`.
#[derive(Trace, Finalize)]
pub struct Closure {
    pub arities: Vec<Arity>,
    pub env: Env,
    pub name: Option<Symbol>,
    // A macro is a fn flagged as macro (mino: the MINO_MACRO value type). The
    // evaluator EXPANDS a macro call with unevaluated argument forms instead
    // of calling it as a function. defmacro sets this true.
    pub is_macro: bool,
}

/// Build a `Closure` from the args of an `(fn ...)` form.
/// Supports `(fn [a b] body...)`, `(fn [a & rest] ..)`, and multi-arity
/// `(fn ([a] ..) ([a b] ..))`. An optional leading name symbol
/// (`(fn name [a] ..)`) is captured for self-reference/printing.
pub fn make_fn(args: &[Value], env: &Env) -> Result<Value, Throw> {
    let mut idx = 0;
    let name = match args.first() {
        Some(Value::Sym(s)) => {
            idx = 1;
            Some(s.clone())
        }
        _ => None,
    };
    let rest = &args[idx..];
    if rest.is_empty() {
        return Err(throw_str("fn requires a parameter list"));
    }

    let arities = match &rest[0] {
        // Single arity: (fn [params] body...)
        Value::Vector(_) => vec![parse_arity(&rest[0], &rest[1..])?],
        // Multi-arity: each remaining form is a clause ([params] body...)
        Value::Cons(_) => {
            let mut arities = Vec::new();
            for clause in rest {
                let items = list_to_vec(clause);
                if items.is_empty() {
                    return Err(throw_str("fn arity clause requires a parameter list"));
                }
                arities.push(parse_arity(&items[0], &items[1..])?);
            }
            arities
        }
        _ => return Err(throw_str("fn requires a parameter list")),
    };

    Ok(Value::Fn(Gc::new(Closure {
        arities,
        env: env.clone(),
        name,
        is_macro: false,
    })))
}

/// Parse one param vector + body into an `Arity`, recording the fixed-arg
/// count and variadic flag for arity dispatch. The full param vector is kept
/// so destructuring patterns (`[a [b c]]`, `{:keys [x]}`) bind correctly.
fn parse_arity(params: &Value, body: &[Value]) -> Result<Arity, Throw> {
    let items = match params {
        Value::Vector(v) => v.iter().cloned().collect::<Vec<Value>>(),
        _ => return Err(throw_str("fn parameter list must be a vector")),
    };
    let mut fixed = 0usize;
    let mut variadic = false;
    for p in &items {
        if matches!(p, Value::Sym(s) if s.ns.is_none() && &*s.name == "&") {
            variadic = true;
            break;
        }
        fixed += 1;
    }
    Ok(Arity {
        params: params.clone(),
        fixed,
        variadic,
        body: body.to_vec(),
    })
}

/// Apply a callable to already-evaluated `args`.
pub fn apply(it: &mut Interp, callee: &Value, args: &[Value]) -> Result<Value, Throw> {
    match callee {
        Value::Prim(p) => (p.0)(it, args),
        Value::PrimClosure(p) => (p.f)(it, args),
        Value::Fn(closure) => apply_closure(it, closure, callee, args),
        // Keywords and symbols are callable as map-lookup fns:
        // `(:k m)` / `('s m)` => (get m callee default?). Ports mino's
        // IFn-on-keyword/symbol behavior (used by juxt :a :b, ('inc m), ...).
        Value::Keyword(_) | Value::Sym(_) => {
            let coll = args.first().cloned().unwrap_or(Value::Nil);
            let default = args.get(1).cloned().unwrap_or(Value::Nil);
            crate::prim::collections::get(it, &[coll, callee.clone(), default])
        }
        // Maps/sets/vectors are callable as lookup fns: `(m k d)`, `(s x)`,
        // `(v i)`. Ports mino's IFn-on-collection behavior.
        Value::Map(_) | Value::Set(_) => {
            let key = args.first().cloned().unwrap_or(Value::Nil);
            let default = args.get(1).cloned().unwrap_or(Value::Nil);
            crate::prim::collections::get(it, &[callee.clone(), key, default])
        }
        Value::Vector(_) => {
            let idx = args.first().cloned().unwrap_or(Value::Nil);
            crate::prim::collections::nth(it, &[callee.clone(), idx])
        }
        // A Var is callable: it invokes its resolved root value (Clojure:
        // `((resolve 'f) ...)` / `(#'f ...)`).
        Value::Var(sym) => match it.var_value(sym) {
            Some(v) => apply(it, &v, args),
            None => Err(crate::error::throw_classified(
                "eval/type",
                "MTY002",
                &format!("cannot call unbound var: {sym}"),
            )),
        },
        _ => Err(crate::error::throw_classified(
            "eval/type",
            "MTY002",
            &format!("not a function (got {})", crate::eval::type_tag_of(callee)),
        )),
    }
}

fn apply_closure(
    it: &mut Interp,
    closure: &Closure,
    callee: &Value,
    args: &[Value],
) -> Result<Value, Throw> {
    // Pick the arity whose fixed count matches exactly, else a variadic one
    // whose fixed count the args can cover.
    let arity = closure
        .arities
        .iter()
        .find(|a| !a.variadic && a.fixed == args.len())
        .or_else(|| {
            closure
                .arities
                .iter()
                .find(|a| a.variadic && args.len() >= a.fixed)
        })
        .ok_or_else(|| {
            throw_str(&format!(
                "wrong number of args ({}) passed to fn",
                args.len()
            ))
        })?;

    // Recur trampoline: bind params, run the body; if the body produced a
    // `Recur` signal, rebind this arity's params to the new args and iterate
    // in this Rust `for`-free loop — constant stack, no recursive Rust call.
    let mut cur_args: Vec<Value> = args.to_vec();
    loop {
        let frame = closure.env.child();
        // Named fn: bind the self-name in the body scope so recursive calls
        // like (fn f [n] ... (f ...)) resolve (mino binds the fn name in its
        // own frame). Anonymous fns skip this.
        if let Some(name) = &closure.name {
            frame.set(name.clone(), callee.clone());
        }
        crate::eval::bindings::bind_params(
            it,
            &frame,
            &arity.params,
            &cur_args,
            crate::eval::bindings::Ctx::Fn,
        )?;
        let result = it.eval_implicit_do(&arity.body, &frame)?;
        match &result {
            Value::Recur(new_args) => {
                // recur re-enters this arity's param loop with new args.
                cur_args = (**new_args).clone();
            }
            _ => return Ok(result),
        }
    }
}

/// Collect a proper cons list into a Vec (stops at nil / improper tail).
fn list_to_vec(list: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = list;
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

    #[test]
    fn fn_def_and_plus() {
        let mut it = Interp::new();
        assert_eq!(print_str(&it.eval_str("(+ 1 2 3)").unwrap()), "6");
        it.eval_str("(def inc (fn [x] (+ x 1)))").unwrap();
        assert_eq!(print_str(&it.eval_str("(inc 41)").unwrap()), "42");
        it.eval_str("(def add (fn ([a] a) ([a b] (+ a b))))").unwrap();
        assert_eq!(print_str(&it.eval_str("(add 5)").unwrap()), "5");
        assert_eq!(print_str(&it.eval_str("(add 5 6)").unwrap()), "11");
    }

    #[test]
    fn variadic_rest_binds_a_list() {
        let mut it = Interp::new();
        // oracle: ((fn [a & r] r) 1 2 3) => (2 3), a list
        assert_eq!(
            print_str(&it.eval_str("((fn [a & r] r) 1 2 3)").unwrap()),
            "(2 3)"
        );
        // oracle: empty rest reads as nil
        assert_eq!(print_str(&it.eval_str("((fn [a & r] r) 1)").unwrap()), "nil");
    }

    #[test]
    fn numeric_equality_is_type_sensitive() {
        let mut it = Interp::new();
        // oracle: (= 1 1.0) => false  (Clojure = distinguishes int/float)
        assert_eq!(print_str(&it.eval_str("(= 1 1.0)").unwrap()), "false");
        assert_eq!(print_str(&it.eval_str("(= 1 1)").unwrap()), "true");
        assert_eq!(print_str(&it.eval_str("(= 1 2)").unwrap()), "false");
    }

    #[test]
    fn division_is_exact() {
        let mut it = Interp::new();
        // oracle: (/ 6 2) => 3 (exact int), (/ 2.0 4) => 0.5, (- 10 3 2) => 5,
        // (/ 7 2) => 7/2 (exact ratio, Phase 5.5).
        assert_eq!(print_str(&it.eval_str("(/ 6 2)").unwrap()), "3");
        assert_eq!(print_str(&it.eval_str("(/ 2.0 4)").unwrap()), "0.5");
        assert_eq!(print_str(&it.eval_str("(- 10 3 2)").unwrap()), "5");
        assert_eq!(print_str(&it.eval_str("(/ 7 2)").unwrap()), "7/2");
    }

    #[test]
    fn comparisons_are_chained() {
        let mut it = Interp::new();
        // oracle: (< 1 2 3) => true
        assert_eq!(print_str(&it.eval_str("(< 1 2 3)").unwrap()), "true");
        assert_eq!(print_str(&it.eval_str("(< 1 3 2)").unwrap()), "false");
        assert_eq!(print_str(&it.eval_str("(> 3 2 1)").unwrap()), "true");
        assert_eq!(print_str(&it.eval_str("(<= 1 1 2)").unwrap()), "true");
        assert_eq!(print_str(&it.eval_str("(>= 3 3 1)").unwrap()), "true");
        // oracle: 0/1 args -> true
        assert_eq!(print_str(&it.eval_str("(<)").unwrap()), "true");
        assert_eq!(print_str(&it.eval_str("(< 1)").unwrap()), "true");
    }

    #[test]
    fn def_returns_a_var() {
        let mut it = Interp::new();
        // oracle: (def x 5) => #'user/x
        assert_eq!(print_str(&it.eval_str("(def x 5)").unwrap()), "#'user/x");
        assert_eq!(print_str(&it.eval_str("x").unwrap()), "5");
    }

    #[test]
    fn named_fn_self_recursion() {
        let mut it = Interp::new();
        // oracle: ((fn fact [n] (if (< n 2) 1 (* n (fact (dec n))))) 5) => 120
        assert_eq!(
            print_str(
                &it.eval_str("((fn fact [n] (if (< n 2) 1 (* n (fact (dec n))))) 5)")
                    .unwrap()
            ),
            "120"
        );
    }
}
