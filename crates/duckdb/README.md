# `mentat_duckdb` — mentat as a DuckDB loadable extension

Exposes the embedded `mentat` Datalog engine (the workspace SQLite store) as a
DuckDB loadable extension, mirroring the SQLite CLI and `pg_mentat`. DuckDB is
the query/exec surface; datoms live in mentat's own SQLite file (plan §3.1
option (a)). See `docs/duckdb-extension-plan.md`.

## Pinned versions (IMPORTANT — version-lock caveat)

| Component | Version |
|---|---|
| `duckdb` / `libduckdb-sys` / `duckdb-loadable-macros` | `~1.10505.0` (= DuckDB **v1.5.5**) |
| `TARGET_DUCKDB_VERSION` | `v1.5.5` |
| `USE_UNSTABLE_C_API` | `1` (required by duckdb-rs today) |

Because duckdb-rs currently requires the **unstable C API**
(`USE_UNSTABLE_C_API=1`), the produced `.duckdb_extension` loads **only** into
DuckDB **v1.5.5**. Forward compatibility is not guaranteed. Bumping DuckDB means
bumping the crate pin **and** `TARGET_DUCKDB_VERSION` together, then rebuilding
(plan §9 risk 1).

This crate is **extension-only**. The `loadable-extension` feature replaces the
DuckDB C API functions; opening a normal DuckDB `Connection` from this crate
panics with "API not initialized" (plan §9 risk 5). Do not add client
`Connection` use here.

## Workspace wiring

`crates/duckdb` is a workspace **member** but **not** a `default-member` (exactly
like `crates/pg/pg_mentat`), so a plain `cargo build` / `cargo test` at the repo
root never pulls the DuckDB toolchain. Build it explicitly with
`-p mentat_duckdb` or via the `Makefile`.

## Build (exact commands that worked, on the EC2 CI host)

Rust 1.90, Python 3.9, gcc 11.5. The `extension-ci-tools` submodule provides the
metadata-footer + test harness.

```bash
cd crates/duckdb
# one-time: clone the ci-tools helper (a git submodule upstream)
git submodule update --init          # or: git clone https://github.com/duckdb/extension-ci-tools

make configure    # creates configure/venv, pip-installs the sqllogictest runner,
                  # writes configure/platform.txt + configure/extension_version.txt
make debug        # cargo build (cdylib) -> append metadata footer
                  #   -> build/debug/mentat.duckdb_extension
# or: make release
```

Notes on this host:
- The crate is a workspace member, so the `Makefile` exports
  `CARGO_TARGET_DIR=crates/duckdb/target` so the ci-tools rust.Makefile finds the
  artifact where it expects (`./target/debug/`).
- The cdylib is `libmentat_duckdb.so` (crate name), not `libmentat.so`; the
  `Makefile` overrides `EXTENSION_LIB_FILENAME`/`RUST_LIBNAME` accordingly. The
  footer output is still `mentat.duckdb_extension`.
- EC2 `~/mentat` is not a git checkout (rsync excludes `.git`), so
  `EXTENSION_VERSION=v1.7.0` is pinned in the `Makefile` (auto git-describe
  fails there).

### Compile-check only (no loadable footer)

```bash
cargo build -p mentat_duckdb   # from repo root; produces a bare .so, NOT loadable
```

## Load & use

DuckDB refuses unsigned extensions unless started with `-unsigned` (or
`allow_unsigned_extensions=true`). Use a **DuckDB v1.5.5** CLI (the pinned
target). The standalone CLI:

```bash
curl -sfL -o duckdb_cli.zip \
  https://github.com/duckdb/duckdb/releases/download/v1.5.5/duckdb_cli-linux-amd64.zip
unzip duckdb_cli.zip
./duckdb --version   # v1.5.5 (Variegata)
```

(The `make configure` venv installs the latest `duckdb` PyPI wheel available for
the host Python; on Python 3.9 that caps at 1.4.5, which can NOT load a v1.5.5
extension — hence the standalone v1.5.5 CLI for load/smoke tests. On Python
3.10+ the venv wheel is 1.5.5 and `make test_debug` works.)

```sql
-- duckdb -unsigned
LOAD './build/debug/mentat.duckdb_extension';

SELECT * FROM mentat_hello();   -- M0 smoke: one constant row
```

## Function surface

| DuckDB call | Kind | Returns | mentat call |
|---|---|---|---|
| `mentat_hello()` | table fn | one constant VARCHAR row | (M0 smoke test) |
| `mentat_transact(db_path, edn)` | scalar | VARCHAR (JSON tx-report) | `Store::transact` |
| `mentat_query(db_path, query, inputs)` | table fn | rows, all VARCHAR columns | `Store::q_once` |

- `db_path` is an explicit first parameter (plan §6 option 1): stateless,
  per-call `Store::open` (plan §3.1). `""` opens an in-memory store (not useful
  across calls). Pass a filesystem path to persist.
- `inputs` is a JSON string; `{}` / `null` / `""` = no inputs (M1 only supports
  no-input queries; `:in` binding is a follow-up).
- Query result columns are all `VARCHAR`, values stringified exactly like the
  SQLite CLI (plan §2.4 v1). Typed columns are M2.

## Milestones

- **M0** — scaffold + loadable extension + `mentat_hello`. **Done.**
- **M1** — `mentat_transact` + `mentat_query` over the embedded store, all
  VARCHAR. **Done.**
- **M2** — `mentat_pull`, typed columns, List/Struct. Follow-up.
- **M3** — session default DB path, `mentat_eval` (`script` feature),
  thread-local store cache. Follow-up.
