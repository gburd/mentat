//! Destructuring + `let` / `loop` / `recur` / `letfn*`. Ports
//! `src/eval/bindings.c` (`bind_form`, `bind_vec_destructure`,
//! `bind_map_destructure`, `bind_params`, `eval_let`, `eval_loop`,
//! `eval_letfn_star`).
//!
//! `recur` is signalled by `Value::Recur` (mino's MINO_RECUR): the `recur`
//! special form evaluates its args and returns that sentinel; `eval_loop` and
//! the fn trampoline (`func::apply_closure`) run their body, and if the result
//! is a `Recur` they rebind the loop/fn locals and iterate in a Rust `for`
//! loop — constant stack, no recursive Rust call. Any `Recur` reaching a
//! non-tail eval site is rejected by `Interp::eval_value` as
//! "recur must be in tail position".

use crate::env::Env;
use crate::error::{throw_str, Throw};
use crate::eval::Interp;
use crate::symbol::Symbol;
use crate::value::Value;
use gc::Gc;

/// Binding context, mirroring the C `ctx` string. Controls strict-arity:
/// `Fn`/`Recur` reject surplus args, `Let`/`Loop` tolerate them.
#[derive(Clone, Copy, PartialEq)]
pub enum Ctx {
    Let,
    Loop,
    Fn,
    Recur,
}

impl Ctx {
    fn is_strict(self) -> bool {
        matches!(self, Ctx::Fn | Ctx::Recur)
    }
    /// Nested patterns always nil-fill and tolerate extras (recur's exact
    /// arity applies only to the top-level slot count).
    fn nested(self) -> Ctx {
        if self == Ctx::Recur {
            Ctx::Loop
        } else {
            self
        }
    }
}

/// Is `sym` the plain symbol `name` (no namespace)?
fn sym_is(v: &Value, name: &str) -> bool {
    matches!(v, Value::Sym(s) if s.ns.is_none() && &*s.name == name)
}

/// Is `v` the keyword `:name`?
fn kw_is(v: &Value, name: &str) -> bool {
    matches!(v, Value::Keyword(k) if k.ns.is_none() && &*k.name == name)
}

/// Bind a single (already-interned) symbol. `_` binds nothing observable but
/// mino still binds it (the reader gives it as a normal symbol) — we do too.
fn bind_sym(env: &Env, sym: &Symbol, val: Value) {
    env.set(sym.clone(), val);
}

/// Recursive destructuring binder: symbol -> direct, vector -> positional,
/// map -> associative. `nil` pattern binds nothing (mino's `bind_sym(NULL)`).
pub fn bind_form(
    it: &mut Interp,
    env: &Env,
    pattern: &Value,
    val: Value,
    ctx: Ctx,
) -> Result<(), Throw> {
    match pattern {
        Value::Sym(s) => {
            bind_sym(env, s, val);
            Ok(())
        }
        Value::Vector(_) => bind_vec_destructure(it, env, pattern, val, ctx.nested()),
        Value::Map(_) => bind_map_destructure(it, env, pattern, val, ctx.nested()),
        _ => Err(throw_str(
            "unsupported binding form (expected symbol, vector, or map)",
        )),
    }
}

/// Reify a sequential value into a `Vec<Value>` for positional walking:
/// vector -> its elements, list -> its elements, nil/empty -> empty. Anything
/// else (a scalar) yields an empty list so slots nil-fill.
fn seq_elems(val: &Value) -> Vec<Value> {
    match val {
        Value::Vector(v) => v.iter().cloned().collect(),
        Value::Cons(_) => {
            let mut out = Vec::new();
            let mut cur = val;
            while let Value::Cons(cell) = cur {
                out.push(cell.0.clone());
                cur = &cell.1;
            }
            out
        }
        _ => Vec::new(),
    }
}

/// Build a proper list from a slice tail (for `& rest`): empty -> `nil`
/// (matching mino, where a `let [[a & r] [1]]` binds `r` to nil), else a
/// cons chain terminating in EmptyList.
fn tail_list(items: &[Value]) -> Value {
    if items.is_empty() {
        return Value::Nil;
    }
    let mut acc = Value::EmptyList;
    for v in items.iter().rev() {
        acc = Value::Cons(Gc::new((v.clone(), acc)));
    }
    acc
}

/// Positional (vector) destructuring: `[a b]`, `[a b & rest]`, `[a [b c]]`,
/// `[a b :as all]`, `[_ b]`. `val` may be any seqable (list/vector/nil).
fn bind_vec_destructure(
    it: &mut Interp,
    env: &Env,
    pattern: &Value,
    val: Value,
    ctx: Ctx,
) -> Result<(), Throw> {
    let pats: Vec<Value> = match pattern {
        Value::Vector(v) => v.iter().cloned().collect(),
        _ => return Err(throw_str("vector destructure requires a vector pattern")),
    };
    let args = seq_elems(&val);
    let plen = pats.len();
    let mut ai = 0usize; // cursor into args
    let mut i = 0usize; // cursor into pats
    while i < plen {
        let p = &pats[i];
        if sym_is(p, "&") {
            if i + 1 >= plen {
                return Err(throw_str("& must be followed by a binding form"));
            }
            i += 1;
            let rest_pat = pats[i].clone();
            bind_form(it, env, &rest_pat, tail_list(&args[ai.min(args.len())..]), ctx)?;
            // Optional `:as sym` after the rest binding.
            if i + 1 < plen && kw_is(&pats[i + 1], "as") {
                if i + 2 >= plen {
                    return Err(throw_str(":as must be followed by a symbol"));
                }
                i += 2;
                match &pats[i] {
                    Value::Sym(s) => bind_sym(env, s, val.clone()),
                    _ => return Err(throw_str(":as must be followed by a symbol")),
                }
            }
            if i + 1 < plen {
                return Err(throw_str("unexpected forms after & binding"));
            }
            return Ok(());
        }
        if kw_is(p, "as") {
            if i + 1 >= plen {
                return Err(throw_str(":as must be followed by a symbol"));
            }
            i += 1;
            match &pats[i] {
                Value::Sym(s) => bind_sym(env, s, val.clone()),
                _ => return Err(throw_str(":as must be followed by a symbol")),
            }
            i += 1;
            continue;
        }
        // Normal positional slot.
        if ai >= args.len() {
            if ctx == Ctx::Recur {
                return Err(throw_str(&format!(
                    "recur expects {plen} args, got {i}"
                )));
            }
            let pat = p.clone();
            bind_form(it, env, &pat, Value::Nil, ctx)?;
        } else {
            let pat = p.clone();
            let a = args[ai].clone();
            bind_form(it, env, &pat, a, ctx)?;
            ai += 1;
        }
        i += 1;
    }
    // Strict-arity contexts reject surplus args; let/loop ignore the tail.
    if ai < args.len() && ctx.is_strict() {
        return Err(throw_str(&format!(
            "wrong number of args (more than {plen}) passed"
        )));
    }
    Ok(())
}

/// Associative (map) destructuring: `{a :a}`, `{:keys [x y]}`, `{:strs [..]}`,
/// `{:syms [..]}`, `{a :a :or {a 0}}`, `{:keys [x] :as m}`. Nested patterns in
/// the key position (`{[a b] :pair}`) recurse.
fn bind_map_destructure(
    it: &mut Interp,
    env: &Env,
    pattern: &Value,
    val: Value,
    ctx: Ctx,
) -> Result<(), Throw> {
    let pmap = match pattern {
        Value::Map(m) => m,
        _ => return Err(throw_str("map destructure requires a map pattern")),
    };

    // Only a real map has lookupable keys; nil / other -> empty (defaults apply).
    let val = match &val {
        Value::Map(_) => val,
        _ => Value::Nil,
    };
    let val_get = |k: &Value| -> Option<Value> {
        if let Value::Map(m) = &val {
            m.get(k).cloned()
        } else {
            None
        }
    };

    // First pass: collect :keys / :strs / :syms / :or / :as, and process
    // explicit `{pattern key}` bindings INLINE (matching mino's single loop:
    // an explicit binding only sees an :or default if :or was iterated
    // earlier in the pattern map's insertion order). :keys/:strs/:syms are
    // applied after the loop, so they always see :or.
    let mut keys_vec: Option<Value> = None;
    let mut strs_vec: Option<Value> = None;
    let mut syms_vec: Option<Value> = None;
    let mut or_map: Option<Value> = None;
    let mut as_sym: Option<Symbol> = None;

    // Snapshot the pattern entries so we can borrow `it` mutably in the loop.
    let pentries: Vec<(Value, Value)> = pmap.entries().map(|(k, v)| (k.clone(), v.clone())).collect();

    // Helper closures capture-free: implemented inline below.
    for (pkey, pval) in &pentries {
        match pkey {
            Value::Keyword(k) if k.ns.is_none() && &*k.name == "keys" => {
                keys_vec = Some(pval.clone())
            }
            Value::Keyword(k) if k.ns.is_none() && &*k.name == "strs" => {
                strs_vec = Some(pval.clone())
            }
            Value::Keyword(k) if k.ns.is_none() && &*k.name == "syms" => {
                syms_vec = Some(pval.clone())
            }
            Value::Keyword(k) if k.ns.is_none() && &*k.name == "or" => {
                or_map = Some(pval.clone())
            }
            Value::Keyword(k) if k.ns.is_none() && &*k.name == "as" => match pval {
                Value::Sym(s) => as_sym = Some(s.clone()),
                _ => return Err(throw_str(":as must be followed by a symbol")),
            },
            Value::Sym(_) | Value::Vector(_) | Value::Map(_) => {
                // Explicit `{pattern key-expr}`: if the value position is a
                // symbol it is evaluated (a lookup expression); self-eval
                // forms pass through. :or applies only to leaf symbols, and
                // only when :or has already been collected (order-sensitive).
                let is_leaf = matches!(pkey, Value::Sym(_));
                let lookup_key = match pval {
                    Value::Sym(_) => it.eval_value(pval, env)?,
                    other => other.clone(),
                };
                let mut found = val_get(&lookup_key);
                if found.is_none() && is_leaf {
                    if let Some(Value::Map(m)) = &or_map {
                        if let Some(deflt) = m.get(pkey) {
                            let d = deflt.clone();
                            found = Some(it.eval_value(&d, env)?);
                        }
                    }
                }
                bind_form(it, env, pkey, found.unwrap_or(Value::Nil), ctx)?;
            }
            _ => {}
        }
    }

    let or_get = |it: &mut Interp, sym: &Value| -> Result<Option<Value>, Throw> {
        // :or defaults are keyed by the leaf symbol; the default form is
        // evaluated in the surrounding env.
        if let Some(Value::Map(m)) = &or_map {
            if let Some(deflt) = m.get(sym) {
                let d = deflt.clone();
                return Ok(Some(it.eval_value(&d, env)?));
            }
        }
        Ok(None)
    };

    // :keys [a b] -> look up :a :b.
    if let Some(Value::Vector(kv)) = keys_vec.as_ref().map(|v| v.clone()).as_ref() {
        for ksym in kv.iter() {
            let Value::Sym(s) = ksym else {
                return Err(throw_str(":keys elements must be symbols"));
            };
            let key = Value::Keyword(Symbol::plain(&s.name));
            let mut found = val_get(&key);
            if found.is_none() {
                found = or_get(it, ksym)?;
            }
            bind_sym(env, s, found.unwrap_or(Value::Nil));
        }
    }

    // :strs [a b] -> look up "a" "b".
    if let Some(Value::Vector(sv)) = strs_vec.as_ref().map(|v| v.clone()).as_ref() {
        for ssym in sv.iter() {
            let Value::Sym(s) = ssym else {
                return Err(throw_str(":strs elements must be symbols"));
            };
            let key = Value::Str(Gc::new(s.name.to_string()));
            let mut found = val_get(&key);
            if found.is_none() {
                found = or_get(it, ssym)?;
            }
            bind_sym(env, s, found.unwrap_or(Value::Nil));
        }
    }

    // :syms [a b] -> look up 'a 'b.
    if let Some(Value::Vector(sv)) = syms_vec.as_ref().map(|v| v.clone()).as_ref() {
        for ssym in sv.iter() {
            let Value::Sym(s) = ssym else {
                return Err(throw_str(":syms elements must be symbols"));
            };
            let key = Value::Sym(Symbol::plain(&s.name));
            let mut found = val_get(&key);
            if found.is_none() {
                found = or_get(it, ssym)?;
            }
            bind_sym(env, s, found.unwrap_or(Value::Nil));
        }
    }

    // :as sym -> whole map.
    if let Some(s) = as_sym {
        bind_sym(env, &s, val.clone());
    }
    Ok(())
}

/// Bind an `fn` parameter vector to already-evaluated args, with full
/// destructuring. Shared by `func::apply_closure`. `ctx` is `Fn` (strict
/// arity) here.
pub fn bind_params(
    it: &mut Interp,
    env: &Env,
    params: &Value,
    args: &[Value],
    ctx: Ctx,
) -> Result<(), Throw> {
    // Reuse the vector-destructure path: args become a list value.
    bind_vec_destructure(it, env, params, tail_list(args), ctx)
}

// --- let / loop / recur / letfn* special forms ---

/// Split a binding vector into `[pat val pat val ...]` pairs. Errors on odd
/// length. Returns the pattern/value pairs in order.
fn binding_pairs(bindings: &Value, what: &str) -> Result<Vec<(Value, Value)>, Throw> {
    let items: Vec<Value> = match bindings {
        Value::Vector(v) => v.iter().cloned().collect(),
        _ => {
            return Err(throw_str(&format!(
                "{what} requires a vector binding form"
            )))
        }
    };
    if items.len() % 2 != 0 {
        return Err(throw_str(&format!(
            "{what} vector bindings must have even number of forms"
        )));
    }
    Ok(items.chunks(2).map(|c| (c[0].clone(), c[1].clone())).collect())
}

/// `(let [pat val ...] body...)`: sequential binding (later values see earlier
/// bindings), body is an implicit do. Each binding layers a fresh child env so
/// closures capture the env that existed before the shadow (Clojure-correct).
pub fn eval_let(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let Some(bindings) = args.first() else {
        return Err(throw_str("let requires a binding form and body"));
    };
    let mut local = env.child();
    for (pat, val_form) in binding_pairs(bindings, "let")? {
        let v = it.eval_value(&val_form, &local)?;
        let next = local.child();
        bind_form(it, &next, &pat, v, Ctx::Let)?;
        local = next;
    }
    it.eval_implicit_do(&args[1..], &local)
}

/// `(loop [pat init ...] body...)` with `(recur v ...)` in tail position.
/// Runs the body in a Rust loop: a `Recur` result rebinds the loop locals in a
/// fresh child env and iterates — constant stack.
pub fn eval_loop(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let Some(bindings) = args.first() else {
        return Err(throw_str("loop requires a binding form and body"));
    };
    let pairs = binding_pairs(bindings, "loop")?;
    // Bind the initial iteration in a child env (sequential, like let).
    let mut local = env.child();
    for (pat, init_form) in &pairs {
        let v = it.eval_value(init_form, &local)?;
        bind_form(it, &local, pat, v, Ctx::Loop)?;
    }
    let patterns: Vec<Value> = pairs.iter().map(|(p, _)| p.clone()).collect();
    loop {
        let result = it.eval_implicit_do(&args[1..], &local)?;
        let new_args: Vec<Value> = match &result {
            Value::Recur(a) => (**a).clone(),
            other => return Ok(other.clone()),
        };
        // Fresh env per iteration so closures over recur slots keep their frame.
        let next = env.child();
        if new_args.len() != patterns.len() {
            return Err(throw_str(&format!(
                "recur expects {} args, got {}",
                patterns.len(),
                new_args.len()
            )));
        }
        for (pat, v) in patterns.iter().zip(new_args.iter()) {
            bind_form(it, &next, pat, v.clone(), Ctx::Recur)?;
        }
        local = next;
    }
}

/// `(letfn* [name fn-form ...] body...)`: pre-bind names to nil in one child
/// env, evaluate each fn form in it (so closures share the scope), rebind to
/// the fn. Supports mutual recursion.
pub fn eval_letfn_star(it: &mut Interp, args: &[Value], env: &Env) -> Result<Value, Throw> {
    let Some(bindings) = args.first() else {
        return Err(throw_str("letfn* requires a binding vector and body"));
    };
    let pairs = binding_pairs(bindings, "letfn*")?;
    let local = env.child();
    for (sym, _) in &pairs {
        let Value::Sym(s) = sym else {
            return Err(throw_str("letfn* binding names must be symbols"));
        };
        local.set(s.clone(), Value::Nil);
    }
    for (sym, fn_form) in &pairs {
        let Value::Sym(s) = sym else { unreachable!() };
        let f = it.eval_value(fn_form, &local)?;
        local.set(s.clone(), f);
    }
    it.eval_implicit_do(&args[1..], &local)
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    fn ev(it: &mut Interp, s: &str) -> String {
        print_str(&it.eval_str(s).unwrap())
    }

    #[test]
    fn let_sequential_and_shadow() {
        let mut it = Interp::new();
        assert_eq!(ev(&mut it, "(let [x 5] x)"), "5");
        assert_eq!(ev(&mut it, "(let [x 1 y (+ x 10)] y)"), "11");
        // shadow: inner let doesn't leak.
        it.eval_str("(def a 1)").unwrap();
        assert_eq!(ev(&mut it, "(let [a 99] a)"), "99");
        assert_eq!(ev(&mut it, "a"), "1");
    }

    #[test]
    fn seq_and_nested_and_rest_and_as_destructure() {
        let mut it = Interp::new();
        // basic + rest (binary: [1 2 (3 4)]).
        assert_eq!(ev(&mut it, "(let [[a b & r] [1 2 3 4]] [a b r])"), "[1 2 (3 4)]");
        // nested (binary: [1 2 3]).
        assert_eq!(ev(&mut it, "(let [[a [b c]] [1 [2 3]]] [a b c])"), "[1 2 3]");
        // :as binds the whole (binary: [1 2 [1 2 3]]).
        assert_eq!(ev(&mut it, "(let [[a b :as all] [1 2 3]] [a b all])"), "[1 2 [1 2 3]]");
        // ignore slot (binary: 2).
        assert_eq!(ev(&mut it, "(let [[_ b] [1 2]] b)"), "2");
        // short vector nil-fills (binary: [1 nil nil]).
        assert_eq!(ev(&mut it, "(let [[a b c] [1]] [a b c])"), "[1 nil nil]");
        // from a list, and from nil.
        assert_eq!(ev(&mut it, "(let [[a b] (list 1 2 3)] [a b])"), "[1 2]");
        assert_eq!(ev(&mut it, "(let [[a b] nil] [a b])"), "[nil nil]");
        // empty rest is nil (binary: nil).
        assert_eq!(ev(&mut it, "(let [[a & r] [1]] r)"), "nil");
    }

    #[test]
    fn map_destructure() {
        let mut it = Interp::new();
        // :keys (binary: [1 2]).
        assert_eq!(ev(&mut it, "(let [{:keys [x y]} {:x 1 :y 2}] [x y])"), "[1 2]");
        // :keys + :or default when key absent (binary: 9).
        assert_eq!(ev(&mut it, "(let [{:keys [a] :or {a 9}} {}] a)"), "9");
        assert_eq!(ev(&mut it, "(let [{:keys [a] :or {a 9}} {:a 5}] a)"), "5");
        // explicit {sym :key} (binary: nil for :or on explicit — matches C).
        assert_eq!(ev(&mut it, "(let [{a :a :or {a 9}} {}] a)"), "nil");
        assert_eq!(ev(&mut it, "(let [{a :a} {:a 7}] a)"), "7");
        // :strs and :syms.
        assert_eq!(ev(&mut it, r#"(let [{:strs [x]} {"x" 5}] x)"#), "5");
        assert_eq!(ev(&mut it, "(let [{:syms [x]} {(quote x) 7}] x)"), "7");
        // :as binds the whole map (binary: [1 {:x 1, :y 2}]).
        assert_eq!(ev(&mut it, "(let [{:keys [x] :as m} {:x 1 :y 2}] [x m])"), "[1 {:x 1, :y 2}]");
        // nested vector pattern in map value (binary: [1 2]).
        assert_eq!(ev(&mut it, "(let [{[a b] :pair} {:pair [1 2]}] [a b])"), "[1 2]");
        // nested map pattern inside a vector (binary: 9).
        assert_eq!(ev(&mut it, "(let [[{:keys [x]}] [{:x 9}]] x)"), "9");
        // map from nil (binary: nil).
        assert_eq!(ev(&mut it, "(let [{:keys [x]} nil] x)"), "nil");
    }

    #[test]
    fn loop_recur_sum() {
        let mut it = Interp::new();
        // binary: 10.
        assert_eq!(
            ev(&mut it, "(loop [i 0 acc 0] (if (< i 5) (recur (inc i) (+ acc i)) acc))"),
            "10"
        );
    }

    #[test]
    fn fn_recur_deep_no_overflow() {
        let mut it = Interp::new();
        // 100k iterations must not overflow the Rust stack (binary: :done).
        assert_eq!(
            ev(&mut it, "((fn f [n] (if (zero? n) :done (recur (dec n)))) 100000)"),
            ":done"
        );
    }

    #[test]
    fn fn_param_destructure() {
        let mut it = Interp::new();
        // binary: 7.
        assert_eq!(ev(&mut it, "((fn [[a b]] (+ a b)) [3 4])"), "7");
    }

    #[test]
    fn recur_arity_and_position_errors() {
        let mut it = Interp::new();
        // recur with wrong arity in a loop throws.
        assert!(it.eval_str("(loop [a 1 b 2] (recur 1))").is_err());
        // recur in non-tail position throws.
        assert!(it.eval_str("(+ 1 (recur 1))").is_err());
    }
}
