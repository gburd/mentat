//! Regex primitives. Ports the FOUR C prims in `src/prim/regex.c`
//! (`re-pattern`, `re-find`, `re-matches`, `re-find-from`); `re-seq`,
//! `re-matcher`, `re-groups`, and the matcher-aware `re-find` arity live in
//! `resources/core.clj` on top of these, exactly as in mino. `re-seq` works
//! (its `lazy-seq` is evaluated eagerly). `re-matcher`/`re-groups` LOAD but
//! throw when called: they are `atom`-backed and atoms are Phase 5.3 (not yet
//! ported), so stateful matching is deferred with the atoms task.
//!
//! ENGINE: `fancy-regex` (pure Rust) rather than the `regex` crate, because
//! mino's bundled engine supports BACKREFERENCES (`(a+)\1`, `(\w)\1`) which
//! the `regex` crate's DFA cannot express. fancy-regex is a backtracking
//! engine with backrefs, lazy quantifiers, `{n,m}`, inline flags `(?i)`,
//! `\b`, `\d\w\s`, and positional/named capture groups -- the full surface
//! mino's `tests/regex_test.clj` exercises.
//!
//! DEVIATIONS matched to mino: mino's engine now SUPPORTS lookahead `(?=`/`(?!`
//! (mino 9c65bb50); it still REJECTS lookbehind `(?<=`/`(?<!` and scoped flag
//! groups `(?flags:...)`. `uses_unsupported` below rejects those remaining
//! constructs so `(re-find #"(?<=a)b" "ab")` throws (MCT001), matching mino,
//! while `(re-find #"(?=a)" "a")` works via fancy-regex.

use crate::collections::vector::PVec;
use crate::error::{throw_classified, Throw};
use crate::eval::Interp;
use crate::value::{RegexVal, Value};
use fancy_regex::Regex;
use gc::Gc;

/// Pull the pattern source out of a `Value::Regex` or a bare `Value::Str`
/// (mino's `regex_source_view`: several call sites pass a string pattern).
fn pattern_source(v: &Value) -> Option<&str> {
    match v {
        Value::Str(s) => Some(s.as_str()),
        Value::Regex(r) => Some(&r.source),
        _ => None,
    }
}

/// Detect the constructs mino's engine rejects. Called before compiling so a
/// `#"(?<=a)b"` / `#"(?i:foo)"` throws MCT001 like mino, instead of quietly
/// working via fancy-regex. Scans for the `(?` prefixes; the check is
/// intentionally a substring scan (not a full parse) -- these operators are
/// unambiguous by their two/three-char sigil.
///
/// mino 9c65bb50 ADDED lookahead `(?=` / `(?!` (re_compile.c, zero-width
/// assertions), so those are NO LONGER rejected — fancy-regex implements them
/// natively. Lookbehind `(?<=` / `(?<!` is still rejected (upstream's flag
/// parser rejects it); a scoped flag group `(?flags:...)` is still rejected.
fn uses_unsupported(src: &str) -> bool {
    let b = src.as_bytes();
    let mut i = 0;
    while i + 1 < b.len() {
        if b[i] == b'(' && b[i + 1] == b'?' {
            match b.get(i + 2) {
                // Lookahead (?= (?! are now SUPPORTED (mino 9c65bb50): accept.
                Some(b'=') | Some(b'!') => {}
                // (?<= (?<! lookbehind stays rejected; (?<name> is a named group (kept).
                Some(b'<') => match b.get(i + 3) {
                    Some(b'=') | Some(b'!') => return true,
                    _ => {}
                },
                // Scoped flag group (?flags:...) -- flags then ':'. Distinguish
                // from a bare inline op (?flags) which has no ':' before ')'.
                _ => {
                    let mut j = i + 2;
                    let mut saw_flag = false;
                    while j < b.len() && matches!(b[j], b'i' | b's' | b'm' | b'x' | b'-') {
                        saw_flag = true;
                        j += 1;
                    }
                    if saw_flag && b.get(j) == Some(&b':') {
                        return true;
                    }
                }
            }
        }
        i += 1;
    }
    false
}

/// Compile a regex `Value` for the clojure.string ops (split/replace). Public
/// so `prim::string` can share the same mino-matching compile/reject path
/// (lookahead etc. throw MCT001). Returns the compiled matcher or a Throw.
pub fn compile_for(v: &Value, ctx: &str) -> Result<Regex, Throw> {
    compile(v, ctx)
}

/// Compile a regex, caching the result on the `RegexVal`. mino compiles at
/// match time and returns MCT001 on failure; likewise here. When the pattern
/// arrives as a bare string (no RegexVal to cache on), compile transiently.
fn compile(v: &Value, ctx: &str) -> Result<Regex, Throw> {
    let src = match v {
        Value::Regex(r) => {
            let cached = r.compiled.get_or_init(|| compile_source(&r.source));
            return cached.clone().map_err(|_| invalid(ctx));
        }
        Value::Str(s) => s.as_str(),
        _ => return Err(bad_pattern(ctx)),
    };
    compile_source(src).map_err(|_| invalid(ctx))
}

fn compile_source(src: &str) -> Result<Regex, String> {
    if uses_unsupported(src) {
        return Err("unsupported construct".to_string());
    }
    let clamped = clamp_repeat_counts(src);
    Regex::new(&clamped).map_err(|e| e.to_string())
}

/// Clamp `{n}` / `{n,}` / `{n,m}` repeat counts to 255, matching mino's engine
/// (which saturates the count at its 255 clamp). fancy-regex would otherwise
/// reject a count past its own limit and throw, whereas mino treats a huge
/// count as 255. Only rewrites a run of digits immediately inside a `{...}`
/// quantifier; a literal `{abc}` (no digits) is left untouched, staying a
/// literal brace.
fn clamp_repeat_counts(src: &str) -> String {
    let b = src.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'{' {
            // Try to parse `{digits}` or `{digits,}` or `{digits,digits}`.
            if let Some((rewritten, next)) = parse_and_clamp_brace(b, i) {
                out.extend_from_slice(rewritten.as_bytes());
                i = next;
                continue;
            }
        }
        // Copy one raw byte -- multibyte UTF-8 (e.g. `é`) is preserved because
        // only ASCII `{`/digits/`,`/`}` are rewritten.
        out.push(b[i]);
        i += 1;
    }
    // Safe: we only ever splice ASCII into an otherwise-unchanged UTF-8 stream.
    String::from_utf8(out).unwrap_or_else(|_| src.to_string())
}

/// If `b[i..]` opens a numeric `{...}` quantifier, return its clamped text and
/// the index just past the closing `}`; else None (leave the `{` as-is).
fn parse_and_clamp_brace(b: &[u8], i: usize) -> Option<(String, usize)> {
    let mut j = i + 1;
    let lo = read_clamped_uint(b, &mut j)?; // at least one digit required
    let mut result = format!("{{{lo}");
    if b.get(j) == Some(&b',') {
        result.push(',');
        j += 1;
        // Optional upper bound.
        if b.get(j).is_some_and(|c| c.is_ascii_digit()) {
            let hi = read_clamped_uint(b, &mut j)?;
            result.push_str(&hi.to_string());
        }
    }
    if b.get(j) == Some(&b'}') {
        result.push('}');
        Some((result, j + 1))
    } else {
        None
    }
}

/// Read a run of ASCII digits, returning the value clamped to 255. Advances
/// `*j` past the digits. None if no digit is present.
fn read_clamped_uint(b: &[u8], j: &mut usize) -> Option<u32> {
    let start = *j;
    let mut val: u32 = 0;
    while b.get(*j).is_some_and(|c| c.is_ascii_digit()) {
        val = val
            .saturating_mul(10)
            .saturating_add((b[*j] - b'0') as u32)
            .min(255);
        *j += 1;
    }
    if *j == start {
        None
    } else {
        Some(val)
    }
}

fn invalid(ctx: &str) -> Throw {
    throw_classified(
        "eval/contract",
        "MCT001",
        &format!("{ctx}: invalid regex pattern"),
    )
}

fn bad_pattern(ctx: &str) -> Throw {
    throw_classified(
        "eval/type",
        "MTY001",
        &format!("{ctx}: first argument must be a pattern (regex or string)"),
    )
}

/// Build the match result at a `fancy_regex::Captures`: a plain string when
/// the pattern has NO groups, or `[whole g1 g2 ...]` (nil for unmatched
/// groups) when it does. Ports `match_vector` + the groupless branch.
fn match_result(caps: &fancy_regex::Captures) -> Value {
    let n_groups = caps.len() - 1; // group 0 is the whole match
    let whole = caps.get(0).map(|m| m.as_str()).unwrap_or("");
    if n_groups == 0 {
        return Value::Str(Gc::new(whole.to_string()));
    }
    let mut items = Vec::with_capacity(caps.len());
    items.push(Value::Str(Gc::new(whole.to_string())));
    for i in 1..caps.len() {
        match caps.get(i) {
            Some(m) => items.push(Value::Str(Gc::new(m.as_str().to_string()))),
            None => items.push(Value::Nil),
        }
    }
    Value::Vector(Gc::new(PVec::from_vec(items)))
}

/// `(re-pattern s)` -- compile a regex from a string (no-op on an existing
/// regex). Storage only; the actual regex compile happens at match time, so
/// even a pattern mino would reject is stored here without error.
pub fn re_pattern(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [v @ Value::Regex(_)] => Ok(v.clone()),
        [Value::Str(s)] => Ok(Value::Regex(Gc::new(RegexVal::new((**s).clone())))),
        [_] => Err(throw_classified(
            "eval/type",
            "MTY001",
            "re-pattern: argument must be a string or regex",
        )),
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            "re-pattern requires one argument",
        )),
    }
}

/// `(re-find pattern text)` -- first match. Returns the matched string (no
/// groups), `[whole g1 g2 ...]` (groups), or nil. `(re-find re nil)` is nil.
pub fn re_find(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (pat, text) = two_args(args, "re-find")?;
    if pattern_source(pat).is_none() {
        return Err(bad_pattern("re-find"));
    }
    let text = match text {
        Value::Nil => return Ok(Value::Nil),
        Value::Str(s) => s.as_str(),
        _ => return Err(second_arg_string("re-find")),
    };
    let re = compile(pat, "re-find")?;
    match re.captures(text).map_err(|_| invalid("re-find"))? {
        Some(caps) => Ok(match_result(&caps)),
        None => Ok(Value::Nil),
    }
}

/// `(re-matches pattern text)` -- like re-find but the WHOLE string must
/// match. Returns the whole-match string (no groups), `[whole g1 ...]`
/// (groups), or nil.
pub fn re_matches(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (pat, text) = two_args(args, "re-matches")?;
    let src = match pattern_source(pat) {
        Some(s) => s,
        None => return Err(bad_pattern("re-matches")),
    };
    let text = match text {
        Value::Nil => return Ok(Value::Nil),
        Value::Str(s) => s.as_str(),
        _ => return Err(second_arg_string("re-matches")),
    };
    // Anchor to the whole string with \A...\z so the backtracker satisfies the
    // end anchor (fancy-regex is leftmost, not longest). `(?:...)` keeps group
    // numbering. Reject the unsupported constructs first (they'd be masked by
    // the wrapping otherwise).
    if uses_unsupported(src) {
        return Err(invalid("re-matches"));
    }
    let anchored = format!(r"\A(?:{})\z", clamp_repeat_counts(src));
    let re = Regex::new(&anchored).map_err(|_| invalid("re-matches"))?;
    match re.captures(text).map_err(|_| invalid("re-matches"))? {
        Some(caps) => Ok(match_result(&caps)),
        None => Ok(Value::Nil),
    }
}

/// `(re-find-from pattern text byte-start)` -- like re-find but starting the
/// scan at byte index `byte-start`; reports `[match abs-start abs-end]` (byte
/// offsets into the full text) or nil. Backs `re-seq` and `re-matcher` in
/// core.clj so they advance by real match position (and can represent a
/// zero-width match). Ports `prim_re_find_from`.
pub fn re_find_from(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 3 {
        return Err(throw_classified(
            "eval/arity",
            "MAR001",
            "re-find-from requires three arguments",
        ));
    }
    let pat = &args[0];
    if pattern_source(pat).is_none() {
        return Err(bad_pattern("re-find-from"));
    }
    let text = match &args[1] {
        Value::Nil => return Ok(Value::Nil),
        Value::Str(s) => s.as_str(),
        _ => return Err(second_arg_string("re-find-from")),
    };
    let byte_start = match &args[2] {
        Value::Int(n) => *n,
        _ => {
            return Err(throw_classified(
                "eval/type",
                "MTY001",
                "re-find-from: start must be an integer",
            ))
        }
    };
    if byte_start < 0 || byte_start as usize > text.len() {
        return Ok(Value::Nil);
    }
    let re = compile(pat, "re-find-from")?;
    // fancy-regex `captures_from_pos` scans forward from a byte position but
    // still reports absolute offsets into the full text.
    match re
        .captures_from_pos(text, byte_start as usize)
        .map_err(|_| invalid("re-find-from"))?
    {
        None => Ok(Value::Nil),
        Some(caps) => {
            let whole = caps.get(0).unwrap();
            let abs_start = whole.start() as i64;
            let abs_end = whole.end() as i64;
            let matched = match_result(&caps);
            let items = vec![matched, Value::Int(abs_start), Value::Int(abs_end)];
            Ok(Value::Vector(Gc::new(PVec::from_vec(items))))
        }
    }
}

fn two_args<'a>(args: &'a [Value], name: &str) -> Result<(&'a Value, &'a Value), Throw> {
    match args {
        [a, b] => Ok((a, b)),
        _ => Err(throw_classified(
            "eval/arity",
            "MAR001",
            &format!("{name} requires two arguments"),
        )),
    }
}

fn second_arg_string(name: &str) -> Throw {
    throw_classified(
        "eval/type",
        "MTY001",
        &format!("{name}: second argument must be a string"),
    )
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
    fn re_find_no_group_returns_string() {
        // Verified: mino `(re-find #"\d+" "a12b")` => "12".
        assert_eq!(ev(r#"(re-find #"\d+" "a12b")"#), "\"12\"");
    }

    #[test]
    fn re_find_with_groups_returns_vector() {
        // Verified: mino => ["12-34" "12" "34"].
        assert_eq!(
            ev(r#"(re-find #"(\d+)-(\d+)" "12-34")"#),
            "[\"12-34\" \"12\" \"34\"]"
        );
    }

    #[test]
    fn re_matches_nil_on_partial() {
        // Verified: mino `(re-matches #"\d+" "abc")` => nil; partial => nil.
        assert_eq!(ev(r#"(re-matches #"\d+" "abc")"#), "nil");
        assert_eq!(ev(r#"(re-matches #"\d+" "123abc")"#), "nil");
        assert_eq!(ev(r#"(re-matches #"\d+" "12345")"#), "\"12345\"");
    }

    #[test]
    fn re_matches_groups_vector() {
        // Verified: mino => ["a/b" "a" "b"].
        assert_eq!(
            ev(r#"(re-matches #"(.+)/(.+)" "a/b")"#),
            "[\"a/b\" \"a\" \"b\"]"
        );
    }

    #[test]
    fn re_seq_multiple_matches() {
        // Verified: mino `(count (re-seq #"\d+" "1 2 33 444"))` => 4.
        assert_eq!(ev(r#"(count (re-seq #"\d+" "1 2 33 444"))"#), "4");
    }

    #[test]
    fn backreference_supported() {
        // fancy-regex backrefs; verified mino => ["aaaa" "aa"].
        assert_eq!(ev(r#"(re-find #"(a+)\1" "aaaa")"#), "[\"aaaa\" \"aa\"]");
    }

    #[test]
    fn lookahead_supported_lookbehind_and_scoped_flags_rejected() {
        let mut it = Interp::new();
        // Lookahead now works (mino 9c65bb50): a zero-width positive lookahead.
        assert_eq!(ev(r#"(re-find #"a(?=b)" "ab")"#), "\"a\"");
        // Lookbehind and scoped flag groups still throw MCT001.
        for e in [
            r#"(re-find #"(?<=a)b" "ab")"#,
            r#"(re-find #"(?i:foo)" "FOO")"#,
        ] {
            assert!(it.eval_str(e).is_err(), "expected throw: {e}");
        }
    }

    #[test]
    fn regex_split_and_replace_match_binary() {
        // Verified against the mino binary.
        assert_eq!(
            ev("(clojure.string/split \"a1b2c\" #\"\\d\")"),
            "[\"a\" \"b\" \"c\"]"
        );
        assert_eq!(
            ev("(clojure.string/split \"abc\" #\"\")"),
            "[\"a\" \"b\" \"c\"]"
        );
        assert_eq!(
            ev("(clojure.string/split \"abc\" #\"\" 2)"),
            "[\"a\" \"bc\"]"
        );
        assert_eq!(
            ev("(clojure.string/split \"abc\" #\"x*y?\")"),
            "[\"a\" \"b\" \"c\"]"
        );
        assert_eq!(
            ev("(clojure.string/split \",,a,,\" #\",\")"),
            "[\"\" \"\" \"a\"]"
        );
        assert_eq!(
            ev("(clojure.string/split \",,a,,\" #\",\" -1)"),
            "[\"\" \"\" \"a\" \"\" \"\"]"
        );
        // replace: all matches; $N template; fn replacement; zero-width.
        assert_eq!(
            ev("(clojure.string/replace \"a1b2\" #\"\\d\" \"X\")"),
            "\"aXbX\""
        );
        assert_eq!(
            ev("(clojure.string/replace \"Hello\" #\"(\\w)\" \"[$1]\")"),
            "\"[H][e][l][l][o]\""
        );
        assert_eq!(
            ev("(clojure.string/replace \"a1b2\" #\"(\\w)(\\d)\" \"$2$1\")"),
            "\"1a2b\""
        );
        assert_eq!(
            ev("(clojure.string/replace \"abc\" #\"x*\" \"-\")"),
            "\"-a-b-c-\""
        );
        assert_eq!(
            ev("(clojure.string/replace \"a-bb\" #\"\\w+\" (fn [m] (str \"<\" m \">\")))"),
            "\"<a>-<bb>\""
        );
        assert_eq!(
            ev("(clojure.string/replace-first \"hello\" #\"l\" \"L\")"),
            "\"heLlo\""
        );
        assert_eq!(
            ev("(clojure.string/replace-first \"hello\" #\"(l)\" \"[$1]\")"),
            "\"he[l]lo\""
        );
    }

    #[test]
    fn regex_split_missing_group_throws() {
        // $N past the group count throws MCT001 (verified: mino).
        let mut it = Interp::new();
        assert!(it
            .eval_str("(clojure.string/replace \"Ax\" #\"A\" \"$1x\")")
            .is_err());
    }

    #[test]
    fn regex_literal_roundtrips() {
        // `ev` prints the pr-str/str RESULT string, so quotes/backslashes are
        // escaped once more. Verified against mino:
        //   mino -e '(pr-str #"a\d+")' => "#\"a\\d+\""
        //   mino -e '(str #"a\d+")'    => "a\\d+"
        // Regex source is the 4 chars: a \ d +.
        let mut it = Interp::new();
        let pr = it.eval_str("(pr-str #\"a\\d+\")").unwrap();
        assert_eq!(print_str(&pr), "\"#\\\"a\\\\d+\\\"\"");
        let s = it.eval_str("(str #\"a\\d+\")").unwrap();
        assert_eq!(print_str(&s), "\"a\\\\d+\"");
    }
}
