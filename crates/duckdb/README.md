# `mentat_duckdb` — mentat as a DuckDB loadable extension

A DuckDB loadable extension that stores mentat's datoms **in DuckDB** and runs
Datalog over them, mirroring the SQLite CLI and `pg_mentat`. Each mentat store
is a schema in the database the extension is loaded into (`mentat` for the
`'default'` store, `mentat_<name>_<hash>` for any other name), holding ordinary
DuckDB tables: `datoms`, `timelined_transactions` (+ the `transactions` view),
`idents`, `schema`, `known_parts`, `meta`. The data persists and checkpoints
with the DuckDB database, and plain SQL can read it.

The engine is mentat's own (transactor, algebrizer, projector, pull), reached
through a storage seam; `store/` (`mentat_duckdb_store`) holds the DuckDB
schema and the DuckDB SQL. Design: `docs/duckdb-native-storage-plan.md`.

Install from the registry: `INSTALL mentat FROM community; LOAD mentat;`.

## Pinned versions (IMPORTANT — version-lock caveat)

| Component | Version |
|---|---|
| `duckdb` / `libduckdb-sys` / `duckdb-loadable-macros` | `~1.10506.0` (= DuckDB **v1.5.6**) |
| `TARGET_DUCKDB_VERSION` | `v1.5.6` |
| `USE_UNSTABLE_C_API` | `1` (required by duckdb-rs today) |

Because duckdb-rs currently requires the **unstable C API**
(`USE_UNSTABLE_C_API=1`), the produced `.duckdb_extension` loads **only** into
DuckDB **v1.5.6**. Forward compatibility is not guaranteed. Bumping DuckDB means
bumping the crate pin **and** `TARGET_DUCKDB_VERSION` together, then rebuilding
(plan §9 risk 1).

This crate is **extension-only**. The `loadable-extension` feature replaces the
DuckDB C API functions; opening a normal DuckDB `Connection` from this crate
panics with "API not initialized". The extension's one connection is cloned
from the entrypoint's (the database handle DuckDB passes the entrypoint is
valid only during that call), and every mentat call runs on it, one at a time.

## Workspace wiring

`crates/duckdb` is a workspace **member** but **not** a `default-member` (exactly
like `crates/pg/pg_mentat`), so a plain `cargo build` / `cargo test` at the repo
root never pulls the DuckDB toolchain. Build it explicitly with
`-p mentat_duckdb` or via the `Makefile`.

## Build

Rust 1.90 (see `rust-toolchain.toml`). The `extension-ci-tools` submodule provides the
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

Notes:
- The crate is a workspace member, so the `Makefile` exports
  `CARGO_TARGET_DIR=crates/duckdb/target` so the ci-tools rust.Makefile finds the
  artifact where it expects (`./target/debug/`, or `./target/<triple>/` for the
  macOS cross builds).
- The cdylib is `libmentat_duckdb.{so,dylib}` / `mentat_duckdb.dll` (crate
  name), not `libmentat.*`; the `Makefile` sets `RUST_LIBNAME` per platform. The
  footer output is still `mentat.duckdb_extension`.
- `EXTENSION_VERSION` comes from the workspace version in the root
  `Cargo.toml` (so it works without `.git`); the registry's CI sets its own.

### Compile-check only (no loadable footer)

```bash
cargo build -p mentat_duckdb   # from repo root; produces a bare .so, NOT loadable
```

## Load & use

DuckDB refuses unsigned extensions unless started with `-unsigned` (or
`allow_unsigned_extensions=true`). Use a **DuckDB v1.5.6** CLI (the pinned
target). The standalone CLI:

```bash
curl -sfL -o duckdb_cli.zip \
  https://github.com/duckdb/duckdb/releases/download/v1.5.6/duckdb_cli-linux-amd64.zip
unzip duckdb_cli.zip
./duckdb --version   # v1.5.6 (Variegata)
```

(The `make configure` venv installs the latest `duckdb` PyPI wheel available for
the host Python; on Python 3.9 that caps at 1.4.5, which can NOT load a v1.5.6
extension — hence the standalone v1.5.6 CLI for load/smoke tests. On Python
3.10+ the venv wheel is 1.5.6 and `make test_debug` works.)

```sql
-- duckdb -unsigned my.duckdb
LOAD './build/debug/mentat.duckdb_extension';

SELECT edn_t('default',
  '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]');
SELECT edn_t('default', '[{:person/name "Alice"} {:person/name "Bob"}]');
SELECT * FROM edn_q('default', '[:find ?e ?name :where [?e :person/name ?name]]', '{}');
-- The datoms are rows in the `mentat` schema of my.duckdb.
SELECT count(*) FROM mentat.datoms;
```

## Function surface

| DuckDB call | Kind | Returns | engine call |
|---|---|---|---|
| `edn_t(store VARCHAR, edn VARCHAR)` | scalar, volatile | VARCHAR (JSON tx-report) | `DuckStore::transact` |
| `edn_q(store VARCHAR, query VARCHAR, options VARCHAR)` | table fn | rows, all VARCHAR columns | `DuckStore::q_json` |
| `edn_pull(store VARCHAR, pattern VARCHAR, entity BIGINT)` | scalar, volatile | VARCHAR (JSON map) | `(pull ?e pattern)` via `DuckStore::q` |
| `edn_eval(store VARCHAR, script VARCHAR)` | scalar, volatile | VARCHAR (EDN) | `mentat_duckdb_store::script` (feature `script`) |

Any NULL argument to a scalar yields NULL. `store` names a mentat store in the
current DuckDB database; it is created (schema, tables, bootstrap transaction)
on first use. `'default'` (or `''`) is the schema `mentat`. Any other name maps
to `mentat_<name>_<hash>`: letters, digits and `_` are kept, the rest become `_`,
and a hash of the full name keeps distinct names apart, so a path-like name
such as `'/tmp/demo.mentat'` (the pre-1.11 file argument) is just a name and
writes no file.

### Transactions and connections

Each `edn_t` runs in its own DuckDB transaction and commits it, so it is
atomic (a failed `:db.fn/cas` leaves nothing behind) but is not part of a
caller's `BEGIN … ROLLBACK`. Another DuckDB connection or session sees the
change once it commits. Each store's schema is cached per thread and
revalidated by one primary-key read of a generation that every
schema-changing transaction replaces, so a schema change made elsewhere is
picked up on the next call.

### Storage layout

The tables mirror the embedded SQLite store's, column for column, so the
engine's SQL resolves unchanged. A value `v` is a
`UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)` with SQLite's storage-class
mapping (refs, booleans, longs, instants -> `i`; doubles -> `d`; strings,
keywords -> `s`; uuids, bytes -> `b`), plus plain copies `v_i`, `v_d`, `v_s` that
the DuckDB query dialect uses for filters, joins and numeric comparisons
(DuckDB scans and joins a UNION much more slowly than plain columns). DuckDB
can't index a UNION and has no partial indexes, so value lookups scan the
attribute's rows.

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

All columns are VARCHAR. A string value is
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
as EDN text. A no-arg `(mentat.store/open)` opens the `store` argument;
`(mentat.store/open "other")` opens another store in the same database.
`(mentat.store/q ...)` on an as-of / since db value runs temporal Datalog.
`mentat.store/with` (speculative transact) is not supported on DuckDB stores
and returns an error. The interpreter is `sandboxed()` (no `slurp`,
`spit`, or other host filesystem prims) and bounded per call to 10M eval steps,
64 MiB heap, and depth 1000. Each call gets a fresh interpreter (no state
carries between calls). mino is pure Rust, so the feature adds no toolchain;
build with `--no-default-features` to leave `edn_eval` out.

```sql
SELECT edn_eval('default', '
  (def c (mentat.store/open))
  (mentat.store/transact c [{:person/name "Carol"}])
  (mentat.store/q (mentat.store/db c) (quote [:find ?n :where [_ :person/name ?n]]))');
```

## Running as a server (Quack)

DuckDB v1.5.6 ships the core `quack` extension: a DuckDB process serves SQL
over HTTP to other DuckDB clients. With mentat loaded in that server, every
client gets `edn_t`/`edn_q`/`edn_pull`/`edn_eval` without loading (or even
having) the mentat extension, and all of them share one long-lived process and
its DuckDB database, which holds the stores.

### Start and stop

```sh
export MENTAT_QUACK_TOKEN=$(openssl rand -hex 24)   # clients need this
DUCKDB=/path/to/duckdb-v1.5.6 crates/duckdb/server/serve.sh
# mentat quack server: quack:127.0.0.1:9494 pid 4242 log /run/user/1000/mentat-quack-9494.log
crates/duckdb/server/stop.sh
```

`serve.sh` binds `127.0.0.1:9494` (`MENTAT_QUACK_HOST`, `MENTAT_QUACK_PORT`),
writes a pidfile (`MENTAT_QUACK_PIDFILE`), runs the server in its own session
so it outlives the shell, and keeps the CLI's stdin open (the DuckDB CLI serves
only while stdin is open). The token reaches the server through `getenv()`, so
it is not in any process's argv. `MENTAT_QUACK_FOREGROUND=1` runs it in the
foreground for a supervisor; `server/mentat-quack.service` is an example
systemd unit. Everything is done by the DuckDB statements the script writes:

```sql
LOAD quack; LOAD '/path/to/mentat.duckdb_extension';
SELECT * FROM quack_serve('quack:127.0.0.1:9494',
  token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);
```

### Connect

`quack_query(uri, sql, token =>, disable_ssl =>)` runs `sql` on the server and
returns its rows. The client needs only `LOAD quack`, not mentat:

```sql
LOAD quack;
SELECT * FROM quack_query('quack:127.0.0.1:9494',
  $$SELECT edn_t('people', '[{:person/name "Alice"}]')$$,
  token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);
SELECT * FROM quack_query('quack:127.0.0.1:9494',
  $$SELECT * FROM edn_q('people', '[:find ?e ?n :where [?e :person/name ?n]]', '{}')$$,
  token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);
```

The result is an ordinary table, so it joins against the client's local tables.
Python works the same way (`duckdb.connect().execute("LOAD quack")`, then the
same `quack_query`). `ATTACH` also works against a live server and exposes its
tables (`ATTACH 'quack:127.0.0.1:9494' AS r (TOKEN '…', DISABLE_SSL true);
SELECT * FROM r.t; CREATE TABLE r.t2 AS …`), but NOT its functions:
`SELECT * FROM r.edn_q(…)` fails with "Table Function with name edn_q does not
exist". Call mentat through `quack_query`. A wrong token fails with
`Invalid Input Error: Authentication failed` (no token: `Could not find a
Quack authentication token`), for both `quack_query` and `ATTACH`.

### Security

- **The token is the only gate, and it grants everything.** A client with the
  token runs arbitrary SQL in the server process: any mentat store, and
  DuckDB's own file functions (`read_text('/etc/…')`, `COPY … TO`) as the
  server's OS user. Run the server as a dedicated user confined to the store
  directory (the systemd unit does). DuckDB's in-process lockdowns do not help
  here: with `enable_external_access = false`, `lock_configuration = true` or
  `autoload_known_extensions = false` set, every Quack request fails with
  HTTP 500 at v1.5.6. The server insists on a token of at least 4 characters;
  use 32+ random ones.
- **Bind address.** Quack accepts only localhost unless `allow_other_hostname`
  is set; `serve.sh` sets it automatically when `MENTAT_QUACK_HOST` is not
  loopback, and warns.
- **TLS.** The server speaks plain HTTP (`serve.sh` passes
  `disable_ssl => true`; v1.5.6 reports an `http://` listen URL either way).
  Clients use `http://` for `localhost`/`127.0.0.1` and `https://` for any
  other host unless given `disable_ssl => true` (`disable_ssl => false`
  forces `https://` even on localhost). So for anything beyond one host,
  terminate TLS in a reverse proxy in front of the server and connect to the
  proxy without `disable_ssl`. Plain HTTP across a network sends the token in
  clear text.
- **Unsigned extension.** The server starts DuckDB with `-unsigned` because
  mentat is not a signed community extension yet. That flag lets *the server*
  load any unsigned extension file, including one a token holder asks it to
  `LOAD`; treat that as part of "the token grants everything".
- **Listen backlog** is 5 (`ss -ltn`), hard-coded in Quack, and `somaxconn`
  can't raise it. From about 32 concurrent clients, or a single query returning
  100k+ rows (the client fetches large results over 30+ parallel connections),
  the queue overflows and clients retry after 1, 2 then 4 s: 1-5 s stalls. With
  the backlog raised to 4096 (an `LD_PRELOAD` shim around `listen()`, measured in
  `benchmarks/results/duckdb-quack-*`), throughput at 32 clients went up 22% and
  worst-case latency fell from 3-4.6 s to under 0.5 s. The real fix is upstream.

### Caveats

- **Per-call cost.** Each `quack_query` opens three new TCP connections (no
  keep-alive), about 2.2 ms per call on localhost, plus ~35-110 ns per result row.
  In-process DuckDB is always faster; the server's value is
  that clients don't need mentat loaded and can join results with their own tables.
- **`ATTACH 'quack:...'`** exposes the server's tables, but not its table
  functions: `r.edn_q(...)` fails with "Table Function with name edn_q does not
  exist". Use `quack_query`.
- **Calls are serialized.** The extension runs every mentat call on one
  connection, so concurrent clients' mentat calls take turns (their other SQL
  doesn't).

Quack is pre-2.0 at DuckDB v1.5.6 (`quack` build `c154811`): its wire protocol
and function signatures may change in any DuckDB release, and the mentat
extension is version-locked to v1.5.6 anyway (see "Pinned versions"). Server
and clients must run the same DuckDB version. The stores live in the server's
DuckDB database, so start the server on a database file (not `:memory:`) to
keep them.

## Tests

- `test/smoke.sh` — standalone DuckDB v1.5.6 CLI; asserts on every output line,
  that the datoms are DuckDB rows readable without the extension, that no file
  is written, and on the error paths. `DUCKDB=/path/to/duckdb bash test/smoke.sh`.
- `test/sql/mentat.test` — SQLLogicTest (the registry's runner). Needs a Python
  3.10+ venv with `duckdb==1.5.6` and `duckdb-sqllogictest-python`, e.g.
  `python -m duckdb_sqllogictest --test-dir test/sql --external-extension build/debug/mentat.duckdb_extension`.
- `store/tests-harness` — the storage backend outside the extension, on a
  bundled DuckDB (a separate workspace): a differential test running the same
  transactions and 60 queries (now, as-of and since every transaction) on the
  SQLite engine and on DuckDB, the shared scripting model suite, persistence and
  WAL-replay tests. `cargo test --manifest-path crates/duckdb/store/tests-harness/Cargo.toml`.

## Known gaps

- `:db/fulltext` values are stored as plain strings, not tokenized;
  `(fulltext ...)` queries aren't supported.
- Stores live in the database the extension was loaded into, not in another
  `ATTACH`ed database.
- Mentat calls run one at a time (one connection).
- Stores written by mentat 1.10.x (SQLite files) are not migrated.
