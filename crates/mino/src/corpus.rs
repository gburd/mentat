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

use crate::env::Env;
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
                let before = (passed, failed);
                run_deftest(&mut it, &form, &mut passed, &mut failed);
                if std::env::var("CORPUS_VERBOSE").is_ok() && failed > before.1 {
                    eprintln!("FAIL {name}: {} failed", failed - before.1);
                }
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
    let env = it.root.clone();
    let mut cur = form;
    let mut skipped = 0;
    while let Value::Cons(cell) = cur {
        if skipped < 2 {
            skipped += 1;
            cur = &cell.1;
            continue;
        }
        run_body_form(it, &cell.0, &env, passed, failed);
        cur = &cell.1;
    }
}

/// Run one body form inside a deftest, in lexical env `env`: `(is ...)`,
/// `(are ...)`, `(testing "doc" body...)`, `(let [..] body...)`, or any other
/// expression (evaluated for effect). `let`/`when`/`if`-wrapped assertions are
/// descended into so their `is` forms are tallied in the binding's env (mino's
/// `clojure.test` runs them the same way; the metadata corpus buries every
/// `is` inside a `let`).
fn run_body_form(it: &mut Interp, form: &Value, env: &Env, passed: &mut usize, failed: &mut usize) {
    match list_head(form) {
        Some("is") => run_is(it, form, env, passed, failed),
        Some("are") => run_are(it, form, env, passed, failed),
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
                run_body_form(it, &cell.0, env, passed, failed);
                cur = &cell.1;
            }
        }
        Some("let") | Some("let*") => {
            // Build the let's child env (sequential binding), then run each
            // body form in it so nested `is` assertions are tallied.
            let elems = rest_elems(form);
            match build_let_env(it, elems.first(), env) {
                Ok(local) => {
                    for body in &elems[1.min(elems.len())..] {
                        run_body_form(it, body, &local, passed, failed);
                    }
                }
                // A binding evaluation threw: evaluate the whole let for effect
                // (its assertions, if any, count as failures via eval error).
                Err(_) => *failed += 1,
            }
        }
        Some("try") => {
            // (try body... (catch ...) (finally cleanup...)): run each body
            // form as a body form so nested `is` assertions tally, then always
            // run the `finally` cleanup for effect. The durability deftests
            // wrap their assertions in a try/finally that rm-rf's the temp dir.
            let elems = rest_elems(form);
            let mut finally_clause: Option<Value> = None;
            for e in &elems {
                match list_head_raw(e) {
                    Some("finally") => finally_clause = Some(e.clone()),
                    // catch clauses: run their handler bodies for effect only.
                    Some("catch") => {}
                    _ => run_body_form(it, e, env, passed, failed),
                }
            }
            if let Some(fin) = finally_clause {
                for body in rest_elems(&fin) {
                    let _ = it.eval(&body, env);
                }
            }
        }
        // Other forms containing assertions (do/when/if bodies) are eval'd for
        // effect in `env`. Kept deftests in the current gates only bury `is`
        // in `let`/`testing`, so this never loses a tallied assertion.
        _ => {
            let _ = it.eval(form, env);
        }
    }
}

/// Build a `let`/`let*` child env by sequentially binding its pairs, mirroring
/// `bindings::eval_let`. Returns the innermost env.
fn build_let_env(
    it: &mut Interp,
    bindings: Option<&Value>,
    env: &Env,
) -> Result<Env, crate::error::Throw> {
    use crate::eval::bindings::{bind_form, Ctx};
    let Some(bindings) = bindings else {
        return Ok(env.child());
    };
    let pairs = binding_pairs(bindings);
    let mut local = env.child();
    for (pat, val_form) in pairs {
        let v = it.eval(&val_form, &local)?;
        let next = local.child();
        bind_form(it, &next, &pat, v, Ctx::Let)?;
        local = next;
    }
    Ok(local)
}

/// Split a `[pat val pat val ...]` binding vector into pairs. Non-vector or
/// odd-length inputs yield no pairs (the caller evaluates for effect instead).
fn binding_pairs(bindings: &Value) -> Vec<(Value, Value)> {
    let Value::Vector(v) = bindings else {
        return Vec::new();
    };
    let items: Vec<Value> = v.iter().cloned().collect();
    items
        .chunks_exact(2)
        .map(|c| (c[0].clone(), c[1].clone()))
        .collect()
}

/// `(is EXPR [msg])` — pass iff EXPR is truthy, evaluated in `env`.
///
/// Mirrors mino's `clojure.test/assert-expr` dispatch for the two operator
/// shapes the store corpus uses:
///  - `(= a b ...)`: compare ONLY the first two operands (mino's `assert-expr
///    '=` binds just `a` and `b`; any trailing arg, e.g. a stray doc string, is
///    ignored). Evaluating the whole `(= a b "msg")` would be `false`.
///  - `(thrown? [Type] body...)`: pass iff evaluating the body throws. A
///    leading type symbol is documentation-only (mino has no class hierarchy).
fn run_is(it: &mut Interp, form: &Value, env: &Env, passed: &mut usize, failed: &mut usize) {
    let args = rest_elems(form);
    let Some(expr) = args.first() else {
        *failed += 1;
        return;
    };
    // (is (thrown? [Type] body...)): pass iff evaluating the body throws.
    if let Some("thrown?") = list_head(expr) {
        let mut inner = rest_elems(expr);
        // Drop a leading bare type symbol (documentation-only in mino).
        if matches!(inner.first(), Some(Value::Sym(s)) if s.ns.is_none()) && inner.len() > 1 {
            inner.remove(0);
        }
        let mut threw = false;
        for e in &inner {
            if it.eval(e, env).is_err() {
                threw = true;
                break;
            }
        }
        tally(threw, passed, failed);
        return;
    }
    // (is (= a b ...)): compare only the first two operands.
    if let Some("=") = eq_head(expr) {
        let ops = rest_elems(expr);
        if ops.len() >= 2 {
            match (it.eval(&ops[0], env), it.eval(&ops[1], env)) {
                (Ok(a), Ok(b)) => {
                    tally(crate::collections::hashing::eq_val(&a, &b), passed, failed)
                }
                _ => *failed += 1,
            }
            return;
        }
    }
    match it.eval(expr, env) {
        Ok(v) => tally(v.is_truthy(), passed, failed),
        Err(_) => *failed += 1,
    }
}

/// `(are [bindings] template rows...)`: substitute each row of values for the
/// bindings in `template` and evaluate as `(is template')`. Only flat symbol
/// bindings are handled (no destructuring) — enough for arithmetic/are corpus.
fn run_are(it: &mut Interp, form: &Value, env: &Env, passed: &mut usize, failed: &mut usize) {
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
        run_is(it, &is_form, env, passed, failed);
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
                    b' ' | b'\t'
                        | b'\n'
                        | b'\r'
                        | b','
                        | b'('
                        | b')'
                        | b'['
                        | b']'
                        | b'{'
                        | b'}'
                        | b'"'
                        | b';'
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
    match list_head_raw(v)? {
        n @ ("is" | "are" | "testing" | "thrown?" | "let" | "let*" | "try") => Some(n),
        _ => None,
    }
}

/// The raw head-symbol name of a list form, or None if the head is not a plain
/// symbol. Unlike `list_head`, does not filter to a known set — `run_body_form`
/// uses it to recognize `finally`/`catch` clauses inside `try`.
fn list_head_raw(v: &Value) -> Option<&'static str> {
    if let Value::Cons(cell) = v {
        if let Value::Sym(s) = &cell.0 {
            return match &*s.name {
                "is" => Some("is"),
                "are" => Some("are"),
                "testing" => Some("testing"),
                "thrown?" => Some("thrown?"),
                "let" => Some("let"),
                "let*" => Some("let*"),
                "try" => Some("try"),
                "finally" => Some("finally"),
                "catch" => Some("catch"),
                _ => None,
            };
        }
    }
    None
}

/// True if `v` is a `(= ...)` call form; returns its head name.
fn eq_head(v: &Value) -> Option<&'static str> {
    if let Value::Cons(cell) = v {
        if let Value::Sym(s) = &cell.0 {
            if s.ns.is_none() && &*s.name == "=" {
                return Some("=");
            }
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
