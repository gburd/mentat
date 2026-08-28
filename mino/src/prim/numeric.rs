//! Numeric primitives: `+ - * / = < > <= >=`.
//! Ports the arithmetic + comparison chains from `src/prim/numeric.c`.
//! Numeric-tower rule (`args_have_float`): all-Int -> Int, any-Float -> Float.
//! Bignum/ratio (Phase 5.5) are not here: `/` on ints that don't divide
//! evenly promotes to Float (mino returns an exact ratio, e.g. 7/2).

use crate::error::{throw_classified, throw_str, Throw};
use crate::eval::Interp;
use crate::value::Value;

/// A numeric arg as either exact int or float; the fold promotes to Float if
/// any arg is a Float.
fn as_num(v: &Value) -> Result<Num, Throw> {
    match v {
        Value::Int(n) => Ok(Num::Int(*n)),
        Value::Float(x) => Ok(Num::Float(*x)),
        other => Err(throw_str(&format!(
            "not a number: {}",
            crate::printer::print_str(other)
        ))),
    }
}

#[derive(Clone, Copy)]
enum Num {
    Int(i64),
    Float(f64),
}
impl Num {
    fn to_f(self) -> f64 {
        match self {
            Num::Int(n) => n as f64,
            Num::Float(x) => x,
        }
    }
}

fn any_float(args: &[Value]) -> bool {
    args.iter().any(|v| matches!(v, Value::Float(_)))
}

pub fn add(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if any_float(args) {
        let mut acc = 0.0;
        for v in args {
            acc += as_num(v)?.to_f();
        }
        Ok(Value::Float(acc))
    } else {
        let mut acc: i64 = 0;
        for v in args {
            match as_num(v)? {
                Num::Int(n) => acc += n,
                Num::Float(_) => unreachable!(),
            }
        }
        Ok(Value::Int(acc))
    }
}

pub fn mul(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if any_float(args) {
        let mut acc = 1.0;
        for v in args {
            acc *= as_num(v)?.to_f();
        }
        Ok(Value::Float(acc))
    } else {
        let mut acc: i64 = 1;
        for v in args {
            match as_num(v)? {
                Num::Int(n) => acc *= n,
                Num::Float(_) => unreachable!(),
            }
        }
        Ok(Value::Int(acc))
    }
}

pub fn sub(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.is_empty() {
        return Err(throw_str("- requires at least 1 argument"));
    }
    let float = any_float(args);
    // (- x) negates; (- x y z ...) folds subtraction.
    if args.len() == 1 {
        return Ok(match as_num(&args[0])? {
            Num::Int(n) => Value::Int(-n),
            Num::Float(x) => Value::Float(-x),
        });
    }
    if float {
        let mut acc = as_num(&args[0])?.to_f();
        for v in &args[1..] {
            acc -= as_num(v)?.to_f();
        }
        Ok(Value::Float(acc))
    } else {
        let mut acc = match as_num(&args[0])? {
            Num::Int(n) => n,
            Num::Float(_) => unreachable!(),
        };
        for v in &args[1..] {
            if let Num::Int(n) = as_num(v)? {
                acc -= n;
            }
        }
        Ok(Value::Int(acc))
    }
}

pub fn div(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.is_empty() {
        return Err(throw_str("/ requires at least 1 argument"));
    }
    // (/ x) is (/ 1 x).
    let (head, tail): (Num, &[Value]) = if args.len() == 1 {
        (Num::Int(1), args)
    } else {
        (as_num(&args[0])?, &args[1..])
    };

    // Any float -> float division.
    if any_float(args) {
        let mut acc = head.to_f();
        for v in tail {
            let d = as_num(v)?.to_f();
            acc /= d;
        }
        return Ok(Value::Float(acc));
    }

    // All ints: divide exactly when evenly divisible, else promote to float.
    // Phase 5.5: exact ratio (mino returns 7/2 for (/ 7 2)).
    let mut int_acc = match head {
        Num::Int(n) => n,
        Num::Float(_) => unreachable!(),
    };
    let mut exact = true;
    let mut float_acc = int_acc as f64;
    for v in tail {
        let d = match as_num(v)? {
            Num::Int(n) => n,
            Num::Float(_) => unreachable!(),
        };
        if d == 0 {
            return Err(throw_classified("eval/type", "MTY001", "division by zero"));
        }
        float_acc /= d as f64;
        if exact && int_acc % d == 0 {
            int_acc /= d;
        } else {
            exact = false;
        }
    }
    Ok(if exact {
        Value::Int(int_acc)
    } else {
        Value::Float(float_acc)
    })
}

/// `=`: value equality across the tower. Int and Float are NOT equal even at
/// the same magnitude (`(= 1 1.0)` -> false, matching Clojure/mino). Variadic:
/// every arg must equal the first; 0/1 args -> true.
pub fn eq(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    // Delegate to the canonical structural equality (hashing::eq_val), the
    // single source of truth: handles the whole tower incl. maps/sets and the
    // empty-list/vector sequential group ((= () []) true, (= () nil) false).
    let all = args
        .windows(2)
        .all(|w| crate::collections::hashing::eq_val(&w[0], &w[1]));
    Ok(Value::Bool(all))
}

/// Chained numeric comparison: `(op a b c ...)` is true iff `a op b`,
/// `b op c`, ... all hold. 0/1 args -> true (matches mino).
fn compare_chain(args: &[Value], ok: fn(std::cmp::Ordering) -> bool) -> Result<Value, Throw> {
    for w in args.windows(2) {
        let a = as_num(&w[0])?.to_f();
        let b = as_num(&w[1])?.to_f();
        match a.partial_cmp(&b) {
            Some(o) if ok(o) => {}
            // NaN or failed relation -> false.
            _ => return Ok(Value::Bool(false)),
        }
    }
    Ok(Value::Bool(true))
}

use std::cmp::Ordering::{Equal, Greater, Less};

pub fn lt(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, |o| o == Less)
}
pub fn gt(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, |o| o == Greater)
}
pub fn le(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, |o| o == Less || o == Equal)
}
pub fn ge(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    compare_chain(args, |o| o == Greater || o == Equal)
}
