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

## Pinned versions

| Pin | Value | Where |
|---|---|---|
| `duckdb` / `libduckdb-sys` / `duckdb-loadable-macros` | `~1.10506.0` (= DuckDB **v1.5.6**; lockfile 1.10506.1) | `Cargo.toml` |
| C API | **stable** (`USE_UNSTABLE_C_API` unset), floor `TARGET_DUCKDB_VERSION=v1.5.6` | `Makefile` |
| Test venv | `DUCKDB_TEST_VERSION ?= 1.5.6` | `Makefile` |
| Tested DuckDB | v1.5.6 (CLI, Python) and 2.0 (CLI built at d54cbbe3, the registry's pin, reporting `v2.0.0`; Python dev wheel `2.0.0.dev2610192346`) | `test/cross-version.sh`, `scripts/duckdb-clis.sh`, CI `test-duckdb` |

The extension uses DuckDB's **stable** C API. Its footer says `C_STRUCT` with
`v1.5.6` as a C API *floor*: DuckDB loads it when the floor is at or below
its own version and hands it the v1 function-pointer struct. One release
binary loads into DuckDB v1.5.6 and v2.0. Until 1.17.0 the extension was
built with `USE_UNSTABLE_C_API=1`; that footer (`C_STRUCT_UNSTABLE`) loads
only into exactly v1.5.6, and DuckDB 2.0 refuses it ("built specifically for
DuckDB version 'v1.5.6'").

Why the stable API, and why now:

- **The registry builds against v2.0.** Since duckdb/community-extensions
  PR #3079 (2026-10-15) its `build.yml` builds every merged descriptor with
  DuckDB 2.0 and ci-tools `v2.0-cyanoptera`, and deploys it for 2.0 users. It
  first pinned commit b15358c3 (`v2.0.0-alpha46057`); since 2026-10-20 it
  pins d54cbbe3 and calls it `v2.0.0` (`DUCKDB_LATEST_STABLE`). A v1.5.6
  build is now made only on request (`repo.variegata`). Its test venv runs
  `pip install duckdb`, which gives 1.5.6 until 2.0 is on PyPI. So a
  v1.5.6-locked binary passes the registry's CI and then fails to load for
  2.0 users.
- **The v1 struct is append-only.** In 2.0 (both b15358c3 and d54cbbe3),
  `duckdb_ext_api_v1` is v1.5.6's 546 entries, with the same signatures and
  order, plus 2 appended (548). 2.0 routes `C_STRUCT_UNSTABLE` extensions to
  a separate v2 entrypoint, so an unstable build can't be made to load
  there.
  duckdb-rs 1.10506's struct is the same 546 (it generates it with the
  unstable entries enabled, but v1.5.6's header has none left inside the
  struct: everything is stable as of v1.5.6). A v1.5.6-floor binary is
  valid against 2.0's struct.
- **Nothing mentat uses is unstable-only.** `vscalar`, `vtab`, data chunks,
  `Connection::try_clone` and the SQL the pool runs through `duck_conn` are
  all in the stable struct. The switch is a Makefile change; `src/` did not
  change.
- **Kept tested.** `test/cross-version.sh` checks the footer is `C_STRUCT`,
  runs `test/smoke.sh` under the v1.5.6 and 2.0 (d54cbbe3) CLIs, and runs
  `test/sql/mentat.test` and `test/concurrency.py` under the duckdb 1.5.6
  and 2.0 dev wheels. CI's `test-duckdb` and release's `build-duckdb-ext`
  run it; `scripts/duckdb-clis.sh` fetches v1.5.6 and builds the 2.0 CLI
  from source at the registry's commit (no download exists for it). When
  the registry moves its pin, update `V20_COMMIT` there and the CI cache
  keys.

Consequences: a DuckDB patch or minor release no longer needs a mentat
release. What can still break it is DuckDB dropping or renumbering the v1 C
API (there is a `duckdb_extension_v2.h`); if 2.0 final does that, the 2.0
smoke test fails first, and the fix is to bump duckdb-rs to its 2.0 line.
To raise the floor, bump the duckdb-rs pin and `TARGET_DUCKDB_VERSION`
together: the floor must never be lower than the struct duckdb-rs was
generated from. Do not set `USE_UNSTABLE_C_API=1` without reading this
section again.

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
`allow_unsigned_extensions=true`). Use a DuckDB CLI at v1.5.6 (the C API
floor) or later; v2.0 works too. The v1.5.6 standalone CLI:

```bash
curl -sfL -o duckdb_cli.zip \
  https://github.com/duckdb/duckdb/releases/download/v1.5.6/duckdb_cli-linux-amd64.zip
unzip duckdb_cli.zip
./duckdb --version   # v1.5.6 (Variegata)
```

(The `make configure` venv installs `duckdb==1.5.6` (`DUCKDB_TEST_VERSION`),
which needs Python 3.10+; there `make test_debug` works. On Python 3.9 pip
cannot install it (that Python caps at 1.4.5, below the floor), so use a
standalone CLI and `test/smoke.sh`.)

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
| `mentat_import(store VARCHAR, path VARCHAR)` | scalar, volatile | VARCHAR (JSON: datoms, transactions, last_tx) | `DuckStore::import_sqlite` |

Any NULL argument to a scalar yields NULL. `store` names a mentat store in the
current DuckDB database; it is created (schema, tables, bootstrap transaction)
on first use. `'default'` (or `''`) is the schema `mentat`. Any other name maps
to `mentat_<name>_<hash>`: letters, digits and `_` are kept, the rest become `_`,
and a hash of the full name keeps distinct names apart, so a path-like name
such as `'/tmp/demo.mentat'` (the pre-1.11 file argument) is just a name and
writes no file.

### Importing a 1.10.x store (`mentat_import`)

Through 1.10.x this extension kept each store in a SQLite file named by the
store argument. Since 1.11.0, stores are DuckDB tables, so that file is no
longer read. `mentat_import(store, path)` copies such a file into a new
DuckDB store, row for row: all datoms and the whole transaction log
(history), with every entid, tx id, tx instant, ident, schema attribute and
partition high-water mark unchanged. `as-of`, `since` and history queries
answer as they did, and the next transaction gets the tx id and entids the
old store would have given it. It also reads stores written by the embedded
engine, the CLI and the SQLite extension (schema version 2 or later).

```sql
INSTALL sqlite; LOAD sqlite;   -- once: the file is read with DuckDB's sqlite extension
SELECT mentat_import('people', '/srv/people.mentat');
-- {"datoms":172,"last_tx":268435461,"store":"people","transactions":6}
SELECT * FROM edn_q('people', '[:find ?n :where [_ :person/name ?n]]', '{}');
```

The target store must be new (or hold only its bootstrap transaction). The
import is one DuckDB transaction: on any error the store is left as it was.
The source file is attached read-only and detached afterwards. mentat writes
its SQLite files in WAL mode, and SQLite creates a `-shm` file beside one even
to read it, so the file's directory must be writable; if it isn't, the call
says so: copy the file somewhere writable and import the copy. Doubles are
copied bit for bit, except that `-0.0` comes back as `0.0`. Fulltext values
become plain strings, which is how DuckDB stores hold them. A version-1 file
(mentat before 1.10) is refused with a message: open it once with the 1.10+
CLI or SQLite extension, which upgrades it, then import it.

This extension links no SQLite, so DuckDB's `sqlite` extension must be
loadable. With DuckDB's default settings it is installed and loaded on first
use. If it can't be (autoloading is off, or there's no network), the call fails
with a message saying to run `INSTALL sqlite; LOAD sqlite;`. Offline, copy the
extension file into the extension directory first.

If you `ATTACH` a mentat SQLite file yourself (to look at it before
importing, say), detach it when you're done: while it is attached, DuckDB's
own `duckdb_indexes()` and `information_schema.tables` fail with `Parser
Error: syntax error at or near "0"`. The sqlite extension hands DuckDB
SQLite's partial-index definitions (`WHERE index_avet IS NOT 0`), which
DuckDB can't parse. mentat's own calls don't use those catalog functions,
so they keep working; other tools and queries that read them won't.

### Transactions and connections

Each `edn_t` runs in its own DuckDB transaction and commits it, so it is
atomic (a failed `:db.fn/cas` leaves nothing behind) but is not part of a
caller's `BEGIN … ROLLBACK`. Another DuckDB connection or session sees the
change once it commits. Each store's schema is cached per thread and
revalidated by one primary-key read of a generation that every
schema-changing transaction replaces, so a schema change made elsewhere is
picked up on the next call.

The extension opens a pool of connections to the database when it is loaded
(`MENTAT_DUCKDB_POOL`, read at `LOAD`; default one per core, at most 8). Each
call borrows one, so that many mentat calls from concurrent clients run in
parallel; past that, calls wait for a free connection. Writes to one store take
turns: `edn_t` holds a per-store lock for its whole transaction, because two
DuckDB transactions that both update the store's partition counters conflict.
Reads never wait for it, and writes to different stores run in parallel.

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

The tables have no secondary indexes: lookups are zone-map-pruned scans.
1.11.0-1.14.0 created ART indexes on `datoms (e, a)`, `datoms (a)` and
`timelined_transactions (tx)`. DuckDB 1.5.6 never used them (lookups timed the
same with and without them at 12M rows), they made inserts 8.5x slower, and
[duckdb/duckdb#26828](https://github.com/duckdb/duckdb/issues/26828) corrupts
them. After a session that never checkpointed, the first plain open replays and
checkpoints the WAL; from then on every `[?e :attr _]` returns nothing, and
DELETEs fail FATAL. Since 1.15.0, opening a store drops those indexes, which
also repairs a store that was already hit.

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
Map specs follow a ref and nest the target's pull, with its `":db/id"`:
`[:person/name {:person/friend [:person/name]}]` gives
`{":person/friend": [{":db/id": 2, ":person/name": "Bob"}]}` (an array for a
cardinality-many or reverse attribute, one object for cardinality-one).
Reverse map specs (`{:person/_friend [...]}`) and nesting work too; pg_mentat's
recursion limits (`{:person/friend 2}`, `...`), `:limit` and `:default` do not.

A find-pull in an `edn_q` with `{"asOf": T}` / `{"since": T}` reads that basis,
and so does `mentat.store/pull` on an as-of / since db value. (Before 1.18.0
both returned current values.)

### `edn_eval` (feature `script`, default ON)

Runs a mino script with the `mentat.store/*` prims and returns the last value
as EDN text. A no-arg `(mentat.store/open)` opens the `store` argument;
`(mentat.store/open "other")` opens another store in the same database.
`(mentat.store/q ...)` on an as-of / since db value runs temporal Datalog.
`mentat.store/with` (speculative transact) applies the tx-data inside a DuckDB
transaction on the call's connection and rolls it back, so no other connection
sees it and nothing is left behind, on success or error. It takes the store's
write lock while it runs, as `edn_t` does. The interpreter is `sandboxed()` (no `slurp`,
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
- **Listen backlog** is 5 (`ss -ltn`): the httplib vendored by DuckDB 1.5.x
  hard-codes `CPPHTTPLIB_LISTEN_BACKLOG 5`, and `somaxconn` can't raise it. From about 32 concurrent clients, or a single query returning
  100k+ rows (the client fetches large results over 30+ parallel connections),
  the queue overflows and clients retry after 1, 2 then 4 s: 1-5 s stalls. With
  the backlog raised to 4096 (an `LD_PRELOAD` shim around `listen()`, measured in
  `benchmarks/results/duckdb-quack-*`), throughput at 32 clients went up 22% and
  worst-case latency fell from 3-4.6 s to under 0.5 s. The fix is upstream:
  [duckdb/duckdb-quack#318](https://github.com/duckdb/duckdb-quack/issues/318)
  (DuckDB main's httplib defaults to 128).

### Caveats

- **Per-call cost.** Each `quack_query` opens three new TCP connections (no
  keep-alive), about 2.2 ms per call on localhost, plus ~35-110 ns per result row.
  In-process DuckDB is always faster; the server's value is
  that clients don't need mentat loaded and can join results with their own tables.
- **`ATTACH 'quack:...'`** exposes the server's tables, but not its table
  functions: `r.edn_q(...)` fails with "Table Function with name edn_q does not
  exist". Use `quack_query`.
- **Concurrency.** Up to `MENTAT_DUCKDB_POOL` mentat calls run at once (see
  "Transactions and connections"); `edn_t` calls on the same store take turns.

Quack is pre-2.0 at DuckDB v1.5.6 (`quack` build `c154811`): its wire protocol
and function signatures may change in any DuckDB release. The mentat
extension itself loads into v1.5.6 and later (see "Pinned versions"), but
server and clients must run the same DuckDB version. The stores live in the server's
DuckDB database, so start the server on a database file (not `:memory:`) to
keep them.

## Tests

- `test/smoke.sh` — a standalone DuckDB CLI (v1.5.6 or later); asserts on every
  output line, that the datoms are DuckDB rows readable without the extension,
  that no file is written, and on the error paths.
  `DUCKDB=/path/to/duckdb [EXT=...] bash test/smoke.sh`.
- `test/sql/mentat.test` — SQLLogicTest (the registry's runner). Needs a Python
  3.10+ venv with `duckdb>=1.5.6` and `duckdb-sqllogictest-python` (`make
  configure` makes one), e.g. `make test_debug`, or
  `python -m duckdb_sqllogictest --test-dir test/sql --external-extension build/debug/mentat.duckdb_extension`.
- `test/cross-version.sh` — the footer check plus all of the above (and
  `test/concurrency.py`) under DuckDB v1.5.6 and v2.0, on one binary:
  `scripts/duckdb-clis.sh ~/duckdb-cli`, then
  `EXT=build/release/mentat.duckdb_extension CLIS=~/duckdb-cli bash test/cross-version.sh`.
- `store/tests-harness` — the storage backend outside the extension, on a
  bundled DuckDB (a separate workspace): a differential test running the same
  transactions and 60 queries (now, as-of and since every transaction) on the
  SQLite engine and on DuckDB, the shared scripting model suite, persistence and
  WAL-replay tests, and `mentat_import` of a SQLite store (needs DuckDB's
  `sqlite` extension, autoinstalled from extensions.duckdb.org on first run;
  where it can't load, the import tests skip with a message, or fail with
  `MENTAT_REQUIRE_SQLITE_EXT=1`, which CI sets).
  `cargo test --manifest-path crates/duckdb/store/tests-harness/Cargo.toml`.
- `test/import-1.10.py` — end to end: the real 1.10.x extension (under a
  v1.5.6 CLI, in its own process) writes a store, this build imports it, and
  both answer the differential queries now, as of and since every transaction.
  `python3 test/import-1.10.py <1.10.x mentat.duckdb_extension> build/release/mentat.duckdb_extension <v1.5.6 duckdb CLI>`.

## Known gaps

- `:db/fulltext` values are stored as plain strings, not tokenized;
  `(fulltext ...)` queries aren't supported.
- Stores live in the database the extension was loaded into, not in another
  `ATTACH`ed database.
- Mentat calls run one at a time (one connection).
