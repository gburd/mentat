# `mentat_duckdb` — mentat as a DuckDB loadable extension

Exposes the embedded `mentat` Datalog engine (the workspace SQLite store) as a
DuckDB loadable extension, mirroring the SQLite CLI and `pg_mentat`. DuckDB is
the query/exec surface; datoms live in mentat's own SQLite file (plan §3.1
option (a)). See `docs/duckdb-extension-plan.md`.

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
parameter (plan §6 option 1). `""` opens an in-memory store (not useful across
calls); pass a path to persist.

### Store cache

`edn_t`, `edn_q` and `edn_pull` reuse an open store per `db_path` (canonical
path + inode), one per DuckDB worker thread, instead of opening the store on
every call. Before each call the cached store compares its last tx with the tx
high-water mark persisted in the file (one primary-key read); if another
connection or process has committed since, the store is reopened, so it never
sees a stale schema or hands out an entid that is already taken. Writes make
that check inside their `BEGIN IMMEDIATE`. A call that fails drops its store.
`MENTAT_STORE_CACHE=N` sets the stores kept per thread (default 16, least
recently used evicted); `0` opens per call. `edn_eval` is not cached. The
same code (`crates/sqlite/ext/src/store_cache.rs`) backs the SQLite extension.

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

## Running as a server (Quack)

DuckDB v1.5.6 ships the core `quack` extension: a DuckDB process serves SQL
over HTTP to other DuckDB clients. With mentat loaded in that server, every
client gets `edn_t`/`edn_q`/`edn_pull`/`edn_eval` without loading (or even
having) the mentat extension, and all of them share one long-lived process, so
the store cache (above) stays warm across clients and connections.

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
  $$SELECT edn_t('/srv/people.mentat', '[{:person/name "Alice"}]')$$,
  token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);
SELECT * FROM quack_query('quack:127.0.0.1:9494',
  $$SELECT * FROM edn_q('/srv/people.mentat', '[:find ?e ?n :where [?e :person/name ?n]]', '{}')$$,
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
  token runs arbitrary SQL in the server process: any mentat store path, and
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
  In-process DuckDB with the store cache is always faster; the server's value is
  that clients don't need mentat loaded and can join results with their own tables.
- **`ATTACH 'quack:...'`** exposes the server's tables, but not its table
  functions: `r.edn_q(...)` fails with "Table Function with name edn_q does not
  exist". Use `quack_query`.
- **One cached store per server thread.** The store cache is per thread, so a
  server can hold one connection per HTTP worker (up to ~128) per store, and each
  write makes all of them reopen.

Quack is pre-2.0 at DuckDB v1.5.6 (`quack` build `c154811`): its wire protocol
and function signatures may change in any DuckDB release, and the mentat
extension is version-locked to v1.5.6 anyway (see "Pinned versions"). Server
and clients must run the same DuckDB version. A server holds its stores open,
so stop it before moving or deleting store files.

## Tests

- `test/smoke.sh` — standalone DuckDB v1.5.6 CLI; asserts on every output line
  and on the error paths. `DUCKDB=/path/to/duckdb bash test/smoke.sh`.
- `test/sql/mentat.test` — SQLLogicTest, same coverage. Needs a Python 3.10+
  venv with `duckdb==1.5.6` and `duckdb-sqllogictest-python`, e.g.
  `python -m duckdb_sqllogictest --test-dir test/sql --external-extension build/debug/mentat.duckdb_extension`.

## Milestones

- **M0** — scaffold + loadable extension. **Done.**
- **M1** — transact + query over the embedded store, all VARCHAR. **Done.**
- **M2 (partial)** — `edn_pull`, `edn_q` inputs/asOf/since, raw string cells.
  **Done.** Typed columns, List/Struct: follow-up.
- **M3 (partial)** — `edn_eval` (`script` feature). **Done.** Session default DB
  path, thread-local store cache: follow-up.
