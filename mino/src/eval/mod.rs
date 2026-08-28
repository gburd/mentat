//! Eval front door. Ports `eval/eval.c` (`eval_value`, `eval_implicit_do`) and
//! the `if`/`do`/`quote` inline handlers from `special_registry.c`.
//! Task 1.1 scope: self-eval, symbol lookup, and those three special forms.
//! Fn application (Task 1.2) and the rest of the special forms land later.

use crate::env::Env;
use crate::error::{throw_str, Throw};
use crate::reader::read_all;
use crate::value::Value;

pub mod bindings;
pub mod func;
pub mod special;

pub struct Interp {
    pub root: Env,
}

impl Interp {
    pub fn new() -> Self {
        let root = Env::root();
        crate::prim::install_core(&root);
        Interp { root }
    }

    /// Read ALL forms from `src`, eval each in the root env, return the last.
    /// Empty input yields `Nil` (mirrors mino's load/eval-string semantics).
    pub fn eval_str(&mut self, src: &str) -> Result<Value, Throw> {
        let forms = read_all(src).map_err(|e| throw_str(&format!("read error: {e:?}")))?;
        let mut last = Value::Nil;
        let env = self.root.clone();
        for form in &forms {
            last = self.eval(form, &env)?;
        }
        Ok(last)
    }

    pub fn eval(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        match form {
            // Self-evaluating: scalars plus already-built fns/prims/vars.
            Value::Nil
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Char(_)
            | Value::Str(_)
            | Value::Keyword(_)
            | Value::Fn(_)
            | Value::Prim(_)
            | Value::Var(_) => Ok(form.clone()),

            // A `recur` signal only appears here when re-evaluated as data
            // (it never occurs in source); pass it through so the loop/fn
            // trampoline sees it. Non-tail eval sites use `eval_value`, which
            // rejects it.
            Value::Recur(_) => Ok(form.clone()),

            // The empty list self-evaluates to itself (Clojure: `()` => `()`),
            // it is NOT an empty call.
            Value::EmptyList => Ok(form.clone()),

            // Collection literals evaluate their elements (Clojure semantics:
            // `[a b]` => a vector of the *values* of a and b).
            Value::Vector(v) => {
                let mut out = crate::collections::vector::PVec::empty();
                for e in v.iter() {
                    out = out.conj(self.eval_value(e, env)?);
                }
                Ok(Value::Vector(gc::Gc::new(out)))
            }
            Value::Map(m) => {
                let mut out = crate::collections::map::PMap::empty();
                for (k, val) in m.entries() {
                    let ek = self.eval_value(k, env)?;
                    let ev = self.eval_value(val, env)?;
                    out = out.assoc(ek, ev);
                }
                Ok(Value::Map(gc::Gc::new(out)))
            }
            Value::Set(s) => {
                let mut out = crate::collections::map::PSet::empty();
                for e in s.iter() {
                    out = out.conj(self.eval_value(e, env)?);
                }
                Ok(Value::Set(gc::Gc::new(out)))
            }

            Value::Sym(sym) => env
                .get(sym)
                .ok_or_else(|| throw_str(&format!("Unable to resolve symbol: {sym}"))),

            Value::Cons(_) => self.eval_list(form, env),
        }
    }

    /// Evaluate `form` for its value at a NON-tail position: a stray `recur`
    /// (`Value::Recur`) here is an error ("recur must be in tail position").
    /// Ports mino's `eval_value`.
    pub fn eval_value(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        let v = self.eval(form, env)?;
        if matches!(v, Value::Recur(_)) {
            return Err(throw_str("recur must be in tail position"));
        }
        Ok(v)
    }

    fn eval_list(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        let (head, rest) = match form {
            Value::Cons(cell) => (&cell.0, &cell.1),
            _ => unreachable!("eval_list called on non-cons"),
        };

        if let Value::Sym(sym) = head {
            if sym.ns.is_none() {
                match &*sym.name {
                    "if" => return self.eval_if(rest, env),
                    "do" => return self.eval_do(rest, env),
                    "quote" => return self.eval_quote(rest),
                    "def" => {
                        let args = collect(rest);
                        return special::eval_def(self, &args, env);
                    }
                    "fn" | "fn*" => {
                        let args = collect(rest);
                        return special::eval_fn(self, &args, env);
                    }
                    "let" | "let*" => {
                        let args = collect(rest);
                        return bindings::eval_let(self, &args, env);
                    }
                    "loop" | "loop*" => {
                        let args = collect(rest);
                        return bindings::eval_loop(self, &args, env);
                    }
                    "letfn*" => {
                        let args = collect(rest);
                        return bindings::eval_letfn_star(self, &args, env);
                    }
                    "recur" => {
                        // Eval args at non-tail (they must be values), then
                        // return the recur signal for the loop/fn trampoline.
                        let mut vals = Vec::new();
                        let mut cur = rest;
                        while let Value::Cons(cell) = cur {
                            vals.push(self.eval_value(&cell.0, env)?);
                            cur = &cell.1;
                        }
                        return Ok(Value::Recur(gc::Gc::new(vals)));
                    }
                    _ => {}
                }
            }
        }

        // Application: eval the head, then args left-to-right, then apply.
        let callee = self.eval_value(head, env)?;
        if !matches!(callee, Value::Fn(_) | Value::Prim(_)) {
            return Err(throw_str(&format!(
                "not callable: {}",
                crate::printer::print_str(head)
            )));
        }
        let mut args = Vec::new();
        let mut cur = rest;
        while let Value::Cons(cell) = cur {
            args.push(self.eval_value(&cell.0, env)?);
            cur = &cell.1;
        }
        func::apply(self, &callee, &args)
    }

    /// `(if cond then else?)`: eval cond, then-branch when truthy else
    /// else-branch; a missing else yields nil.
    fn eval_if(&mut self, args: &Value, env: &Env) -> Result<Value, Throw> {
        let (cond, tail) = pop(args);
        let cond = cond.ok_or_else(|| throw_str("if: too few forms"))?;
        let (then_form, tail) = pop(&tail);
        let then_form = then_form.ok_or_else(|| throw_str("if: too few forms"))?;
        let (else_form, _) = pop(&tail);

        // Condition is a value (non-tail); branches are tail positions and
        // may legitimately produce a `recur` signal, so use plain `eval`.
        if self.eval_value(&cond, env)?.is_truthy() {
            self.eval(&then_form, env)
        } else {
            match else_form {
                Some(e) => self.eval(&e, env),
                None => Ok(Value::Nil),
            }
        }
    }

    /// `(do e1 e2 ... en)`: eval each, return the last; empty yields nil.
    /// Ports `eval_implicit_do`. Non-last forms are values (reject `recur`);
    /// the last form is tail position (propagates a `recur` signal).
    pub fn eval_implicit_do(&mut self, body: &[Value], env: &Env) -> Result<Value, Throw> {
        let Some((last, init)) = body.split_last() else {
            return Ok(Value::Nil);
        };
        for form in init {
            self.eval_value(form, env)?;
        }
        self.eval(last, env)
    }

    /// The `(do ...)` special form: body is the cons tail.
    fn eval_do(&mut self, body: &Value, env: &Env) -> Result<Value, Throw> {
        let forms = collect(body);
        self.eval_implicit_do(&forms, env)
    }

    /// `(quote x)`: return x unevaluated.
    fn eval_quote(&mut self, args: &Value) -> Result<Value, Throw> {
        match args {
            Value::Cons(cell) => Ok(cell.0.clone()),
            _ => Ok(Value::Nil),
        }
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

/// Split a cons list into (first?, rest). Returns `(None, Nil)` at the end.
fn pop(list: &Value) -> (Option<Value>, Value) {
    match list {
        Value::Cons(cell) => (Some(cell.0.clone()), cell.1.clone()),
        _ => (None, Value::Nil),
    }
}

/// Collect a proper cons list's elements into a Vec (special-form args).
fn collect(list: &Value) -> Vec<Value> {
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
    use super::*;
    use crate::printer::print_str;

    #[test]
    fn eval_core_forms() {
        let mut it = Interp::new();
        assert_eq!(print_str(&it.eval_str("42").unwrap()), "42");
        assert_eq!(print_str(&it.eval_str("(quote (a b))").unwrap()), "(a b)");
        assert_eq!(print_str(&it.eval_str("(if true 1 2)").unwrap()), "1");
        assert_eq!(print_str(&it.eval_str("(if nil 1 2)").unwrap()), "2");
        assert_eq!(print_str(&it.eval_str("(do 1 2 3)").unwrap()), "3");
    }

    #[test]
    fn eval_edge_cases_match_oracle() {
        let mut it = Interp::new();
        // (if nil 1) -> nil, (do) -> nil, empty input -> nil
        assert_eq!(print_str(&it.eval_str("(if nil 1)").unwrap()), "nil");
        assert_eq!(print_str(&it.eval_str("(do)").unwrap()), "nil");
        assert_eq!(print_str(&it.eval_str("").unwrap()), "nil");
        assert_eq!(print_str(&it.eval_str("(if true 1)").unwrap()), "1");
        // unbound symbol throws
        assert!(it.eval_str("nope").is_err());
    }

    /// The empty list `()` is a distinct value from `nil` (mino's
    /// MINO_EMPTY_LIST). Every expected value below is copied verbatim from
    /// the `mino -e '(pr-str ...)'` oracle.
    #[test]
    fn empty_list_distinct_from_nil() {
        let mut it = Interp::new();
        let ev = |it: &mut Interp, s: &str| print_str(&it.eval_str(s).unwrap());

        // Reading/printing: () self-evaluates and prints "()", not "nil".
        assert_eq!(ev(&mut it, "()"), "()");
        assert_eq!(ev(&mut it, "'()"), "()");
        assert_eq!(ev(&mut it, "(list)"), "()");

        // Equality: () != nil, () == (), () == [], (list) == ().
        assert_eq!(ev(&mut it, "(= () nil)"), "false");
        assert_eq!(ev(&mut it, "(= () ())"), "true");
        assert_eq!(ev(&mut it, "(= () [])"), "true");
        assert_eq!(ev(&mut it, "(= (list) ())"), "true");

        // Predicates: nil? true only for nil; seq?/list? true for the empty
        // list but false for nil; empty? true for both () and nil.
        assert_eq!(ev(&mut it, "(nil? ())"), "false");
        assert_eq!(ev(&mut it, "(nil? nil)"), "true");
        assert_eq!(ev(&mut it, "(seq? ())"), "true");
        assert_eq!(ev(&mut it, "(list? ())"), "true");
        assert_eq!(ev(&mut it, "(seq? nil)"), "false");
        assert_eq!(ev(&mut it, "(list? nil)"), "false");
        assert_eq!(ev(&mut it, "(empty? ())"), "true");
        assert_eq!(ev(&mut it, "(empty? nil)"), "true");
        assert_eq!(ev(&mut it, "(empty? [1])"), "false");

        // seq on empty -> nil; on non-empty -> a seq.
        assert_eq!(ev(&mut it, "(seq ())"), "nil");
        assert_eq!(ev(&mut it, "(seq nil)"), "nil");
        assert_eq!(ev(&mut it, "(seq [1])"), "(1)");

        // first on empty/nil -> nil.
        assert_eq!(ev(&mut it, "(first ())"), "nil");
        assert_eq!(ev(&mut it, "(first nil)"), "nil");

        // rest ALWAYS returns a seq (the empty list), never nil.
        assert_eq!(ev(&mut it, "(rest ())"), "()");
        assert_eq!(ev(&mut it, "(rest nil)"), "()");
        assert_eq!(ev(&mut it, "(rest (list 1))"), "()");
        assert_eq!(ev(&mut it, "(rest [1])"), "()");

        // next returns nil when there is no more.
        assert_eq!(ev(&mut it, "(next (list 1))"), "nil");
        assert_eq!(ev(&mut it, "(next ())"), "nil");
        assert_eq!(ev(&mut it, "(next nil)"), "nil");

        // cons onto nil / () / a list all make proper lists.
        assert_eq!(ev(&mut it, "(cons 1 nil)"), "(1)");
        assert_eq!(ev(&mut it, "(cons 1 ())"), "(1)");
        assert_eq!(ev(&mut it, "(cons 1 (list 2))"), "(1 2)");

        // count of both empties is 0.
        assert_eq!(ev(&mut it, "(count ())"), "0");
        assert_eq!(ev(&mut it, "(count nil)"), "0");

        // conj on nil or () makes a one-element list.
        assert_eq!(ev(&mut it, "(conj nil 1)"), "(1)");
        assert_eq!(ev(&mut it, "(conj () 1)"), "(1)");

        // () is truthy (only nil and false are falsy).
        assert_eq!(ev(&mut it, "(if () :t :f)"), ":t");
    }
}
