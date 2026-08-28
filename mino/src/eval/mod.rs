//! Eval front door. Ports `eval/eval.c` (`eval_value`, `eval_implicit_do`) and
//! the `if`/`do`/`quote` inline handlers from `special_registry.c`.
//! Task 1.1 scope: self-eval, symbol lookup, and those three special forms.
//! Fn application (Task 1.2) and the rest of the special forms land later.

use crate::env::Env;
use crate::error::{throw_str, Throw};
use crate::reader::read_all;
use crate::value::Value;

pub struct Interp {
    pub root: Env,
}

impl Interp {
    pub fn new() -> Self {
        Interp { root: Env::root() }
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
            // Self-evaluating scalars.
            Value::Nil
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Char(_)
            | Value::Str(_)
            | Value::Keyword(_) => Ok(form.clone()),

            // Phase 2: eval collection elements (needs fn calls first).
            Value::Vector(_) | Value::Map(_) | Value::Set(_) => Ok(form.clone()),

            Value::Sym(sym) => env
                .get(sym)
                .ok_or_else(|| throw_str(&format!("Unable to resolve symbol: {sym}"))),

            Value::Cons(_) => self.eval_list(form, env),
        }
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
                    "do" => return self.eval_implicit_do(rest, env),
                    "quote" => return self.eval_quote(rest),
                    _ => {}
                }
            }
        }

        // Task 1.2 adds fn/prim application here.
        Err(throw_str(&format!(
            "unknown form / not callable: {}",
            crate::printer::print_str(form)
        )))
    }

    /// `(if cond then else?)`: eval cond, then-branch when truthy else
    /// else-branch; a missing else yields nil.
    fn eval_if(&mut self, args: &Value, env: &Env) -> Result<Value, Throw> {
        let (cond, tail) = pop(args);
        let cond = cond.ok_or_else(|| throw_str("if: too few forms"))?;
        let (then_form, tail) = pop(&tail);
        let then_form = then_form.ok_or_else(|| throw_str("if: too few forms"))?;
        let (else_form, _) = pop(&tail);

        if self.eval(&cond, env)?.is_truthy() {
            self.eval(&then_form, env)
        } else {
            match else_form {
                Some(e) => self.eval(&e, env),
                None => Ok(Value::Nil),
            }
        }
    }

    /// `(do e1 e2 ... en)`: eval each, return the last; empty yields nil.
    /// Ports `eval_implicit_do`.
    fn eval_implicit_do(&mut self, body: &Value, env: &Env) -> Result<Value, Throw> {
        let mut cur = body;
        let mut last = Value::Nil;
        while let Value::Cons(cell) = cur {
            last = self.eval(&cell.0, env)?;
            cur = &cell.1;
        }
        Ok(last)
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
}
