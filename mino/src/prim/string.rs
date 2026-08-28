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
        // Regex separator: Task 5.2.
        _ => return Err(throw_str("split: regex separators are not supported yet (Task 5.2)")),
    };
    let pieces = split_string(s, sep, limit);
    let pv = PVec::from_vec(pieces.into_iter().map(|p| Value::Str(Gc::new(p))).collect());
    Ok(Value::Vector(Gc::new(pv)))
}

/// `(clojure.string/replace s match repl)`: STRING match, single pass, all
/// occurrences. Char/regex `match` dispatch lives in the clojure.string
/// wrapper (char -> string here; regex -> Task 5.2 throw).
pub fn replace(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    str_replace(args, false)
}

/// `(clojure.string/replace-first s match repl)`: STRING match, first only.
pub fn replace_first(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    str_replace(args, true)
}

fn str_replace(args: &[Value], first_only: bool) -> Result<Value, Throw> {
    if args.len() != 3 {
        return Err(throw_str("str-replace requires three arguments"));
    }
    let s = as_str(&args[0], "str-replace: first argument")?;
    let m = match &args[1] {
        Value::Str(m) => m.as_str(),
        // Regex match: Task 5.2.
        _ => return Err(throw_str("str-replace: regex match is not supported yet (Task 5.2)")),
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
