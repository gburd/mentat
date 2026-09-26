# Plan: a `mentat` DuckDB extension in Rust (`crates/duckdb`)

**Status:** research / plan only. No extension code is written yet. This
document is the concrete implementation plan and the research report behind it.

**Goal:** a third consumer of the embedded `mentat` engine — load `mentat` into
DuckDB as a loadable extension, usable from the DuckDB CLI (`LOAD`) and
embedded, mirroring the SQLite crate and `pg_mentat`. First cut embeds the
existing `mentat` SQLite store; DuckDB is only the query surface.

All crate versions below were confirmed against crates.io and the duckdb-rs
`main` README/examples on the research date. Anything unverified is flagged
explicitly in **§9 Risks / unverified**.

---

## 0. TL;DR

- Use the official **duckdb-rs** project. Crates: `duckdb`, `libduckdb-sys`,
  `duckdb-loadable-macros`, all at **`~1.10505.0`** (= bundled DuckDB **v1.5.5**;
  the crate version encodes the DuckDB version as `1.MAJOR_MINOR_PATCH.x`).
- `duckdb-extension-framework` (0.7.0, "purely experimental") is **superseded**
  by duckdb-rs' `loadable-extension` feature. Do not use it.
- Extension surface (mirrors `pg_mentat`):
  - **Table function** `mentat_query(query VARCHAR, inputs VARCHAR) → table`
    (Datalog → rows). `VTab` trait.
  - **Scalar function** `mentat_transact(edn VARCHAR) → VARCHAR` (JSON tx-report).
    `VScalar` trait.
  - **Scalar function** `mentat_pull(...) → VARCHAR` (JSON) and, behind a
    feature, `mentat_eval(script VARCHAR) → VARCHAR` (mino).
- Storage: **embed the existing `mentat` SQLite store** (option (a)). The
  extension links the workspace `mentat` crate by `path`; that crate opens its
  own SQLite file via rusqlite `bundled`. DuckDB does **not** bundle sqlite3
  into core, and the whole thing compiles into one `cdylib` with hidden symbol
  visibility, so there is **no duplicate-`sqlite3_*` clash**. Native DuckDB
  storage backend = future work (option (b)).
- Build: the extension is a `cdylib`, but `cargo build` alone does **not**
  produce a loadable file — DuckDB requires a metadata footer + `.duckdb_extension`
  name. Use the official **`extension-template-rs`** layout + `extension-ci-tools`
  Makefiles (`make configure && make debug`), then load into `duckdb -unsigned`.
- Wire `crates/duckdb` as a workspace **member but not a `default-member`**
  (exactly how `crates/pg/pg_mentat` is handled today), so a plain
  `cargo build`/`cargo test` at the root never pulls the DuckDB toolchain.

---

## 1. Research: DuckDB extension mechanisms for Rust (2024–2026)

DuckDB extensions were historically C++. Since DuckDB ~v0.10/1.0 there is a
**stable C Extension API** (the `duckdb_extension_api` / C-API entrypoint), which
lets non-C++ languages produce loadable extensions. Rust binds to it through the
**duckdb-rs** project.

### 1.1 The crates (confirmed on crates.io)

| Crate | Newest version | Role |
|---|---|---|
| `duckdb` | `1.10505.0` | Ergonomic wrapper (rusqlite-style) **and** the extension API (`vtab`, `vscalar`, `duckdb_entrypoint_c_api`). MSRV `1.85.1`. |
| `libduckdb-sys` | `1.10505.0` | Native C-API bindings; vendors/downloads DuckDB. |
| `duckdb-loadable-macros` | `1.10505.0` | Proc-macros for the loadable-extension entrypoint. |
| `duckdb-extension-framework` | `0.7.0` | **Deprecated/experimental**, "purely experimental DuckDB extension framework." Superseded. Do **not** use. |

**Version scheme (confirmed from the duckdb-rs README):** starting with DuckDB
`v1.5.0`, the crate version is `1.MAJOR_MINOR_PATCH.x` — e.g. DuckDB `v1.5.0` →
`1.10500.x`, and `1.10505.0` → **DuckDB v1.5.5**. Older releases use the plain
DuckDB version (`1.4.5`, `1.3.2`, …). Pin with a tilde (`~1.10505.0`) so you get
crate patch releases without silently changing the bundled DuckDB version.

**LTS option:** the duckdb-rs `v1.4-andium` git branch stays on DuckDB 1.4
(Andium LTS). Only relevant if a downstream must target 1.4; default target is
1.5.5 via crates.io.

### 1.2 Loadable extension: how it actually works

- Enable the `duckdb` feature **`loadable-extension`** (it pulls in `vtab`,
  `duckdb-loadable-macros`, and `libduckdb-sys/loadable-extension`).
  Confirmed feature graph on `duckdb 1.10505.0`:
  `loadable-extension = ['vtab', 'duckdb-loadable-macros', 'libduckdb-sys/loadable-extension']`.
- The entrypoint is the **`#[duckdb_entrypoint_c_api]`** attribute macro on a
  function `fn(con: Connection) -> Result<(), Box<dyn Error>>`, where you call
  `con.register_table_function::<T>(name)` / `con.register_scalar_function::<T>(name)`.
- **Signing / loading:** DuckDB refuses to `LOAD` a shared library that lacks a
  DuckDB metadata footer, and refuses unsigned extensions unless started with the
  `-unsigned` CLI flag (or the DB opened with `allow_unsigned_extensions=true`).
  - `cargo build` produces only a bare `.so`/`.dylib`. Loading it fails with
    *"The file is not a DuckDB extension. The metadata at the end of the file is
    invalid."*
  - The metadata footer (and the `.duckdb_extension` rename) is appended by
    DuckDB's build tooling — the `extension-ci-tools` `c_api_extensions`
    Makefiles that `extension-template-rs` uses. That is why we adopt the
    template's build wiring rather than hand-rolling.
  - For local dev, no signing is needed: `duckdb -unsigned` + `LOAD '<path>'`.
    Signing is only required to publish to DuckDB's Community Extensions or to
    load into a stock (signed-only) DuckDB. **Out of scope for v1.**

> ⚠ **Unstable C API.** `extension-template-rs` sets `USE_UNSTABLE_C_API=1`
> because duckdb-rs currently relies on unstable C-API surface. Consequence:
> the built extension is only guaranteed to load into the **exact**
> `TARGET_DUCKDB_VERSION` it was built against (`v1.5.5` here). Forward
> compatibility is explicitly not guaranteed. This is the single most important
> operational constraint — see §9.

### 1.3 The recommended path (confirmed current)

The duckdb-rs README states plainly: start from the official
**[`extension-template-rs`](https://github.com/duckdb/extension-template-rs)**
template. It handles the metadata footer, platform/version detection, tests, and
CI. There is also a C template (`extension-template-c`) but that is for C
extensions, not relevant here. The Rust `hello-ext` example inside duckdb-rs is
the minimal reference for the API; the template is the build harness.

---

## 2. Research: what surface to expose, and value mapping

Mirror `pg_mentat` (its functions today: `mentat_query(text, jsonb)`,
`mentat_transact(text)`, `mentat_pull(...)`, `mentat_eval(text)`), adapted to
DuckDB's registration model.

### 2.1 Function → DuckDB entrypoint mapping

| mentat function | DuckDB entrypoint | Rationale |
|---|---|---|
| `mentat_query(query, inputs)` | **Table function** (`VTab`) | Returns *rows of typed columns* — the natural DuckDB table-producing shape. Column count/names come from the query's `FindSpec`. |
| `mentat_transact(edn)` | **Scalar function** (`VScalar`), returns `VARCHAR` (JSON tx-report) | One input → one JSON result value; matches `pg_mentat.mentat_transact` returning JSON text. |
| `mentat_pull(entity, pattern)` | **Scalar function**, returns `VARCHAR` (JSON) | Pull result is a nested map → JSON string is the pragmatic first cut. |
| `mentat_eval(script)` | **Scalar function**, returns `VARCHAR`, behind a `script`/`mino` feature | Mirrors `pg_mentat`'s optional scripting surface; off by default. |

Why `mentat_query` is a **table function and not** a scalar returning JSON: it
lets users write real SQL — `SELECT ... FROM mentat_query('[:find ?e ?name ...]', '{}')`,
join against DuckDB tables, filter, aggregate — which is the whole point of
putting Datalog behind DuckDB. `transact`/`pull`/`eval` are single-value
side-effecting/producing calls, so scalar-returning-JSON is the lazy correct fit
(and matches `pg_mentat` exactly).

### 2.2 Table-function shape (the `VTab` lifecycle)

The `VTab` trait (confirmed from duckdb-rs `hello-ext` and `extension-template-rs`
`src/lib.rs`) has three phases:

- **`bind(&BindInfo) -> BindData`** — read the parameters
  (`bind.get_parameter(0)`), and declare the result columns with
  `bind.add_result_column(name, LogicalTypeHandle)`. This is where we run the
  query's algebrizer far enough to know the `FindSpec` column count/names/types.
- **`init(&InitInfo) -> InitData`** — per-scan state (e.g. an index/cursor into
  the already-computed result set).
- **`func(&TableFunctionInfo, &mut DataChunkHandle)`** — fill an output chunk;
  called repeatedly until it emits a zero-length chunk. Write columns via
  `output.flat_vector(col_idx)` + `.insert(row, value)` / `.set_null(row)`, then
  `output.set_len(n)`.
- **`parameters() -> Option<Vec<LogicalTypeHandle>>`** — declare parameter types
  (`Varchar` for both `query` and `inputs`).

**Column discovery.** mentat already exposes what we need:
`store.q_once(query, inputs)?` returns a `QueryOutput` whose
`output.spec.columns()` yields the `FindSpec` `Element`s (used verbatim by the
SQLite CLI to print headers — `crates/sqlite/cli/.../repl.rs:439`). For v1 the
simplest correct approach:

- In `bind`: parse+algebrize the query to get the `FindSpec`; declare N result
  columns. Type per column: for v1, declare **all columns `VARCHAR`** and
  stringify values (see §2.4) — smallest correct diff, no per-column type
  inference. `ponytail:` naive all-VARCHAR projection; add typed columns per
  §2.4 when a consumer needs to filter/aggregate on native types.
- Execute the query once (in `bind` or lazily in `init`), materialize
  `QueryResults` into a `Vec<Vec<String>>` (or typed rows later), store in
  `InitData`, and stream chunks in `func`.

`QueryResults` (`crates/sqlite/query-projector/src/lib.rs:85`) is one of
`Scalar | Tuple | Coll | Rel`. Normalize all four into rows-of-columns:
- `Rel(RelResult)` → N columns, M rows (the common case).
- `Coll` → 1 column, M rows.
- `Tuple` → N columns, 0-or-1 rows.
- `Scalar` → 1 column, 0-or-1 rows.

### 2.3 mentat value model (confirmed source of truth)

- `ValueType` (`crates/core-traits/lib.rs:276`):
  `Ref, Boolean, Instant, Long, Double, String, Keyword, Uuid, Bytes`.
- `TypedValue` (`:406`): `Ref(Entid=i64) | Boolean(bool) | Long(i64) |
  Double(OrderedFloat<f64>) | Instant(DateTime<Utc>) | String(Rc<String>) |
  Keyword(Rc<Keyword>) | Uuid(Uuid) | Bytes(Bytes)`.
- `Binding` (`:737`): `Scalar(TypedValue) | Vec(Rc<Vec<Binding>>) |
  Map(Rc<StructuredMap>)` — the `Vec`/`Map` arms appear for pull expressions
  inside a query.

### 2.4 `TypedValue` → DuckDB `LogicalType` mapping (v2, typed columns)

`LogicalTypeId` variants confirmed present in duckdb-rs
(`crates/duckdb/src/core/logical_type.rs`): `Boolean, Bigint, Double, Varchar,
Blob, Uuid, Timestamp, List, Struct, Map`, etc.

| mentat `TypedValue` | v1 (all VARCHAR) | v2 (native `LogicalTypeId`) | Note |
|---|---|---|---|
| `Ref(i64)` | text of the entid | `Bigint` | entity id |
| `Long(i64)` | text | `Bigint` | |
| `Boolean(bool)` | `"true"/"false"` | `Boolean` | |
| `Double(f64)` | text | `Double` | |
| `String(str)` | the string | `Varchar` | |
| `Keyword(kw)` | `":ns/name"` | `Varchar` | keywords render with leading `:` |
| `Instant(DateTime<Utc>)` | RFC3339 text | `Timestamp` (micros) | mentat stores micros; DuckDB `Timestamp` is micros |
| `Uuid(Uuid)` | hyphenated text | `Uuid` | |
| `Bytes(Bytes)` | hex/base64 text | `Blob` | |
| `Binding::Vec` | JSON array string | `List<...>` | pull sub-collection |
| `Binding::Map` | JSON object string | `Struct`/`Map` or `Varchar(JSON)` | pull map; JSON string is the lazy correct v1 |

**Decision:** v1 = all columns `Varchar`, values stringified exactly like the
SQLite CLI's `binding_as_string` (reuse that rendering logic;
`crates/sqlite/cli/.../repl.rs:551`). v2 = per-column native types once a real
consumer needs to `WHERE`/`GROUP BY` on a native type. This is the ponytail
ladder: ship the JSON/text surface that already exists, add vectors when
measured need appears.

---

## 3. Research: storage decision

### 3.1 Option (a) — embed the `mentat` SQLite store (**chosen for v1**)

The extension links the workspace `mentat` crate (`crates/sqlite/mentat`) by
`path`. The public API is exactly what we need
(`crates/sqlite/mentat/src/{lib.rs,store.rs}`):

```rust
let mut store = mentat::Store::open("/path/to/mentat.sqlite")?; // or "" for in-memory
let report   = store.transact(edn_str)?;                        // -> TxReport
let out      = store.q_once(query_str, inputs)?;                // -> QueryOutput/QueryResults
```

DuckDB is purely the query/exec surface; datoms live in mentat's own SQLite file.
The DB path comes from a bind parameter or a session setting (see §6).

**Store lifecycle across DuckDB calls.** DuckDB scalar/table functions are
stateless between calls, and mentat's `Store` is `!Sync` (holds a
`rusqlite::Connection`). Open per-call for v1 (open → use → drop within one
function invocation), keyed by the DB path. `ponytail:` per-call open; add a
`thread_local!` path→Store cache (mirroring `pg_mentat`'s thread-local caches)
if open latency shows up in profiles. Do **not** share one `Store` across DuckDB
threads.

### 3.2 Option (b) — native DuckDB storage backend (future work)

A DuckDB-native datom store (datoms in DuckDB tables, a DuckDB-side query
planner) is what `pg_mentat` is for Postgres (`crates/pg/pg_mentat` is a full
~7.6k-line storage+planner implementation with its own `datoms_*` tables and SQL
generation). That is a large effort and explicitly **out of scope for v1**.
Note it as the natural follow-on if DuckDB's columnar/vectorized execution over
datoms becomes the point (rather than just exposing the Datalog surface).

### 3.3 ABI / duplicate-symbol analysis (the flagged risk — resolved)

- The `mentat` crate depends on `rusqlite` with the **`bundled`** feature
  (`crates/sqlite/mentat/Cargo.toml`; lock: `rusqlite 0.40.2`,
  `libsqlite3-sys 0.38.2`), i.e. it statically compiles its own `sqlite3`.
- **DuckDB core does not export `sqlite3_*` C symbols.** DuckDB's SQLite
  compatibility is the separate `sqlite_scanner` community extension, not part of
  the core library the extension links against. So there is no `sqlite3_open`
  vs `sqlite3_open` collision at the DuckDB boundary.
- Even if there were: the extension is a **`cdylib`**. Rust cdylibs export only
  explicitly-exported symbols (here: the single `#[duckdb_entrypoint_c_api]`
  entrypoint, `#[no_mangle]`); all transitively-linked C symbols
  (`sqlite3_*` from `libsqlite3-sys`) default to hidden/local visibility inside
  the shared object and cannot be interposed by, or clash with, the host duckdb
  process.
- **Rule to keep it that way:** the `mentat` dependency stays on rusqlite
  `bundled` (never a *system/dynamic* sqlite that could get pulled into the
  global namespace). This is already the default (`mentat`'s
  `default = ["bundled_sqlite3"]`).

**Conclusion:** linking the `mentat` Rust crate into the DuckDB Rust extension
is free and safe. No duplicate-symbol mitigation needed beyond keeping sqlite
statically bundled (the current default).

---

## 4. Research: build/test on this EC2 host

**Host actually observed** (differs from the brief — corrected here):

| Tool | Brief said | Actually on host |
|---|---|---|
| Rust | 1.90 | **1.98.0** (satisfies duckdb-rs MSRV 1.85.1) ✅ |
| cmake | 3.22 | **4.1.6** (only needed for `bundled-cmake`/`icu`, which we don't use) |
| clang | 15 | **not on PATH** (Nix env ships gcc-wrapper 15.3.0) |
| gcc | present | **15.3.0** ✅ |
| cores | 32 | **8** |
| python3 | yes | **3.13.15**, `venv` OK ✅ |
| make / git | — | make 4.4.1 ✅, git 2.54.0 ✅ |
| system `duckdb` | — | **none** (fine — see below) |

**Clang not required.** We use `bundled` (the `cc` backend + gcc) and duckdb-rs
ships **pregenerated** bindings, so `bindgen`/clang are not invoked (clang would
only be needed with `buildtime_bindgen`). We also do **not** use `bundled-cmake`
(that is for CMake-only extensions like `icu`), so cmake version is irrelevant.

**No system DuckDB needed.** `extension-template-rs`'s `make configure` creates a
Python venv and `pip install`s DuckDB + its SQLLogicTest runner, and derives the
build platform. That venv-installed `duckdb` also serves as the loader for
`make test`. (A separate system `duckdb` CLI is only needed for ad-hoc manual
`LOAD`; you can also invoke the venv's duckdb.)

### 4.1 Exact commands

```bash
# One-time: scaffold from the official template's build harness.
# We keep our own crate under crates/duckdb, but reuse the template's
# Makefile + extension-ci-tools submodule + .cargo/config.toml for the
# footer/metadata/test wiring.
cd ~/ws/mentat/crates/duckdb
git submodule add https://github.com/duckdb/extension-ci-tools extension-ci-tools
# Makefile mirrors extension-template-rs, with:
#   EXTENSION_NAME=mentat
#   USE_UNSTABLE_C_API=1
#   TARGET_DUCKDB_VERSION=v1.5.5

# Build (from crates/duckdb):
make configure          # creates venv, pip-installs duckdb + test runner, detects platform
make debug              # cargo build (cdylib) + append metadata footer -> build/debug/.../mentat.duckdb_extension
# or: make release

# Test with the template's SQLLogicTest runner:
make test_debug

# Manual smoke test with the venv duckdb (or any duckdb v1.5.5):
./extension-ci-tools/... /  or  python -m duckdb   # (venv duckdb)
# Then, in a duckdb started with -unsigned:
```

```sql
-- duckdb -unsigned   (or open DB with allow_unsigned_extensions=true)
LOAD './build/debug/extension/mentat/mentat.duckdb_extension';

-- transact some schema + data (returns JSON tx-report string)
SELECT mentat_transact('[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]');
SELECT mentat_transact('[{:person/name "Alice"} {:person/name "Bob"}]');

-- Datalog query as a table function
SELECT * FROM mentat_query('[:find ?e ?name :where [?e :person/name ?name]]', '{}');

-- join Datalog output against native DuckDB SQL
SELECT name, count(*) FROM mentat_query('[:find ?e ?name :where [?e :person/name ?name]]','{}') AS m(e, name)
GROUP BY name;
```

Behind the scenes, `mentat_*` open the mentat SQLite store (path from a bind
parameter or session setting; see §6), so the smoke test needs the store path
wired first — the exact DB-path surface is a design choice in §6.

**Cargo-only build (no footer, for compile-checking during dev):**
```bash
cargo build -p mentat_duckdb --features loadable-extension   # from repo root
```
This compiles the cdylib but the result is NOT loadable (no footer) — use it
only to check that the code compiles, not to run in DuckDB.

---

## 5. Research: cargo wiring

### 5.1 Workspace membership

Add `crates/duckdb` (crate name e.g. `mentat_duckdb`) as a workspace **member**
but **exclude it from `default-members`** — exactly the pattern already used for
`crates/pg/pg_mentat` (it is in `members` but not `default-members`, so a plain
`cargo build`/`cargo test` at the root skips the pgrx toolchain). Do the same so
the DuckDB toolchain (bundled DuckDB C++ compile) is never pulled by a plain
workspace build.

`Cargo.toml` (root) edits — **not to be applied by this task**, shown for the plan:
```toml
[workspace]
members = [
  # ...existing...
  "crates/pg/pg_mentat",
  "crates/pg/mentatd",
  "crates/duckdb",          # <-- add: member, but NOT in default-members
]
default-members = [
  # ...unchanged; crates/duckdb intentionally absent...
]
```

### 5.2 `crates/duckdb/Cargo.toml` sketch

```toml
[package]
name = "mentat_duckdb"
version.workspace = true
edition = "2021"            # workspace uses 2021; template used 2024 but 2021 is fine
rust-version.workspace = true
license.workspace = true
repository.workspace = true
authors.workspace = true
description = "Datomic-compatible Datalog engine exposed as a DuckDB extension"

[lib]
crate-type = ["cdylib"]     # loadable extension is a cdylib

[features]
default = []
# The DuckDB loadable-extension surface. Kept behind a feature so that even if
# the crate is ever built standalone without the template harness, the intent is
# explicit. In practice the Makefile always builds with this on.
extension = ["duckdb/loadable-extension", "duckdb/vscalar"]
# Optional mino scripting surface (mentat_eval), mirroring pg_mentat's `script`.
script = ["mentat/mino"]

[dependencies]
# Pin to the DuckDB v1.5.5 line. Tilde => crate patch updates, same DuckDB.
duckdb = { version = "~1.10505.0", features = ["loadable-extension", "vscalar"] }

# The embedded engine (option (a)). Path dep on the workspace mentat crate.
mentat        = { path = "../sqlite/mentat" }
edn           = { path = "../edn", features = ["serde_support"] }
core_traits   = { path = "../core-traits" }

serde_json = "1"            # tx-report / pull / inputs JSON

[lints]
workspace = true            # inherit the workspace clippy config
```

Notes:
- `loadable-extension` transitively enables `vtab` and `duckdb-loadable-macros`;
  add `vscalar` for the scalar functions.
- Do **not** enable `bundled` explicitly for a *client* build — but for an
  *extension* build the template links the correct DuckDB. With plain
  crates.io usage and no `bundled`, `libduckdb-sys` downloads the pinned
  prebuilt DuckDB (`download-lib` on by default) — acceptable, but for
  reproducible offline EC2 builds prefer letting the template drive it, or add
  `bundled` if a from-source build is wanted (uses `cc` + gcc; no clang/cmake).
- **Do not** enable `loadable-extension` in any consumer that also opens a
  normal DuckDB `Connection` — the README warns it replaces the C API funcs and
  such a client panics with *"DuckDB API not initialized"*. Our crate is
  extension-only, so this is fine.

### 5.3 Target DuckDB version

**Target DuckDB v1.5.5** (`duckdb`/`libduckdb-sys`/`duckdb-loadable-macros`
`~1.10505.0`). With `USE_UNSTABLE_C_API=1` the built `.duckdb_extension` loads
only into DuckDB **v1.5.5**. Pick this deliberately and document it; bumping
DuckDB later means bumping the crate pin and `TARGET_DUCKDB_VERSION` together and
rebuilding. (If a stable-C-API-only, forward-compatible build is ever needed,
that is a separate investigation — see §9.)

---

## 6. Design notes for the (future) implementation

- **Entrypoint** (`src/lib.rs`):
  ```rust
  #[duckdb_entrypoint_c_api]
  pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
      con.register_table_function::<MentatQueryVTab>("mentat_query")?;
      con.register_scalar_function::<MentatTransactScalar>("mentat_transact")?;
      con.register_scalar_function::<MentatPullScalar>("mentat_pull")?;
      #[cfg(feature = "script")]
      con.register_scalar_function::<MentatEvalScalar>("mentat_eval")?;
      Ok(())
  }
  ```
- **DB path surface (choose one; recommend the first for v1):**
  1. Extra leading parameter: `mentat_query(db_path, query, inputs)` and
     `mentat_transact(db_path, edn)`. Explicit, stateless, trivial. Lazy + correct.
  2. A session-scoped setting (DuckDB config/`SET`) read at call time. Nicer UX,
     more plumbing.
  `ponytail:` v1 uses an explicit `db_path` parameter; add a session default
  setting if the explicit arg becomes annoying in practice.
- **`bind` for `mentat_query`:** open store → `q_once` (or at least algebrize) to
  learn the `FindSpec`; `add_result_column(name, Varchar)` per column. Reuse
  `FindSpec::columns()` for names (as the SQLite CLI does).
- **`func`:** stream the materialized rows into `DataChunkHandle` in
  `STANDARD_VECTOR_SIZE`-sized batches; `set_len(0)` when exhausted.
- **Errors:** map `mentat::Result`/`MentatError` to
  `Box<dyn std::error::Error>`; DuckDB surfaces the message to the user. Match
  `pg_mentat`'s convention of a JSON tx-report string for `transact`.
- **Reuse, don't reinvent:** value→string rendering already exists in the SQLite
  CLI (`binding_as_string`/`value_as_string`, `repl.rs`). Lift/adapt that for the
  v1 all-VARCHAR projection instead of writing new formatting.

---

## 7. Function surface summary (mirror of pg_mentat)

| DuckDB call | Kind | Returns | mentat call |
|---|---|---|---|
| `mentat_query([db_path,] query, inputs)` | table fn | rows (VARCHAR cols v1) | `Store::q_once` |
| `mentat_transact([db_path,] edn)` | scalar | VARCHAR (JSON tx-report) | `Store::transact` |
| `mentat_pull([db_path,] entity, pattern)` | scalar | VARCHAR (JSON) | `Pullable::pull_*` |
| `mentat_eval([db_path,] script)` *(feature `script`)* | scalar | VARCHAR | `mentat::ScriptInterpreter` |

---

## 8. Milestones

1. **M0 – scaffold (no logic):** `crates/duckdb` as a non-default member;
   `extension-ci-tools` submodule + Makefile (`EXTENSION_NAME=mentat`,
   `TARGET_DUCKDB_VERSION=v1.5.5`, `USE_UNSTABLE_C_API=1`); a `hello`-style
   table fn that returns a constant row. Prove `make configure && make debug`
   yields a loadable `mentat.duckdb_extension` and `duckdb -unsigned; LOAD ...`
   works on this host.
2. **M1 – transact + query (v1 all-VARCHAR):** embed `mentat::Store`;
   `mentat_transact` (JSON report) and `mentat_query` table fn over
   `q_once` with all-VARCHAR columns; per-call store open. SQLLogicTests.
3. **M2 – pull + typed columns:** `mentat_pull`; per-column native
   `LogicalType`s (§2.4 v2); List/Struct for pull sub-shapes.
4. **M3 – ergonomics:** session default DB path; optional `mentat_eval`
   (`script` feature); thread-local store cache if warranted.

---

## 9. Risks / unverified

**Hard risks**

1. **Unstable C API version-lock (highest).** `USE_UNSTABLE_C_API=1` (required by
   duckdb-rs today) means the built `.duckdb_extension` loads **only** into the
   exact `TARGET_DUCKDB_VERSION` (v1.5.5). Every DuckDB upgrade requires a
   coordinated crate-pin + `TARGET_DUCKDB_VERSION` bump and rebuild. Document
   the pinned version prominently.
2. **Extension signing.** Local/dev use needs `duckdb -unsigned` or
   `allow_unsigned_extensions=true`. Distributing to stock DuckDB or the
   Community Extensions repo requires signing — deferred, out of v1 scope.
3. **Metadata footer is mandatory.** `cargo build` output is not loadable; the
   `extension-ci-tools` Makefile step that appends the footer + renames to
   `.duckdb_extension` is non-optional. Don't try to `LOAD` a raw `.so`.
4. **Reproducible/offline builds.** Without `bundled`, `libduckdb-sys` downloads
   a pinned prebuilt DuckDB at build time (needs network). For a hermetic EC2
   build, either pre-warm that download cache, use `bundled` (from-source via
   `cc`+gcc — slower but offline), or point `DUCKDB_LIB_DIR` at a prebuilt lib.
5. **`loadable-extension` poisons client use.** A crate built with
   `loadable-extension` cannot also be used as a normal DuckDB client (panics
   "API not initialized"). Keep `crates/duckdb` extension-only. (Not a problem
   for us; flagged so no one adds a `Connection::open` test to this crate.)

**Duplicate-sqlite risk: analyzed and NOT a blocker** — DuckDB core doesn't
export `sqlite3_*`, and the cdylib hides the statically-bundled sqlite symbols
(§3.3). Keep `mentat` on rusqlite `bundled` (current default).

**Verified against sources**
- Crate versions (`duckdb`/`libduckdb-sys`/`duckdb-loadable-macros` = `1.10505.0`;
  `duckdb-extension-framework` = `0.7.0` "experimental"): crates.io API.
- `loadable-extension`/`vtab`/`vscalar` feature graphs, MSRV `1.85.1`: crates.io
  version metadata for `duckdb 1.10505.0`.
- Version scheme `1.MAJOR_MINOR_PATCH.x` (v1.5.0→`1.10500.x`), `-unsigned`,
  footer requirement, "start from extension-template-rs", `loadable-extension`
  poisoning clients: duckdb-rs `main` `README.md`.
- `#[duckdb_entrypoint_c_api]`, `VTab` (`bind`/`init`/`func`/`parameters`),
  `VScalar` (`invoke`/`signatures`), `register_table_function`/
  `register_scalar_function`: duckdb-rs `hello-ext` example + `extension-template-rs`
  `src/lib.rs`.
- `USE_UNSTABLE_C_API=1`, `TARGET_DUCKDB_VERSION=v1.5.5`, `extension-ci-tools`
  submodule, `make configure/debug/test`: `extension-template-rs` `Makefile`,
  `.gitmodules`, `README.md`, `Cargo.toml` (`duckdb = "~1.10505.0"`,
  `crate-type=["cdylib"]`).
- `LogicalTypeId` variants: duckdb-rs `crates/duckdb/src/core/logical_type.rs`.
- mentat data model (`ValueType`/`TypedValue`/`Binding`/`QueryResults`/
  `Store`/`q_once`/`transact`, `FindSpec::columns()`), rusqlite `bundled`,
  pg_mentat function names, host toolchain: this repo + host inspection.

**Unverified / to confirm during M0**
- Exact `extension-ci-tools` Makefile target names and the produced artifact
  path (`build/debug/extension/mentat/mentat.duckdb_extension` is inferred from
  the template's `rusty_quack` layout; confirm when scaffolding).
- Whether the venv-installed `duckdb` on this host is exactly v1.5.5 (must match
  the extension target to `LOAD`); pin `DUCKDB_TEST_VERSION`/venv install
  accordingly during M0.
- Precise duckdb-rs API for streaming multiple output chunks and for
  reading multiple scalar input rows at v1.5.5 (the examples show the shapes;
  confirm method names against the pinned crate docs when coding).
- `edition = 2021` vs the template's `2024`: 2021 is expected to work with the
  crate (MSRV 1.85.1 / host 1.98) but confirm at M0.
```
