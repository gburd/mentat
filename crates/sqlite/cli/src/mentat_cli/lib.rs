// Copyright 2017-2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

#![crate_name = "mentat_cli"]

use std::path::PathBuf;

/// Return early with an error, converting via `From`. Replaces `failure::bail!`.
macro_rules! bail {
    ($e:expr) => {
        return ::std::result::Result::Err(::std::convert::From::from($e))
    };
}

#[macro_use]
extern crate log;
#[macro_use]
extern crate lazy_static;

extern crate anyhow;
extern crate combine;
extern crate dirs;
extern crate env_logger;
extern crate getopts;
extern crate linefeed;
extern crate rusqlite;
extern crate tabwriter;
extern crate termion;
extern crate thiserror;
extern crate time;

extern crate core_traits;
extern crate edn;
extern crate mentat;
extern crate mentat_db;
extern crate serde_json;

use getopts::Options;

use termion::color;

static HISTORY_FILE_PATH: &str = ".mentat_history";

/// The Mentat CLI stores input history in a readline-compatible file like "~/.mentat_history".
/// This accords with main other tools which prefix with "." and suffix with "_history": lein,
/// node_repl, python, and sqlite, at least.
pub(crate) fn history_file_path() -> PathBuf {
    let mut p = dirs::home_dir().unwrap_or_default();
    p.push(HISTORY_FILE_PATH);
    p
}

static BLUE: color::Rgb = color::Rgb(0x99, 0xaa, 0xFF);
static GREEN: color::Rgb = color::Rgb(0x77, 0xFF, 0x99);

pub mod command_parser;
pub mod input;
pub mod repl;

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    CommandParse(String),
}

pub fn run() -> i32 {
    env_logger::init();

    let args = std::env::args().collect::<Vec<_>>();
    let mut opts = Options::new();

    opts.optopt("d", "", "The path to a database to open", "DATABASE");
    if cfg!(feature = "sqlcipher") {
        opts.optopt(
            "k",
            "key",
            "The key to use to open the database (only available when using sqlcipher)",
            "KEY",
        );
    }
    opts.optflag("h", "help", "Print this help message and exit");
    opts.optmulti(
        "q",
        "query",
        "Execute a query on startup. Queries are executed after any transacts.",
        "QUERY",
    );
    opts.optmulti(
        "t",
        "transact",
        "Execute a transact on startup. Transacts are executed before queries.",
        "TRANSACT",
    );
    opts.optmulti(
        "i",
        "import",
        "Execute an import on startup. Imports are executed before queries.",
        "PATH",
    );
    opts.optmulti(
        "e",
        "execute",
        "Run a REPL command (e.g. '.q [:find ...] {\"asOf\": 5}') and exit. Repeatable.",
        "COMMAND",
    );
    opts.optopt(
        "f",
        "file",
        "Run the REPL commands in FILE ('-' = stdin) and exit.",
        "FILE",
    );
    opts.optflag("v", "version", "Print version and exit");
    opts.optflag(
        "",
        "no-tty",
        "Don't try to use a TTY for readline-like input processing",
    );

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(e) => {
            println!("{}: {}", args[0], e);
            return 1;
        }
    };

    if matches.opt_present("version") {
        print_version();
        return 0;
    }

    if matches.opt_present("help") {
        print_usage(&args[0], &opts);
        return 0;
    }

    // It's still possible to pass this in even if it's not a documented flag above.
    let key = match cfg!(feature = "sqlcipher") {
        true => matches.opt_str("key"),
        false => None,
    };

    let mut last_arg: Option<&str> = None;

    let cmds: Vec<command_parser::Command> = args
        .iter()
        .filter_map(|arg| match last_arg {
            Some("-d") => {
                last_arg = None;
                if let Some(ref k) = key {
                    Some(command_parser::Command::OpenEncrypted(
                        arg.clone(),
                        k.clone(),
                    ))
                } else {
                    Some(command_parser::Command::Open(arg.clone()))
                }
            }
            Some("-q") => {
                last_arg = None;
                Some(command_parser::Command::Query(arg.clone()))
            }
            Some("-i") => {
                last_arg = None;
                Some(command_parser::Command::Import(arg.clone()))
            }
            Some("-t") => {
                last_arg = None;
                Some(command_parser::Command::Transact(arg.clone()))
            }
            Some(_) | None => {
                last_arg = Some(arg);
                None
            }
        })
        .collect();

    // Batch mode: -e commands, else --file, else a non-TTY stdin. Commands
    // are REPL lines; the exit status is 1 if any of them failed.
    let execs = matches.opt_strs("e");
    let batch: Option<Box<dyn std::io::BufRead>> = if !execs.is_empty() {
        Some(Box::new(std::io::Cursor::new(
            execs.join("\n").into_bytes(),
        )))
    } else {
        match matches.opt_str("f").as_deref() {
            Some("-") => Some(Box::new(std::io::BufReader::new(std::io::stdin()))),
            Some(path) => match std::fs::File::open(path) {
                Ok(f) => Some(Box::new(std::io::BufReader::new(f))),
                Err(e) => {
                    eprintln!("{}: {}", path, e);
                    return 1;
                }
            },
            None if !termion::is_tty(&std::io::stdin()) => {
                Some(Box::new(std::io::BufReader::new(std::io::stdin())))
            }
            None => None,
        }
    };
    let is_batch = batch.is_some();

    let mut repl = match repl::Repl::with_reader(!matches.opt_present("no-tty"), batch) {
        Ok(repl) => repl,
        Err(e) => {
            println!("{}", e);
            return 1;
        }
    };

    repl.run(Some(cmds), !is_batch);

    if is_batch && repl.errors > 0 {
        1
    } else {
        0
    }
}

/// Returns a version string.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn print_usage(arg0: &str, opts: &Options) {
    print!(
        "{}",
        opts.usage(&format!(
            "Usage: {} [OPTIONS]\n\n\
             With -e, --file, or piped stdin, runs REPL commands non-interactively\n\
             (no prompts) and exits 1 if any failed. Example:\n  \
             {} -d my.db -e '.q [:find ?n :in ?e :where [?e :person/name ?n]] {{\"inputs\": [65536]}}'",
            arg0, arg0
        ))
    );
}

fn print_version() {
    println!("mentat {}", version());
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {}
}
