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
        // (str #"a\d+") => the pattern source, unescaped (verified: mino).
        Value::Regex(r) => out.push_str(&r.source),
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
pub fn println_(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        arg_to_str(&mut out, a);
    }
    out.push('\n');
    emit(it, &out)
}

/// Write print output: captured into `it.out` (charged to the heap budget)
/// when the interpreter captures output, else stdout.
fn emit(it: &mut Interp, s: &str) -> Result<Value, Throw> {
    if it.out.is_some() {
        it.charge(0, s.len() as u64)?;
        if let Some(o) = it.out.as_mut() {
            o.push_str(s);
        }
    } else {
        use std::io::Write;
        let mut so = std::io::stdout().lock();
        let _ = so.write_all(s.as_bytes());
        let _ = so.flush();
    }
    Ok(Value::Nil)
}

/// `(prn & xs)`: print the readable forms space-separated + newline, nil.
pub fn prn(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&print_str(a));
    }
    out.push('\n');
    emit(it, &out)
}

/// `(print & xs)`: like println without the trailing newline.
pub fn print_(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = String::new();
    for (i, a) in args.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        arg_to_str(&mut out, a);
    }
    emit(it, &out)
}

// ---------------------------------------------------------------------------
// Core string prims (subs, char-at) + clojure.string C primitives.
// ---------------------------------------------------------------------------

use crate::collections::vector::PVec;
use crate::error::throw_str;

fn as_str<'a>(v: &'a Value, ctx: &str) -> Result<&'a str, Throw> {
    match v {
        Value::Str(s) => Ok(s.as_str()),
        _ => Err(throw_str(&format!("{ctx}: argument must be a string"))),
    }
}

/// `(subs s start)` / `(subs s start end)`. Codepoint-based indices, matching
/// mino: strings are sequences of chars. Out-of-range throws (mino MBD001).
pub fn subs(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 && args.len() != 3 {
        return Err(throw_str("subs requires 2 or 3 arguments"));
    }
    let s = as_str(&args[0], "subs")?;
    let chars: Vec<char> = s.chars().collect();
    let total = chars.len() as i64;
    let start = match &args[1] {
        Value::Int(n) => *n,
        _ => return Err(throw_str("subs: start index must be an integer")),
    };
    let end = match args.get(2) {
        None => total,
        Some(Value::Int(n)) => *n,
        Some(_) => return Err(throw_str("subs: end index must be an integer")),
    };
    if start < 0 || end < start || end > total {
        return Err(throw_str("subs: index out of range"));
    }
    let out: String = chars[start as usize..end as usize].iter().collect();
    Ok(Value::Str(Gc::new(out)))
}

/// `(char-at s i)`: the 1-char string at codepoint index `i`. mino's C prim is
/// byte-indexed and returns broken UTF-8 for multibyte chars; the port uses a
/// codepoint index so it never yields invalid UTF-8 and matches how
/// clojure.string's char-by-char loops (blank?/triml/trimr) index. Only ASCII
/// is exercised by the gate, where the two agree.
pub fn char_at(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 {
        return Err(throw_str("char-at requires two arguments"));
    }
    let s = as_str(&args[0], "char-at")?;
    let i = match &args[1] {
        Value::Int(n) => *n,
        _ => return Err(throw_str("char-at: requires a string and integer index")),
    };
    match s.chars().nth(i.max(0) as usize) {
        Some(c) if i >= 0 => Ok(Value::Str(Gc::new(c.to_string()))),
        _ => Err(throw_str("char-at: index out of range")),
    }
}

/// `(clojure.string/upper-case s)`: ASCII-uppercase. Requires a string (the
/// nil/number coercion is added by the clojure.string wrapper via `as-str`).
pub fn upper_case(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let s = one_string(args, "upper-case")?;
    Ok(Value::Str(Gc::new(s.chars().map(ascii_upper).collect())))
}

/// `(clojure.string/lower-case s)`: ASCII-lowercase.
pub fn lower_case(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let s = one_string(args, "lower-case")?;
    Ok(Value::Str(Gc::new(s.chars().map(ascii_lower).collect())))
}

/// `(clojure.string/trim s)`: strip leading+trailing ASCII whitespace. mino
/// uses C `isspace`; the wrapper adds no coercion.
pub fn trim(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let s = one_string(args, "trim")?;
    let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
    Ok(Value::Str(Gc::new(t.to_string())))
}

/// `(clojure.string/starts-with? s prefix)`: both must be strings (mino MTY001).
pub fn starts_with_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (s, p) = two_strings(args, "starts-with?")?;
    Ok(Value::Bool(s.starts_with(p)))
}

/// `(clojure.string/ends-with? s suffix)`.
pub fn ends_with_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (s, p) = two_strings(args, "ends-with?")?;
    Ok(Value::Bool(s.ends_with(p)))
}

/// `(clojure.string/includes? s substr)`.
pub fn includes_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (s, p) = two_strings(args, "includes?")?;
    Ok(Value::Bool(s.contains(p)))
}

/// `(clojure.string/join sep coll)` / `(clojure.string/join coll)`. Joins the
/// `str` forms of each item; nil items contribute nothing. Ports `prim_join`.
pub fn join(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (sep, coll) = match args {
        [coll] => ("", coll),
        [Value::Str(sep), coll] => (sep.as_str(), coll),
        [Value::Nil, coll] => ("", coll),
        [_, _] => return Err(throw_str("join: separator must be a string or nil")),
        _ => return Err(throw_str("join requires 1 or 2 arguments")),
    };
    let items = seq_items(coll)?;
    let mut out = String::new();
    let mut first = true;
    for it in &items {
        if matches!(it, Value::Nil) {
            continue;
        }
        if !first {
            out.push_str(sep);
        }
        arg_to_str(&mut out, it);
        first = false;
    }
    Ok(Value::Str(Gc::new(out)))
}

/// `(clojure.string/split s sep)` / `(... sep limit)`. STRING separator only
/// (regex separators are Task 5.2 and throw). Matches mino's JVM split rules:
/// empty separator splits into codepoints; limit 0/absent trims trailing empty
/// pieces; limit<0 keeps them; limit>0 caps piece count with the last piece
/// absorbing the rest. Returns a vector of strings.
pub fn split(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 && args.len() != 3 {
        return Err(throw_str("split requires a string and a separator"));
    }
    let s = as_str(&args[0], "split")?;
    let limit = match args.get(2) {
        None => 0,
        Some(Value::Int(n)) => *n,
        Some(_) => return Err(throw_str("split: limit must be an integer")),
    };
    let sep = match &args[1] {
        Value::Str(sep) => sep.as_str(),
        // Regex separator (Task 5.2): delegate to a regex split.
        pat @ Value::Regex(_) => return regex_split(s, pat, limit),
        _ => return Err(throw_str("split: separator must be a string or regex")),
    };
    let pieces = split_string(s, sep, limit);
    let pv = PVec::from_vec(pieces.into_iter().map(|p| Value::Str(Gc::new(p))).collect());
    Ok(Value::Vector(Gc::new(pv)))
}

/// `(clojure.string/replace s match repl)`: STRING match, single pass, all
/// occurrences. Char/regex `match` dispatch lives in the clojure.string
/// wrapper (char -> string here; regex -> Task 5.2 throw).
pub fn replace(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    str_replace(it, args, false)
}

/// `(clojure.string/replace-first s match repl)`: STRING match, first only.
pub fn replace_first(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    str_replace(it, args, true)
}

fn str_replace(it: &mut Interp, args: &[Value], first_only: bool) -> Result<Value, Throw> {
    if args.len() != 3 {
        return Err(throw_str("str-replace requires three arguments"));
    }
    let s = as_str(&args[0], "str-replace: first argument")?;
    // Regex match dispatches to the regex replacer, which honors `$N`
    // template replacements, `\$`/`\\` escapes, and function replacements.
    if let Value::Regex(_) = &args[1] {
        return regex_replace(it, s, &args[1], &args[2], first_only);
    }
    let m = match &args[1] {
        Value::Str(m) => m.as_str(),
        _ => return Err(throw_str("str-replace: match must be a string or regex")),
    };
    let r = match &args[2] {
        Value::Str(r) => r.as_str(),
        _ => return Err(throw_str(
            "str-replace: replacement must be a string when match is a string",
        )),
    };
    if m.is_empty() {
        return Ok(Value::Str(Gc::new(s.to_string())));
    }
    let out = if first_only {
        s.replacen(m, r, 1)
    } else {
        s.replace(m, r)
    };
    Ok(Value::Str(Gc::new(out)))
}

// ---- helpers ----
// ---- regex-backed split/replace (Task 5.2) ----

use crate::eval::func::apply;
use crate::prim::regex::compile_for;

/// `(clojure.string/split s #"re" limit)`: JVM `Pattern.split` semantics.
/// Emits the substrings between successive non-overlapping matches. A
/// zero-width match at position 0 is ignored (Java rule), so `#""` on "abc"
/// yields per-char pieces. limit 0 trims trailing empties; <0 keeps them; >0
/// caps the piece count with the last piece absorbing the rest.
fn regex_split(s: &str, pat: &Value, limit: i64) -> Result<Value, Throw> {
    let re = compile_for(pat, "split")?;
    let mut pieces: Vec<String> = Vec::new();
    let mut last_end = 0usize;
    let mut search = 0usize;
    while search <= s.len() {
        if limit > 0 && pieces.len() as i64 + 1 == limit {
            break;
        }
        let m = match re.find_from_pos(s, search).map_err(|_| throw_str("split: regex error"))? {
            Some(m) => m,
            None => break,
        };
        let (ms, me) = (m.start(), m.end());
        // Ignore a zero-width match at the very start of the string (Java).
        if ms == me {
            if ms == 0 {
                search = next_char_boundary(s, search);
                continue;
            }
            // Zero-width match elsewhere: split before this position.
            pieces.push(s[last_end..ms].to_string());
            last_end = ms;
            search = next_char_boundary(s, ms);
            continue;
        }
        pieces.push(s[last_end..ms].to_string());
        last_end = me;
        search = me;
    }
    pieces.push(s[last_end..].to_string());
    if limit == 0 {
        while pieces.last().is_some_and(|p| p.is_empty()) {
            pieces.pop();
        }
    }
    let pv = PVec::from_vec(pieces.into_iter().map(|p| Value::Str(Gc::new(p))).collect());
    Ok(Value::Vector(Gc::new(pv)))
}

fn next_char_boundary(s: &str, mut i: usize) -> usize {
    i += 1;
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

/// `(clojure.string/replace s #"re" repl)` / replace-first. `repl` is either a
/// `$N` template string (with `\$`/`\\` escapes; a `$N` past the group count
/// throws MCT001) or a function called per match with the whole-match string
/// (no groups) or `[whole g1 ...]` (groups). Zero-width matches are replaced at
/// every position, matching JVM `replaceAll`.
fn regex_replace(
    it: &mut Interp,
    s: &str,
    pat: &Value,
    repl: &Value,
    first_only: bool,
) -> Result<Value, Throw> {
    let re = compile_for(pat, "replace")?;
    let mut out = String::new();
    let mut last_end = 0usize;
    let mut search = 0usize;
    loop {
        let m = match re.captures_from_pos(s, search).map_err(|_| throw_str("replace: regex error"))? {
            Some(caps) => caps,
            None => break,
        };
        let whole = m.get(0).unwrap();
        let (ms, me) = (whole.start(), whole.end());
        out.push_str(&s[last_end..ms]);
        let replacement = build_replacement(it, &m, repl)?;
        out.push_str(&replacement);
        last_end = me;
        if first_only {
            break;
        }
        // Advance the scan; step one char past a zero-width match so we make
        // progress and emit the skipped char.
        if ms == me {
            let step = next_char_boundary(s, me);
            out.push_str(&s[me..step.min(s.len())]);
            last_end = step.min(s.len());
            if step > s.len() {
                break;
            }
            search = step;
        } else {
            search = me;
        }
        if search > s.len() {
            break;
        }
    }
    out.push_str(&s[last_end..]);
    Ok(Value::Str(Gc::new(out)))
}

/// Compute one match's replacement text: expand a `$N` template or call a fn.
fn build_replacement(
    it: &mut Interp,
    caps: &fancy_regex::Captures,
    repl: &Value,
) -> Result<String, Throw> {
    let n_groups = caps.len() - 1;
    match repl {
        Value::Str(tmpl) => expand_template(tmpl, caps),
        Value::Fn(_) | Value::Prim(_) | Value::PrimClosure(_) => {
            // The match arg mirrors re-find: string (no groups) or
            // [whole g1 ...] (groups).
            let arg = if n_groups == 0 {
                Value::Str(Gc::new(caps.get(0).unwrap().as_str().to_string()))
            } else {
                let mut items = Vec::with_capacity(caps.len());
                for i in 0..caps.len() {
                    items.push(match caps.get(i) {
                        Some(mm) => Value::Str(Gc::new(mm.as_str().to_string())),
                        None => Value::Nil,
                    });
                }
                Value::Vector(Gc::new(PVec::from_vec(items)))
            };
            let result = apply(it, repl, &[arg])?;
            let mut buf = String::new();
            arg_to_str(&mut buf, &result);
            Ok(buf)
        }
        _ => Err(throw_str("replace: replacement must be a string or function")),
    }
}

/// Expand a `$N` template: `$0` whole match, `$1`.. groups; `\$`/`\\` are
/// literal `$`/`\`. A `$N` past the last group throws MCT001 (matches mino).
fn expand_template(tmpl: &str, caps: &fancy_regex::Captures) -> Result<String, Throw> {
    let b = tmpl.as_bytes();
    let mut out = String::new();
    let mut i = 0;
    let n_groups = caps.len() - 1;
    while i < b.len() {
        match b[i] {
            b'\\' => {
                // `\X` -> literal X (Java escapes `\$` and `\\`).
                if i + 1 < b.len() {
                    out.push(b[i + 1] as char);
                    i += 2;
                } else {
                    out.push('\\');
                    i += 1;
                }
            }
            b'$' => {
                // `$N`: read the digit run.
                let mut j = i + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j == i + 1 {
                    // Bare `$` with no digits: literal `$`.
                    out.push('$');
                    i += 1;
                    continue;
                }
                let num: usize = tmpl[i + 1..j].parse().unwrap_or(usize::MAX);
                if num > n_groups {
                    return Err(throw_str_mct(
                        "str-replace: replacement references missing capture group",
                    ));
                }
                if let Some(m) = caps.get(num) {
                    out.push_str(m.as_str());
                }
                i = j;
            }
            _ => {
                // Copy one UTF-8 char.
                let ch = tmpl[i..].chars().next().unwrap();
                out.push(ch);
                i += ch.len_utf8();
            }
        }
    }
    Ok(out)
}

fn throw_str_mct(msg: &str) -> Throw {
    crate::error::throw_classified("eval/contract", "MCT001", msg)
}

fn ascii_upper(c: char) -> char {
    c.to_ascii_uppercase()
}
fn ascii_lower(c: char) -> char {
    c.to_ascii_lowercase()
}

fn one_string<'a>(args: &'a [Value], name: &str) -> Result<&'a str, Throw> {
    match args {
        [v] => as_str(v, name).map_err(|_| throw_str(&format!("{name} requires one string argument"))),
        _ => Err(throw_str(&format!("{name} requires one string argument"))),
    }
}

fn two_strings<'a>(args: &'a [Value], name: &str) -> Result<(&'a str, &'a str), Throw> {
    match args {
        [a, b] => Ok((
            as_str(a, name).map_err(|_| throw_str(&format!("{name} requires two string arguments")))?,
            as_str(b, name).map_err(|_| throw_str(&format!("{name} requires two string arguments")))?,
        )),
        _ => Err(throw_str(&format!("{name} requires two string arguments"))),
    }
}

/// Realize a collection into a Vec for `join` (mirrors collections::to_vec).
fn seq_items(v: &Value) -> Result<Vec<Value>, Throw> {
    match v {
        Value::Nil | Value::EmptyList => Ok(Vec::new()),
        Value::Cons(_) => {
            let mut out = Vec::new();
            let mut cur = v;
            while let Value::Cons(cell) = cur {
                out.push(cell.0.clone());
                cur = &cell.1;
            }
            Ok(out)
        }
        Value::Vector(vec) => Ok(vec.iter().cloned().collect()),
        Value::Set(set) => Ok(set.iter().cloned().collect()),
        Value::Str(s) => Ok(s.chars().map(Value::Char).collect()),
        Value::Map(m) => Ok(m
            .entries()
            .map(|(k, val)| {
                Value::Vector(Gc::new(PVec::from_vec(vec![k.clone(), val.clone()])))
            })
            .collect()),
        other => Err(throw_str(&format!(
            "don't know how to create seq from: {}",
            print_str(other)
        ))),
    }
}

/// JVM-style string split, char-based. `limit`: 0 trims trailing empties,
/// <0 keeps them, >0 caps piece count (last piece absorbs the rest). Empty
/// separator splits into individual codepoints.
fn split_string(s: &str, sep: &str, limit: i64) -> Vec<String> {
    // Empty input -> [""] (mino / JVM String.split).
    if s.is_empty() {
        return vec![String::new()];
    }
    let mut pieces: Vec<String> = Vec::new();
    if sep.is_empty() {
        // Split into codepoints; limit>0 caps with the final piece absorbing.
        let chars: Vec<char> = s.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if limit > 0 && pieces.len() as i64 + 1 == limit {
                pieces.push(chars[i..].iter().collect());
                return pieces;
            }
            pieces.push(chars[i].to_string());
            i += 1;
        }
        return pieces;
    }
    let mut rest = s;
    loop {
        if limit > 0 && pieces.len() as i64 + 1 == limit {
            pieces.push(rest.to_string());
            break;
        }
        match rest.find(sep) {
            Some(idx) => {
                pieces.push(rest[..idx].to_string());
                rest = &rest[idx + sep.len()..];
            }
            None => {
                pieces.push(rest.to_string());
                break;
            }
        }
    }
    if limit == 0 {
        while pieces.last().is_some_and(|p| p.is_empty()) {
            pieces.pop();
        }
    }
    pieces
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    fn ev(src: &str) -> String {
        let mut it = Interp::new();
        print_str(&it.eval_str(src).unwrap())
    }

    #[test]
    fn core_string_prims_match_binary() {
        // subs is codepoint-based (verified: mino binary).
        assert_eq!(ev("(subs \"hello\" 1 3)"), "\"el\"");
        assert_eq!(ev("(subs \"héllo\" 1 3)"), "\"él\"");
        assert_eq!(ev("(count \"héllo\")"), "5");
        assert_eq!(ev("(str 1 :a \"x\")"), "\"1:ax\"");
    }

    #[test]
    fn clojure_string_c_prims() {
        assert_eq!(ev("(clojure.string/upper-case \"abc\")"), "\"ABC\"");
        assert_eq!(ev("(clojure.string/lower-case \"ABC\")"), "\"abc\"");
        assert_eq!(ev("(clojure.string/trim \"  x \")"), "\"x\"");
        assert_eq!(ev("(clojure.string/includes? \"hello\" \"ell\")"), "true");
        assert_eq!(ev("(clojure.string/starts-with? \"hello\" \"hel\")"), "true");
        assert_eq!(ev("(clojure.string/ends-with? \"hello\" \"llo\")"), "true");
        assert_eq!(ev("(clojure.string/join \", \" [1 2 3])"), "\"1, 2, 3\"");
        assert_eq!(ev("(clojure.string/replace \"aaa\" \"a\" \"b\")"), "\"bbb\"");
        assert_eq!(ev("(clojure.string/replace \"a.b.c\" \".\" \"\")"), "\"abc\"");
        assert_eq!(ev("(clojure.string/replace-first \"a.b.c\" \".\" \"!\")"), "\"a!b.c\"");
    }

    #[test]
    fn split_edge_cases_match_binary() {
        assert_eq!(ev("(clojure.string/split \"a,b,c\" \",\")"), "[\"a\" \"b\" \"c\"]");
        assert_eq!(ev("(clojure.string/split \",,a,,\" \",\" 0)"), "[\"\" \"\" \"a\"]");
        assert_eq!(ev("(clojure.string/split \",,a,,\" \",\" -1)"), "[\"\" \"\" \"a\" \"\" \"\"]");
        assert_eq!(ev("(clojure.string/split \"abc\" \"\")"), "[\"a\" \"b\" \"c\"]");
        assert_eq!(ev("(clojure.string/split \"hél\" \"\")"), "[\"h\" \"é\" \"l\"]");
        assert_eq!(ev("(clojure.string/split \"abc\" \"\" 2)"), "[\"a\" \"bc\"]");
    }

    #[test]
    fn clojure_string_clj_wrappers() {
        // Defined by lib/clojure/string.clj on top of the C prims.
        assert_eq!(ev("(clojure.string/blank? \"  \")"), "true");
        assert_eq!(ev("(clojure.string/blank? nil)"), "true");
        assert_eq!(ev("(clojure.string/blank? \"x\")"), "false");
        assert_eq!(ev("(clojure.string/capitalize \"hello WORLD\")"), "\"Hello world\"");
        assert_eq!(ev("(clojure.string/capitalize 1)"), "\"1\"");
        assert_eq!(ev("(clojure.string/upper-case nil)"), "\"\"");
        assert_eq!(ev("(clojure.string/reverse \"a-test\")"), "\"tset-a\"");
        assert_eq!(ev("(clojure.string/triml \"  x \")"), "\"x \"");
        assert_eq!(ev("(clojure.string/trimr \" x  \")"), "\" x\"");
        assert_eq!(ev("(clojure.string/index-of \"hello\" \"ll\")"), "2");
        assert_eq!(ev("(clojure.string/last-index-of \"hello\" \"l\")"), "3");
        assert_eq!(ev("(clojure.string/re-quote-replacement \"a$1\\\\b\")"), "\"a\\\\$1\\\\\\\\b\"");
    }

    #[test]
    fn str_alias_reaches_clojure_string() {
        // `str/X` resolves via the `str -> clojure.string` alias; the two
        // collided names (reverse/replace) reach the string versions while
        // bare `reverse`/`replace` stay clojure.core's collection fns.
        assert_eq!(ev("(str/blank? \"  \")"), "true");
        assert_eq!(ev("(str/replace \"hello world\" \" \" \"-\")"), "\"hello-world\"");
        assert_eq!(ev("(str/replace \"abc\" \\b \\X)"), "\"aXc\""); // char match
        assert_eq!(ev("(str/replace \"abc\" \\b \"X\")"), "\"aXc\"");
        assert_eq!(ev("(str/replace-first \"a.b.c\" \".\" \"!\")"), "\"a!b.c\"");
        assert_eq!(ev("(str/replace-first \"abc\" \\b \"X\")"), "\"aXc\"");
        assert_eq!(ev("(str/reverse \"hello\")"), "\"olleh\"");
        assert_eq!(ev("(str/join \", \" [1 2 3])"), "\"1, 2, 3\"");
        assert_eq!(ev("(str/escape \"abc\" {\\a \"A_A\" \\c \"C_C\"})"), "\"A_AbC_C\"");
        assert_eq!(ev("(str/trim-newline \"ab\n\n\")"), "\"ab\"");
        assert_eq!(ev("(str/starts-with? nil \"x\")"), "false"); // as-str coerces
        // clojure.core collection reverse/replace are NOT shadowed.
        assert_eq!(ev("(reverse [1 2 3])"), "(3 2 1)");
        assert_eq!(ev("(replace {1 :a} [1 2 1])"), "[:a 2 :a]");
    }

    #[test]
    fn non_string_args_throw() {
        // blank?/reverse assert their arg is a string (str-*-throws deftests).
        let mut it = Interp::new();
        for e in [
            "(str/blank? 1)",
            "(str/blank? :a)",
            "(str/blank? (quote a))",
            "(str/reverse nil)",
            "(str/reverse 1)",
        ] {
            assert!(it.eval_str(e).is_err(), "expected throw: {e}");
        }
        assert_eq!(ev("(true? (str/blank? \"\"))"), "true");
        assert_eq!(ev("(false? (str/blank? \"hello\"))"), "true");
    }
}
