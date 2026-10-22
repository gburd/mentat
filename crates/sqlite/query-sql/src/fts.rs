// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! `(fulltext ...)` on DuckDB: SQLite FTS4's `MATCH` as plain DuckDB SQL.
//!
//! The FTS4 query (enhanced syntax: `AND`, `OR`, `NOT`, `NEAR`, implicit
//! AND, parentheses, `"phrases"`, `prefix*`, `^first`, `text:`) is lexed as
//! SQLite's `fts3_expr.c` reads it and parsed with its precedence (NEAR, NOT,
//! AND, OR). Each phrase becomes a regex over the text's tokens, found in SQL
//! with SQLite's own `unicode61` (`remove_diacritics=0`) token characters: a
//! letter or number by SQLite's table (`fts_unicode61`), then more of those
//! or one of the 25 diacritics it keeps in a token (within U+0300-U+0331). A
//! text is matched as `' tok1 tok2 ... '`, so a phrase is a substring match
//! on whole tokens.
//!
//! shortcut: where this still differs from SQLite FTS4 (use the SQLite
//! engine, or DuckDB's `fts` extension, if one matters):
//! - `NEAR` / `NEAR/n` is AND: no distance check;
//! - no ranking: scores are 0.0, as on SQLite;
//! - case folding is DuckDB's `lower()` plus the 12 folds SQLite makes that
//!   it doesn't (final `ς` to `σ`, `ϐ` to `β`, ...). `lower()` also folds
//!   about 400 characters SQLite leaves as they are: `İ` (to `i`), Cherokee,
//!   Georgian Mtavruli, and capitals added to Unicode since 6.1;
//! - `searchid:x` matches nothing (SQLite searches its internal search-id
//!   column), and a column filter right before a quote or parenthesis
//!   (`text:"a b"`) is dropped (SQLite then skips the rest of that level);
//! - a text is read whole; SQLite's tokenizer stops at a NUL;
//! - parentheses nest at most 100 deep (SQLite: 1000), and a too-deep query
//!   fails with "malformed MATCH expression" (SQLite: "FTS expression tree is
//!   too large").

use core_traits::TypedValue;
use sql_traits::errors::{BuildQueryResult, SQLError};

use mentat_sql::QueryBuilder;

use crate::fts_unicode61::{ALNUM_ASCII, ALNUM_EXCEPT};
use crate::{push_comparand, ColumnOrExpression, Comparand};

/// One word of a phrase: its token, whether it is a prefix (`tok*`), and
/// whether it must be the text's first token (`^tok`).
#[derive(Debug, PartialEq)]
struct Word {
    token: String,
    prefix: bool,
    first: bool,
}

#[derive(Debug, PartialEq)]
enum Lexeme {
    Open,
    Close,
    And,
    Or,
    Not,
    Near,
    /// A quoted phrase or a bare word's token; `""` has no words.
    Phrase(Vec<Word>),
}

#[derive(Debug, PartialEq)]
enum Node {
    /// No terms at all (``, `---`, `()`): matches nothing.
    Empty,
    /// No words (`""`) matches nothing.
    Phrase(Vec<Word>),
    /// Two or more operands (a chain of one operator is one node, so a long
    /// query never nests deeply).
    And(Vec<Node>),
    Or(Vec<Node>),
    /// `a NEAR b ...`, phrases only; matched as AND (see the module docs).
    Near(Vec<Node>),
    Not(Box<Node>, Box<Node>),
}

/// SQLite's limits: the tree must rebalance within
/// `SQLITE_FTS3_MAX_EXPR_DEPTH` (12) levels, and parentheses nest at most
/// `SQLITE_MAX_EXPR_DEPTH` (1000) deep; past either the query is malformed.
/// Checked while parsing, so a hostile query can't overflow the stack.
// shortcut: parentheses nest at most 100 deep here (1000 levels of this
// recursive parser overflow a 2 MB thread stack in a debug build). Past 12
// they can only wrap a single operand; make the parser iterative if that
// ever matters.
const MAX_PARENS: usize = 100;
const MAX_DEPTH: u32 = 12;

impl Node {
    /// The tree's depth after SQLite's `fts3ExprBalance(_, m)`, or None if
    /// SQLite rejects it. An AND (or OR) chain's operands, each balanced with
    /// `m - 1`, are merged pairwise like a binary counter of `m` slots (more
    /// than `2^m - 1` operands overflow it); a NOT costs one level, a NEAR
    /// chain one per NEAR; nothing may sit at level `m == 0`.
    fn balanced_depth(&self, m: u32) -> Option<u32> {
        if m == 0 {
            return None;
        }
        match self {
            Node::Empty | Node::Phrase(_) => Some(0),
            Node::Near(xs) => Some(xs.len() as u32 - 1),
            Node::Not(l, r) => Some(1 + l.balanced_depth(m - 1)?.max(r.balanced_depth(m - 1)?)),
            Node::And(xs) | Node::Or(xs) => {
                let mut slots: Vec<Option<u32>> = vec![None; m as usize];
                for x in xs {
                    let mut carry = Some(x.balanced_depth(m - 1)?);
                    for slot in slots.iter_mut() {
                        match (slot.take(), carry) {
                            (None, c) => {
                                *slot = c;
                                carry = None;
                                break;
                            }
                            (Some(d), Some(c)) => carry = Some(1 + d.max(c)),
                            (Some(_), None) => unreachable!(),
                        }
                    }
                    carry.is_none().then_some(())?;
                }
                slots.into_iter().flatten().reduce(|acc, d| 1 + acc.max(d))
            }
        }
    }
}

/// `l AND r` (`and`) or `l OR r` as one chain node. An operand that is itself
/// the same kind of chain (`(a b) c`) is spliced in, as SQLite's rebalancing
/// does.
fn chain(l: Node, r: Node, and: bool) -> Node {
    let same = |n: Node| match n {
        Node::And(xs) if and => Ok(xs),
        Node::Or(xs) if !and => Ok(xs),
        n => Err(n),
    };
    let mut xs = same(l).unwrap_or_else(|l| vec![l]);
    match same(r) {
        Ok(ys) => xs.extend(ys),
        Err(r) => xs.push(r),
    }
    if and {
        Node::And(xs)
    } else {
        Node::Or(xs)
    }
}

fn malformed(q: &str) -> SQLError {
    SQLError::MalformedMatch(q.to_string())
}

/// unicode61's token characters: letters and numbers by SQLite's table
/// (`sqlite3FtsUnicodeIsalnum`).
fn is_alnum(c: char) -> bool {
    let c = c as u32;
    if c < 128 {
        return ALNUM_ASCII[(c >> 5) as usize] & (1 << (c & 31)) == 0;
    }
    if c >= 1 << 22 {
        return true;
    }
    let key = (c << 10) | 0x3FF;
    let e = ALNUM_EXCEPT[ALNUM_EXCEPT.partition_point(|&e| e <= key) - 1];
    c >= (e >> 10) + (e & 0x3FF)
}

/// The combining marks unicode61 keeps inside a token, but never starts one
/// with (`sqlite3FtsUnicodeIsdiacritic`): 25 of U+0300-U+0331.
fn is_diacritic(c: char) -> bool {
    let c = c as u32;
    match c {
        768..=799 => (0x08029FDF_u32 >> (c - 768)) & 1 == 1,
        800..=817 => (0x000361F8_u32 >> (c - 800)) & 1 == 1,
        _ => false,
    }
}

/// unicode61's tokens in `s`, as byte ranges.
fn tokens(s: &str) -> impl Iterator<Item = (usize, usize)> + '_ {
    let mut at = 0;
    std::iter::from_fn(move || {
        let start = at + s[at..].find(is_alnum)?;
        let len = s[start..]
            .find(|c| !(is_alnum(c) || is_diacritic(c)))
            .unwrap_or(s.len() - start);
        at = start + len;
        Some((start, at))
    })
}

/// The token at `start..end` of `s`, with its `*` (right after it) and `^`
/// (right before it).
fn word(s: &str, (start, end): (usize, usize)) -> Word {
    Word {
        token: s[start..end].to_string(),
        prefix: s[end..].starts_with('*'),
        first: s[..start].ends_with('^'),
    }
}

/// SQLite's whitespace in a query (`fts3isspace`): ASCII only.
fn is_space(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0b' | '\x0c')
}

/// The keyword at the start of `rest` and its length: upper case, and
/// followed by whitespace, a quote, a parenthesis or the end.
fn keyword(rest: &str) -> Option<(Lexeme, usize)> {
    let b = rest.as_bytes();
    let (lexeme, mut n) = [
        ("OR", Lexeme::Or),
        ("AND", Lexeme::And),
        ("NOT", Lexeme::Not),
        ("NEAR", Lexeme::Near),
    ]
    .into_iter()
    .find(|(k, _)| b.starts_with(k.as_bytes()))
    .map(|(k, l)| (l, k.len()))?;
    if lexeme == Lexeme::Near && b.get(4) == Some(&b'/') && b.get(5).is_some_and(u8::is_ascii_digit)
    {
        n += 1 + b[5..].iter().take_while(|c| c.is_ascii_digit()).count();
    }
    match b.get(n) {
        None | Some(b'"' | b'(' | b')') => Some((lexeme, n)),
        Some(&c) if is_space(c as char) => Some((lexeme, n)),
        _ => None,
    }
}

/// The query's lexemes, read as SQLite's `getNextNode` does. A bare word is
/// the first token in the rest of the query before any quote or parenthesis,
/// so punctuation splits it: `hello-world` is `hello` then `world`, `a*b` is
/// `a*` then `b`, and `--- NOT a` is the words `not` and `a`. Text with no
/// token adds nothing, and `*` after a closing quote is skipped.
fn lex(q: &str) -> Result<Vec<Lexeme>, SQLError> {
    // SQLite reads the query as a C string.
    let q = q.split('\0').next().unwrap_or_default();
    let mut out = Vec::new();
    let mut i = 0;
    loop {
        i = q[i..].find(|c| !is_space(c)).map_or(q.len(), |n| i + n);
        let rest = &q[i..];
        if rest.is_empty() {
            return Ok(out);
        }
        if let Some((lexeme, n)) = keyword(rest) {
            out.push(lexeme);
            i += n;
            continue;
        }
        match rest.as_bytes()[0] {
            b'"' => {
                let len = rest[1..].find('"').ok_or_else(|| malformed(q))?;
                let inner = &rest[1..1 + len];
                out.push(Lexeme::Phrase(
                    tokens(inner).map(|t| word(inner, t)).collect(),
                ));
                i += len + 2;
                continue;
            }
            b'(' => {
                out.push(Lexeme::Open);
                i += 1;
                continue;
            }
            b')' => {
                out.push(Lexeme::Close);
                i += 1;
                continue;
            }
            _ => {}
        }
        // A column filter (`text:` is the one text column).
        let column = ["text", "searchid"].into_iter().find(|c| {
            rest.len() > c.len()
                && rest.as_bytes()[c.len()] == b':'
                && rest.as_bytes()[..c.len()].eq_ignore_ascii_case(c.as_bytes())
        });
        let skip = column.map_or(0, |c| c.len() + 1);
        let rest = &rest[skip..];
        // The first token, unless a quote or parenthesis comes first (then
        // the text before it is skipped).
        let stop = rest
            .find(|c| is_alnum(c) || matches!(c, '"' | '(' | ')'))
            .unwrap_or(rest.len());
        let consumed = if rest[stop..].starts_with(is_alnum) {
            let (_, end) = tokens(&rest[stop..]).next().expect("starts with a token");
            let w = word(rest, (stop, stop + end));
            let n = stop + end + usize::from(w.prefix);
            // `searchid:x` searches SQLite's search ids: never a match here.
            out.push(Lexeme::Phrase(match column {
                Some("searchid") => Vec::new(),
                _ => vec![w],
            }));
            n
        } else {
            stop
        };
        i += skip + consumed;
    }
}

/// Precedence, as FTS4's enhanced syntax: NEAR, then NOT, then AND (explicit
/// or implicit), then OR; all left-associative.
struct Parser<'q> {
    lexemes: Vec<Lexeme>,
    pos: usize,
    q: &'q str,
    /// Open parentheses around the current position.
    parens: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Lexeme> {
        self.lexemes.get(self.pos)
    }

    fn or(&mut self) -> Result<Node, SQLError> {
        let mut left = self.and()?;
        while self.peek() == Some(&Lexeme::Or) {
            self.pos += 1;
            let right = self.and()?;
            if left == Node::Empty || right == Node::Empty {
                return Err(malformed(self.q));
            }
            left = chain(left, right, false);
        }
        Ok(left)
    }

    fn and(&mut self) -> Result<Node, SQLError> {
        let mut left = self.not()?;
        loop {
            match self.peek() {
                Some(Lexeme::And) => self.pos += 1,
                Some(Lexeme::Open | Lexeme::Phrase(_)) => {}
                _ => return Ok(left),
            }
            let right = self.not()?;
            left = match (left, right) {
                (Node::Empty, r) => r,
                (l, Node::Empty) => l,
                (l, r) => chain(l, r, true),
            };
        }
    }

    fn not(&mut self) -> Result<Node, SQLError> {
        let mut left = self.near()?;
        let mut nots = 0;
        while self.peek() == Some(&Lexeme::Not) {
            self.pos += 1;
            let right = self.near()?;
            if right == Node::Empty {
                return Err(malformed(self.q));
            }
            if left != Node::Empty {
                left = Node::Not(Box::new(left), Box::new(right));
                // A NOT chain nests; stop while it is still shallow.
                nots += 1;
                if nots >= MAX_DEPTH {
                    return Err(malformed(self.q));
                }
            }
        }
        Ok(left)
    }

    /// `a NEAR b NEAR c`: phrases only, not parenthesized groups.
    fn near(&mut self) -> Result<Node, SQLError> {
        let first = self.primary()?;
        if self.peek() != Some(&Lexeme::Near) {
            return Ok(first);
        }
        let mut xs = vec![first];
        while self.peek() == Some(&Lexeme::Near) {
            self.pos += 1;
            xs.push(self.primary()?);
            // A NEAR chain nests (one level per NEAR); stop past the limit.
            if xs.len() > MAX_DEPTH as usize + 1 {
                return Err(malformed(self.q));
            }
        }
        if !xs.iter().all(|x| matches!(x, Node::Phrase(_))) {
            return Err(malformed(self.q));
        }
        Ok(Node::Near(xs))
    }

    fn primary(&mut self) -> Result<Node, SQLError> {
        let lexeme = self.lexemes.get_mut(self.pos);
        match lexeme {
            Some(Lexeme::Phrase(words)) => {
                let words = std::mem::take(words);
                self.pos += 1;
                Ok(Node::Phrase(words))
            }
            Some(Lexeme::Open) => {
                self.pos += 1;
                if self.peek() == Some(&Lexeme::Close) {
                    self.pos += 1;
                    return Ok(Node::Empty);
                }
                self.parens += 1;
                if self.parens > MAX_PARENS {
                    return Err(malformed(self.q));
                }
                let inner = self.or()?;
                if self.peek() != Some(&Lexeme::Close) {
                    return Err(malformed(self.q));
                }
                self.pos += 1;
                self.parens -= 1;
                Ok(inner)
            }
            _ => Err(malformed(self.q)),
        }
    }
}

fn parse(q: &str) -> Result<Node, SQLError> {
    let mut p = Parser {
        lexemes: lex(q)?,
        pos: 0,
        q,
        parens: 0,
    };
    if p.lexemes.is_empty() {
        return Ok(Node::Empty);
    }
    let node = p.or()?;
    if p.pos != p.lexemes.len() || node.balanced_depth(MAX_DEPTH).is_none_or(|d| d > MAX_DEPTH) {
        return Err(malformed(q));
    }
    Ok(node)
}

/// The case folds SQLite's unicode61 makes that DuckDB's `lower()` doesn't,
/// as `translate()` arguments.
const FOLD_FROM: &str =
    "\u{b5}\u{17f}\u{3c2}\u{3d0}\u{3d1}\u{3d5}\u{3d6}\u{3f0}\u{3f1}\u{3f5}\u{1e9b}\u{1fbe}";
const FOLD_TO: &str =
    "\u{3bc}s\u{3c3}\u{3b2}\u{3b8}\u{3c6}\u{3c0}\u{3ba}\u{3c1}\u{3b5}\u{1e61}\u{3b9}";

/// An RE2 pattern for one unicode61 token, in SQLite's character classes.
/// Only `\x{..}` escapes, `[`, `]`, `-` and `*`: it can sit in a SQL string
/// literal.
fn token_re() -> &'static str {
    static RE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        for c in (0..=0x10FFFF_u32).filter(|&c| char::from_u32(c).is_some_and(is_alnum)) {
            match ranges.last_mut() {
                Some((_, end)) if *end + 1 == c => *end = c,
                _ => ranges.push((c, c)),
            }
        }
        let class: String = ranges
            .iter()
            .map(|&(a, b)| match a == b {
                true => format!(r"\x{{{a:x}}}"),
                false => format!(r"\x{{{a:x}}}-\x{{{b:x}}}"),
            })
            .collect();
        let marks: String = (0x300..=0x331_u32)
            .filter(|&c| char::from_u32(c).is_some_and(is_diacritic))
            .map(|c| format!(r"\x{{{c:x}}}"))
            .collect();
        format!("[{class}][{class}{marks}]*")
    })
}

/// `x` case-folded as unicode61 does (see the module docs).
fn push_folded(out: &mut dyn QueryBuilder, x: &ColumnOrExpression) -> BuildQueryResult {
    out.push_sql("translate(lower(");
    push_comparand(out, x, Comparand::Member("v_s"))?;
    out.push_sql(&format!("), '{FOLD_FROM}', '{FOLD_TO}')"));
    Ok(())
}

/// `x`'s tokens, as one list. A `v` column (a search term bound by another
/// pattern) is read through its plain string copy.
fn push_tokens(out: &mut dyn QueryBuilder, x: &ColumnOrExpression) -> BuildQueryResult {
    out.push_sql("regexp_extract_all(");
    push_folded(out, x)?;
    out.push_sql(&format!(", '{}')", token_re()));
    Ok(())
}

/// `x`'s tokens as `' tok1 tok2 '`.
fn push_token_text(out: &mut dyn QueryBuilder, x: &ColumnOrExpression) -> BuildQueryResult {
    out.push_sql("(' ' || array_to_string(");
    push_tokens(out, x)?;
    out.push_sql(", ' ') || ' ')");
    Ok(())
}

fn push_node(
    out: &mut dyn QueryBuilder,
    text: &ColumnOrExpression,
    node: &Node,
) -> BuildQueryResult {
    match node {
        Node::Empty => out.push_sql("false"),
        // No words (`""`), or `^` on a word after the first: no text matches.
        Node::Phrase(words) if words.is_empty() || words.iter().skip(1).any(|w| w.first) => {
            out.push_sql("false")
        }
        Node::Phrase(words) => {
            // Each word is one token: token characters only, never a regex
            // metacharacter (those are ASCII punctuation), so it is spliced
            // into the pattern. It is folded and inlined as a SQL literal,
            // not bound: the pattern is then a constant DuckDB folds, so the
            // regex compiles once, not per row.
            out.push_sql("regexp_matches(");
            push_token_text(out, text)?;
            out.push_sql(if words[0].first { ", '^ '" } else { ", ' '" });
            for w in words {
                let lit = format!("'{}'", w.token.replace('\'', "''"));
                out.push_sql(&format!(
                    " || translate(lower({lit}), '{FOLD_FROM}', '{FOLD_TO}')"
                ));
                out.push_sql(if w.prefix { " || '[^ ]* '" } else { " || ' '" });
            }
            out.push_sql(")");
        }
        Node::And(xs) | Node::Or(xs) | Node::Near(xs) => {
            let op = if matches!(node, Node::Or(_)) {
                " OR "
            } else {
                " AND "
            };
            out.push_sql("(");
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_sql(op);
                }
                push_node(out, text, x)?;
            }
            out.push_sql(")");
        }
        Node::Not(l, r) => {
            out.push_sql("(");
            push_node(out, text, l)?;
            out.push_sql(" AND NOT ");
            push_node(out, text, r)?;
            out.push_sql(")");
        }
    }
    Ok(())
}

/// `text MATCH query` on DuckDB. A constant query is parsed as FTS4 syntax; a
/// query taken from another column at run time matches when the text has all
/// of its tokens (FTS4's reading of plain words).
pub(crate) fn push_duckdb_match(
    out: &mut dyn QueryBuilder,
    text: &ColumnOrExpression,
    query: &ColumnOrExpression,
) -> BuildQueryResult {
    if let ColumnOrExpression::Value(TypedValue::String(q)) = query {
        return push_node(out, text, &parse(q)?);
    }
    out.push_sql("(list_has_all(");
    push_tokens(out, text)?;
    out.push_sql(", ");
    push_tokens(out, query)?;
    out.push_sql(") AND len(");
    push_tokens(out, query)?;
    out.push_sql(") > 0)");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(q: &str) -> String {
        match parse(q) {
            Ok(n) => show(&n),
            Err(_) => "ERR".into(),
        }
    }

    fn show(n: &Node) -> String {
        let list = |xs: &[Node]| xs.iter().map(show).collect::<Vec<_>>().join(" ");
        match n {
            Node::Empty => "()".into(),
            Node::Phrase(ws) if ws.is_empty() => "\"\"".into(),
            Node::Phrase(ws) => ws
                .iter()
                .map(|w| {
                    format!(
                        "{}{}{}",
                        if w.first { "^" } else { "" },
                        w.token.to_lowercase(),
                        if w.prefix { "*" } else { "" }
                    )
                })
                .collect::<Vec<_>>()
                .join("_"),
            Node::And(xs) => format!("(& {})", list(xs)),
            Node::Or(xs) => format!("(| {})", list(xs)),
            Node::Near(xs) => format!("(~ {})", list(xs)),
            Node::Not(l, r) => format!("(- {} {})", show(l), show(r)),
        }
    }

    /// Shapes checked against SQLite 3.51 FTS4 (enhanced syntax,
    /// `py/fts4.py`).
    #[test]
    fn parses_like_fts4() {
        for (q, want) in [
            ("a OR b c", "(| a (& b c))"),
            ("a b OR c", "(| (& a b) c)"),
            ("a NOT b c", "(& (- a b) c)"),
            ("a NOT b NOT c", "(- (- a b) c)"),
            ("a NOT (b c)", "(- a (& b c))"),
            ("(a OR b) NOT c", "(- (| a b) c)"),
            ("a AND b", "(& a b)"),
            ("a NEAR/3 b", "(~ a b)"),
            // NEAR binds tighter than NOT.
            ("a NOT b NEAR c", "(- a (~ b c))"),
            ("\"hel* dark*\"", "hel*_dark*"),
            // `*` after a closing quote is ignored.
            ("\"a b\"*", "a_b"),
            ("a\"b\"", "(& a b)"),
            ("a(b)", "(& a b)"),
            // Punctuation splits a bare word into an AND of its tokens.
            ("hello-world", "(& hello world)"),
            ("don't", "(& don t)"),
            ("a*b", "(& a* b)"),
            ("hello. OR world", "(& hello or world)"),
            ("C++ OR java", "(& c or java)"),
            ("a and b", "(& a and b)"),
            ("^a b", "(& ^a b)"),
            ("text:abc", "abc"),
            ("TEXT:abc", "abc"),
            ("searchid:1", "\"\""),
            // Token-less terms: dropped from an AND, nothing alone.
            ("hello ---", "hello"),
            ("---", "()"),
            ("", "()"),
            ("()", "()"),
            ("a ()", "a"),
            ("--- NOT a", "(& not a)"),
            ("--- OR a", "(& or a)"),
            // `""` is a phrase with no words: it matches nothing.
            ("a \"\"", "(& a \"\")"),
            ("\"\" OR a", "(| \"\" a)"),
            ("a NOT \"\"", "(- a \"\")"),
        ] {
            assert_eq!(p(q), want, "{q}");
        }
        for bad in [
            "NOT a",
            "a NOT",
            "OR",
            "AND",
            "NEAR",
            "(a",
            "a)",
            "\"a",
            "a OR ---",
            "a NOT ---",
            "a AND AND b",
            "() OR a",
            "(a OR b) NEAR c",
            "a NEAR (b OR c)",
            "a (NOT b)",
        ] {
            assert_eq!(p(bad), "ERR", "{bad}");
        }
    }

    /// SQLite's unicode61 tokens, from its own table: only 25 marks in
    /// U+0300-U+0331 continue a token, none starts one, and other marks
    /// split it.
    #[test]
    fn tokenizes_like_unicode61() {
        let toks = |s: &str| {
            tokens(s)
                .map(|(a, b)| s[a..b].to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(toks("\u{301}abc x\u{301}y"), ["abc", "x\u{301}y"]);
        assert_eq!(toks("हिंदी"), ["ह", "द"]);
        assert_eq!(toks("foo_bar 3.14 ﬁle Ⓐ"), ["foo", "bar", "3", "14", "ﬁle"]);
        assert_eq!(toks("\u{e000}x \u{2160}"), ["\u{e000}x", "\u{2160}"]);
    }

    /// SQLite 3.51 FTS4's limits, measured: up to 11 chained NOTs, 4095
    /// ANDed words, and the 5-deep `(... OR b) c` nest below parse; one more
    /// is an error. Parentheses nest up to 100 deep (SQLite: 1000). A hostile
    /// nest is an error, not a stack overflow.
    #[test]
    fn depth_limits_like_fts4() {
        let nots = |n: usize| format!("a{}", " NOT b".repeat(n));
        let ands = |n: usize| vec!["a"; n].join(" ");
        let alts = |n: usize| (0..n).fold("a".to_string(), |q, _| format!("({q} OR b) c"));
        let nears = |n: usize| format!("a{}", " NEAR b".repeat(n));
        let parens = |n: usize| format!("{}a{}", "(".repeat(n), ")".repeat(n));
        for (ok, bad) in [
            (nots(11), nots(12)),
            (ands(4095), ands(4096)),
            (alts(5), alts(6)),
            (nears(12), nears(13)),
            (parens(100), parens(101)),
        ] {
            assert_ne!(p(&ok), "ERR", "{}", &ok[..ok.len().min(60)]);
            assert_eq!(p(&bad), "ERR", "{}", &bad[..bad.len().min(60)]);
        }
        assert_eq!(p(&parens(100_000)), "ERR");
        assert_eq!(p(&nots(100_000)), "ERR");
        assert_eq!(p(&format!("{}a", "(a NOT ".repeat(100_000))), "ERR");
    }
}
