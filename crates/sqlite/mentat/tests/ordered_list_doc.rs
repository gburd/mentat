// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! #244: docs/src/ordered-list.md, run as written. Every ```edn block is a
//! transaction, or a query when it starts with `[:find`; the plain ``` block
//! after a query (or a transaction expected to fail) holds its expected result.
//!
//! Only compiled with `--features mino` (transaction functions need it).
#![cfg(feature = "mino")]

use mentat::{Binding, IntoResult, Queryable, Store, TypedValue};

fn blocks(md: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut lines = md.lines();
    while let Some(l) = lines.next() {
        if let Some(lang) = l.strip_prefix("```") {
            let body: Vec<&str> = lines.by_ref().take_while(|l| *l != "```").collect();
            out.push((lang.to_string(), body.join("\n")));
        }
    }
    out
}

fn cell(b: &Binding) -> String {
    match b {
        Binding::Scalar(TypedValue::String(s)) => format!("{:?}", s.as_str()),
        Binding::Scalar(TypedValue::Long(n)) => n.to_string(),
        b => panic!("unexpected {b:?}"),
    }
}

#[test]
fn ordered_list_doc_runs_as_written() {
    let md = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../docs/src/ordered-list.md"
    ))
    .expect("read docs/src/ordered-list.md");
    let mut store = Store::open("").expect("open");
    let blocks = blocks(&md);
    let (mut queries, mut txs) = (0, 0);
    let mut i = 0;
    while i < blocks.len() {
        let (lang, body) = &blocks[i];
        i += 1;
        if lang != "edn" {
            continue;
        }
        let expected = match blocks.get(i) {
            Some((l, b)) if l.is_empty() => {
                i += 1;
                Some(b.lines().map(str::trim).collect::<Vec<_>>().join("\n"))
            }
            _ => None,
        };
        if body.trim_start().starts_with("[:find") {
            queries += 1;
            let rows: Vec<String> = store
                .q_once(body, None)
                .into_rel_result()
                .unwrap_or_else(|e| panic!("{body}: {e}"))
                .rows()
                .map(|row| row.iter().map(cell).collect::<Vec<_>>().join(" "))
                .collect();
            assert_eq!(Some(rows.join("\n")), expected, "{body}");
            continue;
        }
        txs += 1;
        match (store.transact(body), expected) {
            (Ok(r), None) => {
                if body.starts_with("[[:list/move") {
                    // The prose: three positions written, three retracted.
                    assert_eq!(
                        (r.datoms_asserted, r.datoms_retracted),
                        (Some(3), Some(3)),
                        "{body}"
                    );
                }
            }
            (Err(e), Some(want)) => {
                let msg = want.strip_prefix("error: ").expect("an error line");
                assert!(e.to_string().contains(msg), "{body}: {e}");
            }
            (r, want) => panic!("{body}: got {r:?}, expected {want:?}"),
        }
    }
    assert_eq!((txs, queries), (8, 2), "the doc's blocks changed");
}
