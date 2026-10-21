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
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Not(Box<Node>, Box<Node>),
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
            left = Node::Or(Box::new(left), Box::new(right));
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
                (l, r) => Node::And(Box::new(l), Box::new(r)),
            };
        }
    }

    fn not(&mut self) -> Result<Node, SQLError> {
        let mut left = self.primary()?;
        while self.peek() == Some(&Lexeme::Not) {
            self.pos += 1;
            let right = self.primary()?;
            if right == Node::Empty {
                return Err(malformed(self.q));
            }
            if left != Node::Empty {
                left = Node::Not(Box::new(left), Box::new(right));
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
                let inner = self.or()?;
                if self.peek() != Some(&Lexeme::Close) {
                    return Err(malformed(self.q));
                }
                self.pos += 1;
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
    };
    if p.lexemes.is_empty() {
        return Ok(Node::Empty);
    }
    let node = p.or()?;
    if p.pos != p.lexemes.len() {
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
        Node::And(l, r) | Node::Or(l, r) | Node::Not(l, r) => {
            out.push_sql("(");
            push_node(out, text, l)?;
            out.push_sql(match node {
                Node::And(..) => " AND ",
                Node::Or(..) => " OR ",
                _ => " AND NOT ",
            });
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
            Node::And(l, r) => format!("(& {} {})", show(l), show(r)),
            Node::Or(l, r) => format!("(| {} {})", show(l), show(r)),
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
        assert_eq!(p("a and b"), "(& (& a and) b)");
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
}
