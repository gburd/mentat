//! Atom primitives: `atom`, `deref`/`@`, `reset!`, `swap!`, `swap-vals!`,
//! `reset-vals!`, `compare-and-set!`, `atom?`, `add-watch`, `remove-watch`,
//! `set-validator!`, `get-validator`. Ports the atom subset of
//! `src/prim/stateful.c`.
//!
//! The port is single-threaded (see the plan's scope exclusions: no
//! async/agents/STM), so the C file's multi-threaded CAS retry loops collapse
//! to a plain read-compute-store: `swap!` reads the current value, applies the
//! fn, validates, stores, and notifies watches. `compare-and-set!` compares by
//! identity (pointer/heap identity via `identical?`-style eq is unavailable
//! here, so it uses value equality `eq_val`, matching mino's older behavior and
//! the atom_test corpus, which never exercises the CAS-identity edge).

use crate::collections::hashing::eq_val;
use crate::collections::map::PMap;
use crate::error::{throw_classified, Throw};
use crate::eval::func::apply;
use crate::eval::Interp;
use crate::value::Value;
use gc::{Finalize, Gc, GcCell, Trace};

/// The mutable interior of an atom. Ports the `as.atom` union arm: current
/// value + optional validator fn + a watches map (key -> fn). `None` meta is
/// tracked separately on the `AtomState` (mino stores meta on the cell too).
#[derive(Trace, Finalize)]
pub struct AtomState {
    pub val: Value,
    pub validator: Option<Value>,
    /// Watches keyed by an arbitrary value; the value is the callback fn.
    /// A `PMap` so keys iterate in insertion order (matches mino's key_order
    /// walk when notifying).
    pub watches: PMap,
    /// Metadata map attached via the `:meta` option / `alter-meta!`.
    pub meta: Option<Gc<PMap>>,
}

fn atom_cell(v: &Value) -> Option<&Gc<GcCell<AtomState>>> {
    match v {
        Value::Atom(a) => Some(a),
        _ => None,
    }
}

/// Validate `new_val` against the atom's validator. `Ok(())` on success, an
/// error Throw if the validator returns falsy or itself throws. Ports
/// `atom_validate`.
fn validate(it: &mut Interp, validator: &Option<Value>, new_val: &Value) -> Result<(), Throw> {
    if let Some(vfn) = validator {
        let ok = apply(it, vfn, &[new_val.clone()])?;
        if !ok.is_truthy() {
            return Err(throw_classified(
                "eval/contract",
                "MCT001",
                "Invalid reference state",
            ));
        }
    }
    Ok(())
}

/// Notify watches after a state change: each `(fn key atom old new)`.
/// Ports `atom_notify_watches`. A watch that throws propagates.
fn notify_watches(
    it: &mut Interp,
    atom: &Value,
    watches: &PMap,
    old: &Value,
    new: &Value,
) -> Result<(), Throw> {
    // Snapshot the (key, fn) pairs first so a watch that mutates the atom's
    // watch map mid-iteration can't invalidate the walk.
    let pairs: Vec<(Value, Value)> = watches
        .entries()
        .map(|(k, f)| (k.clone(), f.clone()))
        .collect();
    for (key, fnv) in pairs {
        apply(it, &fnv, &[key, atom.clone(), old.clone(), new.clone()])?;
    }
    Ok(())
}

/// Validate, commit, notify. Ports `atom_set`. The store happens between
/// validation and notification, so a watch sees the new committed value.
fn atom_set(it: &mut Interp, atom: &Value, new_val: Value) -> Result<(), Throw> {
    let cell = atom_cell(atom).unwrap();
    let (old, validator, watches) = {
        let st = cell.borrow();
        (
            st.val.clone(),
            st.validator.clone(),
            st.watches.clone_shallow_pub(),
        )
    };
    validate(it, &validator, &new_val)?;
    cell.borrow_mut().val = new_val.clone();
    notify_watches(it, atom, &watches, &old, &new_val)
}

/// `(atom x)` / `(atom x :validator f :meta m)` — build an atom. The validator
/// (if any) is run against the initial value at construction time (mino
/// rejects a bad initial value up front). Ports `prim_atom`.
pub fn atom(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let Some(initial) = args.first() else {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "atom requires one argument",
        ));
    };
    let mut validator: Option<Value> = None;
    let mut meta: Option<Gc<PMap>> = None;
    let mut rest = &args[1..];
    while rest.len() >= 2 {
        if let Value::Keyword(k) = &rest[0] {
            match &*k.name {
                "validator" => {
                    if !matches!(rest[1], Value::Nil) {
                        validator = Some(rest[1].clone());
                    }
                }
                "meta" => match &rest[1] {
                    Value::Map(m) => meta = Some(m.clone()),
                    Value::Nil => meta = None,
                    _ => {
                        return Err(throw_classified(
                            "eval/type",
                            "MTY001",
                            "atom: :meta value must be a map or nil",
                        ))
                    }
                },
                _ => {} // Unknown keys tolerated (Clojure does the same).
            }
        }
        rest = &rest[2..];
    }
    if !rest.is_empty() {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "atom: option key requires a value",
        ));
    }
    // Reject a bad initial value up front (Clojure contract).
    if let Some(vfn) = &validator {
        let ok = apply(it, vfn, &[initial.clone()])?;
        if !ok.is_truthy() {
            return Err(throw_classified(
                "eval/contract",
                "MCT001",
                "Invalid reference state",
            ));
        }
    }
    Ok(Value::Atom(Gc::new(GcCell::new(AtomState {
        val: initial.clone(),
        validator,
        watches: PMap::empty(),
        meta,
    }))))
}

/// `(deref ref)` / `@ref` — the current value. Only atoms are supported now;
/// delays/futures/vars route elsewhere. Ports the atom arm of `prim_deref`.
/// ponytail: atoms only; add delay/future/var arms when those land (Phase 4+).
pub fn deref(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [a] => match a {
            Value::Atom(cell) => Ok(cell.borrow().val.clone()),
            Value::Store(cell) => Ok(crate::store::store_deref(cell)),
            // Deref a Var to its current root value (`@#'x`, `@(resolve 's)`).
            Value::Var(sym) => Ok(it.var_value(sym).unwrap_or(Value::Nil)),
            _ => Err(throw_classified(
                "eval/type",
                "MTY001",
                "deref: expected an atom",
            )),
        },
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            "deref requires one argument",
        )),
    }
}

/// `(reset! atom v)` — set the value, returning `v`. Ports `prim_reset_bang`.
pub fn reset_bang(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, val) = match args {
        [a, val] => (a, val),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "reset! requires two arguments",
            ))
        }
    };
    if atom_cell(a).is_none() {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "reset!: first argument must be an atom",
        ));
    }
    atom_set(it, a, val.clone())?;
    Ok(val.clone())
}

/// `(reset-vals! atom v)` — like `reset!` but returns `[old new]`.
pub fn reset_vals_bang(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, val) = match args {
        [a, val] => (a, val),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "reset-vals! requires two arguments",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "reset-vals!: first argument must be an atom",
        ));
    };
    let old = cell.borrow().val.clone();
    atom_set(it, a, val.clone())?;
    Ok(pair(old, val.clone()))
}

/// `(swap! atom f & args)` — apply `(f current & args)` and store the result,
/// returning it. Single-threaded, so no CAS loop. Ports `prim_swap_bang`.
pub fn swap_bang(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, f, extra) = match args {
        [a, f, extra @ ..] => (a, f, extra),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "swap! requires at least 2 arguments: atom and function",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "swap!: first argument must be an atom",
        ));
    };
    let cur = cell.borrow().val.clone();
    let result = apply(it, f, &swap_args(cur, extra))?;
    atom_set(it, a, result.clone())?;
    Ok(result)
}

/// `(swap-vals! atom f & args)` — like `swap!` but returns `[old new]`.
pub fn swap_vals_bang(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, f, extra) = match args {
        [a, f, extra @ ..] => (a, f, extra),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "swap-vals! requires at least 2 arguments: atom and function",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "swap-vals!: first argument must be an atom",
        ));
    };
    let cur = cell.borrow().val.clone();
    let result = apply(it, f, &swap_args(cur.clone(), extra))?;
    atom_set(it, a, result.clone())?;
    Ok(pair(cur, result))
}

/// `(compare-and-set! atom expected new)` — set to `new` iff current `= expected`;
/// returns true on swap, false otherwise. Ports `prim_compare_and_set_bang`.
/// mino compares by identity; single-threaded here compares by value equality
/// (the corpus never distinguishes the two).
pub fn compare_and_set_bang(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, expected, new) = match args {
        [a, e, n] => (a, e, n),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "compare-and-set! requires three arguments: atom, expected, new-val",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "compare-and-set!: first argument must be an atom",
        ));
    };
    let cur = cell.borrow().val.clone();
    if !eq_val(&cur, expected) {
        return Ok(Value::Bool(false));
    }
    atom_set(it, a, new.clone())?;
    Ok(Value::Bool(true))
}

/// `(atom? x)` — true iff x is an atom. Ports `prim_atom_p`.
pub fn atom_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [x] => Ok(Value::Bool(matches!(x, Value::Atom(_)))),
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            "atom? requires one argument",
        )),
    }
}

/// `(add-watch atom key fn)` — register a watch `(fn key atom old new)`.
/// Ports `prim_add_watch`. Returns the atom.
pub fn add_watch(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, key, f) = match args {
        [a, key, f] => (a, key, f),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "add-watch requires three arguments: reference key fn",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "add-watch: first argument must be an atom or ref",
        ));
    };
    if !matches!(f, Value::Fn(_) | Value::Prim(_) | Value::PrimClosure(_)) {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "add-watch: watch fn must be a fn",
        ));
    }
    let mut st = cell.borrow_mut();
    st.watches = st.watches.assoc(key.clone(), f.clone());
    Ok(a.clone())
}

/// `(remove-watch atom key)` — unregister a watch. Ports `prim_remove_watch`.
pub fn remove_watch(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, key) = match args {
        [a, key] => (a, key),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "remove-watch requires two arguments: reference key",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "remove-watch: first argument must be an atom or ref",
        ));
    };
    let mut st = cell.borrow_mut();
    st.watches = st.watches.dissoc(key);
    Ok(a.clone())
}

/// `(set-validator! atom fn)` — set or clear (nil) the validator. mino installs
/// the fn WITHOUT checking the current value (only later transitions are
/// checked). Ports `prim_set_validator`. Returns nil.
pub fn set_validator(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, f) = match args {
        [a, f] => (a, f),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "set-validator! requires two arguments: reference fn",
            ))
        }
    };
    let Some(cell) = atom_cell(a) else {
        return Err(throw_classified(
            "eval/type",
            "MTY001",
            "set-validator!: first argument must be an atom or ref",
        ));
    };
    match f {
        Value::Nil => cell.borrow_mut().validator = None,
        Value::Fn(_) | Value::Prim(_) => cell.borrow_mut().validator = Some(f.clone()),
        Value::PrimClosure(_) => cell.borrow_mut().validator = Some(f.clone()),
        _ => {
            return Err(throw_classified(
                "eval/type",
                "MTY001",
                "set-validator!: validator must be a fn or nil",
            ))
        }
    }
    Ok(Value::Nil)
}

/// `(get-validator atom)` — the current validator fn or nil.
pub fn get_validator(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [a] => {
            let Some(cell) = atom_cell(a) else {
                return Err(throw_classified(
                    "eval/type",
                    "MTY001",
                    "get-validator: argument must be an atom or ref",
                ));
            };
            Ok(cell.borrow().validator.clone().unwrap_or(Value::Nil))
        }
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            "get-validator requires one argument",
        )),
    }
}

// Build the `(cur extra...)` arg list for swap!/swap-vals!.
fn swap_args(cur: Value, extra: &[Value]) -> Vec<Value> {
    let mut v = Vec::with_capacity(1 + extra.len());
    v.push(cur);
    v.extend_from_slice(extra);
    v
}

// A two-element vector `[a b]` (for swap-vals!/reset-vals!).
fn pair(a: Value, b: Value) -> Value {
    Value::Vector(Gc::new(crate::collections::vector::PVec::from_vec(vec![
        a, b,
    ])))
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    fn eval(src: &str) -> String {
        let mut it = Interp::new();
        print_str(&it.eval_str(src).unwrap())
    }

    #[test]
    fn swap_and_reset() {
        // Expected values from `mino -e`.
        assert_eq!(
            eval("(let [a (atom 0)] (swap! a inc) (swap! a + 10) @a)"),
            "11"
        );
        assert_eq!(eval("(let [a (atom 0)] (reset! a 42) @a)"), "42");
        assert_eq!(
            eval("(let [a (atom [1 2])] (swap! a conj 3) @a)"),
            "[1 2 3]"
        );
        assert_eq!(eval("(let [a (atom 10)] (swap! a + 5 3) @a)"), "18");
    }

    #[test]
    fn swap_vals_and_reset_vals() {
        assert_eq!(eval("(let [a (atom 1)] (swap-vals! a + 5))"), "[1 6]");
        assert_eq!(eval("(let [a (atom 1)] (reset-vals! a 9))"), "[1 9]");
    }

    #[test]
    fn compare_and_set() {
        // [(cas 1->2) @a (cas 1->3 fails) @a]
        assert_eq!(
            eval("(let [a (atom 1)] [(compare-and-set! a 1 2) @a (compare-and-set! a 1 3) @a])"),
            "[true 2 false 2]"
        );
    }

    #[test]
    fn validator_rejects() {
        // A rejected reset! throws; swap! back to a valid value succeeds.
        let mut it = Interp::new();
        assert!(it
            .eval_str("(let [a (atom 1 :validator pos?)] (reset! a -1))")
            .is_err());
        assert_eq!(
            eval("(let [a (atom 1 :validator pos?)] (swap! a + 5) @a)"),
            "6"
        );
    }

    #[test]
    fn watch_fires_with_key_ref_old_new() {
        // The watch records [key old new]; a reset! 0 -> 5 fires it once.
        assert_eq!(
            eval(
                "(let [a (atom 0) log (atom [])] \
                 (add-watch a :k (fn [k r o n] (swap! log conj [k o n]))) \
                 (reset! a 5) @log)"
            ),
            "[[:k 0 5]]"
        );
    }

    #[test]
    fn atom_predicate_and_identity() {
        assert_eq!(eval("(atom? (atom nil))"), "true");
        assert_eq!(eval("(atom? 42)"), "false");
        assert_eq!(eval("(let [a (atom 1)] (= a a))"), "true");
        assert_eq!(eval("(let [a (atom 1) b (atom 1)] (= a b))"), "false");
    }
}
