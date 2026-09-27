// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! End to end: the `mentat_cli` binary in batch mode on a temp store.

use std::io::Write;
use std::process::{Command, Stdio};

fn cli(db: &str, cmds: &[&str], stdin: Option<&str>) -> (bool, String, String) {
    let mut c = Command::new(env!("CARGO_BIN_EXE_mentat_cli"));
    c.arg("-d").arg(db);
    for cmd in cmds {
        c.arg("-e").arg(cmd);
    }
    c.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = c.spawn().expect("spawn mentat_cli");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.unwrap_or("").as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

struct Db(tempfile::TempDir);
impl Db {
    fn new() -> Db {
        let d = Db(tempfile::tempdir().unwrap());
        let (ok, _, err) = cli(
            &d.path(),
            &[
                r#".t [{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true}
                       {:db/ident :person/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]"#,
                r#".t [{:db/id "a" :person/name "Alice" :person/age 30} {:db/id "b" :person/name "Bob" :person/age 40}]"#,
            ],
            None,
        );
        assert!(ok, "{err}");
        d
    }
    fn path(&self) -> String {
        self.0.path().join("x.db").to_str().unwrap().to_string()
    }
}

/// The value cells of a single-column result table (`| v |` rows, not the header).
fn cells(out: &str) -> Vec<String> {
    out.lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| ?") && !l.starts_with("| ("))
        .map(|l| l.trim_matches(|c| c == '|' || c == ' ').to_string())
        .collect()
}

fn tx_id(out: &str) -> i64 {
    let i = out.rfind("tx_id: ").expect(out) + 7;
    out[i..].split(',').next().unwrap().parse().unwrap()
}

#[test]
fn test_query_with_inputs_and_as_of() {
    let db = Db::new();
    let p = db.path();
    let (ok, out, err) = cli(
        &p,
        &[
            r#".q [:find ?a . :in ?n :where [?e :person/name ?n] [?e :person/age ?a]] {"inputs": ["Alice"]}"#,
        ],
        None,
    );
    assert!(ok, "{err}");
    assert!(out.contains("| 30"), "{out}");

    // Collection input.
    let (ok, out, _) = cli(
        &p,
        &[
            r#".q [:find ?a :in [?n ...] :where [?e :person/name ?n] [?e :person/age ?a]] {"inputs": [["Alice", "Bob"]]}"#,
        ],
        None,
    );
    assert!(ok && out.contains("| 30") && out.contains("| 40"), "{out}");

    // as-of: the age before an update.
    let (ok, out, err) = cli(
        &p,
        &[r#".t [[:db/add (lookup-ref :person/name "Alice") :person/age 31]]"#],
        None,
    );
    assert!(ok, "{err}");
    let t = tx_id(&out) - 1;
    let q = "[:find ?a . :where [?e :person/name \"Alice\"] [?e :person/age ?a]]";
    let (_, now, _) = cli(&p, &[&format!(".q {q}")], None);
    let (ok, then, err) = cli(&p, &[&format!(".q {q} {{\"asOf\": {t}}}")], None);
    assert!(ok, "{err}");
    assert!(
        now.contains("| 31") && then.contains("| 30"),
        "{now}\n{then}"
    );
    // since: only the update.
    let (_, out, _) = cli(
        &p,
        &[&format!(
            ".q [:find ?a :where [_ :person/age ?a]] {{\"since\": {t}}}"
        )],
        None,
    );
    assert!(out.contains("| 31") && !out.contains("| 40"), "{out}");
}

#[test]
fn test_pull_and_errors_and_stdin() {
    let db = Db::new();
    let p = db.path();
    let (ok, out, err) = cli(
        &p,
        &[r#".q [:find ?e . :where [?e :person/name "Bob"]]"#],
        None,
    );
    assert!(ok, "{err}");
    let e: i64 = cells(&out)[0].parse().expect(&out);
    let (ok, out, _) = cli(
        &p,
        &[&format!(".pull [:person/name :person/age] {e}")],
        None,
    );
    assert!(ok, "{out}");
    assert!(
        out.contains(":person/name \"Bob\"") && out.contains(":person/age 40"),
        "{out}"
    );

    // A failing command makes batch mode exit 1.
    let (ok, _, err) = cli(&p, &[r#".q [:find ?x :where [?e :person/name _]]"#], None);
    assert!(!ok, "{err}");
    let (ok, _, err) = cli(
        &p,
        &[r#".q [:find ?e :in ?n :where [?e :person/name ?n]] {"bogus": 1}"#],
        None,
    );
    assert!(!ok && err.contains("unknown option"), "{err}");

    // Commands from stdin, including a query spanning lines.
    let (ok, out, err) = cli(
        &p,
        &[],
        Some(".q [:find ?n\n    :where [_ :person/name ?n]]\n.q [:find (count ?e) . :where [?e :person/age _]]\n"),
    );
    assert!(ok, "{err}");
    assert!(
        out.contains("\"Alice\"") && out.contains("\"Bob\"") && out.contains("| 2"),
        "{out}"
    );
}

#[test]
fn test_tune() {
    let db = Db::new();
    let p = db.path();
    let (ok, out, err) = cli(&p, &[".tune", ".tune!", ".tune adaptive"], None);
    assert!(ok, "{err}");
    assert!(
        out.contains("no index changes") && out.contains("auto index: Adaptive"),
        "{out}"
    );
    let (ok, _, err) = cli(&p, &[".tune bogus"], None);
    assert!(!ok, "{err}");
}

#[cfg(feature = "mino")]
#[test]
fn test_eval() {
    let db = Db::new();
    let (ok, out, err) = cli(
        &db.path(),
        &[".eval (let [c (mentat.store/open)]\n  (mentat.store/q (mentat.store/db c) (quote [:find ?a . :where [_ :person/name \"Bob\"] [?e :person/age ?a] [?e :person/name \"Bob\"]])))"],
        None,
    );
    assert!(ok, "{err}");
    assert_eq!(out.lines().last().unwrap(), "40", "{out}");
    let (ok, _, _) = cli(&db.path(), &[".eval (throw (ex-info \"boom\" {}))"], None);
    assert!(!ok);
}
