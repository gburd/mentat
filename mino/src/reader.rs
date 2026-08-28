//! The reader: parse mino/Clojure source text into `Value` forms.
//! Ports `src/eval/read.c` (scanner + form dispatch) and `read_numeric.c`
//! (numeric literals). Task 0.3 covers the everyday literals; the reader
//! macros (`'` `` ` `` `~` `~@` `@`) expand to `(sym form)` cons lists,
//! matching mino.

use crate::symbol::Symbol;
use crate::value::Value;
use gc::Gc;
use std::fmt;

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
    let mut r = Reader { s: src.as_bytes(), i: 0 };
    r.skip_ws();
    let v = r.read_form()?;
    Ok((v, r.i))
}

/// Read every form in `src`.
pub fn read_all(src: &str) -> Result<Vec<Value>, ReadError> {
    let mut r = Reader { s: src.as_bytes(), i: 0 };
    let mut out = Vec::new();
    loop {
        r.skip_ws();
        if r.i >= r.s.len() {
            return Ok(out);
        }
        out.push(r.read_form()?);
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
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
            b'[' => self.read_seq(b']').map(|v| Value::Vector(Gc::new(v))),
            b'{' => self.read_map(),
            b')' | b']' | b'}' => Err(ReadError::Unexpected(c as char)),
            b'"' => self.read_string(),
            b'\'' => self.read_wrap("quote"),
            b'`' => self.read_wrap("quasiquote"),
            b'@' => self.read_wrap("deref"),
            b'~' => {
                self.i += 1;
                let name = if self.peek() == Some(b'@') {
                    self.i += 1;
                    "unquote-splicing"
                } else {
                    "unquote"
                };
                self.wrap_next(name)
            }
            b'#' => self.read_dispatch(),
            b'\\' => self.read_char_literal(),
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
            cons(inner, Value::Nil),
        ))
    }

    // read.c read_dispatch: `#{`-set. Phase 5: #foo tagged literals, #? reader
    // conditionals, ## special floats, #" regex, #' var-quote, #( anon-fn,
    // #_ discard, #: namespaced maps all slot in here.
    fn read_dispatch(&mut self) -> Result<Value, ReadError> {
        match self.s.get(self.i + 1).copied() {
            Some(b'{') => {
                self.i += 1; // consume '#'; read_seq consumes '{'
                self.read_seq(b'}').map(|v| Value::Set(Gc::new(v)))
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

    // read.c read_map_form: key/value pairs until '}'.
    fn read_map(&mut self) -> Result<Value, ReadError> {
        self.i += 1; // consume '{'
        let mut pairs = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => return Err(ReadError::Unterminated("map")),
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Value::Map(Gc::new(pairs)));
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
                    pairs.push((k, v));
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
        // Symbol.
        Ok(Value::Sym(parse_symbol(tok)?))
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

fn list_from_vec(items: Vec<Value>) -> Value {
    let mut acc = Value::Nil;
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
