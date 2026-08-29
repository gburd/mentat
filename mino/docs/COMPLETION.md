# mino-rs port — completion report (8-hour unattended run)

Pure-Rust port of the mino Clojure-dialect interpreter, reusable by Mentat and
pg_mentat. Executed subagent-driven (fresh worker per task, orchestrator review
between tasks) against the plan in `docs/plan/mino-rs-port.md`.

## Status: ALL 8 PHASES COMPLETE

- **111 unit tests + 12 conformance corpus files pass. 0 failures. Warning-clean.**
- Ports mino @ `ead6e160`. ~11.7k lines Rust; runs ~6.6k lines of mino's own
  `.clj` (core.clj, clojure.string/set/instant, mino.store) **verbatim as data**.
- Conformance oracle = mino's own test corpus. 12 files gated & green:
  arithmetic (full, no skips), are, atom, binding, clj_control, clj_higher_order,
  clj_math, clj_metadata, clj_predicates, clojure_string, regex, **store (all 231
  deftests)**.

## What works

- Reader + printer (round-trip, output matches the mino binary), full value tower.
- Evaluator: if/do/quote/fn/def/apply, let/loop/**recur (constant-stack)**,
  destructuring (seq + map + nested + :as + :or), try/catch/finally/throw/ex-info,
  **defmacro/macroexpand/quasiquote** (auto-gensym, ns-qualification matching mino).
- Persistent collections: 32-way-trie vector, HAMT map/set (insertion-ordered),
  value hash/eq (**hashes pinned to the mino binary**).
- Numeric tower: Int/BigInt/Ratio/Float32/Float with JVM-Clojure promotion,
  overflow-throw vs primed `+'`, exact ratios, coercions, bitwise/unchecked.
  (bignum via `num-bigint`, not a C port.)
- Regex via `fancy-regex` (backreferences — the `regex` crate can't).
- Atoms (validators/watches), real metadata (doesn't affect eq/hash), reflection.
- **`mino.store`: the Datomic-shaped EAVT store** — transact (EAVT + map-sugar),
  schema (unique/identity/cardinality/ref), lookup-refs, upserts, datalog `q`,
  `datoms`, `pull`, and **durability (snapshot + WAL) that is byte-compatible
  with mino's C store in both directions** (mino writes → port reads, and vice
  versa — verified).
- **Host embedding API** (`mino_rs::embed::Interpreter`): `eval`,
  `eval_to_string` (EDN ABI), `register_prim`, `alias_namespace`, `def_global`.

## Mentat integration (as requested: `mentat.store`, not `mino.store`)

- `mentat` crate: optional `mino` feature (`mino = ["dep:mino-rs"]`), **default
  OFF** — pure-Rust Mentat users pay nothing. `mino-rs` is an optional path dep.
- `mentat::script::Interpreter` aliases `mentat.store`→`mino.store` (and
  `mentat`→`mino`) so users write `(mentat.store/open)`, `(mentat.store/transact
  ...)`, `(mentat.store/q ...)`. Gated integration test passes.
- `// TODO(next)`: back the `mentat.store` prims with Mentat's real SQLite engine
  via `register_prim` (the store currently uses mino-rs's own in-process EAVT
  store — the language surface is done; wiring it to Mentat's storage is the next
  task).

## Deliberately not ported (YAGNI for a data layer; documented in the plan)

Bytecode VM + copy-and-patch JIT, TLS/HTTP/net/pool, async/agents/STM, SLAD
images, subprocess, host interop. The tree-walker is the semantic reference those
merely optimize. GC uses the `gc` crate, not a port of mino's collector.

## Known deviations / debt (all marked `ponytail:` / `// Phase` in-code)

- Lazy seqs are eager (map/filter fully realize) — a handful of infinite-seq
  deftests skipped.
- No distinct PersistentList-vs-lazy-seq type; `list?` true for any cons.
- No map-entry type (`key`/`val` take a 2-vector); symbol/var metadata dropped.
- `Value::Var` is a flat-env placeholder, not a real var cell (no ns tables).
- 2 pre-existing clippy lints in reader.rs/printer.rs (build is warning-clean).
- One fancy-regex limitation: `(?i)` + backreference (1 assertion skipped).
- bigdec (`1.5M`) deferred — no corpus test needs it.

## Repos (neither pushed)

- `~/src/mino-rs` — the crate. 20 commits, one per task + the empty-list fix.
- `~/ws/mentat` — 1 commit: the optional `mino` scripting layer.
