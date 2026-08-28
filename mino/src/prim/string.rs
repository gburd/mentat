//! String / print primitives. Ports the `str`/`pr-str`/`println`/`str?`
//! subset of `src/prim/string.c` and `io.c` that core.clj and the gate
//! corpus need. Full string library (replace/split/regex) is Phase 5.1.

use crate::error::Throw;
use crate::eval::Interp;
use crate::printer::print_str;
use crate::value::Value;
use gc::Gc;

/// `(str & xs)`: concatenate the string forms of each arg. Strings append
/// raw, nil contributes nothing, chars append their character, everything
/// else uses the readable printer. Ports `prim_str` (string.c) for the value
/// kinds the port has. No args -> "".
pub fn str_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for a in args {
        arg_to_str(&mut out, a);
    }
    Ok(Value::Str(Gc::new(out)))
}

/// Append one arg's `str` form: strings raw, nil nothing, char raw, else pr.
fn arg_to_str(out: &mut String, a: &Value) {
    match a {
        Value::Str(s) => out.push_str(s),
        Value::Nil => {}
        Value::Char(c) => out.push(*c),
        other => out.push_str(&print_str(other)),
    }
}

/// `(pr-str & xs)`: readable form of each arg, space-separated. Ports
/// `prim_pr_str` (string.c).
pub fn pr_str(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&print_str(a));
    }
    Ok(Value::Str(Gc::new(out)))
}

/// `(println & xs)`: print the `str` forms space-separated + newline, return
/// nil. Output goes to stdout (io.c). Kept minimal for corpus use.
pub fn println_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        arg_to_str(&mut out, a);
    }
    println!("{out}");
    Ok(Value::Nil)
}

/// `(prn & xs)`: print the readable forms space-separated + newline, nil.
pub fn prn(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&print_str(a));
    }
    println!("{out}");
    Ok(Value::Nil)
}

/// `(print & xs)`: like println without the trailing newline.
pub fn print_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        arg_to_str(&mut out, a);
    }
    print!("{out}");
    Ok(Value::Nil)
}
