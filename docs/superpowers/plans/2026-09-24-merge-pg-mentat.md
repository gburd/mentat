# Merge pg_mentat into mentat — one repo, two storage backends

> **For agentic workers:** REQUIRED SUB-SKILL: use superpowers:subagent-driven-development
> (recommended) or superpowers:executing-plans. Steps use checkbox (`- [ ]`) syntax.

**Goal:** One git repository (`codeberg.org/gregburd/mentat`) that builds either
(a) the `mentat` crate, an embedded Datomic-like store on SQLite, or (b) the
`pg_mentat` PostgreSQL extension on pgrx, plus `mentatd`. One copy of the shared
front-end (`edn`, `core-traits`, `core`), one copy of the mino scripting glue,
one toolchain, one lockfile, one CI.

**Architecture:** The two projects already share their front-end and differ only
in their storage/query backend. Reconcile the three shared crates first (they
drifted in both directions), then bring pg_mentat's history into the mentat repo
with `git subtree`-style path rewriting so its 1,258 commits survive, then point
both backends at the single front-end. `mino-rs` moves into the repo too, as
`crates/mino` (§ 1.24): upstream mino is archived, so this project maintains the
interpreter from here on.

**Tech stack:** Rust 1.98 (stable, pinned), edition 2021, pgrx 0.17 (pg13–18),
rusqlite 0.40 (bundled SQLite), mino-rs, nix flake devShells.

**Spec:** this document. Evidence for every difference below was taken from the
two working trees on 2026-09-24: `~/ws/mentat` @ `a48aec6d` (branch `pg` /
`mino-scripting`) and `~/ws/pg_mentat` @ `6188a12` (branch `main`), plus
`~/src/mino` @ `9c65bb50` and `~/src/mino-rs` @ `v0.1.0`.

## Global constraints

- Version: **1.6.1** everywhere (highest across both repos; pg_mentat and mentatd
  are at 1.6.1, mentat at 0.14.0). The first release of the merged repo is **1.7.0**.
- Toolchain floor `rust-version = "1.88"` (pg_mentat's); pinned toolchain **1.98**.
- License: Apache-2.0 (both repos already are). `crates/mino` keeps mino's MIT
  license file (MIT-licensed code may sit in an Apache-2.0 repo; its `Cargo.toml`
  says `license = "MIT"`).
- **No local-filesystem references** anywhere in the repo: no `path =` that
  leaves the repo, no `/home/...` or `~/src/...` in any `Cargo.toml`,
  `.cargo/config.toml`, script, or test. Workspace-internal `path =` deps are
  fine. CI checks this (Task 6).
- Upstream mino is archived. We find and fix its bugs ourselves; `~/src/mino`
  is a reference only, never a build or test input.
- The default `cargo build` at the repo root builds the SQLite side only. It must
  never require `pg_config`, libclang, or a PostgreSQL install.
- Nothing is lost: every feature present in either copy of a shared crate survives.
- pg_mentat's full git history is preserved (no squash import).
- Work on a branch `merge/pg-mentat`; `master`/`main` are touched only at release.
- Every task ends green on: `cargo test --workspace` (SQLite side) and
  `cargo pgrx test pg16` (extension), unless the task says otherwise.

---

# Part 1 — Every difference, and what to do about it

Ranked by consequence. Each item: what differs, evidence, decision, why.

## 1.1 Security: `mentat_eval` gives any database user the server's filesystem

**What.** pg_mentat 1.6.0 added `#[pg_extern] fn mentat_eval(script)` behind the
`script` feature. It builds `mino_rs::Interpreter::new()`, which unconditionally
installs `slurp`, `spit`, `mkdir-p`, `rm-rf`, `file-exists?` (mino-rs
`src/store.rs:398-402`) plus the store's WAL/snapshot file prims. The extension
issues no `REVOKE`, so PostgreSQL's default applies: **`EXECUTE` is granted to
`PUBLIC`**. Any role that can connect can run
`SELECT mentat_eval('(slurp "/etc/passwd")')` or `(rm-rf "/var/lib/postgresql")`
with the postgres OS user's privileges. There is also no step or heap limit, so
`(loop [] (recur))` pins a backend forever.

**Exposure today.** `script` is off by default (`default = ["pg16"]`) and none of
the flake, Dockerfile, or release workflow enable it, so shipped artifacts are
safe. Anyone who builds with `--features script` is not.

**Decision (settled 2026-09-24: `mentat_eval` stays open to every role).** No
`REVOKE`. That makes the sandbox the entire defense, so it must hold against a
hostile caller, not just a careless one. Three distinct ways a script can hurt the
server, each needing its own guard:

| Attack | Guard |
|---|---|
| Host access: `slurp`, `spit`, `rm-rf`, `mkdir-p`, `file-exists?`, the store's file-path `open` | **Capability gating.** `Interpreter::sandboxed()` installs the language, regex, bignum, atoms and the in-memory store only. Every prim that touches the host is absent (unbound), not merely refused. |
| CPU: `(loop [] (recur))` | **Step limit** (upstream `MINO_OPT_LIMIT_STEPS`): counter in `eval`, throws `:eval/limit`. Plus a hook every 4096 steps that calls `pgrx::check_for_interrupts!()` so `statement_timeout` and `pg_cancel_backend` work. |
| Memory in one step: `(range 100000000000)`, `(apply str (repeat 1e9 "x"))`, `(vec (range …))` — a single prim call that allocates unboundedly, invisible to a step counter | **Allocation budget** (upstream `MINO_OPT_LIMIT_HEAP`): bulk-producing prims (`range`, `repeat`, `str`, `into`, `vec`, `concat`, `mapv`, `apply`, string builders) charge the budget per element and throw `:eval/limit` when exceeded. Charging per element also advances the step counter, so these prims can't dodge the step limit either. |
| Stack: non-tail recursion `(defn f [n] (inc (f (dec n))))` with n = 10⁶ | **Depth limit.** Verified: today this aborts the process with `fatal runtime error: stack overflow` (SIGABRT). In a Postgres backend that crashes the server into recovery for every connected client. A counter on `Interp` for eval/apply nesting throws `:eval/limit` past `max_depth`; in pg_mentat the hook also calls `pg_sys::stack_is_too_deep()` (exposed by pgrx-pg-sys 0.17) as a second line. |

Limits come from GUCs, all `PGC_SUSET` (a superuser or `ALTER SYSTEM` sets them;
ordinary roles cannot raise them for their own session):

| GUC | Default |
|---|---|
| `pg_mentat.script_max_steps` | 10,000,000 |
| `pg_mentat.script_max_heap_bytes` | 64 MB |
| `pg_mentat.script_max_depth` | 2,000 |

`statement_timeout` still applies on top, via the interrupt hook.

What an open `mentat_eval` means for data access: a script runs as the calling
role through SPI, so it can read and write exactly the pg_mentat stores that role
can already reach with `mentat_query`/`mentat_transact`. No privilege is gained.
Do not make `mentat_eval` `SECURITY DEFINER` — that would turn it into a
privilege escalation.

`Interpreter::new()` keeps full host access for the mino CLI and tests. Both
scripting layers (mentat `script.rs`, pg_mentat `script.rs`) use `sandboxed()`.

**Why first.** Today's code is the version the decision makes public. It is the
only item that is an active hazard rather than a maintenance cost.

## 1.2 `:rules` vs `:with` — the two parsers disagree on what a query means

**What.** mentat parses rule definitions under `:rules`
(`edn/src/lib.rs:601`). pg_mentat parses them under **`:with`**, in an alternative
listed *before* `:with ?var…` (`pg_mentat/edn/src/lib.rs:592-593`), and its tests
use `:with [[(adult ?p) …]]` (`or_rule_tests.rs:94`).

**Why it matters.** In Datomic, `:with` names variables that keep duplicate
tuples in aggregates; rules arrive through `:in %`. pg_mentat's parser tries the
rules alternative first and falls back, so `:with ?x` still works, but the same
query keyword means two different things depending on its argument shape, and
neither project matches Datomic for rules.

**Decision.** Accept all three spellings in the merged grammar, with Datomic's as
canonical:
- `:in $ %` + a rules input — Datomic's form. Add it (pg_mentat's query layer
  already has input-binding plumbing for `%`-style inputs via `in_bindings`).
- `:rules [...]` — keep (mentat's, explicit and unambiguous).
- `:with [[(rule …) …]]` — keep **as a deprecated alias**, recognized only when
  the argument is a vector of rule clauses (the parser already distinguishes this
  by shape). Emit a deprecation warning from pg_mentat's query entry point; remove
  in 2.0.
Keep the `:with ?var…` meaning unchanged. Update pg_mentat's tests to use `:rules`
and add tests for all three spellings.

## 1.3 `:in` binding forms — pg_mentat has them, mentat throws them away

**What.** pg_mentat's grammar parses `:in` as binding forms
(`:in binding()+ → InBindings`, pg `lib.rs:587`), supporting `?x`, `[?x ...]`,
`[?a ?b]`, `[[?a ?b]]`. mentat's grammar still parses `:in variable()+ → InVars`
(`lib.rs:596`); the `InBindings` variant and `binding()` rule exist in mentat's
`edn` but are dead code (the `ponytail:` note at `query.rs:1014`). pg_mentat's
query engine consumes `in_bindings` (`functions/query.rs:891,1190`); mentat's
algebrizer does not.

**Decision.** Take pg_mentat's grammar: `:in` parses to `InBindings`, and
`ParsedQuery::in_vars` stays derived from the scalar bindings (pg_mentat already
does this, `query.rs:1104-1118`), so mentat's algebrizer keeps working unchanged
for scalar inputs. Then teach mentat's algebrizer `BindColl`/`BindTuple`/`BindRel`
inputs as a follow-up task (§ Task 12): until then it must *reject* non-scalar
bindings with a clear error rather than silently dropping them. Also make sure
`variable()` accepts `$` (source vars) inside `:in` — check against the existing
`src_var()` rule.

## 1.4 History queries: the 5th pattern place `?added`

**What.** pg_mentat's `Pattern` has a fifth place, `added`, so
`[?e ?a ?v ?tx ?added]` works in `:where`; `Pattern::new` takes six arguments
(pg `query.rs:795-855`, grammar `lib.rs:422-447`). mentat's `Pattern` has four
places. mentat supports `?added` only through `(tx-data $ ?tx)` binding
(`query-algebrizer/src/clauses/tx_log_api.rs:435`). mentat's algebrizer builds
`Pattern { … }` literals in 33 places across 5 files, which will all fail to
compile once the field exists.

**Decision.** Adopt the 5-place `Pattern` in the shared `edn`. It is Datomic's
history-db shape and pg_mentat's history queries need it.
- Add the field; keep `Pattern::simple` for 3-place construction.
- Add a `Pattern::with_added(...)` builder and make the 33 literal sites in
  mentat's algebrizer use `..Pattern::default_places()` or pass
  `PatternNonValuePlace::Placeholder`. Mechanical.
- mentat's algebrizer: a pattern with a non-placeholder `added` place is rejected
  with `AlgebrizerError::UnsupportedHistoryPattern` (SQLite's `datoms` table has
  no retractions; history lives in `transactions`). A follow-up (§ Task 12) routes
  such patterns to the `transactions` table, which does have `added`.
- The `collect_mentioned_variables` change in pg `query.rs:1313` comes along.

## 1.5 Reverse pull `:person/_friends` — both have it, one in the parser, one outside

**What.** mentat added reverse pull *to the shared parser*: a
`raw_backward_namespaced_keyword` pull alternative, a `reverse: bool` field on
`NamedPullAttribute`, and `Display` support (mentat `lib.rs:371-382`,
`query.rs:467-521`), consumed by `query-pull/src/lib.rs:138-311`. pg_mentat
implements reverse pull entirely inside its own pull-pattern parser
(`functions/pull.rs:265-300`, its own `PullAttrSpec { reverse, forward_ident }`)
and never touches `edn`'s pull types.

**Decision.** Keep mentat's `edn` version (the field is additive; nothing in
pg_mentat constructs `NamedPullAttribute`). pg_mentat's own parser stays for now —
it supports pull options the `edn` grammar doesn't (`:default`, `:limit`, map specs,
recursion depth). Record as later debt: move pg_mentat's richer pull grammar into
`edn` so both backends parse pull the same way.

## 1.6 Plain keywords as pattern values — pg_mentat accepts `[?e :status :active]`

**What.** In a pattern's value place, mentat accepts only namespaced keywords
(`if x.is_namespaced()`, mentat `query.rs:381`) and returns `None` for plain ones
(`// … yet.`). pg_mentat accepts any keyword (pg `query.rs:378`). Datomic accepts
both.

**Decision.** Take pg_mentat's behavior. Check mentat's algebrizer first: it
resolves `PatternValuePlace::IdentOrKeyword` against the schema
(`clauses/pattern.rs:524-538`) and falls back to a keyword constant when the ident
is unknown — a plain keyword takes the fallback path, so this should just work.
Add algebrizer tests with a plain-keyword value to prove it before merging.

## 1.7 `Limit::None` vs `Limit::Unlimited`

**What.** Same variant, two names. mentat: `Limit::None` (used in 10 places in
`query-algebrizer`, `query-projector`, `query-sql`). pg_mentat: `Limit::Unlimited`
(`functions/query.rs:5101`), which also matches its `Offset::Unlimited`.

**Decision.** `Limit::Unlimited` — it matches `Offset::Unlimited`, and `None`
reads as `Option::None` in a match. Rename mentat's 10 uses.

## 1.8 Built-in transaction functions `:db.fn/cas`, `:db/retractEntity`

**What.** pg_mentat's `edn` defines `BuiltinTxFn { Cas, RetractEntity }`
(pg `entities.rs:227-247`) and its transactor recognizes `:db.fn/cas`,
`:db/cas`, `:db.fn/retractEntity`, `:db/retractEntity`
(`functions/transact.rs:452-457`). mentat has neither — no grep hit anywhere in
`db/`, `transaction/`, or `edn/`.

**Decision.** Keep the `BuiltinTxFn` enum in the shared `edn` (additive, zero
cost). Port both functions to mentat's SQLite transactor as their own task
(§ Task 11): `retractEntity` expands to retractions of every datom with that `e`
(plus component entities, as Datomic does); `cas` reads the current value inside
the write transaction and fails with a typed error on mismatch. These are among
the most-used Datomic transaction functions; mentat should have them.

## 1.9 `enum-set` 0.0.8 vs `enumset` 1.1

**What.** mentat's `core-traits` and `core` use the long-dead `enum-set` crate,
which requires a hand-written `unsafe fn from_u32` using `mem::transmute`
(mentat `core-traits/lib.rs:305-313`). pg_mentat switched to `enumset` 1.1:
`#[derive(EnumSetType)]` on `ValueType`, by-value set ops, a test for the
extension trait (pg `value_type_set.rs`). `enumset`'s derive supplies
`Clone, Copy, PartialEq, Eq`, which is why pg dropped those from the derive list.

**Decision.** Take pg_mentat's. It deletes an `unsafe` block and a 2016-era
dependency. Nothing outside `core-traits`/`core` names `enum_set`
(grep: only those three files). `ValueTypeSet::iter()` changes return type
(`enumset::EnumSetIter`); check mentat's algebrizer for callers that name the old
type — callers that only iterate are unaffected.

## 1.10 Modern-Rust idioms (the rest of the drift)

**What.** Behavior-identical rewrites present only in pg_mentat's copies:
`f64::NAN` for `std::f64::NAN`; `is_none_or` for `map_or(true, …)`;
`is_multiple_of` for `% == 0`; `Self::` in `From` impls; `#[expect(dead_code)]`
for `#[allow(dead_code)]`; inline format args; runnable doctests replacing
`ignore`d ones in `types.rs` (using `Value::from_symbol`/`from_keyword`, which both
copies have); a `#[expect(clippy::expect_used, reason = …)]` in
`microsecond_precision`.

**Decision.** Take all of pg_mentat's. They need Rust ≥ 1.82/1.87 (`is_none_or`,
`is_multiple_of`), which the 1.88 floor covers, and the doctest change turns two
dead examples into tests.

**One exception.** In `core/src/lib.rs:335`, pg_mentat wrote
`edn::parse::value(&expected_output)` where mentat has
`edn::parse::value(expected_output)` — pg's is a needless borrow that clippy flags
and that pg's workspace lints merely allow. Keep mentat's line.

## 1.11 The EDN escape fix — the same fix, committed twice

**What.** `fix(edn): unescape \n/\t/\r, and escape text when printing` exists as
mentat `a48aec6d` and pg_mentat `6188a12`. The trees match; the commits don't.

**Decision.** Nothing to reconcile in code. During the history import (§ Task 4)
the two commits both land; the second becomes a no-op. Keep both for provenance.

(Correction to my earlier review: I said pg_mentat lacked this fix. It doesn't.)

## 1.12 Copyright headers

**What.** mentat's files say `Copyright 2016-2018 Mozilla`; pg_mentat's say
`Copyright 2016 Mozilla`. The original upstream header was `Copyright 2016
Mozilla`; mentat's range was introduced in its own `8dbb2c2f WIP` commit.

**Decision.** `Copyright 2016-2018 Mozilla` — Mozilla's work on these files ran
through 2018, and a range is the accurate statement. Contributors after that are
credited in `authors` and git history, not in the header. Mechanical sweep.

## 1.13 Dependency declarations in the shared crates

**What.** mentat declares tilde requirements (`~0.4`, `~5.0`, `~1.25`); pg_mentat
declares exact-ish minimums (`0.4`, `5.1.0`, `1.21.0`). Real resolution drift is
small: chrono 0.4.45/0.4.44, uuid 1.25.0/1.23.1, ordered-float 5.0.0/5.3.0,
itertools **0.15 vs 0.14** (edn uses only `diff_with` and `Itertools`, both stable
across those). pg_mentat's `core` also depends on `thiserror 2.0`; mentat's
error crates already use `thiserror ~2.0`.

**Decision.** Move every shared third-party dependency to
`[workspace.dependencies]` in the root manifest, one version each, and have
crates say `chrono.workspace = true`. Pick the higher of each pair
(itertools 0.15, ordered-float 5.3, uuid 1.25, chrono 0.4.45, peg 0.8.6,
pretty 0.12.5, indexmap 2.14, bytes 1.12, serde 1.0.229). One `Cargo.lock`,
**committed** (pg_mentat commits its lock; mentat gitignores it — an extension
and a server binary want a committed lock, and cargo's current guidance says
commit it for libraries too).

## 1.14 Toolchain and flake

**What.** mentat: `rust-toolchain.toml` says `stable`, flake uses
`rust-bin.stable.latest` (→ 1.98 today, floats). pg_mentat: no
`rust-toolchain.toml`, flake pins `rust-bin.stable."1.90.0"`, Cargo says
`rust-version = "1.88"`, and its flake carries the real pgrx machinery: LLVM/clang
for bindgen, per-PG-version packages, a sandbox-safe `cargo pgrx package` that
never calls `cargo pgrx init`.

**Decision.** Pin **1.98** in `rust-toolchain.toml` (a floating `stable` makes the
nix build and the rustup build disagree the day a release lands), keep
`rust-version = "1.88"` as the MSRV. Take pg_mentat's flake as the base — it is
the harder one to get right — and add a lean `devShells.sqlite` without
clang/postgres next to a `devShells.pg` (default). Package outputs:
`packages.mentat-cli`, `packages.pg_mentat-pg{13..18}`, `packages.mentatd`.

## 1.15 Workspace lints

**What.** pg_mentat has a `[workspace.lints.clippy]` table (deny `todo`,
`unimplemented`, `dbg_macro`, `print_stdout`, `print_stderr`, `exit`,
`mem_forget`, `await_holding_lock`, …). mentat has none, and contains 15
`todo!`/`unimplemented!`/`dbg!` sites (e.g. `query-algebrizer/src/clauses/resolve.rs:194,210-212`,
`db/src/cache.rs:1926`). `tools/cli` prints to stdout by design.

**Decision.** Adopt pg_mentat's lint table at the workspace level. Crates opt in
with `[lints] workspace = true`: the shared front-end and the pg side on day one;
mentat's SQLite crates after Task 10 replaces their `unimplemented!()`s with typed
errors (each of those is a reachable panic on a query shape the parser accepts).
`tools/cli` opts in with `print_stdout = "allow"`.

## 1.16 Release profile

**What.** mentat: `opt-level = 3, lto = true, debug = false`. pg_mentat:
`opt-level = 3, lto = "fat", codegen-units = 1`.

**Decision.** pg_mentat's (`lto = true` is `"fat"` already; `codegen-units = 1`
is the real difference and matters for an extension's per-query hot path).
Add `[profile.release-dev]` inheriting `release` with `lto = "thin"`,
`codegen-units = 16` for fast local optimized builds.

## 1.17 CI

**What.** mentat: eight GitHub workflows from the Mozilla era (Travis,
Taskcluster, cross-compile, grcov, msrv…) — mostly dead. pg_mentat: a Forgejo
`ci.yml` (Codeberg runs Forgejo Actions), a GitHub `ci.yml`, installcheck across
PG versions, docker, nix, release, `cargo-deny` via `deny.toml`, and a
`.gitlab-ci.yml`.

**Decision (settled 2026-09-24).** Codeberg is the only place anyone pushes;
Codeberg push-mirrors to GitHub (pg_mentat's mirror already does — its `main`
tips match on both sides). So two CI systems run on every push, each for a
different job:

- **Forgejo Actions on Codeberg** — the gate. One `.forgejo/workflows/ci.yml`:
  `fmt`, `clippy`, `deny`, `test-sqlite`, `test-pg (13-18)`, `test-mino`, `test-ffi`,
  `nix-build`.
- **GitHub Actions on the mirror** — publishing. It runs whatever is in
  `.github/workflows/` of the mirrored tree, so that directory is the merged
  repo's release pipeline. Keep, with paths updated for `crates/pg/pg_mentat`:
  `release.yml` (on tag: `cargo pgrx package` per PG version → GitHub Release
  assets), `docs.yml` (mdBook → GitHub Pages), `installcheck.yml`,
  `docker-test.yml`, `nix-test.yml`. Drop pg_mentat's GitHub `ci.yml` and
  `audit.yml` — Forgejo's CI is the gate; duplicating it on GitHub only produces a
  second, occasionally-disagreeing red X.
- Delete mentat's eight Mozilla-era GitHub workflows (`clippy_check`, `clippy-ng`,
  `cross_compile`, `grcov` (wants a `COVERALLS_TOKEN` nobody has), `msrv`,
  `nightly_lints`, `quickstart`, `audit`), `dependabot.yml` (GitHub-side PRs
  against a mirror can't be merged there), `FUNDING.yml`, `.travis.yml`,
  `.taskcluster.yml`, and `.gitlab-ci.yml` (no GitLab mirror).
- The `docs.yml` Pages site will now come from the mentat mirror, so its URL
  changes from `gburd.github.io/pg_mentat` to `gburd.github.io/mentat`. The
  mentat GitHub repo still has Mozilla's old `gh-pages` branch; `docs.yml`
  replaces it.
- **Verify before Task 14** that `codeberg.org/gregburd/mentat` has a push mirror
  to `github.com/gburd/mentat`. It isn't mirroring today: GitHub `master` is
  `e55376e9`, Codeberg `master` is `201ec39d` (Codeberg is ahead), and
  `mino-scripting`/`v0.14.0` never reached GitHub. `tea` can't read mirror
  settings with the current token (needs repo admin scope), so check in the
  Codeberg web UI: *Settings → Repository → Mirror settings → Push mirrors*.
  Enable *Sync when commits are pushed*.

## 1.18 Dead weight in mentat

**What.** `sdks/android`, `sdks/swift` (last touched 2018), `ffi/` (their C ABI),
`automation/`, `build/`, `fixtures/`, `NOTES`, `.ignore`, `_/` (a scratch
`flake.nix`, `mov.edn`, `shell.nix`), `.vscode/`, `docs/` Jekyll site.

**Decision.** Delete `sdks/`, `automation/`, `_/`, `.vscode/`, `NOTES`, `.ignore`
(and the CI files listed in § 1.17). **Keep `ffi/`** — a C ABI over the embedded
store is exactly what non-Rust hosts need; build it in CI so it stops rotting.
Keep `fixtures/` and `build/` if the test suite reads them (check first; delete if
not). Replace the Jekyll site with pg_mentat's mdBook (`docs/book.toml`) and fold
mentat's `about.md`/`tutorial.md` into it.

## 1.19 Two mino scripting layers, 998 and 732 lines, one design

**What.** mentat `src/script.rs` (998 lines) and pg_mentat
`functions/script.rs` (732 lines) implement the same `mentat.store/*` surface
(same 14 prims). They share nine function names but drifted in bodies:
`db_value`, `inst_value`, `uuid_value` differ. mentat converts
`Binding`/`TypedValue` → mino; pg_mentat converts JSON → mino (its engine returns
JSON). The storage calls differ by backend, as they must.

**Decision.** Extract a `mentat-script` crate holding everything
backend-independent: the db-value map shape, handle/eid/keyword argument parsing,
`tx_report_value`, the inst/uuid/keyword value builders, and the prim registration
driven by a trait:

```rust
pub trait ScriptBackend {
    fn basis_tx(&self) -> Result<i64, String>;
    fn transact(&mut self, edn: &str) -> Result<TxReport, String>;
    fn with(&mut self, db: &DbRef, edn: &str) -> Result<TxReport, String>;
    fn q(&self, db: &DbRef, query_edn: &str) -> Result<Value, String>;
    fn pull(&self, db: &DbRef, eids: &[i64], pattern_edn: &str) -> Result<Value, String>;
    fn datoms(&self, db: &DbRef) -> Result<Vec<(i64, String, Value)>, String>;
    // entity / read / entities default-implemented on top of q + pull
}
```

`mentat` implements it over `Store`; `pg_mentat` over its engine functions. Each
side keeps only its conversion code (Binding→Value, JSON→Value). One test suite
(`mentat-script/tests/`) runs the Datomic-model tests against both backends — the
same 11 behaviors, proven twice. This is where the two layers' behavior was
already drifting (the three differing helper bodies), so it pays for itself fast.

## 1.20 Two temporal models

**What.** pg_mentat supports `as-of`/`since` for full Datalog `q` (its query
engine accepts `{"asOf": T}` / `{"since": T}` inputs; `functions/time_travel.rs`).
mentat supports them only for `datoms`/`entity`/`read` by replaying the
`transactions` table, and makes `q` on a historical db an error.

**Decision.** Not a reconciliation — a capability gap. It stays until someone
teaches mentat's algebrizer to target `transactions` with a tx bound (same work as
routing `?added` patterns, § 1.4, so do them together in Task 12). The shared
`mentat-script` test suite marks the historical-`q` test `#[cfg(backend = "pg")]`
until then, so the gap is visible in one place.

## 1.21 Functional gaps (not drift — features only one side has)

From pg_mentat's own port inventory (`docs/pg_mentat-port-inventory.md`, already
in mentat's repo) plus this review:

| Feature | mentat | pg_mentat | Plan |
|---|---|---|---|
| `:db.fn/cas`, `retractEntity` | no | yes | port (Task 11) |
| `?added` / history in `:where` | via `tx-data` only | yes | port (Task 12) |
| Historical `q` (as-of/since) | no | yes | port (Task 12) |
| `:in` coll/tuple/rel bindings | no | yes | port (Task 12) |
| Reactive subscriptions (LISTEN/NOTIFY) | no | yes | PG-only, stays |
| BM25 full-text, pgvector, pg_trgm, PostGIS, rum | no | yes | PG-only, stays |
| Materialized/virtual views, multi-store, excision | partial | yes | later, per inventory |
| `mentatd` HTTP/WebSocket server | no | yes | stays PG-only; see 1.22 |
| Embedded C ABI (`ffi`) | yes | no | keep, SQLite-only |

## 1.22 `mentatd` could serve either backend — later

`mentatd` depends on `edn` and talks to PostgreSQL through `tokio-postgres`; it
does not link the extension. It could front the SQLite store too. Out of scope for
the merge; it moves under `crates/pg/mentatd` and keeps working as-is.

## 1.23 Git history and branches

**What.** Both repos descend from Mozilla's `f7621712` (2016-07-05); mentat has
1,056 commits since, pg_mentat 1,258. mentat's `origin/pg` still holds
`d7e2b554 mid-s/failure/thiserror/g`, which our tip already supersedes (no
`failure` crate remains; both error files it touches use `thiserror`).
`origin/improv-base` holds `1af15e15`, a second copy of the escape fix.
pg_mentat has seven stale feature branches from April/May 2026.

**Decision.**
- Import pg_mentat's history under `crates/pg/` with `git filter-repo
  --to-subdirectory-filter` on a clone, then merge with
  `--allow-unrelated-histories` (they are related, but the rewritten paths make
  git treat them as unrelated; that's expected). Shared crates from the pg side
  land in `crates/pg/_import/{edn,core,core-traits}` and are deleted in the same
  merge after their changes are applied to the canonical copies (Task 2 does the
  application first, so the delete loses nothing).
- Tags: pg_mentat's `v1.2.1..v1.6.1` come along (they point into its history).
  mentat's own `v0.*` tags stay. No collisions (checked).
- Close `origin/pg` (`d7e2b554`): superseded. Close `origin/improv-base`:
  duplicate of `a48aec6d`. Don't import pg_mentat's April branches; they stay
  in the read-only pg_mentat repo.
- Default branch stays **`master`** (settled 2026-09-24). pg_mentat's `main`
  history arrives under `crates/pg/` via the import merge; no `main` branch is
  created in the merged repo.
- When 1.7.0 ships you mark both pg_mentat repos (Codeberg and its GitHub mirror)
  read-only with a message pointing at `mentat` (settled 2026-09-24). Task 14
  prepares the text and a final pointer commit; the settings change is yours.

**Hygiene note from this review:** computing a merge-base with
`git fetch ../pg_mentat HEAD:refs/tmp/x` imports that repo's tags. I did that once
and cleaned up the 13 leaked tags; use `git fetch --no-tags` in every task below.

## 1.24 Deep nesting crashes the server — in the default build (found 2026-09-24)

**What.** Reproduced on a throwaway PG16 cluster with the installed pg_mentat
1.6.1, default build, no `script` feature: an unprivileged role runs
`mentat_query` with a query containing a vector nested 3,000 deep. The backend
dies with SIGSEGV; the postmaster terminates every server process and runs crash
recovery. Depth 2,000 returns a parse error. `mentat_transact` crashes the same
way. The shared `edn` crate's `peg` grammar recurses once per nesting level
(`vector() = "[" value()* "]"`); pg_mentat's `MAX_EDN_NESTING = 100` check
(`types/edn.rs:15`) inspects the value *after* `edn::parse::value` returns, and
`mentat_query`/`mentat_transact` never call it.

mino-rs has the same class of bug on paths an eval-depth limit doesn't cover
(release build, 8 MB stack): recursive calls ~10⁶; GC marking of live nested
data ~200k (`gc` 0.5 traces recursively); `pr-str` ~50k; the reader ~50k; `=`
~100k; `pr-str` of an atom that contains itself, always.

**Can this be fixed by making the recursion a tail call?** Tested, no
(rustc 1.95, 1 MB stack, depth 10⁶):

| Shape | opt-level 0 | opt-level 3 |
|---|---|---|
| Accumulator `f(n-1, acc+1)` — a true tail call | overflows | survives |
| Parser `[ value* ]` — child pushed into parent `Vec` after the call | overflows | overflows |
| `1 + f(n-1)` | overflows | survives, but only because LLVM rewrote it into a loop (zero self-calls left in the asm) |
| Same, with the `+1` chosen by runtime data, as an interpreter does | overflows | overflows |

Rust never guarantees tail calls: they happen at `-O`, not in debug builds,
and not across `Result`/`?` or `Drop`. Nightly `become` (`explicit_tail_calls`)
guarantees one, but only when the call *is* the return value. That is never true
here: a parser needs the child to finish the parent, an evaluator needs
`(f (dec n))` to compute `(inc …)`, a printer writes the closing `]` after the
children. Recursion whose result is used can't be a tail call in any language;
C compilers would face the same limit.

The fix that works, verified in the same harness: an explicit worklist on the
heap (a `Vec` of partially-built containers) with a depth cap. A 10⁶-deep input
parses on a 1 MB debug stack; a cap of 1,000 returns an error cleanly. The
experiment also found a second trap: **the compiler-generated `Drop` of a 10⁶-deep
tree overflows too.** Every recursive owned type needs an iterative `Drop`, or a
depth cap at construction so no such tree exists.

**Decision.**
- `edn`: fail past `MAX_NESTING` = **256** before the grammar runs. Measured:
  ~2.9 KB of stack per level unoptimized, ~470 B in release, so 256 fits a 1 MB
  stack in a debug build; the deepest query/tx in either test suite nests 5.
  In `peg` that means a depth counter threaded through the recursive rules via a
  parser argument, or a linear bracket prescan before `peg` runs; the prescan is
  simpler and also bounds the `Drop` problem because no deep value is ever built.
  Strings, chars and comments must be skipped by the prescan.
- mino-rs: reader, printer, `=`, `hash`, `compare` get depth caps; the printer
  gets cycle detection (atoms/vars); GC tracing becomes a worklist (vendor the
  `gc` crate into the repo and change its `mark`, or replace it — decided in
  Task 1b after reading its trace code); recursive `Value` drops become iterative.
- Regression tests at every SQL entry point that parses text: `mentat_query`,
  `mentat_transact`, `mentat_pull`, `mentat_pull_many`, the `edn` type's input
  function, `mentat_eval`.

## 1.25 mino-rs moves into this repo

**Decision (2026-09-24).** mino-rs becomes `crates/mino` in the mentat repo, with
its full history (27 commits, `git filter-repo --to-subdirectory-filter`). The
`codeberg.org/gregburd/mino-rs` repo gets a pointer README and goes read-only
with the pg_mentat repos.

What changes:
- The crate name stays `mino-rs` (library name `mino_rs`), so no `use` line
  changes. Version continues from the workspace (1.6.1, then 1.7.0); its
  separate 0.x series ends at v0.1.0.
- mentat's `path = "../../src/mino-rs"` becomes a workspace path; pg_mentat's
  `git = ".../mino-rs", tag = "v0.1.0"` goes away when pg_mentat moves in (Task 4).
  Until then pg_mentat's 1.6.x fixes can't use a workspace path, so its security
  releases pin the in-repo crate by git: `git = "https://codeberg.org/gregburd/mentat",
  tag = "…"` (a remote, not a local path).
- The conformance corpus becomes part of the repo:
  `crates/mino/tests/corpus/*.clj`, the 12 files the harness gates on (208 KB,
  MIT), frozen at mino `ead6e160`. `MINO_SRC` and `.cargo/config.toml`'s
  `/home/gburd/src/mino` go away; tests use `env!("CARGO_MANIFEST_DIR")`.
  Refreshing to upstream's final `9c65bb50` (Task 8) replaces those files in one
  commit, so the diff shows what changed.
- We own the language now. Bugs found in this port are fixed here and recorded
  in `crates/mino/CHANGELOG.md`; `~/src/mino` is a reference, never an input.

---

# Part 2 — mino-rs refresh from upstream mino

mino-rs v0.1.0 ports mino `ead6e160`. Upstream `9c65bb50` is **489 commits
newer** (233 fixes, 54 features) and, as of 2026-09-15, **archived**: its README
now says "Discontinued — archived LLM experiment… Do not use it in production."

**What this means.** mino-rs becomes the maintained implementation of this
language. Pull upstream's last good work once, then own the code; don't plan
around future upstream fixes. Keep `~/src/mino` as the reference oracle binary
(rebuilt at `9c65bb50`).

**What upstream fixed or added that mino-rs lacks** (verified against the
rebuilt binary and a probe of mino-rs v0.1.0):

| Upstream (9c65bb50) | mino-rs v0.1.0 | Take it |
|---|---|---|
| `#uuid "…"` reads to a real UUID value, `type` = `:uuid`, prints `#uuid "…"`, round-trips via `read-string` | `unbound symbol: parse-uuid` | **yes** — unblocks the inst/uuid limitation in both scripting layers |
| `#inst "…"` prints as `#inst "…"` and round-trips | prints a calendar map | **yes** |
| `read-string` | missing | **yes** |
| Classed catch `(catch ArithmeticException e …)`, keyword catch `(catch :eval/type e …)`, `:default` (ADR 32, 37) | syntax error / unbound `e` | **yes** — upstream's corpus relies on it |
| Regex lookahead `(?=…)`/`(?!…)` | rejected (MCT001) | **yes** — fancy-regex supports it natively; just stop rejecting |
| `re-seq` with no match → `()` | `nil` | **yes** |
| `(keyword 42)` → `nil` | throws | **yes** |
| `clojure.string/join` stringifies non-string separators | throws | **yes** |
| `1.5M` bigdec literal | read error | **yes** — bigdec deferred earlier, now in corpus |
| `delay` has its own type | tagged map | **yes** |
| mino.store **backend seam** (ADR 35): durability behind a 5-op `{:kind :initial :wal-entries :commit :checkpoint :close}` map; `:memory`/`:file` built in; third-party backends by keyword | hard-wired file WAL in `src/store.rs` | **yes** — it is the natural home for a SQLite- or Postgres-backed `mino.store`, and ADR 42 says exactly this |
| Store fixes: entity-specs preserved across compact/merge; centralized db-value constructor; bundled-lib errors carry `:mino/kind` | older store.clj | **yes** (comes with store.clj refresh) |
| Reader: reject `::`/`:::`; merge chained `^meta` on symbols | older reader | **yes** |
| `compare` orders vectors by count first; `abs -0.0` keeps sign | already match | nothing |
| 52 new test files, +490 lines in `store_test.clj`, +209 in `regex_test.clj`, new `store_backend_test.clj`, `reader_features_test.clj` | — | **yes** — the corpus is the oracle |
| JIT, bytecode VM, TLS/HTTP/WebSocket/UDP/tar, signals, terminal, `core.logic`/`core.match`/spec bundles | out of scope | **no** (same YAGNI line as the original port) |

**Refresh procedure.** The resources (`core.clj`, `mino/store.clj`,
`clojure/{string,instant}.clj`) are loaded verbatim, so copy the new versions,
run the corpus, and implement whatever prims the new `.clj` calls that the port
lacks. Known new core.clj dependencies: `delay*`, `lazy-keep`, `lazy-map-indexed`,
`lazy-remove`, `__transduce-fuse`, `find-keyword`, `rerun-seq`, `mino-version`.
`lazy-*` can be eager (same stance as the port's existing `map`/`filter`);
`__transduce-fuse` can be the identity (it's an optimization hook). Resource sizes
move: core.clj 4141→4244 lines, store.clj 1899→2144, instant.clj 267→315.

Release as part of the workspace (no separate mino-rs version; § 1.25).

---

# Part 3 — Target layout

```
mentat/                              (codeberg.org/gregburd/mentat)
├── Cargo.toml                       workspace; [workspace.package] version = "1.6.1"
│                                    default-members = the SQLite side only
├── Cargo.lock                       committed
├── rust-toolchain.toml              channel = "1.98"
├── flake.nix                        devShells.{pg,sqlite}; packages.{mentat-cli,pg_mentat-pgNN,mentatd}
├── deny.toml                        from pg_mentat
├── .forgejo/workflows/ci.yml
├── crates/
│   ├── edn/                         shared (reconciled)
│   ├── core-traits/                 shared (reconciled)
│   ├── core/                        shared (reconciled)
│   ├── script/                      NEW: mentat-script (backend-independent mino glue)
│   ├── mino/                        the mino-rs interpreter (moved in, § 1.25) + tests/corpus/
│   ├── sqlite/
│   │   ├── mentat/                  the `mentat` crate (today's root src/ + tests/)
│   │   ├── db/ db-traits/ sql/ sql-traits/ transaction/ public-traits/
│   │   ├── query-algebrizer{,-traits}/ query-projector{,-traits}/
│   │   ├── query-pull{,-traits}/ query-sql/
│   │   ├── ffi/
│   │   └── cli/                     today's tools/cli
│   └── pg/
│       ├── pg_mentat/               the pgrx extension (+ pg_mentat.control, sql/)
│       └── mentatd/
├── docs/                            mdBook (from pg_mentat) + merged mentat docs
├── benchmarks/  docker/  scripts/   from pg_mentat
├── CHANGELOG.md                     pg_mentat's, continued; mentat history summarized
├── META.json  Trunk.toml            pgxn / trunk metadata (paths updated)
└── README.md                        one README, two build recipes
```

Why `default-members`: pgrx crates need `pg_config` at build time. With
`default-members` listing only the SQLite crates and the shared front-end, a plain
`cargo build` / `cargo test` works on any machine; the extension builds with
`cargo pgrx …` from `crates/pg/pg_mentat`, and `cargo build -p mentatd` works
without PostgreSQL headers (it only needs `tokio-postgres`).

Crate names don't change (`mentat`, `pg_mentat`, `mentatd`, `edn`, `mentat_core`,
`core_traits`, …), so downstream `use` paths and the extension's `.so`/control
names stay identical.

---

# Part 4 — Tasks

Order: crash fix → bring mino in and harden it → open `mentat_eval` →
reconcile shared crates in place (both repos, so each stays green) → import
history → restructure → shared script crate → mino refresh → feature ports →
release. Tasks 1a–3 happen in the *existing* repos, which proves the reconciled
front-end against both backends before anything moves.

### Task 1a: EDN nesting cap — the server crash (§ 1.24)

**Repos:** mentat (`edn/`), then the identical change in pg_mentat (`edn/`).
**Files:** `edn/src/lib.rs` (public parse entry points), new `edn/src/depth.rs`
(prescan), `edn/tests/nesting.rs`; pg_mentat `pg_mentat/src/nesting_tests.rs`.

**Interfaces produced:** `pub const MAX_NESTING: usize = 256;` (measured; see § 1.24)
`pub fn check_nesting(input: &str, max: usize) -> Result<(), ParseError>` —
linear, O(1) stack, skips string/char literals and `;` comments. Every public
`parse::` entry point (`value`, `parse_query`, `entities`, and any other `pub rule`
reachable from outside the crate) is wrapped so it runs `check_nesting` first.
The error names the depth and the limit.

- [ ] **Step 1: failing tests** (`edn/tests/nesting.rs`, own process so a
  pre-fix overflow only kills this file):
```rust
fn nested(n: usize) -> String { format!("{}{}", "[".repeat(n), "]".repeat(n)) }
#[test] fn deep_value_is_an_error_not_a_crash() {
    let e = edn::parse::value(&nested(100_000)).unwrap_err();
    assert!(e.to_string().contains("nesting"), "{e}");
}
#[test] fn deep_query_is_an_error() {
    let q = format!("[:find ?e :where [?e :a/b {}]]", nested(100_000));
    assert!(edn::parse::parse_query(&q).is_err());
}
#[test] fn deep_tx_is_an_error() { assert!(edn::parse::entities(&nested(100_000)).is_err()); }
#[test] fn limit_is_exact() {
    assert!(edn::parse::value(&nested(edn::MAX_NESTING)).is_ok());
    assert!(edn::parse::value(&nested(edn::MAX_NESTING + 1)).is_err());
}
#[test] fn brackets_in_strings_and_comments_dont_count() {
    let s = format!("[\"{}\" ; {}\n \\[ ]", "[".repeat(5000), "[".repeat(5000));
    assert!(edn::parse::value(&s).is_ok());
}
#[test] fn mixed_delimiters() {
    let s = format!("{}{}", "[({#{".repeat(300), "}})]".repeat(300)); // 1200 deep
    assert!(edn::parse::value(&s).is_err());
}
```
  Run each test binary on a 1 MB stack as well (`RUST_MIN_STACK=1048576`) — the
  cap must hold on small stacks, since PostgreSQL backends often run with
  `max_stack_depth` = 2 MB.
- [x] **Step 2:** implement `depth.rs` and wrap the entry points. (Done: 1,000
  did not fit a 1 MB debug stack — ~350 levels do — so the limit is 256.)
- [ ] **Step 3:** all existing `edn` and workspace tests pass. Commit in mentat:
  `fix(edn): cap nesting depth before parsing (stack overflow on deep input)`.
- [ ] **Step 4: pg_mentat.** Same `edn` change (copy the files). Add
  `#[pg_test]`s, run as a role created in the test, for `mentat_query`,
  `mentat_transact`, `mentat_pull`, `mentat_pull_many`, and the `edn` type's input
  function, each with 100,000-deep input: each returns an ERROR and the next
  statement on the connection succeeds. Also make `types/edn.rs`'s
  `MAX_EDN_NESTING` check run *before* parsing by calling `check_nesting` with 100.
  Repro script for the record (throwaway cluster, not the dev one): `initdb` into a
  `mktemp -d`, `pg_ctl -o "-p 54399 -k $D -c listen_addresses=''"`,
  `CREATE EXTENSION pg_mentat; CREATE ROLE nobody LOGIN`, then as `nobody`:
  `SELECT mentat_query($q$[:find ?e :where [?e :a/b <3000 × [ ]>]]$q$, '{}'::jsonb)`.
- [ ] **Step 5:** `cargo pgrx test pg13 … pg18` green. Commit, bump to 1.6.2,
  CHANGELOG entry (disclosure wording per your decision), tag `v1.6.2`, push to
  Codeberg. Push mentat's commit too.

### Task 1b: Move mino-rs into the repo and harden it (§ 1.1, § 1.24, § 1.25)

**Repo:** mentat (branch `pg`, before the restructure: the crate lands at
`mino/`, and Task 5 moves it to `crates/mino` with the others).

- [ ] **Step 1: import with history.**
```bash
git clone --no-local ~/src/mino-rs /tmp/mino-import && cd /tmp/mino-import
git filter-repo --to-subdirectory-filter mino --tag-rename 'v:mino-v'
cd ~/ws/mentat
git fetch --no-tags /tmp/mino-import main:refs/import/mino
git merge --allow-unrelated-histories refs/import/mino -m "merge: import mino-rs (27 commits) as mino/"
git update-ref -d refs/import/mino
```
  (`--tag-rename` keeps `v0.1.0` from colliding with mentat's own tags; bring it
  over explicitly as `mino-v0.1.0` if wanted.)
- [ ] **Step 2: vendor the corpus.** Copy the 12 gated files from mino
  `ead6e160` to `mino/tests/corpus/` (`git -C ~/src/mino show ead6e160:tests/<f>`),
  plus `LICENSE` as `mino/tests/corpus/LICENSE`. Change `tests/conformance.rs` to
  `concat!(env!("CARGO_MANIFEST_DIR"), "/tests/corpus/…")`. Delete
  `mino/.cargo/config.toml`. Add `mino` to the workspace members; mentat's
  `mino-rs` dependency becomes `{ path = "mino", optional = true }`. Confirm
  `grep -rn '/home/\|\.\./\.\./' --include=*.toml .` finds nothing outside
  intra-workspace paths.
- [ ] **Step 3:** `cargo test -p mino-rs` — all 112 unit + 12 conformance pass
  with no environment variables set.
- [ ] **Step 4:** the hardening below (was "Task 1"): sandbox, step/heap/depth
  limits, plus the § 1.24 paths — reader/printer/`=`/`hash`/`compare` depth caps,
  printer cycle detection, iterative GC tracing, iterative `Drop` for deep
  `Value`s. Each gets a test in `mino/tests/sandbox.rs` (separate process) that
  crashes today and returns `:eval/limit` (or a reader/printer error) after.
- [ ] **Step 5:** point `codeberg.org/gregburd/mino-rs`'s README at
  `mentat/mino` and push that one commit; you mark the repo read-only with the
  pg_mentat ones.

### Task 1c: Open `mentat_eval` on the hardened interpreter (pg_mentat 1.6.3)

pg_mentat's `script` feature switches from `mino-rs` git tag `v0.1.0` to
`git = "https://codeberg.org/gregburd/mentat"` at the commit/tag from Task 1b
(cargo finds the `mino-rs` package inside that workspace), then the pg_mentat
steps of the hardening task below (GUCs, check hook, tests), released as 1.6.3.

### Hardening details for Tasks 1b/1c (the sandbox and limits)

**Repos:** mentat (`mino/`), pg_mentat.
**Files:**
- Modify: `mino/src/embed.rs` (add `Interpreter::sandboxed()`, `set_limits`, `set_check_hook`)
- Modify: `mino-rs/src/eval/mod.rs` (split `Interp::new` into core + optional
  host installs; step counter checked in `eval`)
- Modify: `mino-rs/src/store.rs:390-405` (move fs prims to `install_host_fs`)
- Modify: `pg_mentat/pg_mentat/src/functions/script.rs:79` (use `sandboxed()`,
  limits from GUCs, interrupt + stack check)
- Modify: `pg_mentat/pg_mentat/src/lib.rs` (register the three GUCs in `_PG_init`)
- Modify: `mino-rs/src/prim/collections.rs:408` (`range`) and the other bulk
  producers listed in § 1.1 (charge the allocation budget per element)
- Modify: `mentat/src/script.rs` (use `sandboxed()`)
- Test: `mino-rs/tests/sandbox.rs`; `pg_mentat/src/script_security_tests.rs`

**Interfaces produced:**
```rust
pub struct Limits { pub steps: Option<u64>, pub heap_bytes: Option<u64>, pub depth: Option<u32> }
impl Interpreter {
    pub fn sandboxed() -> Interpreter;
    pub fn set_limits(&mut self, limits: Limits);
    /// Runs every 4096 steps and on every depth increase past depth/2.
    pub fn set_check_hook(&mut self, hook: Box<dyn FnMut() -> Result<(), Throw>>);
}
// On any limit: Throw of {:mino/kind :eval/limit, :mino/data {:limit :steps|:heap|:depth, :value N}}
// Internal: Interp::charge(&mut self, elements: u64, bytes: u64) -> Result<(), Throw>
```

- [ ] **Step 1: failing tests in mino-rs**
```rust
#[test]
fn sandboxed_has_no_filesystem() {
    let mut it = Interpreter::sandboxed();
    for f in ["slurp", "spit", "rm-rf", "mkdir-p", "file-exists?"] {
        let err = it.eval(&format!("({f} \"/tmp/x\")")).unwrap_err();
        assert!(err.contains("unbound symbol"), "{f} reachable: {err}");
    }
    assert_eq!(it.eval_to_string("(+ 1 2)").unwrap(), "3");
    assert!(it.eval("(mino.store/open)").is_ok()); // in-memory store still there
}
fn limited() -> Interpreter {
    let mut it = Interpreter::sandboxed();
    it.set_limits(Limits { steps: Some(100_000), heap_bytes: Some(8 << 20), depth: Some(500) });
    it
}
#[test]
fn step_limit_stops_infinite_loop() {
    let err = limited().eval("(loop [] (recur))").unwrap_err();
    assert!(err.contains(":eval/limit") && err.contains(":steps"), "{err}");
}
#[test]
fn heap_limit_stops_one_step_allocation() {
    for bomb in ["(range 100000000000)", "(count (vec (range 100000000)))",
                 "(apply str (repeat 100000000 \"x\"))"] {
        let err = limited().eval(bomb).unwrap_err();
        assert!(err.contains(":eval/limit"), "{bomb}: {err}");
    }
}
#[test]
fn depth_limit_turns_stack_overflow_into_an_error() {
    // Today this SIGABRTs the whole process. It must return an error instead.
    let err = limited()
        .eval("(defn f [n] (if (zero? n) 0 (inc (f (dec n))))) (f 1000000)")
        .unwrap_err();
    assert!(err.contains(":eval/limit") && err.contains(":depth"), "{err}");
}
#[test]
fn limits_leave_ordinary_scripts_alone() {
    let mut it = limited();
    assert_eq!(it.eval_to_string("(reduce + (range 1000))").unwrap(), "499500");
    assert_eq!(it.eval_to_string("(loop [i 0] (if (< i 10000) (recur (inc i)) i))").unwrap(), "10000");
}
```
  Put these in `mino-rs/tests/sandbox.rs`: each integration test file is its own
  process, so the depth test's pre-fix SIGABRT fails that file without taking the
  rest of the suite down with it.
- [ ] **Step 2:** `cargo test -p mino-rs sandboxed step_limit` → FAIL.
- [ ] **Step 3:** implement. `Interp::new_bare` + `install_language` (core, regex,
  bignum, atoms, store prims minus file I/O, core.clj, bundled libs) +
  `install_host_fs`. `new()` = both; `sandboxed()` = language only. Store
  `open` with a path must error `:store/backend` under `sandboxed()` (no
  `:file` backend installed). Step counter: a `u64` on `Interp`, incremented at
  the top of `eval`; the check hook runs every 4096 steps. Depth: increment on
  entry to `eval` and `func::apply`, decrement on every exit path (use a guard
  struct with `Drop` so `?` returns can't skip it). Heap: bulk prims call
  `it.charge(n, n * size_of::<Value>())` *before* allocating; loops that build
  output charge in chunks of 1024. Default `Limits` for `sandboxed()`: none —
  the host chooses; pg_mentat always sets all three.
- [ ] **Step 4:** pass; full `cargo test` green (112 unit + 12 corpus).
- [ ] **Step 5: pg_mentat.** Register the three GUCs from § 1.1 in `_PG_init`
  (`GucRegistry::define_int_guc`, `GucContext::Suset`). `build_interpreter()`:
```rust
let mut it = mino_rs::Interpreter::sandboxed();
it.set_limits(Limits {
    steps: Some(SCRIPT_MAX_STEPS.get() as u64),
    heap_bytes: Some(SCRIPT_MAX_HEAP_BYTES.get() as u64),
    depth: Some(SCRIPT_MAX_DEPTH.get() as u32),
});
it.set_check_hook(Box::new(|| {
    pgrx::check_for_interrupts!();
    if unsafe { pgrx::pg_sys::stack_is_too_deep() } {
        return Err(mino_rs::error::throw_str("mentat_eval: stack depth limit"));
    }
    Ok(())
}));
```
  No `REVOKE`: `mentat_eval` keeps PostgreSQL's default `EXECUTE` for `PUBLIC`
  (decision recorded in § 1.1). Not `SECURITY DEFINER`.
- [ ] **Step 6: pg tests** (`#[pg_test]`, `--features script`), run as an
  ordinary role created in the test (`CREATE ROLE eval_user LOGIN; SET ROLE eval_user;`):
  - `mentat_eval('(slurp "/etc/passwd")')` → error containing `unbound symbol`;
    same for `spit`, `rm-rf`, `mkdir-p`, `(mentat.store/open "/tmp/x")`.
  - `(loop [] (recur))` → `:eval/limit`; with `statement_timeout = '200ms'` and
    `pg_mentat.script_max_steps` raised by a superuser to 10¹², the same script
    ends with `canceling statement due to statement timeout` (proves the
    interrupt hook).
  - `(range 100000000000)` → `:eval/limit`.
  - `(defn f [n] (if (zero? n) 0 (inc (f (dec n))))) (f 1000000)` → `:eval/limit`,
    and the next statement on the same connection succeeds (proves the backend
    survived).
  - `SET pg_mentat.script_max_steps = 1000000000` as `eval_user` →
    `permission denied to set parameter`.
  - `eval_user` without `SELECT` on a store's tables gets the same permission
    error from `mentat_eval` as from `mentat_query` (SPI runs as the caller).
- [ ] **Step 7:** mentat `script.rs` → `sandboxed()`; its 11 tests stay green.
- [ ] **Step 8:** commit. mentat side lands with Task 1b; pg_mentat side is
  release 1.6.3 (Task 1c). CHANGELOG entry: names the exposure in 1.6.0–1.6.2
  builds that enabled `--features script`, states that shipped artifacts didn't,
  documents the three GUCs, and says `mentat_eval` is intentionally callable by
  every role. Push to Codeberg; pg_mentat's push mirror carries it to GitHub
  (Task 14 is where pg_mentat stops receiving pushes).

### Task 2: Reconcile the shared front-end (edn, core-traits, core)

**Where:** a branch in *each* repo, with byte-identical results for the three
crates. Do it in mentat first, then copy the three directories into pg_mentat
and fix pg_mentat's call sites. Both must pass their full suites.

**Files:** `edn/src/{lib,query,entities,types,*}.rs`, `core-traits/{lib,value_type_set,values}.rs`,
`core/src/*.rs`, their `Cargo.toml`s; mentat call sites in
`query-algebrizer/src/{clauses/pattern.rs,clauses/predicate.rs,validate.rs,types.rs,lib.rs}`,
`query-projector/src/translate.rs`, `query-sql/src/lib.rs`.

**Interfaces produced (the reconciled edn):**
- `Pattern { source, entity, attribute, value, tx, added }`;
  `Pattern::new(src, e, a, v, tx, added) -> Option<Pattern>`;
  `Pattern::simple(e, a, v)` unchanged.
- `NamedPullAttribute { attribute, alias, reverse }` (mentat's).
- `Limit::{Unlimited, Fixed, Variable}`; `Offset::{Unlimited, Fixed, Variable}`.
- `QueryPart::{InBindings, Rules, WithVars, …}`; `:in` → `InBindings`;
  `:rules` and `:with [[…]]` → `Rules`; `ParsedQuery.in_vars` derived from scalar
  bindings.
- `entities::BuiltinTxFn::{Cas, RetractEntity}`.
- `PatternValuePlace` from any keyword (namespaced or plain).
- `core_traits::ValueType: EnumSetType`; `ValueTypeSet::iter() -> enumset::EnumSetIter<ValueType>`.

- [ ] **Step 1: pin current behavior with tests, before changing anything.** In
  mentat's `edn/tests/` (create `parse_contract.rs`) add parse tests for every
  item above using today's spellings, so each change below shows up as a
  deliberate test edit:
```rust
use edn::parse;
#[test] fn in_bindings() {
    let q = parse::parse_query("[:find ?x :in $ [?a ...] :where [?x :foo/bar ?a]]").unwrap();
    assert_eq!(q.in_bindings.len(), 2);
}
#[test] fn five_place_pattern() {
    let q = parse::parse_query("[:find ?e :where [?e ?a ?v ?tx ?added]]").unwrap();
    assert!(matches!(&q.where_clauses[0], edn::query::WhereClause::Pattern(p)
        if matches!(p.added, edn::query::PatternNonValuePlace::Variable(_))));
}
#[test] fn rules_three_spellings() {
    for q in ["[:find ?p :rules [[(adult ?p) [?p :person/age ?a]]] :where (adult ?p)]",
              "[:find ?p :with [[(adult ?p) [?p :person/age ?a]]] :where (adult ?p)]"] {
        assert_eq!(parse::parse_query(q).unwrap().rules.len(), 1, "{q}");
    }
}
#[test] fn with_vars_still_with_vars() {
    let q = parse::parse_query("[:find (count ?x) :with ?y :where [?x :a/b ?y]]").unwrap();
    assert_eq!(q.with.len(), 1);
}
#[test] fn plain_keyword_value() {
    assert!(parse::parse_query("[:find ?e :where [?e :task/status :done]]").is_ok());
}
#[test] fn reverse_pull() {
    assert!(parse::parse_query("[:find (pull ?e [:person/_friends]) :where [?e :person/name]]").is_ok());
}
#[test] fn cas_is_a_known_tx_fn() {
    let _ = edn::entities::BuiltinTxFn::Cas;
}
```
- [ ] **Step 2:** run; the tests that encode pg_mentat-only features fail on
  mentat (`in_bindings`, `five_place_pattern`, the `:with [[…]]` rules case,
  `plain_keyword_value`, `cas_is_a_known_tx_fn`). That's the gap list.
- [ ] **Step 3: apply, in this order, re-running the suite after each:**
  1. core-traits/core → `enumset` 1.1 (pg_mentat's `lib.rs`, `value_type_set.rs`,
     `core/src/lib.rs` extern), keep mentat's `parse::value(expected_output)` line.
  2. Modern idioms from § 1.10 across the three crates.
  3. `Limit::None` → `Limit::Unlimited`; fix 10 sites.
  4. Plain keyword value place (pg `query.rs:378`, drop the `// … yet.` arm).
     Add an algebrizer test: `[:find ?e :where [?e :task/status :done]]` against a
     schema where `:task/status` is `:db.type/keyword`; must return the entity.
  5. `BuiltinTxFn` enum into `entities.rs`.
  6. `:in` → `InBindings` grammar (pg `lib.rs:587`) + the `ParsedQuery` derivation.
     In mentat's algebrizer, reject non-scalar `in_bindings` with
     `AlgebrizerError::UnsupportedInputBinding(String)` (new variant).
  7. Rules: keep `:rules`, add pg's `:with rule_definitions()` alternative
     **before** `:with variable()+`. Keep pg's placement of `where_clause` after
     `rule_invocation` or mentat's before — peg rules are order-independent by
     name; keep mentat's (documented) comment.
  8. 5-place `Pattern` (pg `query.rs`, grammar `lib.rs:419-447`). Fix mentat's 33
     `Pattern {` literals by adding `added: PatternNonValuePlace::Placeholder`.
     In `query-algebrizer/src/clauses/pattern.rs` reject a non-placeholder
     `added` with `AlgebrizerError::UnsupportedHistoryPattern`.
  9. Copyright header sweep to `2016-2018`.
  10. Deps to `[workspace.dependencies]` versions from § 1.13 (in mentat's root
      manifest; pg_mentat gets the same table in Step 5).
- [ ] **Step 4:** mentat green: `nix develop --command cargo test --workspace`
  and `--features mino`. Commit: `refactor(edn,core): reconcile shared front-end with pg_mentat`.
- [ ] **Step 5: pg_mentat.** Copy the three reconciled directories over pg_mentat's.
  Fix fallout: `NamedPullAttribute` gains `reverse` (pg never constructs it —
  expect zero sites), `Limit::Unlimited` already matches, `Pattern` already
  6-arg. Add pg tests for `:rules` spelling and reverse-pull parsing via `edn`.
  `cargo pgrx test pg16` green (plus pg17/pg18 in CI). Commit the same message.
- [ ] **Step 6:** `diff -r mentat/edn pg_mentat/edn` (and core, core-traits) is
  empty except `Cargo.toml` `repository`/`description` lines. Record the
  resulting tree hash of each crate in the commit messages — Task 4 checks it.

### Task 3: Adopt workspace lints, release profile, pinned toolchain (both repos)

**Files:** root `Cargo.toml` (both), `rust-toolchain.toml` (both; create in
pg_mentat), pg_mentat `flake.nix:22` (`"1.90.0"` → `"1.98.0"`).

- [ ] **Step 1:** pin `channel = "1.98"` in both `rust-toolchain.toml`; pg flake
  to 1.98; mentat flake from `stable.latest` to `stable."1.98.0"`.
- [ ] **Step 2:** copy pg_mentat's `[workspace.lints.clippy]` into mentat's root
  manifest. Add `[lints] workspace = true` to `edn`, `core`, `core-traits` only.
- [ ] **Step 3:** release profile from § 1.16 + `release-dev` in both.
- [ ] **Step 4:** `cargo clippy --workspace -- -D warnings` on the three shared
  crates, both repos. Fix what fires. Commit.

### Task 4: Import pg_mentat's history into the mentat repo

**Where:** mentat, branch `merge/pg-mentat` from `pg` (`a48aec6d` + Tasks 1–3).

- [ ] **Step 1:** in a scratch clone of pg_mentat:
```bash
git clone --no-local ~/ws/pg_mentat /tmp/pgm-import && cd /tmp/pgm-import
git filter-repo --path-rename edn/:crates/pg/_import/edn/ \
                --path-rename core/:crates/pg/_import/core/ \
                --path-rename core-traits/:crates/pg/_import/core-traits/ \
                --path-rename pg_mentat/:crates/pg/pg_mentat/ \
                --path-rename mentatd/:crates/pg/mentatd/ \
                --path-rename Cargo.toml:crates/pg/_import/Cargo.toml \
                --path-rename Cargo.lock:crates/pg/_import/Cargo.lock \
                --path-rename flake.nix:crates/pg/_import/flake.nix \
                --path-rename flake.lock:crates/pg/_import/flake.lock \
                --path-rename README.md:crates/pg/_import/README.md \
                --path-rename CHANGELOG.md:crates/pg/_import/CHANGELOG.md \
                --path-rename Makefile:crates/pg/_import/Makefile \
                --path-rename LICENSE:crates/pg/_import/LICENSE \
                --path-rename .gitignore:crates/pg/_import/.gitignore \
                --path-rename .github/:crates/pg/_import/.github/ \
                --path-rename docs/:docs/pg/ \
                --path-rename scripts/:scripts/pg/
```
  Everything else in pg_mentat's root (`benchmarks/`, `docker/`, `Dockerfile`,
  `deny.toml`, `demo.sql`, `META.json`, `Trunk.toml`, `.forgejo/`, `.gitlab-ci.yml`,
  `.envrc`, `ChangeLog`) has no mentat counterpart and keeps its path.
- [ ] **Step 2:** in mentat:
```bash
git fetch --no-tags /tmp/pgm-import main:refs/import/pg_mentat
git fetch /tmp/pgm-import 'refs/tags/*:refs/tags/*'     # v1.2.1..v1.6.1
git merge --allow-unrelated-histories refs/import/pg_mentat \
  -m "merge: import pg_mentat history (1,258 commits) under crates/pg/"
git update-ref -d refs/import/pg_mentat
```
  (Tag names don't collide: mentat has only `v0.*`, checked 2026-09-24.)
- [ ] **Step 3:** verify: `git log --oneline -- crates/pg/pg_mentat | wc -l` is
  in the hundreds; `git log --follow crates/pg/pg_mentat/src/functions/query.rs`
  reaches 2025. `diff -r edn crates/pg/_import/edn` is empty (Task 2 made them
  identical) — if not, stop and reconcile before deleting.
- [ ] **Step 4:** keep what later tasks need, then drop the rest:
```bash
git mv crates/pg/_import/.github/workflows crates/pg/github-workflows   # Task 6 Step 3b
git mv crates/pg/_import/CHANGELOG.md CHANGELOG.pg_mentat.md           # Task 6 Step 5
git rm -r crates/pg/_import
```
  Commit
  `chore: drop pg_mentat's copies of the shared crates (identical to edn/, core/, core-traits/)`.
  Nothing builds yet at this commit's `crates/pg/*` — Task 5 wires it. Note that
  in the commit message so bisect users know to skip it.

### Task 5: Restructure into `crates/` and one workspace

**Files:** every crate directory moves; root `Cargo.toml` rewritten; each crate's
`path = "../x"` dependencies rewritten.

- [ ] **Step 1:** `git mv` per § Part 3: `edn core core-traits` → `crates/`;
  SQLite crates → `crates/sqlite/`; root `src/`+`tests/`+`build/version.rs` →
  `crates/sqlite/mentat/`; `tools/cli` → `crates/sqlite/cli`; `ffi` →
  `crates/sqlite/ffi`. One commit, moves only (keeps `--follow` working).
- [ ] **Step 2:** root manifest:
```toml
[workspace]
resolver = "2"
members = ["crates/edn", "crates/core-traits", "crates/core", "crates/mino", "crates/script",
           "crates/sqlite/*", "crates/pg/pg_mentat", "crates/pg/mentatd"]
default-members = ["crates/edn", "crates/core-traits", "crates/core", "crates/mino", "crates/script",
                   "crates/sqlite/*", "crates/pg/mentatd"]

[workspace.package]
version = "1.6.1"
edition = "2021"
rust-version = "1.88"
license = "Apache-2.0"
repository = "https://codeberg.org/gregburd/mentat"
authors = [ …union of both author lists… ]

[workspace.dependencies]   # § 1.13; plus rusqlite 0.40, pgrx 0.17; mino-rs = { path = "crates/mino" }
[workspace.lints.clippy]   # § 1.15
[profile.release]          # § 1.16
```
  Every crate: `version.workspace = true`, `edition.workspace = true`, etc.
- [ ] **Step 3:** rewrite intra-workspace `path =` deps; pg_mentat's `edn`
  dependency keeps `features = ["serde_support"]`.
- [ ] **Step 4:** lockfile: delete both old locks, `cargo generate-lockfile`,
  `git add Cargo.lock`, remove `Cargo.lock` from `.gitignore`.
- [ ] **Step 5:** `cargo test` (default members) green;
  `cd crates/pg/pg_mentat && cargo pgrx test pg16` green;
  `cargo build -p mentatd` green without `pg_config` on PATH. Commit.

### Task 6: One flake, one CI, delete dead weight

**Files:** `flake.nix` (from pg_mentat's, paths updated), `.forgejo/workflows/ci.yml`,
deletions per § 1.17–1.18, `deny.toml`.

- [ ] **Step 1:** flake: base on pg_mentat's; `cargo pgrx package` runs in
  `crates/pg/pg_mentat`; add `devShells.sqlite`, `packages.mentat-cli`,
  `packages.mentatd`. `nix build .#pg_mentat-pg16 .#mentat-cli .#mentatd` all build.
- [ ] **Step 2:** delete `sdks/ automation/ _/ .vscode/ NOTES .ignore .travis.yml
  .taskcluster.yml .gitlab-ci.yml .github/dependabot.yml .github/FUNDING.yml
  .github/workflows/{audit,clippy_check,clippy-ng,cross_compile,grcov,msrv,nightly_lints,quickstart}.yml`
  (mentat's) and pg_mentat's `.github/workflows/{ci,audit}.yml`.
  Check `fixtures/` and `build/` for readers (`grep -rn 'fixtures/' crates/`);
  move used ones under the crate that reads them.
- [ ] **Step 3:** `.forgejo/workflows/ci.yml` (the gate) jobs: `fmt`,
  `clippy -D warnings`, `cargo deny check`, `test-sqlite` (`cargo test` +
  `--features mino`), `test-pg` (matrix pg13–18, `cargo pgrx test`), `test-ffi`,
  `nix-build`.
- [ ] **Step 3b: GitHub publishing workflows** (run on the mirror). Move
  `release.yml`, `docs.yml`, `installcheck.yml`, `docker-test.yml`, `nix-test.yml`
  from `crates/pg/github-workflows/` (parked there by Task 4) to the root
  `.github/workflows/`; delete the rest of that directory. In each, replace
  `pg_mentat/` paths with `crates/pg/pg_mentat/`, and make `release.yml` also
  attach a `mentat-cli` Linux binary. `release.yml` triggers on `tags: ["v*"]`
  today; the mirror will push `v0.14.0` and pg_mentat's 13 old `v1.2.1..v1.6.1`
  tags to GitHub, and each would start a release build. Narrow it: keep
  `tags: ["v*"]` and add a first job step that exits unless the tag is ≥ `v1.7.0`
  (`sort -V` comparison), or list the pattern `v1.[7-9].*` plus `v[2-9].*`.
  Alternatively enable the mirror only after the tag import, so the old tags
  arrive in one push before the workflow file exists on GitHub — but the gate is
  simpler and survives re-syncs.
- [ ] **Step 4:** Makefile: targets `test`, `test-pg`, `package-pg PG=16`,
  `install-pg`, plus pg_mentat's `upgrades`/`install-upgrade-scripts`/`smoke`
  (paths updated) and mentat's `outdated`/`fix`.
- [ ] **Step 5:** docs: mdBook at `docs/` (pg_mentat's `book.toml`), mentat's
  Jekyll pages converted into `docs/src/embedded/`. One README with two quickstarts.
  CHANGELOG: `CHANGELOG.pg_mentat.md` (parked by Task 4) becomes `CHANGELOG.md`,
  replacing mentat's; prepend a "1.7.0 — merged repository"
  section and a condensed history of mentat 0.x. Commit.

### Task 7: `mentat-script` — one scripting layer, two backends

**Files:**
- Create: `crates/script/{Cargo.toml,src/lib.rs,src/values.rs,src/prims.rs,tests/model.rs}`
- Modify: `crates/sqlite/mentat/src/script.rs` → implements `ScriptBackend`
- Modify: `crates/pg/pg_mentat/src/functions/script.rs` → implements `ScriptBackend`

**Interfaces produced:** `pub trait ScriptBackend` (§ 1.19);
`pub fn install(it: &mut mino_rs::Interpreter, backend: Rc<RefCell<dyn ScriptBackend>>)`;
`pub struct DbRef { conn: i64, basis_tx: i64, as_of: Option<i64>, since: Option<i64> }`;
value builders `db_value`, `tx_report_value`, `inst_value`, `uuid_value`, `kw_ns`, `str_val`.

- [ ] **Step 1:** move the 11 Datomic-model tests from mentat's
  `tests/mino_script.rs` into `crates/script/tests/model.rs`, parameterized by a
  backend factory; run them against an in-memory fake backend first.
- [ ] **Step 2:** implement the crate; fake backend green.
- [ ] **Step 3:** mentat implements `ScriptBackend` over `Store`; the model
  tests run against it (`crates/sqlite/mentat/tests/script_model.rs`).
- [ ] **Step 4:** pg_mentat implements it over its engine; the model tests run as
  `#[pg_test]`s. Historical-`q` test gated to pg until Task 12.
- [ ] **Step 5:** delete the duplicated helpers from both `script.rs` files; each
  keeps only its result conversion. Commit.

### Task 8: Refresh `crates/mino` from upstream mino 9c65bb50

**Where:** `crates/mino` in this repo. Reference (not an input): `~/src/mino/mino`
built at `9c65bb50`.

- [ ] **Step 1:** copy upstream `src/core.clj`, `lib/mino/store.clj`,
  `lib/clojure/{string,set,instant}.clj` over `resources/`, and upstream's
  versions of the 12 corpus files over `tests/corpus/` (one commit, so the diff
  shows what changed upstream); add `store_backend_test.clj` and
  `reader_features_test.clj` to the corpus and gate them.
- [ ] **Step 2:** run the corpus; collect failures by category. Implement, each
  with its own commit and an oracle-checked unit test:
  1. `#uuid` → `Value::Uuid(Gc<[u8;16]>)`, `type` → `:uuid`, printer `#uuid "…"`.
  2. `#inst` → an instant value that prints `#inst "…"` (keep the map payload
     that `clojure.instant` expects; add the print method upstream uses).
  3. `read-string` prim over the existing reader.
  4. Classed and keyword `catch` per ADR 32/37 (the table in upstream
     `docs/adr/32-classed-catch-kind-dispatch.md`), first-match-wins, unknown
     class symbol = compile error.
  5. Regex: stop rejecting `(?=` / `(?!` (fancy-regex implements them); keep
     rejecting lookbehind only if upstream still does (check `re_compile.c`).
  6. `re-seq` no-match → `()`; `(keyword <non-string>)` → `nil`;
     `join` stringifies separators; negative-decimal-scale and `::` reader rules.
  7. BigDec: `Value::BigDec` over a pure-Rust decimal (e.g. `bigdecimal`), `M`
     literal, printer, tower contagion as upstream's `bigdec.c`.
  8. `delay` as its own value type.
  9. New core.clj prims: `delay*`, `lazy-keep`, `lazy-map-indexed`, `lazy-remove`
     (eager), `__transduce-fuse` (identity), `find-keyword`, `rerun-seq`,
     `mino-version` (returns `"0.2.0"`).
  10. Store backend seam: the new store.clj routes durability through
      `{:kind :initial :wal-entries :commit :checkpoint :close}` maps. Re-home
      `src/store.rs`'s WAL/snapshot code as the `:file` backend's five ops;
      `:memory` is the in-memory one; keep the byte-for-byte file format.
- [ ] **Step 3:** remove the output-only inst/uuid workaround from
  `crates/script` (values now round-trip for real) and add a test that *writes*
  an instant and a uuid through `transact` and reads them back.
- [ ] **Step 4:** full corpus green (record pass counts per file in the commit);
  bump nothing — `crates/mino` is versioned with the workspace.

### Task 9: `mino.store` backends over the real engines (optional, after 8)

With the seam in place, a mino script can use `mino.store` *on top of* Mentat
storage instead of a file. Register a `:mentat` backend whose `:commit` writes the
tx-info to the host store's log. Only do this if a user asks for `mino.store`
semantics over Mentat storage; `mentat.store/*` already covers Datomic-style use.
Mark as YAGNI in the plan tracker until then.

### Task 10: Replace `unimplemented!()` in the SQLite crates; lints on

**Files:** the 15 sites (e.g. `query-algebrizer/src/clauses/resolve.rs:194,210-212`,
`db/src/cache.rs:1926`).

- [ ] **Step 1:** for each site write a test that reaches it through the public API
  (a query or transact that parses successfully) and asserts a typed error.
- [ ] **Step 2:** replace each with a typed error variant. `cache.rs:1926`: read it
  first — if it's a genuinely unreachable arm, `unreachable!("reason")` is fine
  (clippy's `unimplemented` lint doesn't cover it).
- [ ] **Step 3:** `[lints] workspace = true` on every SQLite crate; `cli` allows
  `print_stdout`. `cargo clippy --workspace -- -D warnings` green. Commit.

### Task 11: `:db.fn/cas` and `:db/retractEntity` in the SQLite transactor

**Files:** `crates/sqlite/db/src/tx.rs` (entity expansion), `crates/sqlite/db/src/internal_types.rs`,
`crates/sqlite/db-traits/errors.rs` (new `CasMismatch { e, a, expected, actual }`),
tests `crates/sqlite/mentat/tests/tx_fns.rs`.

- [ ] **Step 1: tests**, modeled on pg_mentat's `cas_tests.rs` and
  `comprehensive_retract_tests.rs` (port the assertions, not the SPI plumbing):
  cas succeeds on match; cas fails with `CasMismatch` and commits nothing;
  cas with `nil` old value asserts only if absent; retractEntity removes every
  datom with that `e`, removes datoms where it's the `v` of a ref, and recurses
  into `:db/isComponent` refs; both spellings (`:db.fn/*` and `:db/*`) work.
- [ ] **Step 2:** implement in the transactor's entity-expansion pass using
  `BuiltinTxFn` from `edn`; `cas` reads inside the IMMEDIATE transaction.
- [ ] **Step 3:** green; commit.

### Task 12: History patterns, historical `q`, and `:in` collection bindings on SQLite

Closes § 1.3, § 1.4, § 1.20 together, because all three are "teach the algebrizer
a new table source or input shape."

- [ ] **Step 1:** tests (port from pg_mentat's `history_tests.rs`,
  `temporal_tests.rs`, `input_parameter_tests.rs`): `[?e ?a ?v ?tx ?added]`
  over the history source; `q` against an `as-of T` db; `:in $ [?x ...]`,
  `:in $ [?a ?b]`, `:in $ [[?a ?b]]`.
- [ ] **Step 2:** algebrizer: a `DatomsTable::Transactions` source that exposes
  `added`; a pattern with a non-placeholder `added` or a query against an
  as-of/since db uses it with a `tx <= T` / `tx > T` constraint. For as-of on the
  current-state columns, filter to the latest assertion per `(e,a,v)` at or
  before T without a later retraction (a correlated `NOT EXISTS` on `transactions`).
- [ ] **Step 3:** input bindings: `BindColl` → `IN (…)` or a VALUES join,
  `BindTuple` → scalar bindings, `BindRel` → a VALUES table. Remove
  `UnsupportedInputBinding` / `UnsupportedHistoryPattern`.
- [ ] **Step 4:** remove the pg-only gate from the historical-`q` model test in
  `crates/script`. Both backends now pass all model tests. Commit.

### Task 13: Documentation of the combined project

- [ ] One README: what it is, the two targets, `cargo build` vs `cargo pgrx install`,
  the scripting layer, and a security section on `mentat_eval`: callable by every
  role by design; runs sandboxed (no host access) under three superuser-set limits
  (steps, heap, depth) plus `statement_timeout`; reaches only the stores the
  caller can already query; must not be made `SECURITY DEFINER`.
- [ ] `docs/src/architecture.md`: the front-end / backend split, the crate map,
  which features exist on which backend (table from § 1.21, updated).
- [ ] Update `/tmp/pg_mentat-mino-integration-guide.md`'s successor in
  `docs/src/scripting.md` — it describes a design that now lives in `crates/script`.

### Task 14: Release 1.7.0 and retire the pg_mentat repo

- [ ] **Mirror check first** (§ 1.17): confirm the Codeberg → GitHub push mirror
  exists for `mentat` and syncs on push. Without it, the tag in the next steps
  won't trigger GitHub's `release.yml` and no extension binaries get built.
- [ ] Workspace version 1.6.1 → **1.7.0**; CHANGELOG entry lists: merged repository,
  reconciled front-end (with the user-visible grammar changes: `:in` bindings,
  5-place patterns, plain keyword values, `:rules`/`:with [[…]]`/`:in %`),
  SQLite `cas`/`retractEntity`/history/as-of `q`, mino moved into the repo and
  refreshed to upstream's final release, the nesting-depth crash fix, and the
  sandboxed `mentat_eval` with its three limit GUCs.
- [ ] Merge `merge/pg-mentat` → `master` (stays the default branch). Its tip
  `201ec39d` is from 2023, so fast-forward is impossible: `git merge --no-ff`,
  message pointing at this plan.
- [ ] Annotated tag `v1.7.0`. Push `master` and the tag to **Codeberg only**:
  `git push origin master v1.7.0`. Never push to GitHub directly; the mirror
  carries it.
- [ ] After the mirror syncs: GitHub `master` equals Codeberg `master`; the
  GitHub Release for `v1.7.0` has the per-PG-version extension tarballs; the
  Pages site is up at the new URL.
- [ ] pg_mentat's PGXN/Trunk metadata (`META.json`, `Trunk.toml`) point at
  `codeberg.org/gregburd/mentat`; publish the extension from there.
- [ ] Delete remote branches `pg` (`d7e2b554`, superseded) and `improv-base`
  (duplicate). Delete `mino-scripting` after 1.7.0 ships.
- [ ] pg_mentat repo, last push: replace README with a pointer and push to its
  Codeberg `main` (the mirror copies it to GitHub). Suggested text:
  > **Moved.** pg_mentat now lives in the `mentat` repository, which builds both
  > the embedded SQLite store and this PostgreSQL extension:
  > https://codeberg.org/gregburd/mentat (mirror: https://github.com/gburd/mentat).
  > This repository is read-only. Releases after 1.6.2 are published there.
  Then you mark both pg_mentat repos read-only. Leave the pg_mentat mirror
  enabled until the pointer commit shows on GitHub, then turn the mirror off —
  pushing into an archived GitHub repo fails.

---

# Part 5 — Self-review

**Coverage.** Every item from the diff inventory has a decision and a task:
1.1→T1b/T1c, 1.24→T1a/T1b, 1.25→T1b/T5,
1.2/1.3/1.4/1.5/1.6/1.7/1.8/1.9/1.10/1.12/1.13→T2, 1.11→T4 (no-op),
1.14/1.15/1.16→T3/T5/T6, 1.17/1.18→T6, 1.19→T7, 1.20→T12, 1.21→T11/T12,
1.22→T5 (moves, no change), 1.23→T4/T14, Part 2→T8.

**Risk order.** The only step that can lose work is deleting pg_mentat's copies
of the shared crates (T4 Step 4); it's gated on an empty `diff -r` against the
canonical copies, which T2 guarantees, and it parks the two things later tasks
need (`.github/workflows`, `CHANGELOG.md`) before deleting.

The one step that is hard to undo is the first push after the mirror is on: it
sends 13 pg_mentat tags and the new `master` to a public GitHub repo. Do it once,
at Task 14, with `release.yml` already gated (T6 Step 3b).

**What I'd cut if time is short.** T9 (already marked YAGNI), T12 (the gap is
documented and test-gated), T13's architecture page. Never cut T1.

**Decisions recorded 2026-09-24.**
1. Push to Codeberg only; Codeberg mirrors to GitHub. The merged repo is
   `mentat`. You mark the pg_mentat repos read-only once 1.7.0 ships.
   → § 1.17, § 1.23, Task 6 Steps 2–3b, Task 14.
2. `mentat_eval` stays callable by `PUBLIC`, sandboxed and limited.
   → § 1.1, Task 1. Found while applying this: step limits alone don't cover
   single-call allocation or deep recursion (the latter crashes the process
   today), so Task 1 now adds heap and depth limits too.
3. Default branch stays `master`. → § 1.23, Task 14.

**One thing to check before Task 14:** the mentat Codeberg → GitHub push mirror.
GitHub's mentat `master` is behind Codeberg's today, and `v0.14.0` and
`mino-scripting` never arrived, so it's either off or failing.
