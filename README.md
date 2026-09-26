# Mentat

Mentat is a Datomic-like database: an entity-attribute-value store with an
immutable transaction log, a Datalog query language, and a declarative pull
API, all expressed in [EDN](https://github.com/edn-format/edn). One repository
builds three backends that share the same Datalog/EDN front-end:

- **Embedded (`mentat`)** — a Rust library store on SQLite. `cargo build`,
  no external services. Drop it into an application the way you would SQLite.
- **PostgreSQL extension (`pg_mentat`)** — the same data model implemented
  inside PostgreSQL via [pgrx](https://github.com/pgcentralfoundation/pgrx),
  reached through SQL functions. Plus **`mentatd`**, an HTTP/WebSocket server
  that fronts a `pg_mentat` database.
- **DuckDB extension (`mentat_duckdb`)** — loads into DuckDB (`LOAD mentat;`)
  and exposes the embedded store as SQL functions, so Datalog results join
  against native DuckDB tables.

Both backends parse queries, transactions, and schema with one copy of the
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
  `mentat_eval`, with the full security model.

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
nix build .#pg_mentat-pg16     # or .#pg_mentat-pg13 … .#pg_mentat-pg18
```

### Use it

```sql
CREATE EXTENSION pg_mentat;

-- Define a schema attribute. mentat_transact takes EDN transaction text.
SELECT mentat_transact('[
  {:db/ident       :person/name
   :db/valueType   :db.type/string
   :db/cardinality :db.cardinality/one}
]');

-- Assert a fact.
SELECT mentat_transact('[{:person/name "Alice"}]');

-- Query. mentat_query takes an EDN query and a JSONB inputs map; returns JSONB.
SELECT mentat_query(
  '[:find ?name :where [?e :person/name ?name]]',
  '{}'::jsonb
);

-- Pull all attributes for an entity (entity id 10001 here).
SELECT mentat_pull('[*]', 10001);
```

`mentat.q`, `mentat.t`, and `mentat.pull` are shorter aliases for
`mentat_query`, `mentat_transact`, and `mentat_pull`. As of 1.8.0 the embedded
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

`mentat_duckdb` (`crates/duckdb`) is a loadable DuckDB extension that embeds the
mentat SQLite store and exposes it to DuckDB as SQL functions, so Datalog
results can be joined against native DuckDB tables. It is built with
[duckdb-rs](https://github.com/duckdb/duckdb-rs) and pinned to **DuckDB v1.5.5**
(via the DuckDB unstable C API); the extension loads only into that DuckDB
version, and bumping DuckDB means bumping the pin and rebuilding.

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

DuckDB refuses unsigned extensions unless started with `-unsigned`
(or opened with `allow_unsigned_extensions=true`). With a DuckDB v1.5.5 CLI:

```sql
-- duckdb -unsigned
LOAD './build/debug/mentat.duckdb_extension';

-- Define a schema attribute and assert facts into an embedded mentat store.
-- Every function takes the store's file path as its first argument.
SELECT mentat_transact('/tmp/demo.mentat', '[
  {:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
]');
SELECT mentat_transact('/tmp/demo.mentat', '[{:person/name "Alice"} {:person/name "Bob"}]');

-- mentat_query is a table function: run Datalog and get rows back.
SELECT * FROM mentat_query('/tmp/demo.mentat',
  '[:find ?e ?name :where [?e :person/name ?name]]', '{}');

-- ...so it joins against native DuckDB tables.
SELECT m.name, a.age
FROM mentat_query('/tmp/demo.mentat',
       '[:find ?e ?name :where [?e :person/name ?name]]', '{}') AS m(e, name)
JOIN ages a ON a.name = m.name;
```

`mentat_transact` returns a JSON tx-report; `mentat_query` returns rows with all
columns as `VARCHAR` in this first release. A DuckDB-native storage backend,
typed result columns, `mentat_pull`, and `mentat_eval` are planned; see
[`docs/duckdb-extension-plan.md`](docs/duckdb-extension-plan.md). Publishing to
the DuckDB Community Extensions registry is documented in
[`docs/registry-publishing.md`](docs/registry-publishing.md).

---

## Scripting: mino

Both backends embed [mino](docs/src/scripting.md), a Clojure-dialect
interpreter (`crates/mino`), which exposes a `mentat.store/*` primitive surface
for scripting transactions and queries. In `pg_mentat` it is the SQL function
`mentat_eval`, behind the optional `script` cargo feature:

```sql
-- Build with --features script (off by default).
SELECT mentat_eval($$
  (let [conn (mentat.store/open)]
    (mentat.store/transact conn [{:person/name "Bob"}])
    (mentat.store/q (mentat.store/db conn)
                    '[:find ?name :where [?e :person/name ?name]]))
$$);
```

### Security — `mentat_eval`

`mentat_eval` is **callable by every role by design** — the extension issues no
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
  `mentat_query`/`mentat_transact`. It must **not** be made `SECURITY DEFINER` —
  that would turn it into a privilege escalation.

---

## License

Apache-2.0 for the workspace; the `crates/mino` interpreter keeps its MIT
license. See [`LICENSE`](LICENSE).
