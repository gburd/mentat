//! Readable printer (`pr-str`): the round-trip inverse of the reader.
//! Ports the single switch in `src/eval/print.c` (readable form only:
//! `print_readably_flag == 1`). Only the `Value` variants that exist in
//! Task 0.3 are handled; later phases extend the match as the enum grows.

use crate::depth::MAX_DATA_DEPTH;
use crate::value::Value;
use std::fmt::Write;

/// Readable form of a value, matching mino `pr-str`. Infallible: past
/// `MAX_DATA_DEPTH` a subtree prints as the literal marker
/// `#<nesting too deep>` and an atom (or store) already on the print stack
/// prints as `#<cycle>` instead of recursing forever. Called from ~79 sites
/// including `Display`/error paths, so it can never fail; the user-facing
/// print prims use [`print_str_checked`] to surface a limit error instead.
pub fn print_str(v: &Value) -> String {
    let mut s = String::new();
    let mut pr = Printer {
        seen: Vec::new(),
        overflow: false,
    };
    pr.print_into(&mut s, v, 0);
    s
}

/// Fallible readable form for the user-facing print prims (`pr-str`, `str`,
/// `prn`, `println`, `print`). Returns a `:eval/limit` throw with data
/// `{:limit :nesting}` if any subtree nests deeper than `MAX_DATA_DEPTH`,
/// so a script gets an error rather than a silently-truncated string. Cycles
/// still print `#<cycle>` and are not an error.
pub fn print_str_checked(v: &Value) -> Result<String, crate::error::Throw> {
    let mut s = String::new();
    let mut pr = Printer {
        seen: Vec::new(),
        overflow: false,
    };
    pr.print_into(&mut s, v, 0);
    if pr.overflow {
        return Err(crate::error::throw_nesting_limit());
    }
    Ok(s)
}

/// Print state threaded through the walk: `seen` is the stack of atom/store
/// cell addresses currently being printed (for cycle detection); `overflow`
/// records whether the depth cap was hit (so [`print_str_checked`] can fail).
struct Printer {
    seen: Vec<usize>,
    overflow: bool,
}

impl Printer {
    fn print_into(&mut self, s: &mut String, v: &Value, depth: usize) {
        if depth > MAX_DATA_DEPTH {
            self.overflow = true;
            s.push_str("#<nesting too deep>");
            return;
        }
        match v {
            Value::Nil => s.push_str("nil"),
            Value::Bool(true) => s.push_str("true"),
            Value::Bool(false) => s.push_str("false"),
            // print.c: fprintf(out, "%lld", ...)
            Value::Int(n) => {
                let _ = write!(s, "{n}");
            }
            Value::Float(x) => print_float(s, *x),
            // A 32-bit float prints via f32's shortest round-trip decimal, then
            // reshaped to JVM form (forced `.0`, `E` scientific). mino's
            // MINO_FLOAT32 print path uses the float (not double) shortest form.
            Value::Float32(x) => print_float32(s, *x),
            // Bigint prints like an int but with an `N` suffix (mino's print.c).
            Value::BigInt(b) => {
                let _ = write!(s, "{}N", b.0);
            }
            // Ratio prints `num/den` (always reduced, denom != 1).
            Value::Ratio(r) => {
                let _ = write!(s, "{}/{}", r.0.numer(), r.0.denom());
            }
            Value::Char(c) => print_char(s, *c),
            Value::Str(gc) => print_string_escaped(s, gc),
            // print.c: symbols write their name bytes; keywords prefix ':'.
            Value::Sym(sym) => {
                let _ = write!(s, "{sym}");
            }
            Value::Keyword(sym) => {
                let _ = write!(s, ":{sym}");
            }
            Value::Cons(_) => self.print_list(s, v, depth),
            Value::EmptyList => s.push_str("()"),
            Value::Vector(items) => {
                s.push('[');
                for (i, e) in items.iter().enumerate() {
                    if i > 0 {
                        s.push(' ');
                    }
                    self.print_into(s, e, depth + 1);
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
                    self.print_into(s, k, depth + 1);
                    s.push(' ');
                    self.print_into(s, val, depth + 1);
                }
                s.push('}');
            }
            Value::Set(set) => {
                s.push_str("#{");
                for (i, e) in set.iter().enumerate() {
                    if i > 0 {
                        s.push(' ');
                    }
                    self.print_into(s, e, depth + 1);
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
            Value::PrimClosure(p) => {
                let _ = write!(s, "#<prim {}>", p.name);
            }
            // def returns a var, printed `#'ns/name`.
            Value::Var(sym) => {
                let _ = write!(s, "#'{sym}");
            }
            // Internal recur signal; never printed in normal use (matches mino's
            // MINO_RECUR having only a diagnostic print form).
            Value::Recur(_) => s.push_str("#<recur>"),
            Value::TailCall(_) => s.push_str("#<tail-call>"),
            // pr-str form: `#"source"` with the source printed verbatim (the
            // reader stored it raw, no escape processing). Verified: mino
            // `(pr-str #"a\d+")` => `#"a\d+"`.
            Value::Regex(r) => {
                s.push_str("#\"");
                s.push_str(&r.source);
                s.push('"');
            }
            // Atom: `#atom[value]` (mino: `(pr-str (atom 1))` => `#atom[1]`).
            // A cycle (an atom whose cell is already being printed, e.g.
            // `(reset! a a)`) prints `#<cycle>` rather than recursing forever.
            Value::Atom(cell) => {
                let addr = &**cell as *const _ as usize;
                if self.seen.contains(&addr) {
                    s.push_str("#atom[#<cycle>]");
                } else {
                    self.seen.push(addr);
                    s.push_str("#atom[");
                    self.print_into(s, &cell.borrow().val, depth + 1);
                    s.push(']');
                    self.seen.pop();
                }
            }
            Value::Store(cell) => {
                // #store[0xN VAL]: N is the monotonic per-interp counter in hex.
                let addr = &**cell as *const _ as usize;
                let st = cell.borrow();
                s.push_str("#store[0x");
                s.push_str(&format!("{:x}", st.id));
                s.push(' ');
                if self.seen.contains(&addr) {
                    s.push_str("#<cycle>");
                } else {
                    self.seen.push(addr);
                    self.print_into(s, &st.val, depth + 1);
                    self.seen.pop();
                }
                s.push(']');
            }
        }
    }

    /// Walk a cons chain as a list `( ... )`. Proper lists terminate in
    /// `Value::EmptyList` (the empty-list value); a bare `Nil` tail is also
    /// treated as a list terminator. An improper (dotted) tail prints
    /// " . tail".
    fn print_list(&mut self, s: &mut String, v: &Value, depth: usize) {
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
                    self.print_into(s, &cell.0, depth + 1);
                    cur = &cell.1;
                }
                Value::EmptyList | Value::Nil => break,
                // improper tail — see doc comment; print.c uses " . ".
                other => {
                    s.push_str(" . ");
                    self.print_into(s, other, depth + 1);
                    break;
                }
            }
        }
        s.push(')');
    }
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

/// 32-bit float print: identical JVM reshaping as `print_float`, but the
/// shortest-decimal source is f32's (so `(float 3.14159265358979)` prints
/// `3.1415927`, not the f64 form). Threshold uses the f32 magnitude.
fn print_float32(s: &mut String, x: f32) {
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
