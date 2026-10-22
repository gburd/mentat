// Copyright 2017-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

use crate::CliError;

use edn;

use anyhow::Error;

use mentat::{AutoIndex, CacheDirection};

pub static COMMAND_CACHE: &str = "cache";
pub static COMMAND_CLOSE: &str = "close";
pub static COMMAND_EXIT_LONG: &str = "exit";
pub static COMMAND_EVAL: &str = "eval";
pub static COMMAND_EXIT_SHORT: &str = "e";
pub static COMMAND_HELP: &str = "help";
pub static COMMAND_IMPORT_LONG: &str = "import";
pub static COMMAND_IMPORT_SHORT: &str = "i";
pub static COMMAND_OPEN: &str = "open";
pub static COMMAND_OPEN_ENCRYPTED: &str = "open_encrypted";
pub static COMMAND_PULL: &str = "pull";
pub static COMMAND_QUERY_LONG: &str = "query";
pub static COMMAND_QUERY_SHORT: &str = "q";
pub static COMMAND_QUERY_EXPLAIN_LONG: &str = "explain_query";
pub static COMMAND_QUERY_EXPLAIN_SHORT: &str = "eq";
pub static COMMAND_QUERY_PREPARED_LONG: &str = "query_prepared";
pub static COMMAND_SCHEMA: &str = "schema";
pub static COMMAND_TIMER_LONG: &str = "timer";
pub static COMMAND_TRANSACT_LONG: &str = "transact";
pub static COMMAND_TRANSACT_SHORT: &str = "t";
pub static COMMAND_TUNE: &str = "tune";

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Command {
    Cache(String, CacheDirection),
    Close,
    /// A mino script (needs the `mino` feature).
    Eval(String),
    Exit,
    Help(Vec<String>),
    Import(String),
    Open(String),
    OpenEncrypted(String, String),
    /// `(pattern, entity)`.
    Pull(String, String),
    /// A query, optionally followed by JSON options (`mentat::options_from_json`).
    Query(String),
    QueryExplain(String),
    QueryPrepared(String),
    Schema,
    Timer(bool),
    Transact(String),
    /// `.tune MODE` sets the auto-index mode; `.tune` is a dry run, `.tune!` applies.
    Tune(Option<AutoIndex>, bool),
}

/// Byte offset just past the first top-level bracketed form of `s`, or `None`
/// if it isn't closed yet. Skips strings, `\c` characters and `;` comments.
/// With `whole`, instead: `Some(s.len())` iff every bracket in `s` is closed.
// ponytail: bracket counting, not a reader; `#"regex"` etc. may confuse it.
pub fn form_end(s: &str, whole: bool) -> Option<usize> {
    let (mut depth, mut started) = (0i32, false);
    let mut it = s.char_indices();
    while let Some((i, c)) = it.next() {
        match c {
            '"' => {
                let mut closed = false;
                while let Some((_, c)) = it.next() {
                    match c {
                        '\\' => {
                            it.next();
                        }
                        '"' => {
                            closed = true;
                            break;
                        }
                        _ => {}
                    }
                }
                if !closed {
                    return None;
                }
            }
            '\\' => {
                it.next();
            }
            ';' => {
                for (_, c) in it.by_ref() {
                    if c == '\n' {
                        break;
                    }
                }
            }
            '[' | '(' | '{' => {
                depth += 1;
                started = true;
            }
            ']' | ')' | '}' => {
                depth -= 1;
                if depth == 0 && started && !whole {
                    return Some(i + 1);
                }
            }
            _ => {}
        }
    }
    (whole && depth <= 0).then_some(s.len())
}

/// Split a `.q` argument into the query and its (possibly empty) options text.
pub fn split_query(args: &str) -> Option<(&str, &str)> {
    form_end(args, false).map(|i| (&args[..i], args[i..].trim()))
}

impl Command {
    /// is_complete returns true if no more input is required for the command to be successfully executed.
    /// false is returned if the command is not considered valid.
    /// Defaults to true for all commands except Query and Transact.
    /// TODO: for query and transact commands, they will be considered complete if a parsable EDN has been entered as an argument
    pub fn is_complete(&self) -> bool {
        match self {
            Command::Query(args) => match split_query(args) {
                Some((q, opts)) => {
                    edn::parse::value(q).is_ok()
                        && (opts.is_empty()
                            || serde_json::from_str::<serde_json::Value>(opts).is_ok())
                }
                None => false,
            },
            &Command::QueryExplain(ref args)
            | &Command::QueryPrepared(ref args)
            | &Command::Transact(ref args) => edn::parse::value(args).is_ok(),
            Command::Eval(src) => form_end(src, true).is_some(),
            &Command::Cache(_, _)
            | &Command::Pull(_, _)
            | &Command::Tune(_, _)
            | &Command::Close
            | &Command::Exit
            | &Command::Help(_)
            | &Command::Import(_)
            | &Command::Open(_)
            | &Command::OpenEncrypted(_, _)
            | &Command::Timer(_)
            | &Command::Schema => true,
        }
    }

    pub fn is_timed(&self) -> bool {
        match self {
            &Command::Import(_)
            | &Command::Query(_)
            | &Command::QueryPrepared(_)
            | &Command::Transact(_)
            | &Command::Eval(_)
            | &Command::Pull(_, _)
            | &Command::Tune(_, _) => true,

            &Command::Cache(_, _)
            | &Command::Close
            | &Command::Exit
            | &Command::Help(_)
            | &Command::Open(_)
            | &Command::OpenEncrypted(_, _)
            | &Command::QueryExplain(_)
            | &Command::Timer(_)
            | &Command::Schema => true,
        }
    }

    pub fn output(&self) -> String {
        match self {
            Command::Cache(ref attr, ref direction) => {
                format!(".{} {} {:?}", COMMAND_CACHE, attr, direction)
            }
            Command::Close => format!(".{}", COMMAND_CLOSE),
            Command::Eval(ref src) => format!(".{} {}", COMMAND_EVAL, src),
            Command::Pull(ref p, ref e) => format!(".{} {} {}", COMMAND_PULL, p, e),
            Command::Tune(mode, apply) => format!(
                ".{}{}{}",
                COMMAND_TUNE,
                if *apply { "!" } else { "" },
                mode.map(|m| format!(" {m:?}").to_lowercase())
                    .unwrap_or_default()
            ),
            Command::Exit => format!(".{}", COMMAND_EXIT_LONG),
            Command::Help(ref args) => format!(".{} {:?}", COMMAND_HELP, args),
            Command::Import(ref args) => format!(".{} {}", COMMAND_IMPORT_LONG, args),
            Command::Open(ref args) => format!(".{} {}", COMMAND_OPEN, args),
            Command::OpenEncrypted(ref db, ref key) => {
                format!(".{} {} {}", COMMAND_OPEN_ENCRYPTED, db, key)
            }
            Command::Query(ref args) => format!(".{} {}", COMMAND_QUERY_LONG, args),
            Command::QueryExplain(ref args) => format!(".{} {}", COMMAND_QUERY_EXPLAIN_LONG, args),
            Command::QueryPrepared(ref args) => {
                format!(".{} {}", COMMAND_QUERY_PREPARED_LONG, args)
            }
            Command::Schema => format!(".{}", COMMAND_SCHEMA),
            Command::Timer(on) => format!(".{} {}", COMMAND_TIMER_LONG, on),
            Command::Transact(ref args) => format!(".{} {}", COMMAND_TRANSACT_LONG, args),
        }
    }
}

type Parsed = Result<Command, CliError>;

/// `.open`/`.open_encrypted`: exactly `n` arguments.
fn exactly(args: Vec<String>, n: usize) -> Result<Vec<String>, CliError> {
    if args.len() < n {
        return Err(CliError::CommandParse(
            "Missing required argument".to_string(),
        ));
    }
    if args.len() > n {
        return Err(CliError::CommandParse(format!(
            "Unrecognized argument {:?}",
            args[n]
        )));
    }
    Ok(args)
}

/// A command that takes no arguments.
fn no_args(args: Vec<String>, cmd: Command) -> Parsed {
    exactly(args, 0).map(|_| cmd)
}

fn pull(args: String) -> Parsed {
    match split_query(&args) {
        Some((pattern, entity)) if !entity.is_empty() && !entity.contains(char::is_whitespace) => {
            Ok(Command::Pull(pattern.to_string(), entity.to_string()))
        }
        _ => Err(CliError::CommandParse(
            "Usage: .pull [pattern] entity (an entid or :an/ident)".to_string(),
        )),
    }
}

// Text after a command's arguments is ignored, as it was with `combine` (#264).
peg::parser!(grammar cli() for str {
    rule sp() = [c if c.is_whitespace()]
    rule word() -> String = s:$([c if !c.is_whitespace()]+) { s.to_string() }
    rule eof() = ![_]
    rule ignored() = [_]*
    // Whitespace-separated words, with any surrounding whitespace.
    rule args() -> Vec<String> = sp()* a:(word() ** (sp()+)) sp()* { a }
    // Words directly after the command name (`.closefoo`), then the end.
    rule bare_args() -> Vec<String> = a:(word() ** (sp()+)) sp()* eof() { a }
    // An EDN argument: everything from the first `[` or `{`, complete or not.
    rule edn_arg() -> String = sp()* &['[' | '{'] s:$([_]+) { s.to_string() }

    rule direction() -> CacheDirection
        = "forward" { CacheDirection::Forward }
        / "reverse" { CacheDirection::Reverse }
        / "both" { CacheDirection::Both }
    rule mode() -> AutoIndex
        = "off" { AutoIndex::Off } / "schema" { AutoIndex::Schema } / "adaptive" { AutoIndex::Adaptive }

    rule help() -> Parsed = "help" a:args() ignored() { Ok(Command::Help(a)) }
    rule import() -> Parsed = ("import" / "i") sp()* p:word() ignored() { Ok(Command::Import(p)) }
    rule timer() -> Parsed
        = "timer" sp()* on:("on" { true } / "off" { false }) ignored() { Ok(Command::Timer(on)) }
    rule tune() -> Parsed = "tune" t:(
          "!" ignored() { Command::Tune(None, true) }
        / sp()* m:mode() sp()* eof() { Command::Tune(Some(m), false) }
        / sp()* eof() { Command::Tune(None, false) }) { Ok(t) }
    rule eval() -> Parsed = "eval" sp()* s:$([_]+) { Ok(Command::Eval(s.to_string())) }
    rule pull() -> Parsed = "pull" a:edn_arg() { pull(a) }
    rule cache() -> Parsed
        = "cache" sp()* a:word() sp()* d:direction() ignored() { Ok(Command::Cache(a, d)) }
    rule open_encrypted() -> Parsed = "open_encrypted" a:args() ignored() {
        exactly(a, 2).map(|a| Command::OpenEncrypted(a[0].clone(), a[1].clone()))
    }
    rule open() -> Parsed = "open" a:args() ignored() { exactly(a, 1).map(|a| Command::Open(a[0].clone())) }
    rule close() -> Parsed = "close" a:bare_args() { no_args(a, Command::Close) }
    rule explain_query() -> Parsed = ("explain_query" / "eq") a:edn_arg() { Ok(Command::QueryExplain(a)) }
    rule exit() -> Parsed = ("exit" / "e") a:bare_args() { no_args(a, Command::Exit) }
    rule query_prepared() -> Parsed = "query_prepared" a:edn_arg() { Ok(Command::QueryPrepared(a)) }
    rule query() -> Parsed = ("query" / "q") a:edn_arg() { Ok(Command::Query(a)) }
    rule schema() -> Parsed = "schema" a:bare_args() { no_args(a, Command::Schema) }
    rule transact() -> Parsed = ("transact" / "t") a:edn_arg() { Ok(Command::Transact(a)) }

    pub rule command() -> Parsed = sp()* "." c:(
          help() / import() / timer() / tune() / eval() / pull() / cache() / open_encrypted()
        / open() / close() / explain_query() / exit() / query_prepared() / query() / schema()
        / transact()) { c }
});

pub fn command(s: &str) -> Result<Command, Error> {
    match cli::command(s) {
        Ok(parsed) => Ok(parsed?),
        Err(_) => Err(CliError::CommandParse(format!("Invalid command {:?}", s)).into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_query_with_options() {
        let q = r#".q [:find ?e :in ?n :where [?e :p/n ?n]] {"inputs": ["a]"]}"#;
        let cmd = command(q).unwrap();
        assert!(cmd.is_complete());
        let Command::Query(args) = cmd else { panic!() };
        let (query, opts) = split_query(&args).unwrap();
        assert_eq!(query, "[:find ?e :in ?n :where [?e :p/n ?n]]");
        assert_eq!(opts, r#"{"inputs": ["a]"]}"#);
        // Incomplete: the options object is still open.
        assert!(!command(r#".q [:find ?e :where [?e _ _]] {"asOf":"#)
            .unwrap()
            .is_complete());
        assert!(!command(".q [:find ?e :where").unwrap().is_complete());
        // Brackets inside strings and comments don't count.
        assert_eq!(
            form_end(
                r#"["]" ; ]
 ]"#,
                false
            ),
            Some(11)
        );
    }

    #[test]
    fn test_pull_eval_tune_parsers() {
        assert_eq!(
            command(".pull [*] 65536").unwrap(),
            Command::Pull("[*]".into(), "65536".into())
        );
        assert_eq!(
            command(".pull [:a/b {:c/d [*]}] :my/ident").unwrap(),
            Command::Pull("[:a/b {:c/d [*]}]".into(), ":my/ident".into())
        );
        assert!(command(".pull [*]").is_err());
        let e = command(".eval (+ 1\n 2)").unwrap();
        assert!(e.is_complete());
        assert!(!command(".eval (+ 1").unwrap().is_complete());
        assert_eq!(command(".tune").unwrap(), Command::Tune(None, false));
        assert_eq!(command(".tune!").unwrap(), Command::Tune(None, true));
        assert_eq!(
            command(".tune adaptive").unwrap(),
            Command::Tune(Some(AutoIndex::Adaptive), false)
        );
        assert!(command(".tune bogus").is_err());
    }

    #[test]
    fn test_help_parser_multiple_args() {
        let input = ".help command1 command2";
        let cmd = command(&input).expect("Expected help command");
        match cmd {
            Command::Help(args) => {
                assert_eq!(args, vec!["command1", "command2"]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_help_parser_dot_arg() {
        let input = ".help .command1";
        let cmd = command(&input).expect("Expected help command");
        match cmd {
            Command::Help(args) => {
                assert_eq!(args, vec![".command1"]);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_help_parser_no_args() {
        let input = ".help";
        let cmd = command(&input).expect("Expected help command");
        match cmd {
            Command::Help(args) => {
                let empty: Vec<String> = vec![];
                assert_eq!(args, empty);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_help_parser_no_args_trailing_whitespace() {
        let input = ".help ";
        let cmd = command(&input).expect("Expected help command");
        match cmd {
            Command::Help(args) => {
                let empty: Vec<String> = vec![];
                assert_eq!(args, empty);
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_open_parser_multiple_args() {
        let input = ".open database1 database2";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), "Unrecognized argument \"database2\"");
    }

    #[test]
    fn test_open_parser_single_arg() {
        let input = ".open database1";
        let cmd = command(&input).expect("Expected open command");
        match cmd {
            Command::Open(arg) => {
                assert_eq!(arg, "database1".to_string());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_open_parser_path_arg() {
        let input = ".open /path/to/my.db";
        let cmd = command(&input).expect("Expected open command");
        match cmd {
            Command::Open(arg) => {
                assert_eq!(arg, "/path/to/my.db".to_string());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_open_encrypted_parser() {
        let input = ".open_encrypted /path/to/my.db hunter2";
        let cmd = command(&input).expect("Expected open_encrypted command");
        match cmd {
            Command::OpenEncrypted(path, key) => {
                assert_eq!(path, "/path/to/my.db".to_string());
                assert_eq!(key, "hunter2".to_string());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_open_encrypted_parser_missing_key() {
        let input = ".open_encrypted path/to/db.db";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), "Missing required argument");
    }

    #[test]
    fn test_open_parser_file_arg() {
        let input = ".open my.db";
        let cmd = command(&input).expect("Expected open command");
        match cmd {
            Command::Open(arg) => {
                assert_eq!(arg, "my.db".to_string());
            }
            _ => panic!(),
        }
    }

    #[test]
    fn test_open_parser_no_args() {
        let input = ".open";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), "Missing required argument");
    }

    #[test]
    fn test_open_parser_no_args_trailing_whitespace() {
        let input = ".open ";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), "Missing required argument");
    }

    #[test]
    fn test_close_parser_with_args() {
        let input = ".close arg1";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_close_parser_no_args() {
        let input = ".close";
        let cmd = command(&input).expect("Expected close command");
        if cmd != Command::Close {
            panic!()
        }
    }

    #[test]
    fn test_close_parser_no_args_trailing_whitespace() {
        let input = ".close ";
        let cmd = command(&input).expect("Expected close command");
        if cmd != Command::Close {
            panic!()
        }
    }

    #[test]
    fn test_exit_parser_with_args() {
        let input = ".exit arg1";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_exit_parser_no_args() {
        let input = ".exit";
        let cmd = command(&input).expect("Expected exit command");
        if cmd != Command::Exit {
            panic!()
        }
    }

    #[test]
    fn test_exit_parser_no_args_trailing_whitespace() {
        let input = ".exit ";
        let cmd = command(&input).expect("Expected exit command");
        if cmd != Command::Exit {
            panic!()
        }
    }

    #[test]
    fn test_exit_parser_short_command() {
        let input = ".e";
        let cmd = command(&input).expect("Expected exit command");
        if cmd != Command::Exit {
            panic!()
        }
    }

    #[test]
    fn test_schema_parser_with_args() {
        let input = ".schema arg1";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_schema_parser_no_args() {
        let input = ".schema";
        let cmd = command(&input).expect("Expected schema command");
        if cmd != Command::Schema {
            panic!()
        }
    }

    #[test]
    fn test_schema_parser_no_args_trailing_whitespace() {
        let input = ".schema ";
        let cmd = command(&input).expect("Expected schema command");
        if cmd != Command::Schema {
            panic!()
        }
    }

    #[test]
    fn test_query_parser_complete_edn() {
        let input = ".q [:find ?x :where [?x foo/bar ?y]]";
        let cmd = command(&input).expect("Expected query command");
        match cmd {
            Command::Query(edn) => assert_eq!(edn, "[:find ?x :where [?x foo/bar ?y]]"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_query_parser_alt_query_command() {
        let input = ".query [:find ?x :where [?x foo/bar ?y]]";
        let cmd = command(&input).expect("Expected query command");
        match cmd {
            Command::Query(edn) => assert_eq!(edn, "[:find ?x :where [?x foo/bar ?y]]"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_query_parser_incomplete_edn() {
        let input = ".q [:find ?x\r\n";
        let cmd = command(&input).expect("Expected query command");
        match cmd {
            Command::Query(edn) => assert_eq!(edn, "[:find ?x\r\n"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_query_parser_empty_edn() {
        let input = ".q {}";
        let cmd = command(&input).expect("Expected query command");
        match cmd {
            Command::Query(edn) => assert_eq!(edn, "{}"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_query_parser_no_edn() {
        let input = ".q ";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_query_parser_invalid_start_char() {
        let input = ".q :find ?x";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_import_parser() {
        let input = ".import /foo/bar/";
        let cmd = command(&input).expect("Expected import command");
        match cmd {
            Command::Import(path) => assert_eq!(path, "/foo/bar/"),
            _ => panic!("Wrong command!"),
        }
    }

    #[test]
    fn test_transact_parser_complete_edn() {
        let input = ".t [[:db/add \"s\" :db/ident :foo/uuid] [:db/add \"r\" :db/ident :bar/uuid]]";
        let cmd = command(&input).expect("Expected transact command");
        match cmd {
            Command::Transact(edn) => assert_eq!(
                edn,
                "[[:db/add \"s\" :db/ident :foo/uuid] [:db/add \"r\" :db/ident :bar/uuid]]"
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn test_transact_parser_alt_command() {
        let input =
            ".transact [[:db/add \"s\" :db/ident :foo/uuid] [:db/add \"r\" :db/ident :bar/uuid]]";
        let cmd = command(&input).expect("Expected transact command");
        match cmd {
            Command::Transact(edn) => assert_eq!(
                edn,
                "[[:db/add \"s\" :db/ident :foo/uuid] [:db/add \"r\" :db/ident :bar/uuid]]"
            ),
            _ => panic!(),
        }
    }

    #[test]
    fn test_transact_parser_incomplete_edn() {
        let input = ".t {\r\n";
        let cmd = command(&input).expect("Expected transact command");
        match cmd {
            Command::Transact(edn) => assert_eq!(edn, "{\r\n"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_transact_parser_empty_edn() {
        let input = ".t {}";
        let cmd = command(&input).expect("Expected transact command");
        match cmd {
            Command::Transact(edn) => assert_eq!(edn, "{}"),
            _ => panic!(),
        }
    }

    #[test]
    fn test_transact_parser_no_edn() {
        let input = ".t ";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_transact_parser_invalid_start_char() {
        let input = ".t :db/add \"s\" :db/ident :foo/uuid";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    #[test]
    fn test_parser_preceeding_trailing_whitespace() {
        let input = " .close ";
        let cmd = command(&input).expect("Expected close command");
        if cmd != Command::Close {
            panic!()
        }
    }

    #[test]
    fn test_command_parser_no_dot() {
        let input = "help command1 command2";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }

    // #264: the `combine` parser's behaviour, checked input by input against it; except
    // `.timer off`, which it rejected (its `on` alternative consumed the `o`).
    #[test]
    fn test_peg_port_edge_cases() {
        assert_eq!(
            command(".i /x/y extra").unwrap(),
            Command::Import("/x/y".into())
        );
        assert_eq!(command(".timer off").unwrap(), Command::Timer(false));
        assert_eq!(command(".timer on now").unwrap(), Command::Timer(true));
        assert!(command(".timer").is_err());
        assert_eq!(
            command(".cache :foo/bar reverse").unwrap(),
            Command::Cache(":foo/bar".into(), CacheDirection::Reverse)
        );
        assert!(command(".cache :foo/bar sideways").is_err());
        assert_eq!(
            command(".eq [:find ?x :where [?x _ _]]").unwrap(),
            Command::QueryExplain("[:find ?x :where [?x _ _]]".into())
        );
        assert_eq!(
            command(".query_prepared [:find ?x]").unwrap(),
            Command::QueryPrepared("[:find ?x]".into())
        );
        assert_eq!(command(".e").unwrap(), Command::Exit);
        assert_eq!(command(".eval x").unwrap(), Command::Eval("x".into()));
        assert!(command(".eval   ").is_err());
        assert_eq!(command(".tune! now").unwrap(), Command::Tune(None, true));
        assert_eq!(
            command(".tune  schema ").unwrap(),
            Command::Tune(Some(AutoIndex::Schema), false)
        );
        assert_eq!(
            command(".closefoo").unwrap_err().to_string(),
            "Unrecognized argument \"foo\""
        );
        assert_eq!(
            command(".open_encrypted a b c").unwrap_err().to_string(),
            "Unrecognized argument \"c\""
        );
        assert_eq!(command("\t.schema").unwrap(), Command::Schema);
        assert_eq!(
            command(".t  [:a]\n").unwrap(),
            Command::Transact("[:a]\n".into())
        );
    }

    #[test]
    fn test_command_parser_invalid_cmd() {
        let input = ".foo command1";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }
}
