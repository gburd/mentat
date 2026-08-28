# mino-rs: Rust port of the mino Clojure-dialect interpreter — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A standalone pure-Rust crate `mino-rs` that runs mino's Clojure-dialect
language (reader, evaluator, persistent collections, core.clj stdlib) and its
EAVT `store` namespace, reusable by both Mentat and pg_mentat.

**Architecture:** Tree-walking evaluator over a GC'd tagged `Value` enum. The
reader and printer are hand-ported from `src/eval/read.c` / `print.c`. Persistent
collections (vector trie, HAMT, RB-tree) are ported from `src/collections/`.
Primitives are ported by domain. mino's bundled `core.clj` (4141 lines) runs
**as data on the ported interpreter** — it is not rewritten. mino's 222-file
`.clj` test corpus is the conformance oracle: the port is "done" for a subsystem
when its slice of that corpus passes.

**Tech Stack:** Rust 2021, `gc` crate (or `rust-gc`) for cycle-collecting heap
values (NOT a hand-port of mino's generational collector), `criterion` (dev only,
optional). No `unsafe` beyond what the GC crate requires. No C, no FFI.

**Spec:** This plan; the mino source tree at `~/src/mino` (git-pinned); its
`docs/INTERNAL_MODULE_MAP.md` (module responsibilities) and `docs/adr/`.

## Global Constraints

- **Pure Rust, no C, no FFI, no build-time codegen tools.** (This is the entire
  reason for the port; violating it defeats the purpose.)
- **Conformance oracle = mino's own tests.** For every ported subsystem, the
  corresponding files under `~/src/mino/tests/*.clj` must pass, run through a
  `mino-rs` test harness that loads `tests/test.clj` (mino's `deftest`/`is`
  framework, itself ported as core.clj data).
- **core.clj runs as data, not rewritten.** `src/core.clj` and `lib/mino/*.clj`
  are copied verbatim into `mino-rs/resources/` and loaded at runtime. If a core
  fn fails, the bug is in a *primitive* or the *evaluator*, not in core.clj —
  fix the Rust, never the `.clj`.
- **Rename `mino.*` → `mentat.*` ONLY at the user-facing namespace layer**, and
  ONLY in the consuming crates (Mentat/pg_mentat), NOT inside `mino-rs`. Inside
  the crate the bundled namespace stays `mino.store` etc. so the vendored
  `.clj` + test corpus stay bit-identical to upstream and re-portable. The
  rename is a thin alias registered by the host (see Phase 8).
- **Scope exclusions (do NOT port — YAGNI for a data layer):** the bytecode
  compiler + VM (`src/eval/bc/`), the copy-and-patch JIT (`src/eval/bc/jit/`,
  `stencils/`), TLS/HTTP/net/pool (`prim/{tls,http,net,pool,url,codec,gzip}.c`,
  `vendor/bearssl`, `vendor/miniz`), async/agents/STM (`prim/{async,agent,stm}.c`,
  `src/async/`), SLAD images (`runtime/image*.c`, `prim/image.c`), proc/subprocess
  (`prim/proc.c`), host interop (`prim/host.c`, `interop/`). The tree-walker is
  the semantic reference the VM/JIT optimize; the walker alone is correct and
  sufficient. Revisit only if a measured need appears.

---

## File Structure

```
mino-rs/
├── Cargo.toml
├── build.rs                     # embeds resources/*.clj via include_str! index (no codegen tools)
├── resources/
│   ├── core.clj                 # copied verbatim from ~/src/mino/src/core.clj
│   ├── mino/store.clj           # copied verbatim from ~/src/mino/lib/mino/store.clj
│   └── mino/test.clj            # copied from ~/src/mino/tests/test.clj (deftest/is)
├── src/
│   ├── lib.rs                   # crate root, re-exports Interp, Value, eval APIs
│   ├── value.rs                 # Value enum + Gc cells (ports values/layout.h + val.c)
│   ├── symbol.rs                # interned symbols/keywords, namespaced names
│   ├── reader.rs                # ports eval/read.c
│   ├── printer.rs               # ports eval/print.c
│   ├── env.rs                   # ports runtime/env.c + var.c + ns_env.c
│   ├── eval/
│   │   ├── mod.rs               # eval front door (ports eval/eval.c + special.c)
│   │   ├── special.rs           # special forms (ports special_registry.c, defs.c)
│   │   ├── bindings.rs          # destructuring, let/loop/binding (ports bindings.c)
│   │   ├── control.rs           # try/catch/finally (ports control.c)
│   │   └── func.rs              # fn, multi-arity, apply (ports fn.c)
│   ├── collections/
│   │   ├── vector.rs            # 32-way trie (ports collections/vec.c)
│   │   ├── map.rs               # HAMT (ports collections/map.c)
│   │   ├── rbtree.rs            # sorted map/set (ports collections/rbtree.c)
│   │   └── transient.rs         # transient kernel (ports collections/transient.c)
│   ├── prim/
│   │   ├── mod.rs               # install table (ports prim/install.c registration)
│   │   ├── numeric.rs           # ports numeric*.c
│   │   ├── collections.rs       # ports collections.c + sequences*.c
│   │   ├── string.rs            # ports string.c
│   │   ├── reflection.rs        # ports reflection.c + meta.c
│   │   ├── stateful.rs          # atoms only (ports the atom subset of stateful.c)
│   │   ├── regex.rs             # ports regex/re.c + prim/regex.c
│   │   └── io.rs                # println/pr-str/read-string (no fs/proc/net)
│   ├── module.rs                # require + bundled-lib registration (ports module.c)
│   ├── store.rs                 # EAVT store handle backing mino.store (ports prim/store.c
│   │                            #   in-memory path; durability WAL is Phase 7)
│   └── error.rs                 # error kinds + throw/catch payloads (ports error.c/diag)
└── tests/
    ├── conformance.rs           # harness: run a ~/src/mino/tests/*.clj file, assert 0 failures
    └── corpus/                  # symlink or copy of the mino tests we currently pass
```

Each `.rs` file above has one responsibility mirroring a mino module, so a
reviewer can check a port file against exactly one C file.

---

## Phase map (each phase = working, testable software)

| Phase | Deliverable | mino test files that must pass |
|-------|-------------|-------------------------------|
| 0 | Crate + Value + reader/printer round-trip | (Rust unit tests only) |
| 1 | Eval core: self-eval, symbols, `if`/`do`/`quote`, fn/apply, `def` | `arithmetic_test` (non-overflow subset only) |
| 2 | Persistent collections + collection prims | `collections_semantics_test`, `are_test` |
| 3 | `let`/`loop`/`recur`, destructuring, `try`/`catch` | `binding_test`, `clj_control_test` |
| 4 | Load `core.clj`; macros; the `deftest`/`is` harness | `clj_predicates_test`, `clj_higher_order_test` |
| 5 | String, regex, reflection, atoms, numeric tower (bignum/ratio deferred) | `clojure_string_test`, `atom_test`, `clj_math_test` |
| 6 | `mino.store` in-memory (EAVT transact/read/query/entity) | `store` tests (grep `tests/` for store) |
| 7 | Store durability (snapshot + WAL) | store durability tests |
| 8 | Host embedding API + `mentat.*` alias; wire into Mentat behind a feature | Mentat integration test |

Bignum/ratio/bigdec (`prim/{bignum,ratio,bigdec}.c`) is a **required Phase 5.5**,
NOT deferred: `arithmetic_test.clj` asserts `+'`/`-'`/`*'`/`inc'`/`dec'`
auto-promote to `N`-suffixed bigints and that plain `+`/`*` *throw* on i64
overflow (JVM-Clojure contract). Back it with pure-Rust `num-bigint` +
`num-rational`, NOT a port of vendored imath. Consequently Phase 1's gate is the
**non-overflow subset** of `arithmetic_test.clj`; the overflow/`N` assertions
turn on in Phase 5.5.

---

## Task 0.1: Crate skeleton + Value enum

**Files:**
- Create: `mino-rs/Cargo.toml`, `mino-rs/src/lib.rs`, `mino-rs/src/value.rs`
- Test: inline `#[cfg(test)]` in `value.rs`

**Interfaces:**
- Produces: `pub enum Value { Nil, Bool(bool), Int(i64), Float(f64), Char(char),
  Str(Gc<String>), Sym(Symbol), Keyword(Symbol), Cons(Gc<(Value, Value)>),
  Vector(Gc<Vector>), Map(Gc<Map>), Set(Gc<Set>), Fn(Gc<Closure>),
  Prim(PrimFn), Handle(Gc<Handle>) }` — variants added as later phases need
  them; start with the immediates + Str/Sym/Keyword/Cons.
- Produces: `pub fn nil() -> Value`, `Value::is_truthy(&self) -> bool` (only
  `Nil` and `Bool(false)` are falsy — port from `MINO_IS_NIL`/bool semantics).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn truthiness_matches_clojure() {
    assert!(!Value::Nil.is_truthy());
    assert!(!Value::Bool(false).is_truthy());
    assert!(Value::Bool(true).is_truthy());
    assert!(Value::Int(0).is_truthy());       // 0 is truthy in Clojure
    assert!(Value::Str(gc_str("")).is_truthy()); // "" is truthy
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mino-rs truthiness_matches_clojure`
Expected: FAIL (Value/is_truthy not defined).

- [ ] **Step 3: Implement `Value`, `is_truthy`, `gc_str` helper**

```rust
// value.rs
use gc::{Gc, Trace, Finalize};

#[derive(Trace, Finalize, Clone)]
pub enum Value {
    Nil, Bool(bool), Int(i64), Float(f64), Char(char),
    Str(Gc<String>),
    Sym(crate::symbol::Symbol),
    Keyword(crate::symbol::Symbol),
    Cons(Gc<(Value, Value)>),
    // later phases extend this enum
}
impl Value {
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}
#[cfg(test)]
fn gc_str(s: &str) -> Gc<String> { Gc::new(s.to_string()) }
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mino-rs truthiness_matches_clojure`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mino-rs/Cargo.toml mino-rs/src/lib.rs mino-rs/src/value.rs
git commit -m "feat(mino-rs): crate skeleton + Value enum with Clojure truthiness"
```

---

## Task 0.2: Interned symbols + keywords

**Files:**
- Create: `mino-rs/src/symbol.rs`
- Modify: `mino-rs/src/lib.rs` (add `pub mod symbol;`)
- Test: inline in `symbol.rs`

**Interfaces:**
- Consumes: nothing.
- Produces: `pub struct Symbol { pub ns: Option<Rc<str>>, pub name: Rc<str> }`
  with `Symbol::plain(&str)`, `Symbol::namespaced(ns, name)`, `Display`
  (`ns/name` or `name`). Keywords reuse `Symbol` (the `Value` variant
  distinguishes them). Port the namespaced-name split logic from
  `edn/namespaceable_name.rs` in Mentat (already exists — reuse its rules for
  what a valid ns/name split is) rather than re-deriving.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn symbol_display_and_split() {
    assert_eq!(Symbol::plain("foo").to_string(), "foo");
    let s = Symbol::namespaced("mino.store", "open");
    assert_eq!(s.to_string(), "mino.store/open");
    assert_eq!(s.ns.as_deref(), Some("mino.store"));
    assert_eq!(&*s.name, "open");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mino-rs symbol_display_and_split`
Expected: FAIL.

- [ ] **Step 3: Implement `Symbol`**

```rust
use std::rc::Rc;
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Symbol { pub ns: Option<Rc<str>>, pub name: Rc<str> }
impl Symbol {
    pub fn plain(name: &str) -> Self { Self { ns: None, name: name.into() } }
    pub fn namespaced(ns: &str, name: &str) -> Self {
        Self { ns: Some(ns.into()), name: name.into() }
    }
}
impl std::fmt::Display for Symbol {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match &self.ns { Some(n) => write!(f, "{n}/{}", self.name),
                         None => write!(f, "{}", self.name) }
    }
}
// gc Trace: Symbol holds only Rc<str>, no Gc pointers -> unsafe_empty_trace!
gc::unsafe_empty_trace!(Symbol);
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mino-rs symbol_display_and_split`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mino-rs/src/symbol.rs mino-rs/src/lib.rs
git commit -m "feat(mino-rs): interned Symbol/keyword with namespaced names"
```

---

## Task 0.3: Reader (read one form) — round-trip vs printer

**Files:**
- Create: `mino-rs/src/reader.rs`, `mino-rs/src/printer.rs`
- Modify: `mino-rs/src/lib.rs`
- Test: inline in `reader.rs`

**Interfaces:**
- Consumes: `Value`, `Symbol`.
- Produces: `pub fn read_one(src: &str) -> Result<(Value, usize), ReadError>`
  (value + bytes consumed), `pub fn read_all(src: &str) -> Result<Vec<Value>, ReadError>`,
  `pub fn print_str(v: &Value) -> String` (readable form, i.e. `pr-str`).
- **Port reference:** `~/src/mino/src/eval/read.c` (tokenizer + form parser) and
  `print.c` (single switch). Start with: nil, true/false, ints, floats, chars,
  strings (with escapes), symbols, keywords, `( )` list, `[ ]` vector,
  `{ }` map, `#{ }` set, `'`/`` ` ``/`~`/`~@` reader macros. Defer: tagged
  literals, reader conditionals, radix literals (add in Phase 5 when a test
  needs them).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn read_print_roundtrip() {
    for s in ["nil", "true", "42", "-7", "3.14", ":kw", ":a/b",
              "foo", "\"hi\"", "(1 2 3)", "[1 :k \"s\"]",
              "{:a 1 :b 2}", "#{1 2}", "'x", "(a (b c) d)"] {
        let (v, _) = read_one(s).unwrap();
        let printed = print_str(&v);
        let (v2, _) = read_one(&printed).unwrap();
        assert_eq!(print_str(&v2), printed, "roundtrip drift on {s}");
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mino-rs read_print_roundtrip`
Expected: FAIL.

- [ ] **Step 3: Implement reader + printer**

Port `read.c`'s scanner (skip whitespace + `;` comments + `,` as whitespace),
dispatch on first non-ws char; port `print.c`'s switch. (Full code lives in the
port — this step's deliverable is the two files passing the round-trip.) Map
reader macros: `'x`→`(quote x)`, `` `x ``→`(quasiquote x)`, `~x`→`(unquote x)`,
`~@x`→`(unquote-splicing x)` (as `Cons` lists, matching mino).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mino-rs read_print_roundtrip`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mino-rs/src/reader.rs mino-rs/src/printer.rs mino-rs/src/lib.rs
git commit -m "feat(mino-rs): reader + printer with read/print round-trip"
```

---

## Task 1.1: Environment + eval front door (`if`/`do`/`quote`/self-eval)

**Files:**
- Create: `mino-rs/src/env.rs`, `mino-rs/src/eval/mod.rs`, `mino-rs/src/error.rs`
- Modify: `mino-rs/src/lib.rs`
- Test: inline in `eval/mod.rs`

**Interfaces:**
- Consumes: `Value`, `read_one`.
- Produces: `pub struct Env` (lexical frame: `Gc<EnvInner>` with parent link +
  `HashMap<Symbol, Value>`; ports `runtime/env.c`), `Env::root()`, `Env::child()`,
  `Env::get(&Symbol) -> Option<Value>`, `Env::set(&Symbol, Value)`.
- Produces: `pub struct Interp { pub root: Env }`, `Interp::new()`,
  `Interp::eval(&mut self, &Value, &Env) -> Result<Value, Throw>`,
  `Interp::eval_str(&mut self, &str) -> Result<Value, Throw>`.
- Produces: `pub struct Throw(pub Value)` (a thrown Clojure value; ports the
  error/throw payload model from `error.c`).
- **Port reference:** `eval/eval.c` (`eval_value`, `eval_implicit_do`),
  `special_registry.c` (`if`/`do`/`quote` inline handlers).

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn eval_core_forms() {
    let mut it = Interp::new();
    assert_eq!(print_str(&it.eval_str("42").unwrap()), "42");
    assert_eq!(print_str(&it.eval_str("(quote (a b))").unwrap()), "(a b)");
    assert_eq!(print_str(&it.eval_str("(if true 1 2)").unwrap()), "1");
    assert_eq!(print_str(&it.eval_str("(if nil 1 2)").unwrap()), "2");
    assert_eq!(print_str(&it.eval_str("(do 1 2 3)").unwrap()), "3");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mino-rs eval_core_forms`
Expected: FAIL.

- [ ] **Step 3: Implement Env, Interp, eval of self-eval/symbol/if/do/quote**

Self-evaluating: nil/bool/int/float/char/string/keyword/vector/map/set. Symbol:
`env.get` else `Throw` "unable to resolve symbol". List: look at head; if special
form (`if`/`do`/`quote`), dispatch; else Phase 1.2's call path (stub with an
"unknown form" throw for now so the test above passes without fn calls).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mino-rs eval_core_forms`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mino-rs/src/env.rs mino-rs/src/eval/mod.rs mino-rs/src/error.rs mino-rs/src/lib.rs
git commit -m "feat(mino-rs): env + eval of if/do/quote/self-eval"
```

---

## Task 1.2: `fn`, application, `def` + first primitive (`+`)

**Files:**
- Create: `mino-rs/src/eval/func.rs`, `mino-rs/src/eval/special.rs`,
  `mino-rs/src/prim/mod.rs`, `mino-rs/src/prim/numeric.rs`
- Modify: `mino-rs/src/eval/mod.rs`, `mino-rs/src/value.rs` (add `Fn`, `Prim`)
- Test: inline in `func.rs`

**Interfaces:**
- Consumes: `Interp`, `Env`, `Value`, `Throw`.
- Produces: `Value::Fn(Gc<Closure>)` where `Closure { params, body, env }`
  (ports `fn.c`); `Value::Prim(PrimFn)` where
  `PrimFn = fn(&mut Interp, &[Value]) -> Result<Value, Throw>`.
- Produces: `special::eval_def` (ports `defs.c` `def`), `func::apply`
  (ports `fn.c` `apply_callable`, multi-arity dispatch).
- Produces: `prim::install_numeric(root: &Env)` registering `+ - * / = <`.
- **Port reference:** `fn.c`, `defs.c`, `numeric.c`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn fn_def_and_plus() {
    let mut it = Interp::new();
    assert_eq!(print_str(&it.eval_str("(+ 1 2 3)").unwrap()), "6");
    it.eval_str("(def inc (fn [x] (+ x 1)))").unwrap();
    assert_eq!(print_str(&it.eval_str("(inc 41)").unwrap()), "42");
    it.eval_str("(def add (fn ([a] a) ([a b] (+ a b))))").unwrap(); // multi-arity
    assert_eq!(print_str(&it.eval_str("(add 5)").unwrap()), "5");
    assert_eq!(print_str(&it.eval_str("(add 5 6)").unwrap()), "11");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mino-rs fn_def_and_plus`
Expected: FAIL.

- [ ] **Step 3: Implement fn/def/apply + numeric prims + wire install into Interp::new**

`Interp::new` builds root env then calls `prim::install_numeric(&root)`. Call
path in `eval/mod.rs`: eval head; if `Fn`/`Prim`, eval args left-to-right, then
`func::apply`. `+` mixes int/float per mino's `args_have_float` rule (all int →
int, any float → float).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mino-rs fn_def_and_plus`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mino-rs/src/eval/func.rs mino-rs/src/eval/special.rs mino-rs/src/prim/ mino-rs/src/eval/mod.rs mino-rs/src/value.rs
git commit -m "feat(mino-rs): fn/def/apply, multi-arity, numeric primitives"
```

---

## Task 1.3: First conformance test — run mino's `arithmetic_test.clj` (non-overflow subset)

**Files:**
- Create: `mino-rs/tests/conformance.rs`, `mino-rs/resources/mino/test.clj`
  (copied from `~/src/mino/tests/test.clj`)
- Test: `mino-rs/tests/conformance.rs`

**Interfaces:**
- Consumes: `Interp::eval_str`, and a **minimal hand-rolled** `deftest`/`is`/`are`
  subset. NOTE: mino's `tests/test.clj` is a 5-line shim that
  `(require '[clojure.test :refer :all])` — the real framework is `clojure.test`
  in the stdlib and needs macros, which Phase 1 lacks. So Task 1.3 defines its
  own tiny Rust-side `deftest`/`is` runner (recognize `(deftest name body...)`
  and `(is expr)` / `(is (= a b))` forms directly in `run_corpus_file`), and
  the corpus list is gated to the arithmetic file **with overflow/`N`
  assertions skipped** (Phase 5.5 re-enables them). Switch to the real
  `clojure.test` in Phase 4 once `defmacro` + core.clj land.
- Produces: `run_corpus_file(path: &str) -> (usize passed, usize failed)`.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn arithmetic_corpus_passes() {
    // Phase 1 gate: non-overflow arithmetic only. Overflow/N-promotion
    // assertions require bignum (Phase 5.5) and are skipped by the harness
    // via a deftest-name denylist: ["integer-overflow-strict-and-primed"].
    let (passed, failed) = run_corpus_file(
        concat!(env!("MINO_SRC"), "/tests/arithmetic_test.clj"),
        &["integer-overflow-strict-and-primed"]);
    assert!(passed > 0, "no assertions ran");
    assert_eq!(failed, 0, "{failed} arithmetic assertions failed");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `MINO_SRC=$HOME/src/mino cargo test -p mino-rs arithmetic_corpus_passes`
Expected: FAIL (either harness missing, or genuine semantic gaps → fix the
evaluator/prims until it passes; each gap is a real port bug).

- [ ] **Step 3: Implement the harness + close the gaps arithmetic_test needs**

`run_corpus_file(path, skip_deftests)` reads the file, evals each top-level
form, recognizes `(deftest name ...)` (skipping any name in `skip_deftests`)
and inner `(is ...)`/`(are ...)` assertions, tracks pass/fail. Fix whatever the
kept `arithmetic_test.clj` deftests exercise that the port doesn't yet handle.

- [ ] **Step 4: Run to verify it passes**

Run: `MINO_SRC=$HOME/src/mino cargo test -p mino-rs arithmetic_corpus_passes`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mino-rs/tests/conformance.rs mino-rs/resources/mino/test.clj
git commit -m "test(mino-rs): mino arithmetic_test.clj passes against the port"
```

---

## Tasks for Phases 2–8 (same TDD shape; each closes a corpus slice)

Each subsequent task follows the identical five-step rhythm (failing test → run
→ implement by porting the named C file → run → commit) and is gated by the
corpus file(s) in the phase map above. They are enumerated at execution time by
the driving skill because their internal steps depend on which corpus
assertions fail first — but the *boundaries*, *port-reference files*, and
*acceptance test* for each are fixed here:

- **Task 2.1 vector.rs** ← `collections/vec.c`; gate: vector ops in
  `collections_semantics_test.clj`.
- **Task 2.2 map.rs (HAMT)** ← `collections/map.c`; gate: map/set ops in
  `collections_semantics_test.clj`.
- **Task 2.3 collection prims** ← `prim/collections.c` + `sequences.c`; gate:
  `are_test.clj`.
- **Task 3.1 let/loop/recur + destructuring** ← `bindings.c`; gate:
  `binding_test.clj`.
- **Task 3.2 try/catch/finally + throw** ← `control.c`; gate:
  `clj_control_test.clj`.
- **Task 4.1 defmacro + macroexpand + quasiquote** ← `defs.c`, `eval.c`
  (`macroexpand*`, `quasiquote_expand`); gate: `test.clj` fully loads.
- **Task 4.2 load core.clj** ← copy `src/core.clj` to `resources/`, load at
  `Interp::new`; gate: `clj_predicates_test.clj`, `clj_higher_order_test.clj`.
- **Task 5.1 string prims** ← `string.c`; gate: `clojure_string_test.clj`.
- **Task 5.2 regex** ← `regex/re.c` + `prim/regex.c`; gate: any regex test.
- **Task 5.3 reflection/meta/atoms** ← `reflection.c`, `meta.c`, atom subset of
  `stateful.c`; gate: `atom_test.clj`, `clj_metadata_test.clj`.
- **Task 6.1 mino.store in-memory** ← `prim/store.c` (drop WAL/durability) +
  copy `lib/mino/store.clj` to `resources/mino/store.clj`; gate: the store tests
  under `tests/` (find them: `grep -l "mino.store\|store/" ~/src/mino/tests/*.clj`).
- **Task 7.1 store durability** ← WAL + snapshot half of `prim/store.c`; gate:
  store durability tests.
- **Task 8.1 host API + mentat alias** ← a small `pub mod embed` giving
  `Interp::new()`, `eval(&str)`, `register_prim(name, PrimFn)`, and
  `alias_namespace("mentat.store", "mino.store")`; gate: a Mentat-side test that
  `(require 'mentat.store)` resolves.

---

## Self-Review

- **Spec coverage:** every non-excluded mino subsystem in `INTERNAL_MODULE_MAP.md`
  maps to a task; excluded subsystems (VM/JIT/net/async/STM/SLAD/proc/host) are
  listed verbatim under Global Constraints with the YAGNI rationale.
- **Placeholder scan:** Phases 0–1 have full code; Phases 2–8 give exact
  port-reference C files + exact acceptance corpus files + fixed interfaces, and
  defer only the *step-level* code to execution because it's diff-against-a-known-
  C-file work whose shape is fixed. No "TBD"/"handle edge cases".
- **Type consistency:** `Value`, `Symbol`, `Env`, `Interp`, `Throw`, `PrimFn`,
  `Closure` names are used identically across tasks; `Value` grows by explicit
  variant additions noted in each task that adds one.
- **Naming rule:** `mino.*` stays inside `mino-rs`; `mentat.*` is a host-side
  alias (Task 8.1) — consistent with the Global Constraint.

---

## Open questions to resolve before Task 0.1

1. **GC crate choice:** `gc`/`rust-gc` (cycle-collecting, `Trace` derive) vs
   `Rc` + a manual cycle break. Clojure data is immutable/persistent so true
   cycles are rare (only closures capturing their own env). Start with `gc`;
   if `Trace` derive friction is high, fall back to `Rc<RefCell<..>>` for envs
   only. Decide in Task 0.1.
2. **Bignum:** RESOLVED — required in Phase 5.5 (`arithmetic_test.clj` asserts
   `N`-promotion + overflow-throw). Back with `num-bigint` + `num-rational`,
   not imath. Phase 1 gate excludes the overflow deftest.
3. **Crate home:** standalone repo (reused by Mentat + pg_mentat via git dep) vs
   a workspace member. Given two consumers, a **standalone repo** is right; add
   it to Mentat's Cargo.toml as an optional git/path dep behind a `mino` feature.
