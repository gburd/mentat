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

/// `(meta x)` — the port does not track value metadata yet, so always nil.
/// ponytail: metadata untracked; real meta in Phase 5.3 (meta.c).
pub fn meta(_it: &mut Interp, _args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Nil)
}

/// `(with-meta obj m)` — attach metadata. Untracked, so return `obj` as-is.
/// ponytail: metadata untracked; real with-meta in Phase 5.3 (meta.c).
pub fn with_meta(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args.first() {
        Some(v) => Ok(v.clone()),
        None => Err(throw_classified("eval/arity", "MAR001", "with-meta requires two arguments")),
    }
}

/// `(vary-meta obj f & args)` — apply f to obj's metadata. Untracked, so
/// return `obj` unchanged. ponytail: metadata untracked; real in Phase 5.3.
pub fn vary_meta(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args.first() {
        Some(v) => Ok(v.clone()),
        None => Err(throw_classified("eval/arity", "MAR001", "vary-meta requires at least two arguments")),
    }
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
        Value::Var(_) => "var",
        Value::Recur(_) => "recur",
    };
    Ok(Value::Keyword(Symbol::plain(tag)))
}

/// `(mino-installed? kw)` — whether an optional mino subsystem is present.
/// The port ships none of the optional host subsystems yet (bignum/net/etc.),
/// so always false. ponytail: no host subsystems; revisit if a phase adds one.
pub fn mino_installed_p(_it: &mut Interp, _args: &[Value]) -> Result<Value, Throw> {
    Ok(Value::Bool(false))
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
