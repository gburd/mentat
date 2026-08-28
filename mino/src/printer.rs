//! Readable printer (`pr-str`): the round-trip inverse of the reader.
//! Ports the single switch in `src/eval/print.c` (readable form only:
//! `print_readably_flag == 1`). Only the `Value` variants that exist in
//! Task 0.3 are handled; later phases extend the match as the enum grows.

use crate::value::Value;
use std::fmt::Write;

/// Readable form of a value, matching mino `pr-str`.
pub fn print_str(v: &Value) -> String {
    let mut s = String::new();
    print_into(&mut s, v);
    s
}

fn print_into(s: &mut String, v: &Value) {
    match v {
        Value::Nil => s.push_str("nil"),
        Value::Bool(true) => s.push_str("true"),
        Value::Bool(false) => s.push_str("false"),
        // print.c: fprintf(out, "%lld", ...)
        Value::Int(n) => {
            let _ = write!(s, "{n}");
        }
        Value::Float(x) => print_float(s, *x),
        Value::Char(c) => print_char(s, *c),
        Value::Str(gc) => print_string_escaped(s, gc),
        // print.c: symbols write their name bytes; keywords prefix ':'.
        Value::Sym(sym) => {
            let _ = write!(s, "{sym}");
        }
        Value::Keyword(sym) => {
            let _ = write!(s, ":{sym}");
        }
        Value::Cons(_) => print_list(s, v),
        Value::EmptyList => s.push_str("()"),
        Value::Vector(items) => {
            s.push('[');
            for (i, e) in items.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                print_into(s, e);
            }
            s.push(']');
        }
        // print.c print_map: pairs separated by ", ", key/val by a space.
        Value::Map(m) => {
            s.push('{');
            for (i, (k, val)) in m.entries().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                print_into(s, k);
                s.push(' ');
                print_into(s, val);
            }
            s.push('}');
        }
        Value::Set(set) => {
            s.push_str("#{");
            for (i, e) in set.iter().enumerate() {
                if i > 0 {
                    s.push(' ');
                }
                print_into(s, e);
            }
            s.push('}');
        }
        // print.c: fns print `#<fn>` (or `#<fn name>`), prims `#<prim name>`.
        Value::Fn(closure) => match &closure.name {
            Some(n) => {
                let _ = write!(s, "#<fn {n}>");
            }
            None => s.push_str("#<fn>"),
        },
        Value::Prim(p) => {
            let _ = write!(s, "#<prim {}>", p.1);
        }
        // def returns a var, printed `#'ns/name`.
        Value::Var(sym) => {
            let _ = write!(s, "#'{sym}");
        }
        // Internal recur signal; never printed in normal use (matches mino's
        // MINO_RECUR having only a diagnostic print form).
        Value::Recur(_) => s.push_str("#<recur>"),
    }
}

/// Walk a cons chain as a list `( ... )`. Proper lists terminate in
/// `Value::EmptyList` (the empty-list value); a bare `Nil` tail is also
/// treated as a list terminator. An improper (dotted) tail prints " . tail".
fn print_list(s: &mut String, v: &Value) {
    s.push('(');
    let mut cur = v;
    let mut first = true;
    loop {
        match cur {
            Value::Cons(cell) => {
                if !first {
                    s.push(' ');
                }
                first = false;
                print_into(s, &cell.0);
                cur = &cell.1;
            }
            Value::EmptyList | Value::Nil => break,
            // improper tail — see doc comment; print.c uses " . ".
            other => {
                s.push_str(" . ");
                print_into(s, other);
                break;
            }
        }
    }
    s.push(')');
}

/// print.c print_string_escaped: quote and escape " \ \n \t \r \0.
fn print_string_escaped(s: &mut String, raw: &str) {
    s.push('"');
    for c in raw.chars() {
        match c {
            '"' => s.push_str("\\\""),
            '\\' => s.push_str("\\\\"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\r' => s.push_str("\\r"),
            '\0' => s.push_str("\\0"),
            _ => s.push(c),
        }
    }
    s.push('"');
}

/// print.c print_char: named escapes, printable ASCII as `\c`, BMP as
/// `\uXXXX`, astral as the raw glyph.
fn print_char(s: &mut String, c: char) {
    match c {
        ' ' => s.push_str("\\space"),
        '\n' => s.push_str("\\newline"),
        '\t' => s.push_str("\\tab"),
        '\r' => s.push_str("\\return"),
        '\u{8}' => s.push_str("\\backspace"),
        '\u{c}' => s.push_str("\\formfeed"),
        _ => {
            let cp = c as u32;
            if (0x21..=0x7E).contains(&cp) {
                s.push('\\');
                s.push(c);
            } else if cp <= 0xFFFF {
                let _ = write!(s, "\\u{cp:04X}");
            } else {
                s.push('\\');
                s.push(c);
            }
        }
    }
}

/// print.c print_float (double path): shortest re-parsable decimal, always
/// with a decimal point, JVM-style exponent (uppercase `E`, no leading `+`
/// or exponent zero). Rust's default `{}` float format already gives the
/// shortest round-tripping decimal and always includes a `.` for values it
/// prints in fixed form; we only need to (a) force a `.0` when Rust emits a
/// bare integer-looking form and (b) reshape scientific notation to JVM form.
fn print_float(s: &mut String, x: f64) {
    if x.is_nan() {
        s.push_str("##NaN");
        return;
    }
    if x.is_infinite() {
        s.push_str(if x > 0.0 { "##Inf" } else { "##-Inf" });
        return;
    }
    let absx = x.abs();
    let use_sci = x != 0.0 && (absx < 1e-3 || absx >= 1e7);
    if use_sci {
        // Rust "{:E}" -> "1.5E2" / "1E5" (no '+' , no leading exponent zero),
        // which already matches JVM's shape. Ensure a decimal point.
        let raw = format!("{x:E}");
        match raw.split_once('E') {
            Some((mantissa, exp)) if !mantissa.contains('.') => {
                let _ = write!(s, "{mantissa}.0E{exp}");
            }
            _ => s.push_str(&raw),
        }
    } else {
        let raw = format!("{x}");
        s.push_str(&raw);
        if !raw.contains('.') {
            s.push_str(".0");
        }
    }
}
