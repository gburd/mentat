//! Minimal Rust-side conformance runner for mino's `.clj` test corpus.
//!
//! Phase 1 has no macros and no core.clj, so mino's real `clojure.test`
//! (`deftest`/`is`/`are`) does not exist yet. Rather than load `tests/test.clj`,
//! this harness recognizes the `deftest`/`is`/`are`/`testing`/`thrown?` *shapes*
//! directly and evaluates the inner expressions on the ported `Interp`. Phase 4
//! swaps this for the real `clojure.test` once `defmacro` + core.clj land.
//!
//! Top-level forms are split by paren balance (respecting strings, `\c` char
//! literals, and `;` comments) so a *skipped* deftest that contains syntax the
//! Phase-1 reader can't yet parse (ratios `5/2`, `##NaN`, `0x..`, `N` bigints)
//! does not abort the whole file — only the kept deftests are actually read.

use crate::eval::Interp;
use crate::reader::read_one;
use crate::value::Value;

/// Run one corpus `.clj` file. Every `(deftest NAME ...)` whose NAME is not in
/// `skip_deftests` is executed; its inner `(is ...)`/`(are ...)` assertions are
/// tallied. Non-deftest top-level forms (e.g. a leading `(require ...)`) are
/// evaluated best-effort and ignored on error (we don't load test.clj here).
/// One `Interp` backs the whole file, so deftests share the top-level ns —
/// matching mino, where `deftest` just `def`s into the current namespace.
///
/// Returns `(passed, failed)` assertion counts over the kept deftests.
pub fn run_corpus_file(path: &str, skip_deftests: &[&str]) -> (usize, usize) {
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("cannot read corpus file {path}: {e}"));
    let mut it = Interp::new();
    let (mut passed, mut failed) = (0, 0);

    for form_src in top_level_forms(&text) {
        match head_symbol(form_src) {
            Some("deftest") => {
                let name = deftest_name(form_src).unwrap_or("");
                if skip_deftests.contains(&name) {
                    continue;
                }
                // Read + run only the kept deftests.
                let form = read_one(form_src)
                    .unwrap_or_else(|e| panic!("deftest {name}: read error: {e:?}"))
                    .0;
                run_deftest(&mut it, &form, &mut passed, &mut failed);
            }
            // Bare (require ...) / (ns ...) etc.: no-op (no test.clj loaded).
            Some("require") | Some("ns") | Some("in-ns") => {}
            // Any other top-level form: eval best-effort, ignore failure.
            _ => {
                if let Ok((form, _)) = read_one(form_src) {
                    let _ = it.eval(&form, &it.root.clone());
                }
            }
        }
    }
    (passed, failed)
}

/// Walk a `(deftest NAME body...)` form, tallying its assertions. `testing`
/// blocks are transparent groupings; recurse into their bodies.
fn run_deftest(it: &mut Interp, form: &Value, passed: &mut usize, failed: &mut usize) {
    // form = (deftest NAME body...). Skip the first two elements.
    let mut cur = form;
    let mut skipped = 0;
    while let Value::Cons(cell) = cur {
        if skipped < 2 {
            skipped += 1;
            cur = &cell.1;
            continue;
        }
        run_body_form(it, &cell.0, passed, failed);
        cur = &cell.1;
    }
}

/// Run one body form inside a deftest: `(is ...)`, `(are ...)`,
/// `(testing "doc" body...)`, or any other expression (evaluated for effect).
fn run_body_form(it: &mut Interp, form: &Value, passed: &mut usize, failed: &mut usize) {
    match list_head(form) {
        Some("is") => run_is(it, form, passed, failed),
        Some("are") => run_are(it, form, passed, failed),
        Some("testing") => {
            // (testing "doc" body...): recurse past head + doc string.
            let mut cur = form;
            let mut skipped = 0;
            while let Value::Cons(cell) = cur {
                if skipped < 2 {
                    skipped += 1;
                    cur = &cell.1;
                    continue;
                }
                run_body_form(it, &cell.0, passed, failed);
                cur = &cell.1;
            }
        }
        // let/other forms containing assertions aren't handled in Phase 1 (no
        // let yet); eval for effect and ignore. Assertions inside are only in
        // skipped deftests, so this never loses a kept assertion.
        _ => {
            let _ = it.eval(form, &it.root.clone());
        }
    }
}

/// `(is EXPR)` — pass iff EXPR is truthy. `(is (= A B))` uses the port's `=`.
/// `(is (thrown? EXPR))` — pass iff EXPR throws.
fn run_is(it: &mut Interp, form: &Value, passed: &mut usize, failed: &mut usize) {
    let args = rest_elems(form);
    let Some(expr) = args.first() else {
        *failed += 1;
        return;
    };
    // (is (thrown? EXPR)): pass iff evaluating EXPR throws.
    if let Some("thrown?") = list_head(expr) {
        let inner = rest_elems(expr);
        let threw = match inner.first() {
            Some(e) => it.eval(e, &it.root.clone()).is_err(),
            None => false,
        };
        tally(threw, passed, failed);
        return;
    }
    match it.eval(expr, &it.root.clone()) {
        Ok(v) => tally(v.is_truthy(), passed, failed),
        Err(_) => *failed += 1,
    }
}

/// `(are [bindings] template rows...)`: substitute each row of values for the
/// bindings in `template` and evaluate as `(is template')`. Only flat symbol
/// bindings are handled (no destructuring) — enough for arithmetic/are corpus.
fn run_are(it: &mut Interp, form: &Value, passed: &mut usize, failed: &mut usize) {
    let args = rest_elems(form);
    let (Some(binds_v), Some(template)) = (args.first(), args.get(1)) else {
        *failed += 1;
        return;
    };
    let Value::Vector(binds) = binds_v else {
        *failed += 1;
        return;
    };
    let names: Vec<&crate::symbol::Symbol> = binds
        .iter()
        .filter_map(|b| match b {
            Value::Sym(s) => Some(s),
            _ => None,
        })
        .collect();
    if names.is_empty() || names.len() != binds.len() {
        *failed += 1;
        return;
    }
    let rows = &args[2..];
    for chunk in rows.chunks(names.len()) {
        if chunk.len() != names.len() {
            *failed += 1;
            break;
        }
        let subst = substitute(template, &names, chunk);
        // Each expanded row is an `(is ...)`-style assertion (bare expr here).
        let is_form = list2(&Value::Sym(crate::symbol::Symbol::plain("is")), &subst);
        run_is(it, &is_form, passed, failed);
    }
}

/// Substitute `bindings[i]` symbols with `vals[i]` throughout `template`.
fn substitute(template: &Value, names: &[&crate::symbol::Symbol], vals: &[Value]) -> Value {
    match template {
        Value::Sym(s) => {
            for (i, n) in names.iter().enumerate() {
                if *n == s {
                    return vals[i].clone();
                }
            }
            template.clone()
        }
        Value::Cons(cell) => Value::Cons(gc::Gc::new((
            substitute(&cell.0, names, vals),
            substitute(&cell.1, names, vals),
        ))),
        Value::Vector(items) => Value::Vector(gc::Gc::new(
            items
                .iter()
                .map(|e| substitute(e, names, vals))
                .collect::<crate::collections::vector::PVec>(),
        )),
        other => other.clone(),
    }
}

fn tally(ok: bool, passed: &mut usize, failed: &mut usize) {
    if ok {
        *passed += 1;
    } else {
        *failed += 1;
    }
}

// ---- text-level helpers (avoid full parse of skipped deftests) ----

/// Split `text` into the source slices of each top-level form, balancing
/// parens/brackets/braces while skipping strings, `\c` char literals, and
/// `;` line comments.
fn top_level_forms(text: &str) -> Vec<&str> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        // Skip inter-form whitespace/commas/comments.
        while i < b.len() {
            match b[i] {
                b' ' | b'\t' | b'\n' | b'\r' | b',' => i += 1,
                b';' => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        if i >= b.len() {
            break;
        }
        let start = i;
        i = scan_form(b, i);
        out.push(&text[start..i]);
    }
    out
}

/// Return the byte index just past the form starting at `b[i]`.
fn scan_form(b: &[u8], mut i: usize) -> usize {
    match b[i] {
        b'(' | b'[' | b'{' => {
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
                        i = scan_string(b, i);
                        continue;
                    }
                    b'\\' => {
                        i = scan_char_literal(b, i);
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
        b'"' => scan_string(b, i),
        b'\\' => scan_char_literal(b, i),
        // A bare atom (symbol/number/keyword): run to the next delimiter.
        _ => {
            while i < b.len()
                && !matches!(
                    b[i],
                    b' ' | b'\t' | b'\n' | b'\r' | b',' | b'(' | b')' | b'[' | b']' | b'{'
                        | b'}' | b'"' | b';'
                )
            {
                i += 1;
            }
            i
        }
    }
}

/// Advance past a `"..."` string literal starting at the opening quote.
fn scan_string(b: &[u8], mut i: usize) -> usize {
    i += 1; // opening quote
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    i
}

/// Advance past a `\c` / `\newline` / `\uXXXX` char literal starting at `\`.
fn scan_char_literal(b: &[u8], mut i: usize) -> usize {
    i += 1; // backslash
    if i >= b.len() {
        return i;
    }
    i += 1; // the char itself (or first letter of a named char)
    // Named/`uXXXX` literals: consume trailing name chars.
    while i < b.len() && (b[i].is_ascii_alphanumeric()) {
        i += 1;
    }
    i
}

/// Head symbol name of a top-level form given its source text, or None.
fn head_symbol(form_src: &str) -> Option<&str> {
    let s = form_src.strip_prefix('(')?;
    let s = s.trim_start();
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .unwrap_or(s.len());
    let head = &s[..end];
    if head.is_empty() {
        None
    } else {
        Some(head)
    }
}

/// `(deftest NAME ...)` -> NAME, read from the source text.
fn deftest_name(form_src: &str) -> Option<&str> {
    let s = form_src.strip_prefix('(')?.trim_start();
    let s = s.strip_prefix("deftest")?.trim_start();
    let end = s
        .find(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .unwrap_or(s.len());
    let name = &s[..end];
    if name.is_empty() {
        None
    } else {
        Some(name)
    }
}

// ---- Value list helpers ----

/// Head symbol name of a `(sym ...)` Value list, or None.
fn list_head(v: &Value) -> Option<&'static str> {
    // Return an owned-name match by comparing against the interned name.
    if let Value::Cons(cell) = v {
        if let Value::Sym(s) = &cell.0 {
            return match &*s.name {
                "is" => Some("is"),
                "are" => Some("are"),
                "testing" => Some("testing"),
                "thrown?" => Some("thrown?"),
                _ => None,
            };
        }
    }
    None
}

/// Elements of a cons list after the head.
fn rest_elems(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    if let Value::Cons(cell) = v {
        let mut cur = &cell.1;
        while let Value::Cons(c) = cur {
            out.push(c.0.clone());
            cur = &c.1;
        }
    }
    out
}

fn list2(a: &Value, b: &Value) -> Value {
    Value::Cons(gc::Gc::new((
        a.clone(),
        Value::Cons(gc::Gc::new((b.clone(), Value::EmptyList))),
    )))
}
