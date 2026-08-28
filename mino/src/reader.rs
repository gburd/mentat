//! The reader: parse mino/Clojure source text into `Value` forms.
//! Ports `src/eval/read.c` (scanner + form dispatch) and `read_numeric.c`
//! (numeric literals). Task 0.3 covers the everyday literals; the reader
//! macros (`'` `` ` `` `~` `~@` `@`) expand to `(sym form)` cons lists,
//! matching mino.

use crate::symbol::Symbol;
use crate::value::Value;
use gc::Gc;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

// Global monotonic gensym counter (mino keeps this in S->gensym_counter).
// Two separate syntax-quote reads never collide because each `foo#` maps to
// a fresh suffix drawn from this counter.
static GENSYM_COUNTER: AtomicU64 = AtomicU64::new(0);

// One syntax-quote gensym scope. `suppress` frames (pushed by ~ / ~@) stop
// `foo#` rewriting inside unquotes; non-suppress frames (pushed by `) map
// each distinct `foo#` name to one `foo__N__auto__` gensym for the frame's
// lifetime, matching mino's per-syntax-quote GENSYM_ENV (read.c).
struct QqFrame {
    suppress: bool,
    entries: Vec<(String, String)>, // (name-without-#, replacement)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadError {
    Eof,
    Unexpected(char),
    Unterminated(&'static str),
    Malformed(String),
}

impl fmt::Display for ReadError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ReadError::Eof => write!(f, "unexpected end of input"),
            ReadError::Unexpected(c) => write!(f, "unexpected '{c}'"),
            ReadError::Unterminated(what) => write!(f, "unterminated {what}"),
            ReadError::Malformed(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for ReadError {}

/// Read one form. Returns the value and the number of bytes consumed
/// (leading whitespace/comments up to and including the form).
pub fn read_one(src: &str) -> Result<(Value, usize), ReadError> {
    let mut r = Reader { s: src.as_bytes(), i: 0, qq: Vec::new() };
    r.skip_ws();
    let v = r.read_form()?;
    Ok((v, r.i))
}

/// Read every form in `src`.
pub fn read_all(src: &str) -> Result<Vec<Value>, ReadError> {
    let mut r = Reader { s: src.as_bytes(), i: 0, qq: Vec::new() };
    let mut out = Vec::new();
    loop {
        r.skip_ws();
        if r.i >= r.s.len() {
            return Ok(out);
        }
        out.push(r.read_form()?);
    }
}

/// Read every top-level form, but on a read error record it and skip past the
/// offending form (via byte-level paren balance) so the rest of the file still
/// reads. Used by the resilient core.clj bootstrap: a handful of forms using
/// not-yet-ported reader syntax (e.g. `#"regex"`, Phase 5.2) must not block
/// the hundreds that read fine. Returns each slot as Ok(form) or Err(read err).
/// ponytail: resilient read; tighten to read_all once core.clj reads clean.
pub fn read_all_resilient(src: &str) -> Vec<Result<Value, ReadError>> {
    let bytes = src.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        // Skip inter-form whitespace/commas/comments.
        {
            let mut r = Reader { s: bytes, i, qq: Vec::new() };
            r.skip_ws();
            i = r.i;
        }
        if i >= bytes.len() {
            return out;
        }
        let mut r = Reader { s: bytes, i, qq: Vec::new() };
        match r.read_form() {
            Ok(v) => {
                i = r.i;
                out.push(Ok(v));
            }
            Err(e) => {
                // Skip past the whole offending top-level form so the next
                // form still reads. Balance parens byte-wise from `i`.
                let next = skip_top_form(bytes, i);
                i = next;
                out.push(Err(e));
            }
        }
    }
}

/// Byte-level scan past one top-level form starting at `b[i]` (already at a
/// non-whitespace byte). Balances (), [], {} while skipping strings, `\c`
/// char literals, and `;` comments. Mirrors the corpus harness scanner.
fn skip_top_form(b: &[u8], mut i: usize) -> usize {
    let open = b.get(i).copied();
    match open {
        Some(b'(') | Some(b'[') | Some(b'{') => {
            let mut depth = 0i32;
            while i < b.len() {
                match b[i] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' => {
                        depth -= 1;
                        i += 1;
                        if depth == 0 {
                            return i;
                        }
                        continue;
                    }
                    b'"' => {
                        i += 1;
                        while i < b.len() && b[i] != b'"' {
                            if b[i] == b'\\' {
                                i += 1;
                            }
                            i += 1;
                        }
                        i += 1; // closing quote
                        continue;
                    }
                    b';' => {
                        while i < b.len() && b[i] != b'\n' {
                            i += 1;
                        }
                        continue;
                    }
                    _ => {}
                }
                i += 1;
            }
            i
        }
        // Bare atom: run to the next whitespace/delimiter.
        _ => {
            while i < b.len() && !is_ws(b[i]) && !matches!(b[i], b'(' | b')' | b'[' | b']' | b'{' | b'}') {
                i += 1;
            }
            i
        }
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
    // Active syntax-quote gensym frames (read.c qq_gensym_top chain).
    qq: Vec<QqFrame>,
}

// read.c is_ws: space, tab, newline, CR, and comma-as-whitespace.
fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b',')
}

// read.c is_terminator: closes/opens a token boundary.
fn is_terminator(c: u8) -> bool {
    is_ws(c)
        || matches!(
            c,
            b'(' | b')' | b'[' | b']' | b'{' | b'}' | b'"' | b';' | b'`' | b'~' | b'@' | b'^'
        )
}

impl<'a> Reader<'a> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    // read.c skip_ws: whitespace, commas, and ';' line comments.
    fn skip_ws(&mut self) {
        while let Some(c) = self.peek() {
            if c == b';' {
                while let Some(c) = self.peek() {
                    if c == b'\n' {
                        break;
                    }
                    self.i += 1;
                }
            } else if is_ws(c) {
                self.i += 1;
            } else {
                return;
            }
        }
    }

    // read.c read_form_dispatch: dispatch on the first non-ws byte.
    fn read_form(&mut self) -> Result<Value, ReadError> {
        self.skip_ws();
        let c = self.peek().ok_or(ReadError::Eof)?;
        match c {
            b'(' => self.read_seq(b')').map(list_from_vec),
            b'[' => self
                .read_seq(b']')
                .map(|v| Value::Vector(Gc::new(crate::collections::vector::PVec::from_vec(v)))),
            b'{' => self.read_map(),
            b')' | b']' | b'}' => Err(ReadError::Unexpected(c as char)),
            b'"' => self.read_string(),
            b'\'' => self.read_wrap("quote"),
            b'`' => {
                // Push a gensym frame for the duration of the quoted form so
                // `foo#` inside it maps to a per-read auto-gensym (read.c).
                self.i += 1;
                self.qq.push(QqFrame { suppress: false, entries: Vec::new() });
                let r = self.wrap_next("quasiquote");
                self.qq.pop();
                r
            }
            b'@' => self.read_wrap("deref"),
            b'~' => {
                self.i += 1;
                let name = if self.peek() == Some(b'@') {
                    self.i += 1;
                    "unquote-splicing"
                } else {
                    "unquote"
                };
                // Inside a syntax-quote, ~ / ~@ suppress `foo#` rewriting
                // for the unquoted form (read.c pushes a suppress frame).
                if self.qq.is_empty() {
                    self.wrap_next(name)
                } else {
                    self.qq.push(QqFrame { suppress: true, entries: Vec::new() });
                    let r = self.wrap_next(name);
                    self.qq.pop();
                    r
                }
            }
            b'#' => self.read_dispatch(),
            b'\\' => self.read_char_literal(),
            b'^' => {
                // `^meta form`: read (and discard) the metadata, return the
                // target form. The port does not track value metadata yet, so
                // `^:private`/`^:dynamic`/etc. are read-and-dropped — they do
                // not change eval semantics for the forms core.clj uses.
                self.i += 1; // consume '^'
                let _meta = self.read_form()?;
                self.read_form()
            }
            _ => self.read_atom(),
        }
    }

    // `'x` / `` `x `` / `@x` -> (sym x).
    fn read_wrap(&mut self, sym: &str) -> Result<Value, ReadError> {
        self.i += 1;
        self.wrap_next(sym)
    }

    fn wrap_next(&mut self, sym: &str) -> Result<Value, ReadError> {
        let inner = self.read_form()?;
        Ok(cons(
            Value::Sym(Symbol::plain(sym)),
            cons(inner, Value::EmptyList),
        ))
    }

    // read.c read_dispatch: `#{`-set. Phase 5: #foo tagged literals, #? reader
    // conditionals, ## special floats, #" regex, #' var-quote, #( anon-fn,
    // #_ discard, #: namespaced maps all slot in here.
    fn read_dispatch(&mut self) -> Result<Value, ReadError> {
        match self.s.get(self.i + 1).copied() {
            Some(b'{') => {
                self.i += 1; // consume '#'; read_seq consumes '{'
                let items = self.read_seq(b'}')?;
                // mino: a set *literal* with a duplicate element is a read
                // error (verified against the binary: MRE008). `set`/`conj`
                // dedup, but the reader rejects `#{1 1 2}`.
                let mut set = crate::collections::map::PSet::empty();
                for e in items {
                    if set.contains(&e) {
                        return Err(ReadError::Malformed(
                            "set literal contains duplicate element".into(),
                        ));
                    }
                    set = set.conj(e);
                }
                Ok(Value::Set(Gc::new(set)))
            }
            // `#(body)` anon-fn: desugar to `(fn [%1..%N] body)`, normalizing
            // bare `%` to `%1`. Ports read_anon_fn_form (read.c).
            Some(b'(') => {
                self.i += 1; // consume '#'; read_seq consumes '('
                let items = self.read_seq(b')')?;
                let body = list_from_vec(items);
                let (max_arg, has_rest) = scan_percent(&body);
                let body = normalize_percent(&body);
                let mut params = Vec::new();
                for i in 1..=max_arg {
                    params.push(Value::Sym(Symbol::plain(&format!("%{i}"))));
                }
                if has_rest {
                    params.push(Value::Sym(Symbol::plain("&")));
                    params.push(Value::Sym(Symbol::plain("%&")));
                }
                let pv = crate::collections::vector::PVec::from_vec(params);
                Ok(cons(
                    Value::Sym(Symbol::plain("fn")),
                    cons(Value::Vector(Gc::new(pv)), cons(body, Value::EmptyList)),
                ))
            }
            Some(c) => Err(ReadError::Malformed(format!(
                "unsupported reader dispatch macro #{}",
                c as char
            ))),
            None => Err(ReadError::Eof),
        }
    }

    // read.c read_list_form / read_vector_form / read_set_form: read forms
    // until the matching close byte.
    fn read_seq(&mut self, close: u8) -> Result<Vec<Value>, ReadError> {
        self.i += 1; // consume the opener
        let mut out = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(ReadError::Unterminated("collection")),
                Some(c) if c == close => {
                    self.i += 1;
                    return Ok(out);
                }
                _ => out.push(self.read_form()?),
            }
        }
    }

    // read.c read_map_form: key/value pairs until '}'. Duplicate keys resolve
    // last-write-wins via PMap.assoc, first-insertion order preserved.
    fn read_map(&mut self) -> Result<Value, ReadError> {
        self.i += 1; // consume '{'
        let mut map = crate::collections::map::PMap::empty();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(ReadError::Unterminated("map")),
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Map(Gc::new(map)));
                }
                _ => {
                    let k = self.read_form()?;
                    self.skip_ws();
                    if self.peek() == Some(b'}') {
                        return Err(ReadError::Malformed(
                            "map literal must contain an even number of forms".into(),
                        ));
                    }
                    let v = self.read_form()?;
                    map = map.assoc(k, v);
                }
            }
        }
    }

    // read.c read_string_form: escapes \n \t \r \b \f \\ \" \uXXXX.
    fn read_string(&mut self) -> Result<Value, ReadError> {
        self.i += 1; // consume opening quote
        let mut out = String::new();
        while let Some(c) = self.peek() {
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(Value::Str(Gc::new(out)));
                }
                b'\\' => {
                    self.i += 1;
                    let e = self.peek().ok_or(ReadError::Unterminated("string literal"))?;
                    match e {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'\\' => out.push('\\'),
                        b'"' => out.push('"'),
                        b'u' => {
                            // \uXXXX: exactly four hex digits.
                            let hex = self
                                .s
                                .get(self.i + 1..self.i + 5)
                                .ok_or_else(|| {
                                    ReadError::Malformed(
                                        "\\u escape requires four hex digits".into(),
                                    )
                                })?;
                            let cp = u32::from_str_radix(
                                std::str::from_utf8(hex).map_err(|_| {
                                    ReadError::Malformed(
                                        "\\u escape requires four hex digits".into(),
                                    )
                                })?,
                                16,
                            )
                            .map_err(|_| {
                                ReadError::Malformed("\\u escape requires four hex digits".into())
                            })?;
                            let ch = char::from_u32(cp).ok_or_else(|| {
                                ReadError::Malformed("invalid \\u codepoint".into())
                            })?;
                            out.push(ch);
                            self.i += 4; // the four hex digits (the 'u' is consumed below)
                        }
                        other => {
                            return Err(ReadError::Malformed(format!(
                                "unsupported escape character: \\{}",
                                other as char
                            )));
                        }
                    }
                    self.i += 1; // consume the escape char (n/t/.../u)
                }
                _ => {
                    // Advance one whole UTF-8 codepoint.
                    let ch = self.next_char();
                    out.push(ch);
                }
            }
        }
        Err(ReadError::Unterminated("string literal"))
    }

    // read.c read_char_literal: \a, \newline, \space, \tab, \uXXXX, etc.
    fn read_char_literal(&mut self) -> Result<Value, ReadError> {
        self.i += 1; // consume '\'
        if self.i >= self.s.len() {
            return Err(ReadError::Unterminated("character literal"));
        }
        // The token is the run up to the next terminator; but a lone
        // terminator/ws char right after '\' is itself the literal (\{ \; \,).
        let start = self.i;
        let mut end = self.i;
        while end < self.s.len() && !is_terminator(self.s[end]) {
            end += 1;
        }
        let tok = &self.s[start..end];
        if tok.is_empty() {
            // \{ \( \space-as-\<space> etc.: take one raw byte.
            let ch = self.next_char();
            return Ok(Value::Char(ch));
        }
        let cp = match tok {
            b"space" => ' ',
            b"newline" => '\n',
            b"tab" => '\t',
            b"return" => '\r',
            b"backspace" => '\u{8}',
            b"formfeed" => '\u{c}',
            b"delete" => '\u{7f}',
            _ => {
                if tok[0] == b'u' && tok.len() == 5 {
                    let hex = std::str::from_utf8(&tok[1..5])
                        .ok()
                        .and_then(|h| u32::from_str_radix(h, 16).ok())
                        .and_then(char::from_u32)
                        .ok_or_else(|| {
                            ReadError::Malformed("invalid unicode character literal".into())
                        })?;
                    self.i = end;
                    return Ok(Value::Char(hex));
                }
                // A single (possibly multi-byte UTF-8) codepoint literal.
                let text = std::str::from_utf8(tok)
                    .map_err(|_| ReadError::Malformed("invalid character literal".into()))?;
                let mut chars = text.chars();
                let first = chars.next().unwrap();
                if chars.next().is_some() {
                    return Err(ReadError::Malformed(format!(
                        "unknown character literal: \\{text}"
                    )));
                }
                self.i = end;
                return Ok(Value::Char(first));
            }
        };
        self.i = end;
        Ok(Value::Char(cp))
    }

    // read.c read_atom: nil / true / false, numbers, keywords, symbols.
    fn read_atom(&mut self) -> Result<Value, ReadError> {
        let start = self.i;
        while self.i < self.s.len() && !is_terminator(self.s[self.i]) {
            self.i += 1;
        }
        let tok = std::str::from_utf8(&self.s[start..self.i])
            .map_err(|_| ReadError::Malformed("invalid token encoding".into()))?;
        if tok.is_empty() {
            return Err(ReadError::Unexpected(self.s[start] as char));
        }
        match tok {
            "nil" => return Ok(Value::Nil),
            "true" => return Ok(Value::Bool(true)),
            "false" => return Ok(Value::Bool(false)),
            _ => {}
        }
        // Keyword: ':name' or ':ns/name'. (Phase 5: '::auto-resolve'.)
        if let Some(body) = tok.strip_prefix(':') {
            if body.is_empty() {
                return Err(ReadError::Malformed("keyword missing name".into()));
            }
            return Ok(Value::Keyword(parse_symbol(body)?));
        }
        // Number before symbol: a token that starts with a digit (or sign+digit)
        // is numeric syntax; if it fails to parse it is malformed, not a symbol.
        if let Some(n) = try_parse_int(tok) {
            return Ok(Value::Int(n));
        }
        if let Some(f) = try_parse_float(tok) {
            return Ok(Value::Float(f));
        }
        let bytes = tok.as_bytes();
        let d = if bytes[0] == b'+' || bytes[0] == b'-' { 1 } else { 0 };
        if bytes.get(d).is_some_and(|c| c.is_ascii_digit()) {
            return Err(ReadError::Malformed(format!("invalid number: {tok}")));
        }
        // Inside an active (non-suppress) syntax-quote frame, a trailing-#
        // symbol resolves to its per-read auto-gensym (read.c
        // qq_gensym_resolve): `foo#` -> `foo__N__auto__`, same name -> same
        // gensym within the frame.
        if let Some(g) = self.qq_gensym_resolve(tok) {
            return Ok(Value::Sym(Symbol::plain(&g)));
        }
        // Symbol.
        Ok(Value::Sym(parse_symbol(tok)?))
    }

    // read.c qq_gensym_resolve: rewrite `foo#` inside a syntax-quote. Returns
    // None (plain-symbol path) when there is no active non-suppress frame,
    // the token is not a trailing-# name, or it is namespaced.
    fn qq_gensym_resolve(&mut self, tok: &str) -> Option<String> {
        let frame = self.qq.last_mut()?;
        if frame.suppress {
            return None;
        }
        let bytes = tok.as_bytes();
        if bytes.len() < 2 || bytes[bytes.len() - 1] != b'#' || tok.contains('/') {
            return None;
        }
        let base = &tok[..tok.len() - 1];
        if let Some((_, repl)) = frame.entries.iter().find(|(n, _)| n == base) {
            return Some(repl.clone());
        }
        let n = GENSYM_COUNTER.fetch_add(1, Ordering::Relaxed) + 1;
        let repl = format!("{base}__{n}__auto__");
        frame.entries.push((base.to_string(), repl.clone()));
        Some(repl)
    }

    // Decode and consume the next UTF-8 codepoint at self.i.
    fn next_char(&mut self) -> char {
        let rest = std::str::from_utf8(&self.s[self.i..]).unwrap_or("\u{fffd}");
        let ch = rest.chars().next().unwrap_or('\u{fffd}');
        self.i += ch.len_utf8();
        ch
    }
}

fn cons(car: Value, cdr: Value) -> Value {
    Value::Cons(Gc::new((car, cdr)))
}

// Scan an anon-fn body for `%` arg usage. Returns (max positional arg,
// has-rest). Bare `%` counts as `%1`; `%&` sets has_rest; `%N` sets max.
// Ports scan_percent_args (read.c).
fn scan_percent(form: &Value) -> (usize, bool) {
    let mut max = 0usize;
    let mut rest = false;
    scan_percent_walk(form, &mut max, &mut rest);
    (max, rest)
}

fn scan_percent_walk(form: &Value, max: &mut usize, rest: &mut bool) {
    match form {
        Value::Sym(s) if s.ns.is_none() => {
            let n = &*s.name;
            if n == "%" {
                if *max < 1 {
                    *max = 1;
                }
            } else if n == "%&" {
                *rest = true;
            } else if let Some(digits) = n.strip_prefix('%') {
                if let Ok(k) = digits.parse::<usize>() {
                    if k > *max {
                        *max = k;
                    }
                }
            }
        }
        Value::Cons(cell) => {
            scan_percent_walk(&cell.0, max, rest);
            scan_percent_walk(&cell.1, max, rest);
        }
        Value::Vector(v) => {
            for e in v.iter() {
                scan_percent_walk(e, max, rest);
            }
        }
        Value::Set(sset) => {
            for e in sset.iter() {
                scan_percent_walk(e, max, rest);
            }
        }
        Value::Map(m) => {
            for (k, val) in m.entries() {
                scan_percent_walk(k, max, rest);
                scan_percent_walk(val, max, rest);
            }
        }
        _ => {}
    }
}

// Rewrite bare `%` to `%1` throughout an anon-fn body. Ports normalize_percent.
fn normalize_percent(form: &Value) -> Value {
    match form {
        Value::Sym(s) if s.ns.is_none() && &*s.name == "%" => Value::Sym(Symbol::plain("%1")),
        Value::Cons(cell) => cons(normalize_percent(&cell.0), normalize_percent(&cell.1)),
        Value::Vector(v) => {
            let items: Vec<Value> = v.iter().map(normalize_percent).collect();
            Value::Vector(Gc::new(crate::collections::vector::PVec::from_vec(items)))
        }
        other => other.clone(),
    }
}

// Build a proper list from a slice; empty -> the empty-list value `()`
// (distinct from nil, matching mino's MINO_EMPTY_LIST).
fn list_from_vec(items: Vec<Value>) -> Value {
    let mut acc = Value::EmptyList;
    for e in items.into_iter().rev() {
        acc = cons(e, acc);
    }
    acc
}

// Split "ns/name" into a Symbol, matching mino/edn rules: a lone "/" is the
// division symbol (name "/"); otherwise the first '/' splits ns from name.
fn parse_symbol(tok: &str) -> Result<Symbol, ReadError> {
    if tok == "/" {
        return Ok(Symbol::plain("/"));
    }
    match tok.split_once('/') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => {
            Ok(Symbol::namespaced(ns, name))
        }
        Some(_) => Err(ReadError::Malformed(format!("malformed name: {tok}"))),
        None => Ok(Symbol::plain(tok)),
    }
}

// Plain decimal integer, optional sign. Phase 5: octal (0NNN), hex (0xFF),
// radix (2r101), ratio (a/b), bigint (42N), bigdec (1.5M) all live here.
fn try_parse_int(tok: &str) -> Option<i64> {
    tok.parse::<i64>().ok()
}

// Decimal float: must carry a '.' or exponent, else it's not a float token.
fn try_parse_float(tok: &str) -> Option<f64> {
    if !tok.bytes().any(|c| matches!(c, b'.' | b'e' | b'E')) {
        return None;
    }
    tok.parse::<f64>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::print_str;

    #[test]
    fn read_print_roundtrip() {
        for s in [
            "nil",
            "true",
            "42",
            "-7",
            "3.14",
            ":kw",
            ":a/b",
            "foo",
            "\"hi\"",
            "(1 2 3)",
            "[1 :k \"s\"]",
            "{:a 1 :b 2}",
            "#{1 2}",
            "'x",
            "(a (b c) d)",
            // Extra round-trip cases from the task.
            "{:a [1 2] :b #{:x}}",
            "`(a ~b ~@c)",
            "\"a\nb\"",
        ] {
            let (v, _) = read_one(s).unwrap();
            let printed = print_str(&v);
            let (v2, _) = read_one(&printed).unwrap();
            assert_eq!(print_str(&v2), printed, "roundtrip drift on {s}");
        }
    }

    #[test]
    fn char_and_escape_forms() {
        assert_eq!(print_str(&read_one("\\a").unwrap().0), "\\a");
        assert_eq!(print_str(&read_one("\\newline").unwrap().0), "\\newline");
        assert_eq!(print_str(&read_one("\\space").unwrap().0), "\\space");
        assert_eq!(print_str(&read_one("\\tab").unwrap().0), "\\tab");
        assert_eq!(print_str(&read_one("\\u0041").unwrap().0), "\\A");
        // String escapes round-trip as \n \t \" \\.
        assert_eq!(print_str(&read_one("\"a\\nb\"").unwrap().0), "\"a\\nb\"");
        assert_eq!(print_str(&read_one("\"x\\\"y\"").unwrap().0), "\"x\\\"y\"");
    }

    #[test]
    fn reader_macros_expand() {
        assert_eq!(print_str(&read_one("'x").unwrap().0), "(quote x)");
        assert_eq!(print_str(&read_one("`x").unwrap().0), "(quasiquote x)");
        assert_eq!(print_str(&read_one("~x").unwrap().0), "(unquote x)");
        assert_eq!(
            print_str(&read_one("~@x").unwrap().0),
            "(unquote-splicing x)"
        );
    }

    #[test]
    fn syntax_quote_autogensym() {
        // Inside a syntax-quote, `foo#` rewrites to `foo__N__auto__`, and the
        // same name maps to the same gensym within one read (matches the
        // mino binary: `(x# x#) => (x__N__auto__ x__N__auto__)).
        let printed = print_str(&read_one("`(x# x#)").unwrap().0);
        // (quasiquote (x__N__auto__ x__N__auto__))
        assert!(printed.contains("__auto__"), "no gensym: {printed}");
        let inner = &printed["(quasiquote (".len()..printed.len() - 2];
        let (a, b) = inner.split_once(' ').unwrap();
        assert_eq!(a, b, "same name -> same gensym: {printed}");
        assert!(a.starts_with("x__") && a.ends_with("__auto__"), "{a}");

        // Outside any syntax-quote, `x#` is a plain symbol (no rewrite).
        assert_eq!(print_str(&read_one("'x#").unwrap().0), "(quote x#)");

        // `~` suppresses gensym rewriting for the unquoted form.
        let s = print_str(&read_one("`(a ~x#)").unwrap().0);
        assert!(s.contains("x#") && !s.contains("__auto__"), "suppress: {s}");
    }

    #[test]
    fn whitespace_and_comments() {
        assert_eq!(print_str(&read_one("  , 42 ,").unwrap().0), "42");
        assert_eq!(
            print_str(&read_one("; a comment\n:kw").unwrap().0),
            ":kw"
        );
    }

    #[test]
    fn read_all_multiple_forms() {
        let forms = read_all("1 2 :three").unwrap();
        let printed: Vec<_> = forms.iter().map(print_str).collect();
        assert_eq!(printed, ["1", "2", ":three"]);
    }

    #[test]
    fn float_forms_roundtrip() {
        // Fixed, negative, scientific (small + large magnitude), integer-valued.
        for s in ["3.14", "-0.5", "1.0", "1.0E7", "1.5E-4", "100.0"] {
            let (v, _) = read_one(s).unwrap();
            let printed = print_str(&v);
            let (v2, _) = read_one(&printed).unwrap();
            assert_eq!(print_str(&v2), printed, "float drift on {s}");
            // Every printed float carries a decimal point so it re-reads as float.
            assert!(printed.contains('.'), "{printed} lost its decimal point");
        }
    }
}
