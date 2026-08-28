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
