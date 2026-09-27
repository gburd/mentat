// Copyright 2017-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

use combine::parser::char::{space, spaces, string};
use combine::parser::combinator::attempt;
use combine::{any, choice, eof, look_ahead, many1, satisfy, sep_end_by, token, Parser};

use crate::CliError;

use edn;

use anyhow::Error;

use combine::error::StringStreamError;
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

pub fn command(s: &str) -> Result<Command, Error> {
    let path = || many1::<String, _, _>(satisfy(|c: char| !c.is_whitespace()));
    let argument = || many1::<String, _, _>(satisfy(|c: char| !c.is_whitespace()));
    let arguments = || {
        sep_end_by::<Vec<_>, _, _, _>(
            many1(satisfy(|c: char| !c.is_whitespace())),
            many1::<Vec<_>, _, _>(space()),
        )
        .expected("arguments")
    };

    // Helpers.
    let direction_parser = || {
        string("forward")
            .map(|_| CacheDirection::Forward)
            .or(string("reverse").map(|_| CacheDirection::Reverse))
            .or(string("both").map(|_| CacheDirection::Both))
    };

    let edn_arg_parser = || {
        spaces().with(
            look_ahead(string("[").or(string("{")))
                .with(many1::<Vec<_>, _, _>(attempt(any())))
                .and_then(|args| -> Result<String, StringStreamError> {
                    Ok(args.iter().collect())
                }),
        )
    };

    let no_arg_parser = || arguments().skip(spaces()).skip(eof());

    let opener = |command, num_args| {
        string(command)
            .with(spaces())
            .with(arguments())
            .map(move |args| {
                if args.len() < num_args {
                    bail!(CliError::CommandParse(
                        "Missing required argument".to_string()
                    ));
                }
                if args.len() > num_args {
                    bail!(CliError::CommandParse(format!(
                        "Unrecognized argument {:?}",
                        args[num_args]
                    )));
                }
                Ok(args)
            })
    };

    // Commands.
    let cache_parser = string(COMMAND_CACHE).with(spaces()).with(
        argument()
            .skip(spaces())
            .and(direction_parser())
            .map(|(arg, direction)| Ok(Command::Cache(arg, direction))),
    );

    let close_parser = string(COMMAND_CLOSE).with(no_arg_parser()).map(|args| {
        if !args.is_empty() {
            bail!(CliError::CommandParse(format!(
                "Unrecognized argument {:?}",
                args[0]
            )));
        }
        Ok(Command::Close)
    });

    let exit_parser = attempt(string(COMMAND_EXIT_LONG))
        .or(attempt(string(COMMAND_EXIT_SHORT)))
        .with(no_arg_parser())
        .map(|args| {
            if !args.is_empty() {
                bail!(CliError::CommandParse(format!(
                    "Unrecognized argument {:?}",
                    args[0]
                )));
            }
            Ok(Command::Exit)
        });

    let explain_query_parser = attempt(string(COMMAND_QUERY_EXPLAIN_LONG))
        .or(attempt(string(COMMAND_QUERY_EXPLAIN_SHORT)))
        .with(edn_arg_parser())
        .map(|x| Ok(Command::QueryExplain(x)));

    let help_parser = string(COMMAND_HELP)
        .with(spaces())
        .with(arguments())
        .map(|args| Ok(Command::Help(args)));

    let import_parser = attempt(string(COMMAND_IMPORT_LONG))
        .or(attempt(string(COMMAND_IMPORT_SHORT)))
        .with(spaces())
        .with(path())
        .map(|x| Ok(Command::Import(x)));

    let open_parser =
        opener(COMMAND_OPEN, 1).map(|args_res| args_res.map(|args| Command::Open(args[0].clone())));

    let open_encrypted_parser = opener(COMMAND_OPEN_ENCRYPTED, 2).map(|args_res| {
        args_res.map(|args| Command::OpenEncrypted(args[0].clone(), args[1].clone()))
    });

    let query_parser = attempt(string(COMMAND_QUERY_LONG))
        .or(attempt(string(COMMAND_QUERY_SHORT)))
        .with(edn_arg_parser())
        .map(|x| Ok(Command::Query(x)));

    let query_prepared_parser = string(COMMAND_QUERY_PREPARED_LONG)
        .with(edn_arg_parser())
        .map(|x| Ok(Command::QueryPrepared(x)));

    let schema_parser = string(COMMAND_SCHEMA).with(no_arg_parser()).map(|args| {
        if !args.is_empty() {
            bail!(CliError::CommandParse(format!(
                "Unrecognized argument {:?}",
                args[0]
            )));
        }
        Ok(Command::Schema)
    });

    let timer_parser = string(COMMAND_TIMER_LONG)
        .with(spaces())
        .with(string("on").map(|_| true).or(string("off").map(|_| false)))
        .map(|args| Ok(Command::Timer(args)));

    let eval_parser = string(COMMAND_EVAL)
        .with(spaces())
        .with(many1::<String, _, _>(any()))
        .map(|src| Ok(Command::Eval(src)));

    let pull_parser = string(COMMAND_PULL)
        .with(edn_arg_parser())
        .map(|args: String| match split_query(&args) {
            Some((pattern, entity))
                if !entity.is_empty() && !entity.contains(char::is_whitespace) =>
            {
                Ok(Command::Pull(pattern.to_string(), entity.to_string()))
            }
            _ => bail!(CliError::CommandParse(
                "Usage: .pull [pattern] entity (an entid or :an/ident)".to_string()
            )),
        });

    let tune_mode = || {
        string("off")
            .map(|_| AutoIndex::Off)
            .or(string("schema").map(|_| AutoIndex::Schema))
            .or(string("adaptive").map(|_| AutoIndex::Adaptive))
    };
    let tune_parser = string(COMMAND_TUNE)
        .with(
            token('!')
                .map(|_| Command::Tune(None, true))
                .or(
                    attempt(spaces().with(tune_mode()).skip(spaces()).skip(eof()))
                        .map(|m| Command::Tune(Some(m), false)),
                )
                .or(spaces().skip(eof()).map(|_| Command::Tune(None, false))),
        )
        .map(Ok);

    let transact_parser = attempt(string(COMMAND_TRANSACT_LONG))
        .or(attempt(string(COMMAND_TRANSACT_SHORT)))
        .with(edn_arg_parser())
        .map(|x| Ok(Command::Transact(x)));

    let parsers = choice((
        attempt(help_parser),
        attempt(import_parser),
        attempt(timer_parser),
        attempt(tune_parser),
        attempt(eval_parser),
        attempt(pull_parser),
        attempt(cache_parser),
        attempt(open_encrypted_parser),
        attempt(open_parser),
        attempt(close_parser),
        attempt(explain_query_parser),
        attempt(exit_parser),
        attempt(query_prepared_parser),
        attempt(query_parser),
        attempt(schema_parser),
        attempt(transact_parser),
    ));
    spaces()
        .skip(token('.'))
        .with(parsers)
        .parse(s)
        .unwrap_or((
            Err(CliError::CommandParse(format!("Invalid command {:?}", s)).into()),
            "",
        ))
        .0
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

    #[test]
    fn test_command_parser_invalid_cmd() {
        let input = ".foo command1";
        let err = command(&input).expect_err("Expected an error");
        assert_eq!(err.to_string(), format!("Invalid command {:?}", input));
    }
}
