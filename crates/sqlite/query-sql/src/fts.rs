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
//! The FTS4 query (enhanced syntax: `AND`, `OR`, `NOT`, implicit AND,
//! parentheses, `"phrases"`, `prefix*`) is parsed here; each phrase becomes a
//! regex over the text's tokens. Both sides are tokenized in SQL with one
//! regex, `[\pL\pN\p{Mn}\p{Co}]+` over `lower(...)`, which is what SQLite's
//! `unicode61` tokenizer (`remove_diacritics=0`) treats as token characters.
//! A text `t` is matched as `' tok1 tok2 ... '` (tokens joined by spaces),
//! so a phrase is a substring match on whole tokens.
//!
//! shortcut: `NEAR`/`NEAR/n` is AND (no distance check), column filters
//! (`text:x`) and the first-token anchor (`^x`) are not special, and there is
//! no ranking (scores are 0.0, as on SQLite). Upgrade to DuckDB's `fts`
//! extension if users need BM25.

use core_traits::TypedValue;
use sql_traits::errors::{BuildQueryResult, SQLError};

use mentat_sql::QueryBuilder;

use crate::{push_comparand, ColumnOrExpression, Comparand};

/// unicode61's token characters (letters, numbers, non-spacing marks,
/// private use). Contains no `'`, so it can sit in a SQL string literal.
const TOKEN_RE: &str = r"'[\pL\pN\p{Mn}\p{Co}]+'";

#[derive(Debug, PartialEq)]
enum Lexeme {
    Open,
    Close,
    And,
    Or,
    Not,
    /// Words of a phrase (a bare word is a one-word phrase); `true` = prefix.
    Phrase(Vec<(String, bool)>),
}

#[derive(Debug, PartialEq)]
enum Node {
    /// No tokens (`---`, `()`): dropped from an AND, matches nothing alone.
    Empty,
    Phrase(Vec<(String, bool)>),
    /// Two or more operands (a chain of one operator is one node, so a long
    /// query never nests deeply).
    And(Vec<Node>),
    Or(Vec<Node>),
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
    /// than `2^m - 1` operands overflow it); a NOT costs one level; nothing
    /// may sit at level `m == 0`.
    fn balanced_depth(&self, m: u32) -> Option<u32> {
        if m == 0 {
            return None;
        }
        match self {
            Node::Empty | Node::Phrase(_) => Some(0),
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
    let mut xs = Vec::new();
    for n in [l, r] {
        match n {
            Node::And(ys) if and => xs.extend(ys),
            Node::Or(ys) if !and => xs.extend(ys),
            n => xs.push(n),
        }
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

/// A word with no token characters contributes nothing (SQLite drops it).
fn word(w: &str) -> Option<(String, bool)> {
    let prefix = w.ends_with('*');
    let w = w.trim_end_matches('*');
    w.chars()
        .any(char::is_alphanumeric)
        .then(|| (w.to_string(), prefix))
}

fn lex(q: &str) -> Result<Vec<Lexeme>, SQLError> {
    let mut out = Vec::new();
    let mut chars = q.char_indices().peekable();
    while let Some(&(i, c)) = chars.peek() {
        match c {
            c if c.is_whitespace() => {
                chars.next();
            }
            '(' => {
                chars.next();
                out.push(Lexeme::Open);
            }
            ')' => {
                chars.next();
                out.push(Lexeme::Close);
            }
            '"' => {
                chars.next();
                let start = i + 1;
                let end = loop {
                    match chars.next() {
                        Some((j, '"')) => break j,
                        Some(_) => {}
                        None => return Err(malformed(q)),
                    }
                };
                let mut words: Vec<(String, bool)> =
                    q[start..end].split_whitespace().filter_map(word).collect();
                // `"a b"*`: the last word is a prefix.
                if chars.peek().map(|&(_, c)| c) == Some('*') {
                    chars.next();
                    if let Some(last) = words.last_mut() {
                        last.1 = true;
                    }
                }
                out.push(Lexeme::Phrase(words));
            }
            _ => {
                let start = i;
                let mut end = q.len();
                while let Some(&(j, c)) = chars.peek() {
                    if c.is_whitespace() || matches!(c, '(' | ')' | '"') {
                        end = j;
                        break;
                    }
                    chars.next();
                }
                let w = &q[start..end];
                let near = w == "NEAR"
                    || w.strip_prefix("NEAR/")
                        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
                out.push(match w {
                    "AND" => Lexeme::And,
                    "OR" => Lexeme::Or,
                    "NOT" => Lexeme::Not,
                    _ if near => Lexeme::And,
                    _ => Lexeme::Phrase(word(w).into_iter().collect()),
                });
            }
        }
    }
    Ok(out)
}

/// Precedence, as FTS4's enhanced syntax: NOT, then AND (explicit or
/// implicit), then OR; all left-associative.
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
        let mut left = self.primary()?;
        let mut nots = 0;
        while self.peek() == Some(&Lexeme::Not) {
            self.pos += 1;
            let right = self.primary()?;
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

    fn primary(&mut self) -> Result<Node, SQLError> {
        let lexeme = self.lexemes.get_mut(self.pos);
        match lexeme {
            Some(Lexeme::Phrase(words)) => {
                let words = std::mem::take(words);
                self.pos += 1;
                Ok(if words.is_empty() {
                    Node::Empty
                } else {
                    Node::Phrase(words)
                })
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

/// `x`'s tokens, as one list. A `v` column (a search term bound by another
/// pattern) is read through its plain string copy.
fn push_tokens(out: &mut dyn QueryBuilder, x: &ColumnOrExpression) -> BuildQueryResult {
    out.push_sql("regexp_extract_all(lower(");
    push_comparand(out, x, Comparand::Member("v_s"))?;
    out.push_sql(&format!("), {TOKEN_RE})"));
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
        Node::Phrase(words) => {
            // The words' tokens hold only token characters, never a regex
            // metacharacter, so they can be spliced into the pattern.
            out.push_sql("regexp_matches(");
            push_token_text(out, text)?;
            out.push_sql(", ' '");
            for (w, prefix) in words {
                out.push_sql(" || array_to_string(");
                push_tokens(out, &ColumnOrExpression::Value(TypedValue::typed_string(w)))?;
                out.push_sql(", ' ')");
                out.push_sql(if *prefix { " || '[^ ]* '" } else { " || ' '" });
            }
            out.push_sql(")");
        }
        Node::And(xs) | Node::Or(xs) => {
            let op = if matches!(node, Node::And(_)) {
                " AND "
            } else {
                " OR "
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
        match n {
            Node::Empty => "()".into(),
            Node::Phrase(ws) => ws
                .iter()
                .map(|(w, pre)| format!("{w}{}", if *pre { "*" } else { "" }))
                .collect::<Vec<_>>()
                .join("_"),
            Node::And(xs) => format!("(& {})", xs.iter().map(show).collect::<Vec<_>>().join(" ")),
            Node::Or(xs) => format!("(| {})", xs.iter().map(show).collect::<Vec<_>>().join(" ")),
            Node::Not(l, r) => format!("(- {} {})", show(l), show(r)),
        }
    }

    /// Shapes checked against SQLite FTS4 (enhanced syntax) by hand.
    #[test]
    fn parses_like_fts4() {
        assert_eq!(p("a OR b c"), "(| a (& b c))");
        assert_eq!(p("a b OR c"), "(| (& a b) c)");
        assert_eq!(p("a NOT b c"), "(& (- a b) c)");
        assert_eq!(p("a NOT b NOT c"), "(- (- a b) c)");
        assert_eq!(p("a NOT (b c)"), "(- a (& b c))");
        assert_eq!(p("(a OR b) NOT c"), "(- (| a b) c)");
        assert_eq!(p("a AND b"), "(& a b)");
        assert_eq!(p("a NEAR/3 b"), "(& a b)");
        assert_eq!(p("\"hel* dark*\""), "hel*_dark*");
        assert_eq!(p("\"a b\"*"), "a_b*");
        assert_eq!(p("a\"b\""), "(& a b)");
        assert_eq!(p("a(b)"), "(& a b)");
        assert_eq!(p("hello-world"), "hello-world");
        assert_eq!(p("a and b"), "(& a and b)");
        // Token-less terms: dropped from an AND, nothing alone, an error
        // under OR / NOT (all as SQLite).
        assert_eq!(p("hello ---"), "hello");
        assert_eq!(p("---"), "()");
        assert_eq!(p(""), "()");
        assert_eq!(p("()"), "()");
        assert_eq!(p("a ()"), "a");
        assert_eq!(p("--- NOT a"), "()");
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
        ] {
            assert_eq!(p(bad), "ERR", "{bad}");
        }
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
        let parens = |n: usize| format!("{}a{}", "(".repeat(n), ")".repeat(n));
        for (ok, bad) in [
            (nots(11), nots(12)),
            (ands(4095), ands(4096)),
            (alts(5), alts(6)),
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
