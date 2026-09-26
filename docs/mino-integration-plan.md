# Mentat × mino integration — review & plan

Goal: give Mentat a Clojure-like scripting/data layer, the way Datomic is
driven from Clojure. mino (`~/src/mino`) is an **embeddable Clojure-dialect
Lisp in ANSI C**, not JVM Clojure — so "integrate like Datomic+Clojure" means
*embed the mino interpreter in-process* and expose Mentat's store to it.

Decision (per maintainer): **vendor the relevant mino sources into the Mentat
tree** rather than add an external git/crate dependency. mino is built exactly
for this — `./mino task amalgamate` emits a single `dist/mino.c` + `dist/mino.h`
whose stated purpose is "an embedder vendors the dist/ directory into their
tree." MIT-licensed, CalVer.

---

## 1. Review findings

### What mino gives us (the lucky part)

mino already ships `mino.store` (`lib/mino/store.clj` + C API), which is an
**EAVT fact store** with:

- accumulating fact log `[e a v tx instant op]` + materialized entity view,
- schema with `:unique` / `:identity` / cardinality / `:ref`,
- lookup-refs and upserts,
- durability (snapshot + append-only EDN WAL),
- a Clojure surface: `transact`, `read`, `entity`, `entities`, `with`,
  `put`, `retract`, `listen`.

This is the **same data model Mentat implements** (EAVT over SQLite, schema
keywords, `TypedValue`). So the integration is *not* "reimplement Mentat in a
Lisp." It's "wire mino's `store`-shaped Clojure API to real Mentat storage."

### mino C embedding API (from `src/mino.h`)

- Lifecycle: `mino_state_new/free`, `mino_env_new/free`, `mino_install_all`.
- Eval: `mino_eval_string`, `mino_eval` (protected `_ex` variants that don't
  leak into global error state), `mino_call`, `mino_read`.
- Values in: `mino_int/float/string/keyword/symbol/map/vector/set`, builders
  (`mino_vector_builder_*`, `mino_map_builder_*`).
- Values out: `mino_to_int/float/string`, `mino_typeof`, `mino_is_*`.
- Host bridge: `mino_handle`/`mino_handle_ptr`/`mino_handle_tag` (opaque host
  pointer wrapped as a Lisp value — this is how a Mentat `Store` crosses the
  boundary), `mino_prim` / `mino_prim_argv` (register a C function callable
  from Lisp), `mino_env_set`.
- Errors: `mino_last_error`, `mino_last_error_map`.
- Store C API: `mino_store_open/deref/publish/checkpoint/close`,
  `mino_is_store`.

### Mentat Rust API (the call target)

- `mentat::Store::open(path)`, `Store::transact(&str /* EDN */) -> TxReport`.
- `Conn::q_once` / `q_prepare` / `pull_attributes_for_entities` /
  `lookup_value_for_attribute`, `begin_transaction` → `InProgress`, `cache`.
- Queries and tx-data are **EDN strings** — and mino can already read/print
  EDN. That is the natural wire format between the two.
- Existing `ffi/` crate (`ffi/src/lib.rs`, `utils.rs`) has C-FFI patterns to
  copy (C string handling, error marshalling).

### Build reality check (verified on EC2 c7i.xlarge, AL2023, GCC 11.5)

- **Static-lib embedding works, no amalgamation needed.** Compile the
  Makefile's *exact flat per-dir globs* (NOT a recursive `find` — that wrongly
  pulls `vendor/bearssl/src/*`, `vendor/miniz/upstream/*`, `eval/bc/stencils/*`
  which need private includes / are codegen inputs). Verified list:
  `src/{eval,eval/bc,eval/bc/jit,diag,runtime,gc,public,values,collections,prim,interop,regex,async}/*.c`
  + `src/vendor/{imath,bearssl,miniz}/*.c`, excluding `main.c`. 128 files →
  `ar rcs libmino.a` (~4.1 MB).
- Include dirs: `-Isrc -Isrc/public -Isrc/runtime -Isrc/gc -Isrc/eval
  -Isrc/values -Isrc/collections -Isrc/prim -Isrc/async -Isrc/interop
  -Isrc/diag -Isrc/vendor/imath -Isrc/vendor/bearssl -Isrc/vendor/bearssl/inc
  -Isrc/vendor/miniz -Isrc/vendor/miniz/upstream`.
- CFLAGS `-std=c99 -O2 -DMINO_CPJIT=1 -Wno-array-bounds` (GCC 11.5
  array-bounds false positive; drop mino's `-Werror`). `-DMINO_CPJIT=0` is the
  interpreter fallback if the JIT can't build in CI.
- Link: `cc app.c libmino.a -lm -lpthread`.
- **End-to-end verified**: an embedded C program eval'd `(+ 1 2)` → 3 AND
  drove the EAVT store: `(mino.store/transact c {:alice {:age 30}})` then
  `(mino.store/read (mino.store/db c) :alice :age)` → **30**. This proves the
  Datomic-shaped data layer is reachable from embedded C (→ Rust FFI).
- `./mino task amalgamate` **OOM-crashes** (SIGKILL/exit 137) even with 7.6 GB
  free and no ulimit — a real bug in that task, not a resource cap. **Skip it;
  build the static lib from the source tree instead** (the recipe above). This
  is also better for Mentat: `build.rs` + the `cc` crate compiles the vendored
  `src/` directly, no separate generate step.

---

## 2. Architecture

```
Rust  mentat crate  ──(feature "mino")──►  mentat-mino  (new crate)
                                              │
                                    build.rs: cc vendored dist/mino.c
                                              │
                                    unsafe extern "C" bindings to mino.h
                                              │
   register prims:  mentat-transact! / mentat-q / mentat-pull / mentat-open
                                              │
        mino_handle("mentat.store", *mut Store)  ← Store crosses as opaque handle
                                              │
   user writes Clojure:  (require 'mentat)
                         (def db (mentat/open "foo.db"))
                         (mentat/transact db [{:person/name "Alice"}])
                         (mentat/q db '[:find ?n :where [?e :person/name ?n]])
```

Data flow: Lisp value → print to EDN string (mino side) → Rust reads EDN via
`edn` crate / passes to `Store::transact` / `q_once` → results back as EDN
string → `mino_read` into a Lisp value. **EDN is the ABI.** No hand-marshalling
of every type across FFI; reuse what both sides already do.

`ponytail:` EDN-string round-trip is the naive-but-correct bridge (one
serialize + one parse per call). Fast enough for a scripting layer; if a
hot-loop caller shows up, add direct `mino_val`↔`TypedValue` marshalling then.

---

## 3. Plan (phased, each phase independently reviewable)

**Phase 0 — vendor + build spike (VERIFIED on EC2)**
1. Vendor the mino `src/` tree (not the amalgamation — it's broken) into
   `mentat-mino/vendor/mino/`. Record the mino CalVer tag in `VERSION`.
2. New crate `mentat-mino` with a `build.rs` using the `cc` crate to compile
   the verified flat-glob source list (see §1 build recipe) into a static lib;
   no `-Werror`; `MINO_CPJIT=1` with `=0` fallback.
3. Minimal `extern "C"` block + smoke test: `state_new` →
   `eval_string("(+ 1 2)")` → `to_int` == 3 → `state_free`.
   **Status: the C-level equivalent already passes on EC2 (`SMOKE_OK`,
   store round-trip = 30). Remaining: port the smoke to Rust `cargo test`.**

**Phase 1 — expose Store as an opaque handle + `open`/`transact`**
4. Register C prims via `mino_prim_argv`: `mentat/open`, `mentat/transact`.
   `open` returns `mino_handle(store_ptr, "mentat.store")`; `transact` takes the
   handle + an EDN string (mino prints the Lisp arg to EDN), calls
   `Store::transact`, returns the `TxReport` as EDN.
5. Handle lifecycle: finalizer/tag so dropping the Lisp handle drops the Rust
   `Box<Store>` (mirror mino's store finalizer contract — GC sweep must not
   double-free; keep ownership Rust-side, hand out a raw pointer, free in a
   registered finalizer).
   **Gate: open in-memory, transact, reopen durable, read back (mirror
   `examples/embed_store.c`).**

**Phase 2 — query + pull**
6. `mentat/q` (EDN query string → `Conn::q_once` → results as EDN),
   `mentat/pull`, `mentat/lookup`.
   **Gate: a `.clj` script transacts a schema + data and queries it.**

**Phase 3 — ergonomics + feature wiring**
7. Add `mino = { path = "mentat-mino", optional = true }` and a `mino` feature
   to the top `mentat` Cargo.toml (`[features] mino = ["dep:mentat-mino"]`).
   Default OFF — pure-Rust users pay nothing.
8. Re-export a `mentat::script` module behind `#[cfg(feature = "mino")]`:
   `Interpreter::new()`, `eval(&str)`, `bind_store(&mut Store)`.
9. A bundled `mentat.clj` prelude (require'd on init) giving Datomic-flavored
   sugar over the raw prims.
   **Gate: `cargo build` (no feature) unchanged; `cargo build --features mino`
   links; example REPL script runs against a real Mentat DB.**

**Phase 4 — docs + example**
10. `examples/mino_repl.rs`, a short `docs/mino.md`, THIRD_PARTY_LICENSES note.

---

## 4. Open questions to settle before Phase 1

- **JIT in CI**: does `MINO_CPJIT=1` build under Mentat's CI toolchain, or
  ship with `=0`? (Built fine with GCC 11.5 on EC2; re-check Mentat's CI.)
- **Threading**: Mentat `Store` is `!Sync`-ish (holds a rusqlite `Connection`).
  mino states are per-thread; one interpreter ↔ one `Store` on one thread is
  the safe v1. Document it; don't build cross-thread sharing (YAGNI).
- **EDN dialect drift**: mino's EDN printer vs Mentat's `edn` crate reader —
  verify keywords, instants (`#inst`), refs, and `Uuid` round-trip. This is the
  one real risk; add a round-trip test early.
- **Vendor sync**: pin the mino tag; a `Makefile`/script target to regenerate
  the amalgamation so updates are one command, not a manual copy.

---

## 5. What we deliberately are NOT doing (yet)

- No JVM Clojure interop (mino is C; that's the whole point).
- No direct `mino_val`↔`TypedValue` fast path — EDN string bridge first.
- No cross-thread / multi-Store-per-interpreter sharing.
- No re-implementation of Mentat query semantics in Lisp — Mentat stays the
  engine; mino is the language surface.
