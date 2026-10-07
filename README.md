# Mentat

Mentat is a Datomic-like database: an entity-attribute-value store with an
immutable transaction log, a Datalog query language, and a declarative pull
API, all expressed in [EDN](https://github.com/edn-format/edn). One repository
builds three backends that share the same Datalog/EDN front-end:

- **Embedded (`mentat`)** — a Rust library store on SQLite. `cargo build`,
  no external services. Drop it into an application the way you would SQLite.
  Also a **SQLite loadable extension** (`mentat_sqlite_ext`) so any SQLite host
  can call mentat from SQL.
- **PostgreSQL extension (`pg_mentat`)** — the same data model implemented
  inside PostgreSQL via [pgrx](https://github.com/pgcentralfoundation/pgrx),
  reached through SQL functions. Plus **`mentatd`**, an HTTP/WebSocket server
  that fronts a `pg_mentat` database.
- **DuckDB extension (`mentat_duckdb`)** — loads into DuckDB (`LOAD mentat;`)
  and stores its datoms in DuckDB tables of the database it is loaded into,
  so Datalog results join against native DuckDB tables and plain SQL can read
  the datoms.

All three expose the same four SQL functions:

| Function | Does | PostgreSQL | SQLite ext | DuckDB |
|---|---|---|---|---|
| `edn_t` | transact EDN, return a JSON tx-report | `edn_t(edn)` | `edn_t(db_path, edn)` | `edn_t(store, edn)` |
| `edn_q` | run a Datalog query | `edn_q(query, inputs jsonb)` → JSONB | `edn_q(db_path, query, inputs)` → JSON text | `edn_q(store, query, inputs)` → rows (table function) |
| `edn_pull` | pull a pattern for one entity, as JSON | `edn_pull(pattern, entity)` | `edn_pull(db_path, pattern, entity)` | `edn_pull(store, pattern, entity)` |
| `edn_eval` | run a sandboxed mino script | `edn_eval(script)` (`script` feature) | `edn_eval(db_path, script)` | `edn_eval(store, script)` |

`inputs` is the same JSON everywhere: `{"inputs": [...]}` binds the query's `:in`
forms positionally (scalars, `[?x ...]` collections, `[?a ?b]` tuples, `[[?a ?b]]`
relations, in any mix), `{"asOf": tx}` / `{"since": tx}` query the database as of
or since a transaction, and `{}` means none. Where the data lives differs:
PostgreSQL keeps it in the database's own tables; the SQLite extension takes a
mentat store file path first (a separate file from whatever database the host
has open); the DuckDB extension takes a store name first and keeps that store in
the DuckDB database it is loaded into.

All backends parse queries, transactions, and schema with one copy of the
`edn`, `core-traits`, and `core` crates, so a query means the same thing on
every backend. They descend from Mozilla's
[Project Mentat](https://github.com/mozilla/mentat); the PostgreSQL backend was
formerly the separate `pg_mentat` project, now merged here.

Datomic is a trademark of its owner; this project is an independent
reimplementation of the model, not affiliated with or endorsed by Datomic.

## New to EDN or Datalog?

Mentat speaks EDN (Clojure's data notation) and answers questions in Datalog,
the query language Datomic made popular. If you know nothing about EDN or
Datalog, these are good starting points:

- [Learn Datalog Today](https://github.com/jonase/learndatalogtoday) — an
  interactive, exercise-driven Datalog tutorial.
- [An introduction to Datalog](https://blogit.michelin.io/an-introduction-to-datalog/)
  — a short, readable overview from Michelin's engineering blog.
- [Datalog: Biting the Silver Bullet](https://www.youtube.com/watch?v=dQWcD2_FzAU)
  — Norbert Wójtowicz at GeeCON 2018.

## Documentation

The full manual (architecture, Datalog reference, pull API, time travel, the
PostgreSQL cookbook, operations, and the scripting layer) is an mdBook under
[`docs/`](docs/). Build it with `mdbook build docs` and open
`docs/book/index.html`, or read the sources in [`docs/src/`](docs/src/).

- [Architecture](docs/src/architecture.md) — the front-end/backend split, the
  crate map, and which features live on which backend.
- [Scripting](docs/src/scripting.md) — the mino `mentat.store/*` surface and
  `edn_eval`, with the full security model.

---

## Embedded store (`mentat`, on SQLite)

Build and test the embedded side — a plain `cargo build` needs no `pg_config`,
libclang, or PostgreSQL:

```bash
cargo build            # builds the SQLite side (workspace default members)
cargo test --workspace # runs the embedded test suite
```

Add it to a Rust project:

```toml
[dependencies]
mentat = { git = "https://codeberg.org/gregburd/mentat" }
```

Open a store, define an attribute, transact a fact, and query it:

```rust
use mentat::{Store, QueryResults, Queryable};

fn main() -> mentat::Result<()> {
    // A file path, or "" for an in-memory store.
    let mut store = Store::open("example.db")?;

    // Define schema, then assert a fact. `transact` takes EDN text.
    store.transact(r#"[
        {:db/ident       :person/name
         :db/valueType   :db.type/string
         :db/cardinality :db.cardinality/one}
    ]"#)?;
    store.transact(r#"[{:person/name "Alice"}]"#)?;

    // Query. `q_once` runs a query with optional inputs and returns a QueryOutput.
    let results = store
        .q_once(r#"[:find ?name :where [?e :person/name ?name]]"#, None)?
        .into();
    if let QueryResults::Rel(rows) = results {
        for row in rows.into_iter() {
            println!("{:?}", row); // [Alice]
        }
    }
    Ok(())
}
```

The embedded side also ships a C ABI (`crates/sqlite/ffi`) for non-Rust hosts
and a CLI (`crates/sqlite/cli`).

---

## PostgreSQL extension (`pg_mentat`)

`pg_mentat` needs a PostgreSQL install with development headers plus LLVM/clang
(pgrx uses bindgen). It supports PostgreSQL 13–18; `pg16` is the default
feature.

### Build and install

With `cargo pgrx` (0.17):

```bash
cargo install --locked cargo-pgrx --version 0.17.0
cargo pgrx init --pg16 $(which pg_config)     # one-time, points pgrx at your PG

cd crates/pg/pg_mentat
cargo pgrx install --release --no-default-features --features pg16
```

Or with Nix (no pgrx toolchain to set up by hand):

```bash
nix build --option sandbox relaxed .#pg_mentat-pg16   # or .#pg_mentat-pg14 … .#pg_mentat-pg18
# from another flake: github:gburd/mentat/v1.11.0#pg_mentat-pg18
```

The build fetches crates and `cargo-pgrx` from the network, so the derivation
sets `__noChroot` and needs `--option sandbox relaxed` (or `false`); a strictly
sandboxed build fails with "has '__noChroot' set, but that's not allowed".
nixpkgs no longer ships PostgreSQL 13, so the flake has no `pg13` output (CI
still builds and tests 13 through pgrx). The derivation is named
`pg_mentat-pg<N>-<version>`, with the version read from
`crates/pg/pg_mentat/pg_mentat.control`.

### Use it

```sql
CREATE EXTENSION pg_mentat;

-- Define a schema attribute. edn_t takes EDN transaction text.
SELECT edn_t('[
  {:db/ident       :person/name
   :db/valueType   :db.type/string
   :db/cardinality :db.cardinality/one}
]');

-- Assert a fact.
SELECT edn_t('[{:person/name "Alice"}]');

-- Query. edn_q takes an EDN query and a JSONB inputs map; returns JSONB.
SELECT edn_q(
  '[:find ?name :where [?e :person/name ?name]]',
  '{}'::jsonb
);

-- Bind :in inputs, or query the database as of an earlier transaction.
SELECT edn_q('[:find ?e :in ?name :where [?e :person/name ?name]]',
             '{"inputs": ["Alice"]}');
SELECT edn_q('[:find ?name :where [?e :person/name ?name]]', '{"asOf": 268435457}');

-- Pull all attributes for an entity (entity id 10001 here).
SELECT edn_pull('[*]', 10001);
```

`mentat.q`, `mentat.t`, and `mentat.pull` are shorter aliases for `edn_q`,
`edn_t`, and `edn_pull`. The pre-1.9.0 names `mentat_transact`, `mentat_query`,
`mentat_pull`, and `mentat_eval` still work as deprecated aliases and will be
removed in a future major release; `ALTER EXTENSION pg_mentat UPDATE` adds the new
names to an existing 1.8.0 install. As of 1.8.0 the embedded
SQLite backend also does historical (`as-of`/`since`) Datalog queries, `?added`
history patterns, and collection/tuple/relation `:in` bindings. The PostgreSQL
backend adds features unique to it: LISTEN/NOTIFY reactive subscriptions and
integrations with `pgvector`, `pg_trgm`, PostGIS, `rum`, and more. See
[Architecture](docs/src/architecture.md) for the full feature-by-backend table.

### mentatd

`crates/pg/mentatd` is an HTTP/WebSocket server that talks to a `pg_mentat`
database over `tokio-postgres` (`cargo build -p mentatd`; no PostgreSQL headers
needed). See [the mentatd chapter](docs/src/mentatd.md).

---

## DuckDB extension (`mentat_duckdb`)

`mentat_duckdb` (`crates/duckdb`) is a loadable DuckDB extension that stores
mentat's datoms **in DuckDB**: each mentat store is a schema in the database the
extension is loaded into, holding ordinary DuckDB tables (`datoms`,
`timelined_transactions` and its `transactions` view, `idents`, `schema`,
`known_parts`). The data persists, checkpoints and backs up with the DuckDB
database, and plain SQL can read it. Datalog runs on mentat's engine (the same
transactor, query planner and pull as the embedded store), compiled to DuckDB
SQL, so the results join against native DuckDB tables. Install it from the
[DuckDB Community Extensions](https://duckdb.org/community_extensions/)
registry (`INSTALL mentat FROM community; LOAD mentat;`) or build it yourself.
It is built with [duckdb-rs](https://github.com/duckdb/duckdb-rs) and pinned to
**DuckDB v1.5.6** (via the DuckDB unstable C API); the extension loads only into
that DuckDB version, and bumping DuckDB means bumping the pin and rebuilding.

### Build

The extension is a `cdylib` that needs a metadata footer, so it is built with
the DuckDB [`extension-ci-tools`](https://github.com/duckdb/extension-ci-tools)
Makefiles (a git submodule under `crates/duckdb/`):

```bash
git submodule update --init crates/duckdb/extension-ci-tools
cd crates/duckdb
make configure          # one-time: sets up the build platform + test venv
make debug              # -> build/debug/mentat.duckdb_extension
# make release for an optimized build
```

(`mentat_duckdb` is a workspace member but not a default member, so a plain
`cargo build` never pulls the DuckDB toolchain.)

### Use it

From the registry, `INSTALL mentat FROM community; LOAD mentat;`. A local build
is unsigned, so start DuckDB with `-unsigned` (or open it with
`allow_unsigned_extensions=true`). With a DuckDB v1.5.6 CLI:

```sql
-- duckdb -unsigned my.duckdb
LOAD './build/debug/mentat.duckdb_extension';

-- The first argument names a mentat store: 'default' is the DuckDB schema
-- `mentat`; any other name gets its own schema (mentat_<name>_<hash>).
SELECT edn_t('default', '[
  {:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
  {:db/ident :person/age  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}
]');
SELECT edn_t('default', '[{:person/name "Alice" :person/age 30} {:person/name "Bob" :person/age 25}]');

-- edn_q is a table function: run Datalog and get rows back.
SELECT * FROM edn_q('default',
  '[:find ?e ?name :where [?e :person/name ?name]]', '{}');

-- ...so it joins against native DuckDB tables.
CREATE TABLE teams(name VARCHAR, team VARCHAR);
INSERT INTO teams VALUES ('Alice', 'core'), ('Bob', 'web');
SELECT m.name, t.team
FROM edn_q('default',
       '[:find ?e ?name :where [?e :person/name ?name]]', '{}') AS m(e, name)
JOIN teams t ON t.name = m.name;

-- :in inputs and time travel use the same options JSON as PostgreSQL.
SELECT * FROM edn_q('default',
  '[:find ?name . :in ?age :where [?e :person/age ?age] [?e :person/name ?name]]',
  '{"inputs": [25]}');
SELECT * FROM edn_q('default',
  '[:find ?name :where [?e :person/name ?name]]', '{"asOf": 268435458}');

-- Pull an entity as JSON, or run a sandboxed mino script against the store.
SET VARIABLE alice = (SELECT e FROM edn_q('default',
  '[:find ?e . :where [?e :person/name "Alice"]]', '{}') AS t(e));
SELECT edn_pull('default', '[*]', getvariable('alice')::BIGINT);
SELECT edn_eval('default',
  '(mentat.store/q (mentat.store/db (mentat.store/open))
                   (quote [:find (count ?e) . :where [?e :person/name]]))');

-- The datoms are DuckDB rows: read them with plain SQL, no extension needed.
SELECT d.e, i.v AS attribute, d.v AS value
FROM mentat.datoms d JOIN mentat.idents i ON i.e = d.a
WHERE d.a > 65535;
```

`edn_t` returns a JSON tx-report, and commits its own DuckDB transaction (it is
not part of a caller's `BEGIN … ROLLBACK`). `edn_q` returns rows with every
column as `VARCHAR` (cast for arithmetic, e.g. `e::BIGINT`); strings come back as
plain text, keywords keep their leading colon. `edn_pull` returns JSON keyed by
attribute (`":person/name"`, plus `":db/id"`). `edn_eval` runs in the same sandbox
as PostgreSQL's (no filesystem access, step/heap/depth limits) and is on by
default (`--no-default-features` drops it).

Compared with the embedded SQLite store, DuckDB storage is faster for queries
that touch many datoms (at 1M datoms: aggregates 5.5x, as-of 3.5x, ref
traversal 2.2x, bulk load 2.8x) and slower for point lookups and single-datom
transactions; see
[`benchmarks/results/duckdb-native-*/findings.md`](benchmarks/results/). The
design is in
[`docs/duckdb-native-storage-plan.md`](docs/duckdb-native-storage-plan.md), and
publishing to the DuckDB Community Extensions registry is documented in
[`docs/registry-publishing.md`](docs/registry-publishing.md). Before 1.11 this
extension kept its datoms in a separate SQLite file; those files are not
migrated automatically.

---

## SQLite loadable extension (`mentat_sqlite_ext`)

`crates/sqlite/ext` builds `libmentat_sqlite.so`, a SQLite loadable extension
with the same four functions. Any SQLite host can load it: the `sqlite3` CLI,
Python's `sqlite3`, or an application that calls `sqlite3_load_extension`. It
embeds its own copy of the mentat engine (with its own SQLite) and reaches the
host only through the host's API table, so it never shares a file handle with
the host. For that reason `db_path` must not be the host's own database file.

```bash
cargo build --release -p mentat_sqlite_ext   # -> target/release/libmentat_sqlite.so
```

```sql
-- sqlite3
.load ./target/release/libmentat_sqlite
SELECT edn_t('/tmp/demo.mentat', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]');
SELECT edn_t('/tmp/demo.mentat', '[{:person/name "Alice"} {:person/name "Bob"}]');

-- edn_q returns JSON, so json_each() turns it into rows you can join.
SELECT json_extract(r.value, '$[1]') AS name
FROM json_each(edn_q('/tmp/demo.mentat',
       '[:find ?e ?name :where [?e :person/name ?name]]', '{}'), '$.results') AS r;
```

The functions are registered `SQLITE_DIRECTONLY`, so they can't be called from
views or triggers in a database schema you don't control. Host SQLite 3.30 or
newer is required. Like the DuckDB extension, it is a workspace member but not a
default member. See [`crates/sqlite/ext/README.md`](crates/sqlite/ext/README.md).

---

## Benchmarks

`benchmarks/scale/` is a repeatable scale and load suite that runs the same
scenarios against all four deployments: the embedded library, the SQLite
extension, pg_mentat and the DuckDB extension. The scenarios are bulk load,
point lookup, ref traversal, aggregate, predicate scan, pull, as-of/since,
`:in` bindings, mixed read/write, a 1-128 client concurrency sweep, sustained
load, and cold vs warm. Every scenario checks its answer as well as its timing.
`benchmarks/scale/compare.py OLD NEW` flags regressions between two runs. The
latest full run (r6id.metal, 1 TiB RAM, shared_buffers at 85% of RAM, up to 303M
datoms) is in
[`benchmarks/results/scale-2026-09-27T010840Z/`](benchmarks/results/scale-2026-09-27T010840Z/findings.md).
See [`benchmarks/scale/README.md`](benchmarks/scale/README.md) to run it.

The 1.10.0 before/after runs for each fix are in
`benchmarks/results/{pg-autoindex,embedded-fixes,ext-cache,duckdb-quack}-*`.

---

## Indexes: automatic

Mentat manages value indexes itself, on both engines. Out of the box (mode
`schema`), every value is indexed for lookup on PostgreSQL (an AVET index per
current-state table), and on the embedded store every `:db/unique` attribute
and `:db/index` ref gets a value index, created when you declare the attribute
and dropped when you remove the flag. In `adaptive` mode mentat also watches the
queries you run: an attribute that is repeatedly filtered by value (embedded) or
by a range over history (PostgreSQL) gets its own partial index, and an index
mentat created that goes unused for a while is dropped again. Mentat only ever
drops indexes it created and recorded in its registry, never ones you made.

| | Embedded (Rust / CLI) | PostgreSQL |
|---|---|---|
| Choose the mode | `Store::set_auto_index(AutoIndex::Adaptive)`, `MENTAT_AUTO_INDEX=adaptive`, or `.tune adaptive` in the CLI | `SET mentat.auto_index = 'adaptive'` (superuser) |
| See what it would do | `Store::tune_indexes(true)`, `.tune` | `SELECT * FROM mentat_tune_indexes()` (dry run) |
| Apply now | `Store::tune_indexes(false)`, `.tune!` | `SELECT * FROM mentat_tune_indexes(false)` |
| Registry | `mentat_managed_indexes` table | `mentat.managed_indexes` |

Tuning also runs on its own every so often (after a number of queries on the
embedded store, or of transactions on PostgreSQL). On PostgreSQL it gives up
rather than wait for a lock. See `docs/src/configuration.md` for the thresholds
and idle windows.

---

## Scripting: mino

Every backend embeds [mino](docs/src/scripting.md), a Clojure-dialect
interpreter (`crates/mino`), which exposes a `mentat.store/*` primitive surface
for scripting transactions and queries. It is the SQL function `edn_eval` on all
three: in `pg_mentat` behind the optional `script` cargo feature, and on by
default in the SQLite and DuckDB extensions, where `(mentat.store/open)` with no
argument opens the store you passed (a file path for SQLite, a store name in
the current database for DuckDB).

```sql
-- pg_mentat, built with --features script (off by default).
SELECT edn_eval($$
  (let [conn (mentat.store/open)]
    (mentat.store/transact conn [{:person/name "Bob"}])
    (mentat.store/q (mentat.store/db conn)
                    '[:find ?name :where [?e :person/name ?name]]))
$$);
```

### Security — `edn_eval`

`edn_eval` is **callable by every role by design** — the extension issues no
`REVOKE`, so PostgreSQL's default grants `EXECUTE` to `PUBLIC`. That is safe
because the interpreter is **sandboxed** and **resource-limited**:

- **Sandboxed.** It is built with `mino_rs::Interpreter::sandboxed()`: the
  language, regex, bignum, atoms, and the in-memory `mentat.store/*` surface are
  present, but every host-filesystem primitive (`slurp`, `spit`, `rm-rf`,
  `mkdir-p`, `file-exists?`) and the file-backed store are absent (unbound). A
  script cannot touch the server's filesystem.
- **Resource-limited.** Three superuser-only (`PGC_SUSET`) GUCs bound each call,
  and an ordinary role cannot raise them for its own session:
  - `mentat.script_max_steps` — default 10,000,000
  - `mentat.script_max_heap_bytes` — default 64 MiB
  - `mentat.script_max_depth` — default 2000

  Exceeding any of them throws an `:eval/limit` error instead of hanging,
  exhausting memory, or overflowing the stack. `statement_timeout` still applies
  on top, via an interrupt check hook.
- **No privilege gain.** A script runs through SPI as the calling role, so it
  reaches only the stores that role can already query with
  `edn_q`/`edn_t`. It must **not** be made `SECURITY DEFINER` —
  that would turn it into a privilege escalation.

The SQLite and DuckDB extensions build the same sandboxed interpreter with fixed
limits (10M steps, 64 MiB heap, depth 1000). They run inside your own process, so the sandbox mainly stops a script
from reaching the host filesystem; the script can read and write only mentat
stores (the SQLite extension's store file, or the DuckDB extension's stores in
the current database).

---

## License

Apache-2.0 for the workspace; the `crates/mino` interpreter keeps its MIT
license. See [`LICENSE`](LICENSE).
