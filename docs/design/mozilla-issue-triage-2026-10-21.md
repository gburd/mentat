# Plan: the 229 open Mozilla-era issues (#4–#295)

Written 2026-10-21. 229 issues were open, all imported from mozilla/mentat on
2020-08-06; the import created up to six copies of some of them. Each issue was
re-read in full (body + comments) and checked against the current code (master
`ef1e7ddf`). The behavioural ones were checked with probe programs against both
engines: the embedded engine (mentatdb 1.17.1) and pg_mentat 1.17.1 on
PostgreSQL 17.

Part A covers the 49 issues numbered #236–#295. Part B covers the 180 issues
numbered #4–#234. Part B adds one outcome, **Duplicate**: closed as a copy of
the issue named in the reason. The issues date
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

# Part A: #236–#295 (49 issues)

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

## 3. Fix in 1.18.0 (11)

| # | Title | What is still true today | Fix |
|---|---|---|---|
| 266 | Use catch_unwind in the FFI | 102 `extern "C"` fns in `crates/sqlite/ffi/src/lib.rs`, 0 `catch_unwind`. A panic unwinding into C is undefined behaviour (since Rust 1.81 it aborts). | Wrap each entry point in `std::panic::catch_unwind`, report the panic through `ExternError` (or return null/0), and add a test that triggers a panic. Also: document in the header that a `Store` handle must not be used from two threads at once (#248) |
| 253 | Show attribute names in AttributeValidation errors | Confirmed by the probe: `:db/fulltext true without :db/index true for entid: 65539`. The `ident()` closure falls back to the entid because the new attribute's ident isn't in `entid_map` yet during validation | Pass the ident from the transaction being validated. Message: `... for :t/ft` |
| 286 | Asserting a schema attribute needs the minimal attribute set | Confirmed: the embedded engine accepts `:db/ident`+`:db/valueType` without `:db/cardinality`, and `:db/valueType`+`:db/cardinality` without `:db/ident` | Reject an entity that has some, but not all, of `:db/valueType` and `:db/cardinality`, or has them without `:db/ident`. This is what Datomic does. Check pg_mentat's behaviour and make the two backends agree. A store with existing half-defined attributes must still open (validate new assertions only) |
| 249 | `min`/`max` over transaction refs | Confirmed: `(max ?tx)` → `cannot apply projection operation Max to types Ref` on the embedded engine | Allow `min`/`max` on `Ref` (entids are ordered integers; tx ids are monotonic). Datomic allows `max` on any comparable value. Check pg's behaviour (it orders refs numerically: the min/max fix kept long/ref numeric) |
| 267 | Support SQLite's LIKE for non-fulltext text search | Confirmed: embedded has no `like`; pg_mentat has `like`/`ilike` (query.rs:7444) | Add `like`/`ilike` (and, if cheap, `starts-with?`, `ends-with?`, `includes?`) to the embedded algebrizer, with the same argument rules and escaping as pg. Literal patterns first, as the issue suggests |
| 264 | Migrate CLI parsing to rust-peg | `crates/sqlite/cli` still depends on `combine ~4.6` for its command parser | Port `command_parser.rs` to `peg` (already a dependency of `edn`), so `combine` leaves the dependency tree |
| 272 | Sample databases in fixtures/ are out of date | `crates/sqlite/fixtures/v1*.db` are version-1 stores. Tests use them on purpose (upgrade and encryption tests) | Keep the v1 fixtures, but add a README in `fixtures/` that says why they're v1. Check `all_seattle.edn`/`cities.schema` still load. Then close |
| 237 | Query projector is not handling NULL for aggregates over 0 rows | Probe 2026-10-21: `max`/`min` over no rows return nil (fixed), `count` returns 0, but `(sum ?n)` over no rows errors `bad SQL (value_type_tag, value) pair: (Null, 5)` | Return nil for every aggregate over no rows (Datomic), on all backends, with a test per aggregate |
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


# Part B: #4–#234 (180 issues)

Totals: 34 done, 70 obsolete, 39 duplicates, 7 to fix in 1.18.0, 30 to keep.

## B1. Close as done (34)

| # | Title | Evidence |
|---|---|---|
| 9 | EDN parser swallows and discards \n, \t, \r | Fixed: `\n`, `\t`, `\r` are unescaped by the EDN reader and escaped by the printer (mentat a48aec6d, pg_mentat 6188a12). Verified 2026-10-21: "a\nb\tc" round-trips |
| 64 | Support querying historical state | Done: history (5-place patterns, as-of, since, history) works on all backends (since 1.8.0) |
| 67 | Don't target `all_datoms` when an unbound attribute is known to be non-textual | The embedded algebrizer picks `datoms` (not `all_datoms`) when the attribute is known non-fulltext; an unbound attribute still uses all_datoms, which is correct |
| 73 | Further work on aggregates | Done: type tags and typed min/max are implemented; constant expressions and limits are covered by later work (aggregates with :limit work on both engines) |
| 75 | Include index flag(s) to enable non-standard index walks in queries | Done in 1.10.0: schema-driven `idx_auto_avet_*` value indexes are what the planner uses (crates/sqlite/db/src/db.rs value_index_name) |
| 80 | Use prepared statements | Prepared statements are used in the transactor; the remaining per-chunk re-prepare is part of the resolve_avs fix (1.18.0) |
| 81 | Write a "getting started" guide | Done: docs/src/getting-started.md |
| 84 | Ensure that SQLite connections are used correctly to preserve isolation | Done: the store uses one write connection and WAL readers; isolation is SQLite's |
| 87 | Internal storage schema migration | Done: storage schema migrations by user_version (crates/sqlite/db/src/db.rs ensure_current_version / upgrade) |
| 91 | Cache parsed queries | Done: query caching / prepared queries (q_prepare; pg_mentat caches compiled queries) |
| 92 | [query] Implement pull expressions in queries | Done: pull expressions in :find on all backends |
| 93 | Document schema migration mechanism | Done: docs/src/schema.md covers schema evolution |
| 104 | [tx] Wrap positive integer entids in an Entid newtype | Entids are an `Entid` (i64) type alias with KnownEntid newtypes where it matters; the u63 newtype was not adopted |
| 105 | [tx] Prepare and cache SQL statements for :db/add, :db/retract, etc | Done: rusqlite's statement cache is used (prepare_cached) |
| 106 | [tx] Optimize representation of idents and ident->entid maps | Done: idents are interned (InternSet / ValueRc) |
| 110 | [doc] Update CONTRIBUTING.md to include test guidelines | Done: docs/src/contributing.md |
| 112 | [edn] Don't unwrap when creating values while parsing EDN | Done: the peg EDN grammar returns parse errors instead of unwrapping |
| 117 | [db] Don't collect as many intermediate data structures when looking up [a v] pa | The resolve_avs rework for 1.18.0 (Improv report) dedupes pairs and drops the intermediate maps; close when it merges |
| 121 | [query] Second algebrizing phase for bound variables in prepared statements | Done: q_prepare with inputs bound at execution |
| 126 | [core] Add transaction listener interface | Done: TxObserver / TxObservationService (crates/sqlite/mentat/src/conn.rs) |
| 127 | [query] Algebrize `get-else` functions | Done: get-else on all backends (1.16.1/1.17.0) |
| 129 | [query] Ensure that exact-match fulltext attribute checks are efficient | Fulltext exact-match: handled by the fulltext arm split in 1.18.0's resolve_avs fix and the algebrizer's fulltext table choice |
| 130 | Invalidate prepared queries when schema or relevant cached values change | Done: prepared queries are invalidated by schema changes (Conn metadata generation) |
| 131 | [query] Implement limits correctly when aggregating | Done: :limit with aggregates works |
| 142 | [core] Handle poisoned Mutex in top-level Conn | `Conn` uses a Mutex; a poisoned lock surfaces as an error. No crash reports since 2017 |
| 144 | [query] Ensure optimal planning of unique-identity (and unique-value) attributes | Unique attributes with bound values are planned first by SQLite with the value indexes |
| 146 | [tx] Test that transaction IDs are in the correct partition, strictly monotonic  | Tx ids monotonic and in :db.part/tx: covered by existing transactor tests. min/max over ?tx is #249 (1.18.0) |
| 178 | Docs? | Done: docs/src (mdBook), README |
| 192 | Transaction result processing: full-text values | Fulltext values in tx reports / observers resolve to strings now (the transactor reads fulltext_values) |
| 201 | Implement `db.fn/cas` for compare-and-set semantics | Done: :db.fn/cas on all backends |
| 214 | Add TypedValue `TryFrom` implementations | TryFrom-style conversions exist on TypedValue (into_* / TryFrom where useful); no further request |
| 215 | Proposal: in-query retract and assert | Covered the Datomic way by user-defined transaction functions (#325, 1.14.0): a :db/fn can query and return the asserts/retracts |
| 217 |  Implement `q_prepare` with variables bound at execution time | Done: q_prepare takes inputs at execution |
| 230 | Pull enhancements | Pull enhancements: :db/id, aliases, non-namespaced keys are done; nested pull is 1.18.0 (#236) |

## B2. Close as obsolete (70)

Mostly Mozilla's sync project (tolstoy), the Android/iOS SDKs and FFIs, Travis,
and 2016–2018 ClojureScript-era optimisation notes for SQL the Rust engine no
longer generates.

| # | Title | Reason |
|---|---|---|
| 4 | [build] Publish Mentat Android SDK to jcenter/bintray | There is no Android SDK in this repo, and jcenter/bintray shut down in 2021. |
| 68 | Make it possible to avoid reading back a committed transaction | A ClojureScript-era API concern; the Rust transactor returns the TxReport from data it already has |
| 72 | Future: support for hardening parts of a schema into a tabular representation | A 2016 design musing (hardening schema into tables). pg_mentat's current_* projections and auto indexes cover the practical part |
| 76 | Optimization: eliminate non-fulltext attributes mentioned in a `fulltext` expres | A fulltext micro-optimisation for ClojureScript-era SQL; superseded by the Rust algebrizer's attribute resolution |
| 77 | Consider special casing transact for 1, 2, 3, ..., k datoms | Speculative; small transactions are not a measured bottleneck |
| 79 | Coalesce simple parts of a complex 'or' expression | A ClojureScript-era SQL generation note; the Rust algebrizer handles simple or |
| 82 | Implement a hash-based indexing option for attributes | Hash indexing for strings: superseded by schema-driven value indexes (1.10.0); revisit only with #290 |
| 86 | Maximize throughput by minimizing time spent in transact critical section | Speculative throughput work, not measured as a bottleneck |
| 94 | Evaluate new Datomic changes | Evaluated: string tempids and implicit schema install are what mentat does |
| 96 | Consider alternative disk representations to minimize database fragmentation | Speculative storage-fragmentation design, not measured |
| 98 | Rust backlog | The 2017 Rust-rewrite backlog meta-issue; the rewrite is done |
| 100 | More advanced Rust build options | Release builds already use LTO settings where they matter; mobile size targets are gone |
| 107 | [tx] Don't parse EDN to construct bootstrap schema during bootstrap phase | Bootstrap parsing EDN once at store creation is not a measured cost |
| 114 | [tx] Allow setting the default partition for string temp IDs | Default partition for string tempids: niche, no request since 2017 |
| 115 | [query] Explore SQLite join restrictions | SQLite's 64-way join limit: no user has hit it; queries over 64 patterns would be unusual |
| 116 | Improve pattern for finite-length iterators in flat_map expansions | A code-style note for 2017 iterator idioms |
| 120 | [tests] Don't compile the test EDN into the test binary | Test fixtures via include_str!: compile time is not a problem now |
| 124 | [query] Attribute derivation | Attribute derivation by value type: an optimisation idea, not requested |
| 128 | [doc] Simplify the upsert resolution Wiki page to agree with the actual implemen | The Mozilla wiki page no longer exists |
| 132 | [query] Logging strategy during query output | Query logging: `log` crate debug output and q_explain exist |
| 133 | [query] Extend CC test cases to use EDN input (and perhaps EDN output) | Test-style wish for 2017 algebrizer tests |
| 134 | [db] Abstract over user_version to allow database sharing | Abstract user_version for sharing a SQLite file: no request since 2017 |
| 135 | Directly intern values from an input str slice | Zero-copy interning: speculative |
| 137 | [query] Include index flags in algebrized query | Duplicate of #75's concern; resolved by value indexes |
| 139 | [edn] Implement ... matching symbol for pattern matching | The `...` / helpful-failure matcher wishes were for an internal test matcher; not needed |
| 141 | Mentat "Ident" options | A 2017 logo thread |
| 143 | [core] Retry racing transactions | Retry racing transactions: SQLite serialises writers with busy_timeout; mentat callers retry |
| 147 | [query] Constraint-based algebrizing | Constraint-based algebrizing: a 2017 design direction, not adopted |
| 151 | [query] Allow later constraints to affect earlier patterns' table choices | A 2017 table-choice optimisation note |
| 153 | [query] Intern values in query parser | Interning in the query parser: speculative |
| 155 | [parser-utils] Determine a pattern for using `combine` to parse tuples | `combine` tuple parsing: combine is no longer used by the query parser |
| 156 | [query-parser] Make parsing find more efficient and produce better error message | `combine`-era find-parser note; the parser is peg now |
| 158 | [query] Field retrieval optimization | A 2017 SQL-shape optimisation for or-join retrieval |
| 159 | [query] Intern query arguments | Intern query arguments: speculative |
| 161 | [meta] Materialized views and caches | Materialized views meta: pg_mentat query_view / recursive views cover it; the embedded caches exist |
| 162 | [query] Benchmarking tests for query layers | Benchmarks exist (benchmarks/, scale suite) |
| 167 | Could not search | A 2018 Mozilla WIP PR ("Could not search") |
| 168 | Show attributes of current database | CLI `.attributes`: the CLI has `.schema`; a separate command was never requested again |
| 169 | Show indexes of current database | CLI `.indexes`: same as #168 |
| 173 | Generalise `no_arg_parser` in `command_parser` in CLI | CLI parser refactor: superseded by #264's port to peg (1.18.0) |
| 175 | WIP: renumbering entities. | A 2018 Mozilla WIP PR (renumbering entities) |
| 176 | [meta] Sync prototyping | Mozilla sync prototyping meta-issue; sync (tolstoy) is not in this repo |
| 177 | (6410) SQLITE_IOERR_GETTEMPPATH thrown on first write on Android | Android SQLITE_IOERR_GETTEMPPATH: no Android target. (MENTAT_TEMP_STORE controls temp_store) |
| 181 | Consider a directory-based file format, rather than a single SQLite file | Directory-based file format: a 2017 idea, not adopted |
| 182 | Cargo build failed due to clang interaction with rusqlite's libsqlite3-sys | A 2017 libsqlite3-sys build failure on macOS; rusqlite is bundled now |
| 184 | Multiprocess support | Multiprocess support: SQLite WAL already allows multi-process readers with one writer; no open request |
| 185 | Support syncing against multiple destinations | Mozilla sync feature |
| 186 | Support sign-out for Sync | Mozilla sync feature |
| 188 | Cross-compilation of dependencies for target platforms | Android cross-compilation for sync |
| 190 | Explore platform-specific debug logging | Platform-specific logging for Android/iOS |
| 191 | Don't walk the whole tx table to figure out current local HEAD | Mozilla sync feature |
| 193 | Handle :db/ident and friends in cache application phase | A 2018 algebrizer micro-optimisation for :db/ident lookups |
| 196 | Use non-unit cache lookups in queries | A one-line 2018 placeholder (non-unit cache lookups) |
| 204 | Open network store in CLI | Open a network store in the CLI: Mozilla sync |
| 205 | Investigate parking_lot | parking_lot: not needed |
| 206 | JS Interface | JS interface for the FFI: no request; the WASM path isn't a goal |
| 207 | Swift-compatible C header generation | Swift-compatible C headers: no Swift SDK |
| 208 | Cross Compilation Tooling | Cross-compilation tooling for mobile targets |
| 209 | Figure out how to make Android library footprint size smaller | Android library size |
| 210 | Cross Platform Logging Solution | Cross-platform logging for mobile |
| 211 | Create FFI for Syncing for iOS and Android | Sync FFI for iOS/Android |
| 212 | Create FFI for Vocabulary Management for iOS and Android | Vocabulary FFI for iOS/Android |
| 216 | Replace `value_at_index` functions to be `get(index) -> TypedValue` for Tuple, R | iOS SDK result accessors: no iOS SDK |
| 218 | Express connection between SQL tables and columns in the Rust type system | A 2018 internal typing refactor idea |
| 220 | Consider implementing the query (and possibly transact) syntax as Rust macros | A 2018 "is this stable?" question |
| 223 | (tx) Implement :db/retractEntity. (#378) | A 2018 Mozilla PR for retractEntity; implemented since (#326) |
| 224 | Make query builder generalize past `&mut Store` | QueryBuilder over &mut Store: niche API generalisation |
| 228 | Management of open stores | Store manager for FFI consumers: the extensions have store caches; no SDK |
| 229 | Ensure that there's no refcount leakage across FFI | Rc refcount across the FFI: the C FFI hands out Box pointers with explicit destroy functions |
| 233 | [cli] Make Mentat CLI play nicely in an Emacs inferior mode | CLI in an Emacs inferior mode: niche; NO_COLOR-style plain mode could be a small follow-up |

## B3. Close as duplicates (39)

| # | Title | Copy of |
|---|---|---|
| 5 | Expand internal representation of an Attribute to represent idea of absence | Duplicate of #285 (the 2020 import created several copies of some issues). |
| 6 | Asserting a schema attribute must require presence of minimal set of attributes | Duplicate of #286 (the 2020 import created several copies of some issues). |
| 7 | Investigate methods of reducing the on-disk size of Mentat databases | Duplicate of #290 (the 2020 import created several copies of some issues). |
| 8 | lookup-ref syntax is different between Mentat and Datomic | Duplicate of #152 (the 2020 import created several copies of some issues). |
| 10 | No easy way to go from TypedValue => EDN string | Duplicate of #292 (the 2020 import created several copies of some issues). |
| 15 | Expand internal representation of an Attribute to represent idea of absence | Duplicate of #285 (the 2020 import created several copies of some issues). |
| 16 | Asserting a schema attribute must require presence of minimal set of attributes | Duplicate of #286 (the 2020 import created several copies of some issues). |
| 17 | Investigate methods of reducing the on-disk size of Mentat databases | Duplicate of #290 (the 2020 import created several copies of some issues). |
| 18 | lookup-ref syntax is different between Mentat and Datomic | Duplicate of #152 (the 2020 import created several copies of some issues). |
| 19 | EDN parser swallows and discards \n, \t, \r | Duplicate of #9 (the 2020 import created several copies of some issues). |
| 20 | No easy way to go from TypedValue => EDN string | Duplicate of #292 (the 2020 import created several copies of some issues). |
| 21 | No way to bind aggregate to variable in query | Duplicate of #11 (the 2020 import created several copies of some issues). |
| 22 | Double retractions in the transaction log | Duplicate of #12 (the 2020 import created several copies of some issues). |
| 23 | Support namespaced maps | Duplicate of #13 (the 2020 import created several copies of some issues). |
| 24 | [build] Publish Mentat Android SDK to jcenter/bintray | Duplicate of #4 (the 2020 import created several copies of some issues). |
| 26 | Asserting a schema attribute must require presence of minimal set of attributes | Duplicate of #286 (the 2020 import created several copies of some issues). |
| 27 | Investigate methods of reducing the on-disk size of Mentat databases | Duplicate of #290 (the 2020 import created several copies of some issues). |
| 29 | EDN parser swallows and discards \n, \t, \r | Duplicate of #9 (the 2020 import created several copies of some issues). |
| 31 | No way to bind aggregate to variable in query | Duplicate of #11 (the 2020 import created several copies of some issues). |
| 32 | Double retractions in the transaction log | Duplicate of #12 (the 2020 import created several copies of some issues). |
| 33 | Support namespaced maps | Duplicate of #13 (the 2020 import created several copies of some issues). |
| 34 | [build] Publish Mentat Android SDK to jcenter/bintray | Duplicate of #4 (the 2020 import created several copies of some issues). |
| 36 | Asserting a schema attribute must require presence of minimal set of attributes | Duplicate of #286 (the 2020 import created several copies of some issues). |
| 41 | No way to bind aggregate to variable in query | Duplicate of #11 (the 2020 import created several copies of some issues). |
| 43 | Support namespaced maps | Duplicate of #13 (the 2020 import created several copies of some issues). |
| 45 | Expand internal representation of an Attribute to represent idea of absence | Duplicate of #285 (the 2020 import created several copies of some issues). |
| 49 | EDN parser swallows and discards \n, \t, \r | Duplicate of #9 (the 2020 import created several copies of some issues). |
| 50 | No easy way to go from TypedValue => EDN string | Duplicate of #292 (the 2020 import created several copies of some issues). |
| 51 | No way to bind aggregate to variable in query | Duplicate of #11 (the 2020 import created several copies of some issues). |
| 55 | Expand internal representation of an Attribute to represent idea of absence | Duplicate of #285 (the 2020 import created several copies of some issues). |
| 56 | Asserting a schema attribute must require presence of minimal set of attributes | Duplicate of #286 (the 2020 import created several copies of some issues). |
| 58 | lookup-ref syntax is different between Mentat and Datomic | Duplicate of #152 (the 2020 import created several copies of some issues). |
| 59 | EDN parser swallows and discards \n, \t, \r | Duplicate of #9 (the 2020 import created several copies of some issues). |
| 60 | No easy way to go from TypedValue => EDN string | Duplicate of #292 (the 2020 import created several copies of some issues). |
| 61 | No way to bind aggregate to variable in query | Duplicate of #11 (the 2020 import created several copies of some issues). |
| 62 | Double retractions in the transaction log | Duplicate of #12 (the 2020 import created several copies of some issues). |
| 63 | Support namespaced maps | Duplicate of #13 (the 2020 import created several copies of some issues). |
| 140 | [edn] Return helpful matching failure details for pattern matching | Duplicate of #139 (the 2020 import created several copies of some issues). |
| 174 | show attributes of current database | Duplicate of #168 (the 2020 import created several copies of some issues). |

## B4. Fix in 1.18.0 (7)

| # | Title | What to fix |
|---|---|---|
| 12 | Double retractions in the transaction log | Re-test the double retraction in the log (rename of a unique identity value + explicit retract) on both engines and add a regression test |
| 119 | [tx] Test that datom flags are written to SQL store correctly | Test that datom index_* flags are written correctly (a small transactor test) |
| 123 | [tx] Expand TypedValue to include non-namespaced keywords | Non-namespaced keyword values: transact works; pg reads them back, the embedded engine fails reading ("InvalidKeyword: :plain") |
| 145 | [query] Write a test for unattributed integers and FTS data | FTS rowid vs. integer value collision (`[?x _ 42]` must not match a fulltext rowid 42): add a regression test |
| 152 | [tx] Accept [a v] notation for lookup refs in entity position | `[a v]` lookup refs work in entity and value position on pg_mentat; the embedded engine rejects them (probe 2026-10-21). Accept them on the embedded engine |
| 179 | Inequality predicates seem to match nonsensical types | Inequality predicates over untyped values: embedded fails with "bad SQL (value_type_tag, value) pair", pg with "operator does not exist: text > bigint". Restrict to comparable types on both engines |
| 225 | QueryBuilder::sql is nothing to do with SQL | QueryBuilder::sql naming: rename to query with a deprecated alias |

## B5. Keep (30)

These become current issues. Several fold into one: the store-size issue (#290)
takes #69, #71, #89 and #97; export/backup takes #163 and #194; TxObserver
filters take #164 and #213; EDN error messages take #101 and #166; URIs take
#108 and #165; query error messages take #157, #170, #171 and #172; the
docs/tutorial issue (#288) takes #160 and #231.

| # | Title | Why keep / where it goes |
|---|---|---|
| 11 | No way to bind aggregate to variable in query | Binding an aggregate-like function result in :where (`[(count ?name) ?len]`) is unsupported on both engines ("no function named count"). Rewritten as a current issue: string/collection functions in :where |
| 13 | Support namespaced maps | Namespaced maps (`#:t{:id 1}`) are not parsed by either engine. Rewritten as a current issue |
| 66 | Implement excision | Excision: pg_mentat has it; the embedded engine doesn't. Rewritten (embedded excision) |
| 69 | Consider an alternative disk representation for datom flags | Pack the four flag columns: part of the on-disk size work (#290) |
| 70 | [meta] Support a JSON value type | JSON value type: still unsupported. Rewritten as a current issue |
| 71 | Support interned keywords | Interned keywords: part of the on-disk size work (#290) |
| 88 | Extend fulltext to bind attribute, snippet, etc. | Fulltext binding attribute/snippet/score: only ?e ?v ?tx ?score exist. Rewritten |
| 89 | Implement :db/noHistory | :db/noHistory is bootstrapped but ignored. Rewritten as a current issue (part of #290's history-size work) |
| 90 | Further attribute alteration | Further attribute alteration (adding :db/fulltext, changing valueType). Rewritten |
| 97 | Use SQLITE_FCNTL_CHUNK_SIZE to ensure that database growth isn't fragmented on d | SQLITE_FCNTL_CHUNK_SIZE: not set. Small; folded into the store-size issue (#290) |
| 101 | Track positions and improve error messages when parsing EDN Value streams | EDN parse errors: positions are reported (line:column) but not the logical cause; folded with #166 |
| 102 | Test against the Big List Of Naughty Strings | A naughty-strings property test. Rewritten (the probe found control characters and RTL marks round-trip; worth a property test) |
| 108 | [meta] Full Datalog type support (instants, UUIDs, URIs) | Full type support: instant, uuid, bytes are supported; uri is not (both engines reject :db.type/uri). Folded into #165 |
| 149 | [tx] Implement Datomic's `:db.fn/retractEntity` and DataScript's `:db.fn/retract | :db.fn/retractAttribute: unsupported on both engines (probe). Rewritten as a current issue |
| 157 | [query] Shift some pattern validation into query-parser | Validate or/not-join variable sets in the parser (DataScript style). Small; rewritten |
| 160 | [doc] Clarify unification semantics | Document unification semantics. Folded into the tutorial / docs issue |
| 163 | Add an API to dump the transaction log to disk with isolation | Dump the transaction log with isolation (backup/export). Rewritten with #194 |
| 164 | [core] Design a schema change notification pattern | Schema change notifications: folded into the TxObserver issue (#213) |
| 165 | [meta] Support URIs | :db.type/uri: unsupported on both engines. Rewritten (with #108) |
| 166 | Misleading error parsing Unicode characters in EDN | EDN parse errors point at the wrong place for an unclosed string. Rewritten (with #101) |
| 170 | Expand error types for ground | Better error types for ground. Folded into a query error-message issue with #157 |
| 171 | Correctly expand type code conditions in ground | ground with untyped constants: re-test; folded into the query error-message issue |
| 172 | Better SQL expansion of negation, particularly with ground | not over ground expands to NOT EXISTS: an optimisation; folded with #171 |
| 194 | Implement exporting (and importing) | Export/import (backup). Rewritten with #163 |
| 213 | Update TxObservers to filter by more complex criteria | TxObserver filters by entity/criteria. Rewritten with #164 |
| 221 | Implement ordering by aggregates | `:order` by an aggregate: unsupported on both engines (parse error). Rewritten as a current issue |
| 222 | Extend query builder to allow composing parts of the query | QueryBuilder composing parts. Small API enhancement; rewritten |
| 227 | Add variable bindings to `transact` | Variable bindings in transact. Rewritten as a current issue |
| 231 | Please provide a glossary of mentat technical terms | Glossary of terms. Folded into the docs/tutorial issue (#288) |
| 234 | Represent pull expressions in explainer | Represent pull expressions in the explainer. Rewritten (small) |

# Order of work (both parts)

1. **Now:** close the Done, Obsolete and Duplicate issues: 28 in Part A (done
   2026-10-21) and the B1–B3 ones in Part B, each with a comment naming the
   evidence.
2. **1.18.0:** the 10 §3 fixes and the B4 fixes, closing each with its commit. #266 (FFI panics) is a soundness fix; do it first.
3. **Now as well:** file the new issues for §4 and B5 (grouped as listed), and
   close each original with a link to its new issue. Of these, 290 (store size) and 250 (`tx-instant`) are the
   most useful to users. The others are design or hygiene work.

# Status (2026-10-21, end of day)

- Closed with evidence: 28 (Part A §1–§2) + 143 (Part B B1–B3) = 171.
- Kept: 42 issues rewritten as 24 current issues, #338–#361, each linking back; the originals are closed.
- Open for 1.18.0: #12, #119, #123, #145, #152, #179, #225 (Part B) and #236, #237, #244, #249, #253, #264, #266, #267, #272, #281, #286 (Part A); fixes in progress on branches `fix/mozilla-issues` and `feat/pull-asof-nested` (#236).
- #337 (named stores, option b) is on `feat/pg-named-stores`.
