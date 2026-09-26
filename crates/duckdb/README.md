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

SELECT edn_t('/tmp/demo.mentat',
  '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]');
SELECT edn_t('/tmp/demo.mentat', '[{:person/name "Alice"} {:person/name "Bob"}]');
SELECT * FROM edn_q('/tmp/demo.mentat', '[:find ?e ?name :where [?e :person/name ?name]]', '{}');
```

## Function surface

| DuckDB call | Kind | Returns | mentat call |
|---|---|---|---|
| `edn_t(db_path VARCHAR, edn VARCHAR)` | scalar, volatile | VARCHAR (JSON tx-report) | `Store::transact` |
| `edn_q(db_path VARCHAR, query VARCHAR, options VARCHAR)` | table fn | rows, all VARCHAR columns | `Store::q_once` / `q_once_as_of` / `q_once_since` |
| `edn_pull(db_path VARCHAR, pattern VARCHAR, entity BIGINT)` | scalar, volatile | VARCHAR (JSON map) | `(pull ?e pattern)` via `Store::q_once` |
| `edn_eval(db_path VARCHAR, script VARCHAR)` | scalar, volatile | VARCHAR (EDN) | `mentat::script::Interpreter` (feature `script`) |

Any NULL argument to a scalar yields NULL. `db_path` is an explicit first
parameter (plan §6 option 1): stateless, per-call `Store::open` (plan §3.1).
`""` opens an in-memory store (not useful across calls); pass a path to persist.

### `edn_q` options (JSON, same shape pg_mentat accepts)

`''`, `NULL`, `{}` or JSON `null` = no options. Otherwise a JSON object with:

| Key | Meaning |
|---|---|
| `"inputs": [v1, v2, ...]` | Positional, one element per `:in` binding form (`$` source vars are not counted). The count must match. |
| `"asOf": T` | Query the database as of tx `T` (inclusive). |
| `"since": T` | Only datoms transacted after tx `T` (pair with a history pattern `[?e ?a ?v ?tx ?added]`). |

`asOf` and `since` are mutually exclusive; either may be combined with
`inputs`. Unknown keys and malformed JSON are errors.

Binding forms: scalar `?x` -> a JSON value; collection `[?x ...]` -> a JSON
array; tuple `[?a ?b]` -> a JSON array (one row); relation `[[?a ?b]]` -> an
array of arrays. `_` placeholders in a tuple/relation consume a value that is
discarded. A query may bind **either** any number of scalars **or** exactly one
collection/tuple/relation (mentat's `QueryInputs` has no public way to combine
them yet).

JSON -> value (mirrors pg_mentat's `bind_input_value`):

| JSON | mentat value |
|---|---|
| integer | `Long`, or `Ref` if the variable is used as an entity/tx, or as the value of a `:db.type/ref` attribute, in the `:where` patterns (incl. `or`/`not`) |
| float | `Double` |
| `true` / `false` | `Boolean` |
| string starting with `:` | `Keyword` (`":person/name"`) |
| any other string | `String` |

### Output rendering (`edn_q`)

All columns are VARCHAR (typed columns are a follow-up). A string value is
returned **raw** (`Alice`, not `"Alice"`), so it joins against native DuckDB
VARCHAR columns. Keywords keep their colon (`:person/name`); refs and longs are
decimal; instants RFC 3339; uuids hyphenated; booleans `true`/`false`. Nested
values (pull maps, tuples) render as EDN, where strings are quoted.

### `edn_pull`

`pattern` is a Datomic pull pattern (EDN vector): `[*]`,
`[:person/name :person/age]`, `[:person/_friend]`, `[:db/id :person/name]`. It
runs mentat's own pull (`(pull ?e pattern)`). The JSON matches pg_mentat's
`edn_pull`: keys are attribute idents with the colon (`":person/name"`),
`":db/id"` is always the entity id, cardinality-many values are arrays, refs are
`{":db/id": n}`, keywords `":ns/name"`, instants epoch microseconds, bytes hex.
Nested map specs (`{:person/friend [:person/name]}`) are not supported by
mentat's pull grammar yet.

### `edn_eval` (feature `script`, default ON)

Runs a mino script with the `mentat.store/*` prims and returns the last value
as EDN text. A no-arg `(mentat.store/open)` opens `db_path` (via
`mentat::script::Interpreter::with_default_path`); `(mentat.store/open "other")`
still opens an explicit path. The interpreter is `sandboxed()` (no `slurp`,
`spit`, or other host filesystem prims) and bounded per call to 10M eval steps,
64 MiB heap, and depth 1000. Each call gets a fresh interpreter (no state
carries between calls). mino is pure Rust, so the feature adds no toolchain;
build with `--no-default-features` to leave `edn_eval` out.

```sql
SELECT edn_eval('/tmp/demo.mentat', '
  (def c (mentat.store/open))
  (mentat.store/transact c [{:person/name "Carol"}])
  (mentat.store/q (mentat.store/db c) (quote [:find ?n :where [_ :person/name ?n]]))');
```

## Tests

- `test/smoke.sh` — standalone DuckDB v1.5.5 CLI; asserts on every output line
  and on the error paths. `DUCKDB=/path/to/duckdb bash test/smoke.sh`.
- `test/sql/mentat.test` — SQLLogicTest, same coverage. Needs a Python 3.10+
  venv with `duckdb==1.5.5` and `duckdb-sqllogictest-python`, e.g.
  `python -m duckdb_sqllogictest --test-dir test/sql --external-extension build/debug/mentat.duckdb_extension`.

## Milestones

- **M0** — scaffold + loadable extension. **Done.**
- **M1** — transact + query over the embedded store, all VARCHAR. **Done.**
- **M2 (partial)** — `edn_pull`, `edn_q` inputs/asOf/since, raw string cells.
  **Done.** Typed columns, List/Struct: follow-up.
- **M3 (partial)** — `edn_eval` (`script` feature). **Done.** Session default DB
  path, thread-local store cache: follow-up.
