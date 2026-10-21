# Plan: the 49 open Mozilla-era issues (#236–#295)

Written 2026-10-21. Each issue was re-read in full (body + comments) and checked
against the current code (master `ef1e7ddf`), with a probe program for the
behavioural ones (`/tmp/moz/probe`, embedded engine 1.17.1). The issues date
from the 2020 import of mozilla/mentat. The code they refer to has largely been
replaced: there is no Android/iOS SDK, no Travis, no `failure` crate, and no
`combine` in the query parser.

Each issue gets one of four outcomes:

- **Done.** Already fixed. Close it, with the commit or file that fixed it.
- **Obsolete.** The code or platform no longer exists. Close it, with the reason.
- **Fix.** A real defect or a small, worthwhile change. Fix it in the next
  release (1.18.0), with a test, then close it.
- **Keep.** A real design question or a larger feature. Rewrite it as a current
  issue that names today's code, or fold it into an existing one, and close the
  old one.

## 1. Close as done (7)

| # | Title | Evidence |
|---|---|---|
| 260 | Store management (Mozilla PR) | Superseded: `Store` / `Conn`, and the extensions' per-thread store caches (`crates/sqlite/ext/src/store_cache.rs`) |
| 275 | Implement excision (Mozilla PR) | pg_mentat has excision (`src/functions/excision.rs`). The embedded engine has only the `allow_excision` partition flag; filed as a new issue (§4) |
| 282 | Implement :db/isComponent | `retractEntity` cascades components on all backends (#326, 1.14.0; `crates/sqlite/db/src/tx.rs` component_targets), and pull follows component refs |
| 287 | Preserve failure backtraces (Mozilla PR) | The `failure` crate is gone (not in Cargo.lock); errors use `thiserror`/`anyhow` |
| 289 | Bump failure to 0.1.2 (Mozilla PR) | `failure` is no longer a dependency |
| 292 | No easy way from TypedValue to EDN; no transact from EDN | `TypedValue::to_edn_value` / `into_edn_value` (crates/core-traits/lib.rs:204, 359); `Store::transact(&str)` and `q_once(&str)` take EDN text |
| 276 | [docs] API links are broken | mozilla.github.io/mentat is gone; the docs are the mdBook in `docs/`. The broken `docs.yml` trigger is fixed by worker `hygiene` (1.18.0) |

## 2. Close as obsolete (21)

The Android SDK, iOS SDK, Swift/Java FFI wrappers, Travis CI, Jekyll site and
gradle publishing are not in this repo. The C FFI (`crates/sqlite/ffi`) remains.

| # | Title | Reason |
|---|---|---|
| 240 | Run FFI tests in integration (iOS/Android on Travis) | No SDKs, no Travis. CI's `test-ffi (C ABI)` job runs the C FFI tests |
| 241 | Create Rust tests for FFI | `test-ffi (C ABI)` exists |
| 242 | Generate Swiftdoc/JavaDoc/FFI rustdoc in integration | No Swift/Java SDKs |
| 243 | Firefox-like awesome bar library | A product experiment for Firefox, not a Mentat feature |
| 245 | Exception throwing in Android SDK | No Android SDK |
| 246 | Use `macro_rules` lifetime specifier | About the old FFI's `define_destructor`; obsolete |
| 247 | Macros for typed FFI functions | The remaining C FFI is small; no SDK consumers |
| 252 | ExternResult for non-pointer data | Old SDK design discussion |
| 258 | Android SDK crash on API21–23 | No Android SDK |
| 259 | Switch gradle publication to jcenter | No gradle; jcenter itself shut down in 2021 |
| 261 | Document iOS SDK development flow | No iOS SDK |
| 262 | Android SDK debug binary symlinking | No Android SDK |
| 265 | SQLCipher in the Swift/Java FFIs | No Swift/Java FFIs (the C FFI has `store_open_encrypted`) |
| 268 | GitHub pages / mentat.github.io | The repo is Codeberg-first; docs build is §3 |
| 269 | Android build script needs new bash | No Android build script |
| 280 | arm64 Android build failing (mio 0.6) | No Android target; mio 0.6 isn't used |
| 283 | Loosely typed Android SDK | No Android SDK |
| 293 | Build and deploy swiftdoc using jazzy | No Swift SDK; the `gh-pages-api-docs` branch is archived (`archive/gh-pages-api-docs`) |
| 295 | ValueType as keyword in the Android API | No Android API |
| 248 | Document FFI concurrency responsibilities | The SDK docs it refers to don't exist. The C FFI header should say `Store` is not thread-safe: fold into #266's fix (§3) |
| 256 | FFI store_open should return an error, not crash | `store_open` already returns via `translate_result(Store::open(uri), error)` (crates/sqlite/ffi/src/lib.rs:176). Remaining crash paths are panics: covered by #266 |

## 3. Fix in 1.18.0 (10)

| # | Title | What is still true today | Fix |
|---|---|---|---|
| 266 | Use catch_unwind in the FFI | 102 `extern "C"` fns in `crates/sqlite/ffi/src/lib.rs`, 0 `catch_unwind`. A panic unwinding into C is undefined behaviour (since Rust 1.81 it aborts). | Wrap each entry point in `std::panic::catch_unwind`, report the panic through `ExternError` (or return null/0), and add a test that triggers a panic. Also: document in the header that a `Store` handle must not be used from two threads at once (#248) |
| 253 | Show attribute names in AttributeValidation errors | Confirmed by the probe: `:db/fulltext true without :db/index true for entid: 65539`. The `ident()` closure falls back to the entid because the new attribute's ident isn't in `entid_map` yet during validation | Pass the ident from the transaction being validated. Message: `... for :t/ft` |
| 286 | Asserting a schema attribute needs the minimal attribute set | Confirmed: the embedded engine accepts `:db/ident`+`:db/valueType` without `:db/cardinality`, and `:db/valueType`+`:db/cardinality` without `:db/ident` | Reject an entity that has some, but not all, of `:db/valueType` and `:db/cardinality`, or has them without `:db/ident`. This is what Datomic does. Check pg_mentat's behaviour and make the two backends agree. A store with existing half-defined attributes must still open (validate new assertions only) |
| 249 | `min`/`max` over transaction refs | Confirmed: `(max ?tx)` → `cannot apply projection operation Max to types Ref` on the embedded engine | Allow `min`/`max` on `Ref` (entids are ordered integers; tx ids are monotonic). Datomic allows `max` on any comparable value. Check pg's behaviour (it orders refs numerically: the min/max fix kept long/ref numeric) |
| 267 | Support SQLite's LIKE for non-fulltext text search | Confirmed: embedded has no `like`; pg_mentat has `like`/`ilike` (query.rs:7444) | Add `like`/`ilike` (and, if cheap, `starts-with?`, `ends-with?`, `includes?`) to the embedded algebrizer, with the same argument rules and escaping as pg. Literal patterns first, as the issue suggests |
| 264 | Migrate CLI parsing to rust-peg | `crates/sqlite/cli` still depends on `combine ~4.6` for its command parser | Port `command_parser.rs` to `peg` (already a dependency of `edn`), so `combine` leaves the dependency tree |
| 272 | Sample databases in fixtures/ are out of date | `crates/sqlite/fixtures/v1*.db` are version-1 stores. Tests use them on purpose (upgrade and encryption tests) | Keep the v1 fixtures, but add a README in `fixtures/` that says why they're v1. Check `all_seattle.edn`/`cities.schema` still load. Then close |
| 236 | WIP: nested pull | Nested map specs (`{:a [:b]}`) are rejected by the pull grammar today | Worker `pull` is implementing them on all three backends; close when it merges |
| 244 | Worked example with a manually ordered list | Transaction functions now exist on every backend (#325/#333), which is how Datomic users keep an order attribute consistent | Add the worked example (a `:db/fn` that renumbers positions) to `docs/src/`, with a test that runs it |
| 281 | Counts of assertions/retractions in TxReport | Confirmed: `TxReport { tx_id, tx_instant, tempids }`. pg's tx report and the scripting tx-report have `:mentat.store/db-after` but no counts | Add `datoms_asserted` / `datoms_retracted` counts (the transactor knows them) to the embedded `TxReport` and the pg/DuckDB tx reports, so callers can tell a no-op transaction |

## 4. Keep, rewritten as current issues (11 old issues, 8 new ones)

| # | Title | Why keep | New issue |
|---|---|---|---|
| 238 | Better internal representation for NamespaceableName | Still true (`boundary` index, printing allocates). A perf/ergonomics change in `crates/edn` | "edn: store keywords in printed form so Display/AsRef<str> don't allocate". Measure first (query/transaction hot paths) |
| 239 | Ergonomic StructuredMap keyword keys | Still true in `crates/sqlite/query-pull`; relevant to the nested-pull work | Fold into the nested pull follow-up: decide after worker `pull` lands |
| 250 | Cache the (:db/tx, :db/txInstant) relation; `tx-instant` function | No `tx-instant` find function exists. Datomic users want it | "query: `(tx-instant ?tx)` in :find, on all backends". The cache part is moot: SQLite/pg/DuckDB index txInstant |
| 263 | Split edn crate into types and parsers | Still true: `crates/edn` holds the EDN types and the peg parsers for values, transactions and queries | "edn: split types from parsers". Low priority; only worth it for crates.io consumers who want types without peg |
| 273 | Core schema seems insufficient (enum value restrictions) | A real Datomic-parity question: there is no attribute predicate (`:db.attr/preds`) or enum restriction | "schema: attribute predicates (`:db.attr/preds`) and entity specs (`:db/ensure`)", as Datomic has. A design item |
| 274 | Exploit AVET/VAET indexes in the query engine | Partly done: 1.10.0's schema-driven `idx_auto_avet_*` indexes are what the planner uses (the partial `idx_datoms_avet` never matches mentat's SQL, see db.rs:569). VAET (reverse refs) is still a partial index the queries can't use | "query (embedded): reverse-ref lookups use VAET". Measure `[?x :a ?e]` with ?e bound at 1M datoms first |
| 278 | Reduce dependencies and on-disk footprint | The embedded crates no longer pull `regex` (only mentat_cli via env_logger, and DuckDB via arrow). Binary size is less important now: no mobile targets | Fold into #290 |
| 290 | Reduce on-disk size of databases | Still true: the Improv benchmark measured +29% on disk 0.14 → 1.11, from `idx_transactions_aevt` and the auto value indexes | "embedded: store size", with the Improv numbers. Options: make the history index opt-in for stores that never query as-of/since, pack flag columns (Mozilla #29), interned keywords (#32). Measure before changing |
| 279 | Avoid monomorphizing large functions | A binary-size item for the old mobile targets | Fold into #290 as a low-priority sub-item |
| 285 | Attribute representation should record absence | Prerequisite for schema retraction. Still true | Fold into a "schema retraction" issue together with #286's follow-ups |
| 288 | WIP: tutorial (Mozilla PR) | The old tutorial bit-rotted. A tutorial is still worth having | "docs: tutorial", built from `docs/src/` (mdBook); reuse the seattle fixtures |

278 and 279 fold into the #290 replacement and 285 into the schema-retraction one, so 11 old issues become 8 new ones. Excision on the embedded engine (from #275) is a 9th new issue.

## Order of work

1. **Now:** close the 28 Done/Obsolete issues (§1, §2), each with a comment that
   names the evidence.
2. **1.18.0:** the 10 §3 fixes, closing each with its commit. #266 (FFI panics) is a soundness fix; do it first.
3. **Now as well:** file the 9 new issues (§4), and close the 11 originals
   pointing to them. Of these, 290 (store size) and 250 (`tx-instant`) are the
   most useful to users. The others are design or hygiene work.
