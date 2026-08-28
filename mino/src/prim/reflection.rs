//! Reflection / error prims: `throw`, `ex-info`, `ex-message`, `ex-data`.
//! Ports the relevant slice of `src/prim/reflection.c` plus the core.clj
//! definitions of `ex-info`/`ex-message`/`ex-data` (which shadow the C prims
//! in the live binary; we match the core.clj behavior here since core.clj is
//! not loaded until Phase 4).

use crate::collections::map::PMap;
use crate::error::{throw_classified, Throw};
use crate::eval::Interp;
use crate::symbol::Symbol;
use crate::value::Value;
use gc::Gc;
use std::sync::atomic::{AtomicU64, Ordering};

fn kw(name: &str) -> Value {
    Value::Keyword(Symbol::plain(name))
}
fn kw_ns(ns: &str, name: &str) -> Value {
    Value::Keyword(Symbol::namespaced(ns, name))
}

/// `(throw x)` — raise `x`. mino accepts ANY value (string, keyword, map, ...);
/// the value is normalized only when a `catch` binds it. Ports `prim_throw`.
pub fn throw(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [ex] => Err(Throw(ex.clone())),
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            "throw requires one argument",
        )),
    }
}

/// `(ex-info msg data)` / `(ex-info msg data cause)` — build an exception map
/// `{:message msg :data data}`. `data` must be a map or nil. Ports the
/// core.clj `ex-info`. (The 3-arity `cause` is attached via metadata in mino;
/// metadata is Phase 5, so we accept and drop the cause here.)
pub fn ex_info(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (msg, data) = match args {
        [msg, data] | [msg, data, _] => (msg, data),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "ex-info requires two or three arguments",
            ))
        }
    };
    if !matches!(data, Value::Nil | Value::Map(_)) {
        return Err(throw_classified(
            "type",
            "MTY001",
            "ex-info: data must be a map",
        ));
    }
    let m = PMap::empty()
        .assoc(kw("message"), msg.clone())
        .assoc(kw("data"), data.clone());
    Ok(Value::Map(Gc::new(m)))
}

/// `(ex-message ex)` — extract the message. For a diagnostic map (carries
/// `:mino/kind`) read `:mino/message`; for an ex-info map read `:message`;
/// non-maps yield nil. Ports the core.clj `ex-message`.
pub fn ex_message(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let ex = one_arg(args, "ex-message")?;
    Ok(match ex {
        Value::Map(m) => {
            if m.contains(&kw_ns("mino", "kind")) {
                m.get(&kw_ns("mino", "message")).cloned().unwrap_or(Value::Nil)
            } else {
                m.get(&kw("message")).cloned().unwrap_or(Value::Nil)
            }
        }
        _ => Value::Nil,
    })
}

/// `(ex-data ex)` — extract the data map. For a diagnostic map, unwrap
/// `:mino/data`; if that is itself an ex-info map (`{:message :data}`), return
/// its `:data`. For an ex-info map, return `:data`. Non-maps yield nil. Ports
/// the core.clj `ex-data`.
pub fn ex_data(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let ex = one_arg(args, "ex-data")?;
    Ok(match ex {
        Value::Map(m) => {
            if m.contains(&kw_ns("mino", "kind")) {
                // Diagnostic map: unwrap :mino/data; if it's an ex-info map,
                // extract its :data.
                match m.get(&kw_ns("mino", "data")) {
                    Some(Value::Map(om)) if om.contains(&kw("data")) => {
                        om.get(&kw("data")).cloned().unwrap_or(Value::Nil)
                    }
                    Some(orig) => orig.clone(),
                    None => Value::Nil,
                }
            } else {
                m.get(&kw("data")).cloned().unwrap_or(Value::Nil)
            }
        }
        _ => Value::Nil,
    })
}

fn one_arg<'a>(args: &'a [Value], name: &str) -> Result<&'a Value, Throw> {
    match args {
        [v] => Ok(v),
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{name} requires one argument"),
        )),
    }
}

/// `(not x)` — logical negation: true iff x is nil or false. Ports `prim_not`.
pub fn not(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "not")?;
    Ok(Value::Bool(!v.is_truthy()))
}

/// `(meta x)` — the metadata map of a collection/atom, or nil. Ports
/// `prim_meta`. Metadata lives on the heap payload (PVec/PMap/PSet/atom) and
/// is ignored by eq/hash/type. Symbols/vars/fns don't carry meta in the port
/// yet (real var meta is Phase 4), so they return nil.
/// ponytail: symbol/var/fn meta unimplemented; add when Phase 4 vars land.
pub fn meta(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "meta")?;
    Ok(meta_of(v).map(Value::Map).unwrap_or(Value::Nil))
}

/// The meta map (as `Gc<PMap>`) of a meta-carrying value, or None.
fn meta_of(v: &Value) -> Option<Gc<PMap>> {
    match v {
        Value::Vector(x) => x.meta.clone(),
        Value::Map(x) => x.meta.clone(),
        Value::Set(x) => x.meta.clone(),
        Value::Atom(cell) => cell.borrow().meta.clone(),
        _ => None,
    }
}

/// `(with-meta obj m)` — a copy of `obj` carrying metadata `m` (a map or nil).
/// Meta does NOT affect equality or hashing. Ports `prim_with_meta`. Only
/// collections support with-meta here (mino rejects atom/var; symbols/fns carry
/// meta but the port's gates don't exercise that yet).
pub fn with_meta(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (obj, m) = match args {
        [obj, m] => (obj, m),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "with-meta requires 2 arguments",
            ))
        }
    };
    let meta = match m {
        Value::Nil => None,
        Value::Map(mp) => Some(mp.clone()),
        _ => {
            return Err(throw_classified(
                "type",
                "MTY001",
                "with-meta: metadata must be a map or nil",
            ))
        }
    };
    set_meta(obj, meta)
}

/// Return a copy of `obj` with meta set to `meta` (None clears it), or a throw
/// if `obj` is a type that does not support with-meta. Collections carry real
/// meta; symbols/lists/fns support meta in mino but the port has no storage for
/// them yet, so they return unchanged (meta dropped). Atom/var/nil and scalars
/// throw, matching mino's `supports_meta` gate.
/// ponytail: symbol/list/fn meta dropped; store it when Phase 4 vars/symbol-meta land.
fn set_meta(obj: &Value, meta: Option<Gc<PMap>>) -> Result<Value, Throw> {
    match obj {
        Value::Vector(x) => Ok(Value::Vector(Gc::new(x.with_meta(meta)))),
        Value::Map(x) => Ok(Value::Map(Gc::new(x.with_meta(meta)))),
        Value::Set(x) => Ok(Value::Set(Gc::new(x.with_meta(meta)))),
        // Meta-supporting in mino but unstored here: keep the value, drop meta.
        Value::Sym(_) | Value::EmptyList | Value::Cons(_) | Value::Fn(_) => Ok(obj.clone()),
        _ => Err(throw_classified(
            "type",
            "MTY001",
            "with-meta: type does not support metadata",
        )),
    }
}

/// `(vary-meta obj f & args)` — a copy of `obj` with `(apply f (meta obj) args)`
/// as its metadata. Ports `prim_vary_meta`.
pub fn vary_meta(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (obj, f, extra) = match args {
        [obj, f, extra @ ..] => (obj, f, extra),
        _ => {
            return Err(throw_classified(
                "eval/arity",
                "MAR001",
                "vary-meta requires at least two arguments",
            ))
        }
    };
    let old = meta_of(obj).map(Value::Map).unwrap_or(Value::Nil);
    let mut call = Vec::with_capacity(1 + extra.len());
    call.push(old);
    call.extend_from_slice(extra);
    let new_meta = crate::eval::func::apply(it, f, &call)?;
    let meta = match &new_meta {
        Value::Nil => None,
        Value::Map(mp) => Some(mp.clone()),
        _ => {
            return Err(throw_classified(
                "type",
                "MTY001",
                "vary-meta: f must return a map or nil",
            ))
        }
    };
    set_meta(obj, meta)
}

/// `(name x)` — the name string of a symbol/keyword, or the string itself.
/// Ports `prim_name` (reflection.c).
pub fn name(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "name")?;
    let n = match v {
        Value::Str(s) => (**s).clone(),
        Value::Sym(s) | Value::Keyword(s) => (*s.name).to_string(),
        _ => return Err(throw_classified("type", "MTY001", "name: expects a string, symbol, or keyword")),
    };
    Ok(Value::Str(Gc::new(n)))
}

/// `(keyword x)` / `(keyword ns name)` — build a keyword. Ports `prim_keyword`.
pub fn keyword(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [Value::Str(s)] => Ok(Value::Keyword(parse_name(s))),
        [Value::Sym(s)] | [Value::Keyword(s)] => Ok(Value::Keyword(s.clone())),
        [Value::Str(ns), Value::Str(nm)] => Ok(Value::Keyword(Symbol::namespaced(ns, nm))),
        [Value::Nil] => Ok(Value::Nil),
        _ => Err(throw_classified("type", "MTY001", "keyword: bad arguments")),
    }
}

/// `(symbol x)` / `(symbol ns name)` — build a symbol. Ports `prim_symbol`.
pub fn symbol(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [Value::Str(s)] => Ok(Value::Sym(parse_name(s))),
        [Value::Sym(s)] | [Value::Keyword(s)] => Ok(Value::Sym(s.clone())),
        [Value::Str(ns), Value::Str(nm)] => Ok(Value::Sym(Symbol::namespaced(ns, nm))),
        _ => Err(throw_classified("type", "MTY001", "symbol: bad arguments")),
    }
}

/// Split a `"ns/name"` string into a namespaced symbol, else a plain one.
fn parse_name(s: &str) -> Symbol {
    match s.rsplit_once('/') {
        Some((ns, nm)) if !ns.is_empty() && !nm.is_empty() => Symbol::namespaced(ns, nm),
        _ => Symbol::plain(s),
    }
}

/// `(true? x)` — true iff x is the boolean true. Ports `prim_true_p`.
pub fn true_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(matches!(one_arg(args, "true?")?, Value::Bool(true))))
}

/// `(false? x)` — true iff x is the boolean false. Ports `prim_false_p`.
pub fn false_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(matches!(one_arg(args, "false?")?, Value::Bool(false))))
}

/// `(some? x)` — true iff x is not nil. Ports `prim_some_p`.
pub fn some_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(!matches!(one_arg(args, "some?")?, Value::Nil)))
}

/// `(type x)` — a keyword type tag. Ports `tag_kw`/`prim_type` (reflection.c)
/// for the value kinds the port has. Used by core.clj `coll?`.
pub fn type_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "type")?;
    let tag = match v {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Float32(_) => "float32",
        Value::BigInt(_) => "bigint",
        Value::Ratio(_) => "ratio",
        Value::Char(_) => "char",
        Value::Str(_) => "string",
        Value::Sym(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::EmptyList | Value::Cons(_) => "list",
        Value::Vector(_) => "vector",
        Value::Map(_) => "map",
        Value::Set(_) => "set",
        Value::Fn(c) if c.is_macro => "macro",
        Value::Fn(_) | Value::Prim(_) => "fn",
        Value::Regex(_) => "regex",
        Value::Var(_) => "var",
        Value::Atom(_) => "atom",
        Value::Recur(_) => "recur",
    };
    Ok(Value::Keyword(Symbol::plain(tag)))
}

/// `(mino-installed? kw)` — whether an optional mino subsystem is present.
/// The port ships the regex subsystem (Task 5.2); the rest of the optional
/// host subsystems (bignum/net/etc.) are not ported yet.
/// ponytail: only :regex is on; add subsystems as later phases land them.
pub fn mino_installed_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let installed = matches!(
        args.first(),
        Some(Value::Keyword(k)) if matches!(&*k.name, "regex" | "bignum")
    );
    Ok(Value::Bool(installed))
}

// Runtime gensym counter, distinct from the reader's syntax-quote counter
// (mino keeps both on S but they never share a name space in practice).
static GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `(gensym)` / `(gensym prefix)` — a fresh unqualified symbol. Ports
/// `prim_gensym`: default prefix "G__", suffix a monotonic counter.
pub fn gensym(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let prefix = match args {
        [] => "G__".to_string(),
        [Value::Str(s)] => (**s).clone(),
        [Value::Sym(s)] => s.to_string(),
        [_] => return Err(throw_classified("type", "MTY001", "gensym: prefix must be a string or symbol")),
        _ => return Err(throw_classified("eval/arity", "MAR001", "gensym takes zero or one argument")),
    };
    let n = GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed);
    Ok(Value::Sym(Symbol::plain(&format!("{prefix}{n}"))))
}

/// `(namespace x)` — the namespace string of a namespaced symbol/keyword, or
/// nil. Ports `prim_namespace` (reflection.c).
pub fn namespace(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "namespace")?;
    Ok(match v {
        Value::Sym(s) | Value::Keyword(s) => match &s.ns {
            Some(ns) => Value::Str(Gc::new(ns.to_string())),
            None => Value::Nil,
        },
        _ => return Err(throw_classified("type", "MTY001", "namespace: expects a symbol or keyword")),
    })
}

/// `(hash x)` — the value hash (an int), consistent with `=`. Ports
/// `prim_hash`: uses the same `hash_val` that backs the HAMT key discipline.
pub fn hash(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "hash")?;
    Ok(Value::Int(crate::collections::hashing::hash_val(v) as i64))
}

/// `(class x)` — alias of `type` in mino (both return the keyword type tag).
/// Ports `prim_class`.
pub fn class(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    type_(it, args)
}

/// `(resolve sym)` — resolve `sym` to its var/value in the root env, or nil if
/// unbound. Ports `prim_resolve` (the port has placeholder vars, so this
/// returns the bound VALUE the symbol names, or nil). ponytail: returns the
/// resolved value, not a real Var cell; upgrade when Phase 4 vars land.
pub fn resolve(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let v = one_arg(args, "resolve")?;
    match v {
        Value::Sym(s) => Ok(it.root.get(s).unwrap_or(Value::Nil)),
        _ => Err(throw_classified("type", "MTY001", "resolve: expects a symbol")),
    }
}

/// `(eval form)` — evaluate `form` in the root env. Ports `prim_eval`.
pub fn eval(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let form = one_arg(args, "eval")?.clone();
    let root = it.root.clone();
    it.eval(&form, &root)
}

/// `(macroexpand-1 form)` — expand `form` once if its head is a macro, else
/// return it unchanged. Ports `macroexpand1` (eval.c).
pub fn macroexpand_1(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let form = one_arg(args, "macroexpand-1")?.clone();
    it.macroexpand1(&form)
}

/// `(macroexpand form)` — expand repeatedly until the head is no longer a
/// macro. Ports `macroexpand_all` (eval.c).
pub fn macroexpand(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut form = one_arg(args, "macroexpand")?.clone();
    loop {
        let (next, expanded) = it.macroexpand1_flagged(&form)?;
        if !expanded {
            return Ok(form);
        }
        form = next;
    }
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
    fn with_meta_and_meta_round_trip() {
        // Expected values from `mino -e`.
        assert_eq!(eval("(meta (with-meta [1 2] {:a 1}))"), "{:a 1}");
        assert_eq!(eval("(meta [1 2])"), "nil");
        assert_eq!(eval("(meta (with-meta {:x 1} {:a 1}))"), "{:a 1}");
    }

    #[test]
    fn meta_does_not_affect_equality_or_hash_or_type() {
        assert_eq!(eval("(= (with-meta [1] {:a 1}) [1])"), "true");
        assert_eq!(eval("(= (with-meta {:x 1} {:a 1}) {:x 1})"), "true");
        assert_eq!(eval("(= (hash (with-meta [1] {:a 1})) (hash [1]))"), "true");
        assert_eq!(eval("(type (with-meta [1] {:a 1}))"), ":vector");
    }

    #[test]
    fn meta_propagates_through_collection_ops() {
        // assoc/conj/into/dissoc/merge/pop keep the collection's metadata.
        assert_eq!(eval("(meta (assoc (with-meta [1 2 3] {:m 1}) 1 :x))"), "{:m 1}");
        assert_eq!(eval("(meta (conj (with-meta [1] {:m 1}) 2))"), "{:m 1}");
        assert_eq!(eval("(meta (into (with-meta [1] {:m 1}) [2 3]))"), "{:m 1}");
        assert_eq!(eval("(meta (dissoc (with-meta {:a 1 :b 2} {:m 1}) :a))"), "{:m 1}");
        assert_eq!(eval("(meta (merge (with-meta {:a 1} {:m 1}) {:b 2}))"), "{:m 1}");
        assert_eq!(eval("(meta (pop (pop (pop (with-meta [1 2 3] {:m 1})))))"), "{:m 1}");
    }

    #[test]
    fn eval_prim() {
        assert_eq!(eval("(eval (list '+ 1 2))"), "3");
        assert_eq!(eval("(eval 42)"), "42");
    }

    #[test]
    fn gensym_is_unique() {
        assert_eq!(eval("(= (gensym) (gensym))"), "false");
    }
}
