# DuckDB extension: native DuckDB storage

Status: implemented in 1.11.0 (see the CHANGELOG and
`benchmarks/results/duckdb-native-*/findings.md`). Supersedes the storage decision in
`docs/duckdb-extension-plan.md` §3.1 ("option (a): embed the SQLite store").

## Goal

| Deployment | Storage |
|---|---|
| CLI and Rust library (`crates/sqlite/*`) | SQLite (unchanged) |
| `pg_mentat` | PostgreSQL (unchanged) |
| DuckDB extension (`crates/duckdb`) | **DuckDB**: datoms live in tables of the DuckDB database the extension is loaded into |

Today the DuckDB extension embeds mentat's SQLite engine and `edn_t('/x.mentat',
…)` writes a separate SQLite file. After this change every datom, transaction
and schema row lives in DuckDB tables, and every query runs as DuckDB SQL. No
SQLite connection or file is ever opened by the DuckDB extension. The shared
engine crates still depend on rusqlite, but nothing in the extension calls it,
so the release link drops it: the 1.11 binary has no `sqlite3_*` symbols (1.10.3
had 226) and is 7.0 MB instead of 9.2 MB.

## What the spike proved (2026-10-05, DuckDB v1.5.6)

A 40-line extension (`/tmp/spike.*`), holding a `Connection` cloned from the
one the entrypoint receives (`Connection::try_clone`) in a global, and running
SQL from inside a scalar function:

1. **Can create tables and write to the host database**: `CREATE TABLE`,
   `INSERT` and `SELECT` against the attached file all worked. The table
   persisted and was visible to a later session that never loaded the
   extension.
2. **It runs on its own connection, so it has its own transaction.** A caller's
   `BEGIN; SELECT probe(); ROLLBACK` does **not** roll back the extension's
   write, and the extension does not see the caller's uncommitted rows. So
   `edn_t` is atomic by itself (it commits its own transaction), and it is not
   part of the caller's transaction.
3. **Default catalog:** with `ATTACH … AS other; USE other`, the extension
   still wrote to the database it was loaded into (the connection's default
   catalog is fixed at clone time). See "Which database" below.
4. **Parallel scalar evaluation is safe** with the connection behind a Mutex
   (200k-row `SELECT probe(i)` with `threads=8`: no deadlock, no error).
5. **DuckDB SQL differences that matter**: backtick identifiers are rejected
   (use `"…"`), partial indexes are not supported, and a `UNION` column
   cannot be an index key. `rowid` exists and `DELETE … WHERE rowid IN
   (subquery)` works. TEMP tables exist (`CREATE TEMP TABLE x`, referenced as
   `temp.x`; `CREATE TABLE temp.x` is rejected). `IS NOT DISTINCT FROM` works,
   SQLite's `IS 1` / `IS NOT v` don't. Prepared parameters bind into a `UNION`
   column by member type (`$1` BIGINT -> `i`, VARCHAR -> `s`). A `UNION`
   column compares by tag and then by value; `VARIANT` errors when comparing
   different types and is unusable for `v`.

## Design

### 1. Storage layout (DuckDB tables, schema `mentat`)

Mirror the SQLite engine's layout, column for column, so the existing query
engine's SQL needs only dialect changes, not a new planner:

```sql
CREATE SCHEMA IF NOT EXISTS mentat;
CREATE TABLE mentat.datoms (e BIGINT NOT NULL, a BIGINT NOT NULL, v <value> NOT NULL,
                            tx BIGINT NOT NULL, value_type_tag SMALLINT NOT NULL,
                            index_avet BOOLEAN, index_vaet BOOLEAN,
                            index_fulltext BOOLEAN, unique_value BOOLEAN);
CREATE TABLE mentat.transactions (e, a, v, tx, added BOOLEAN, value_type_tag);
CREATE TABLE mentat.idents (...), mentat.schema (...), mentat.known_parts (...);
CREATE TABLE mentat.meta (key VARCHAR PRIMARY KEY, value VARCHAR);   -- schema version
```

`v` is the hard part: SQLite stores INTEGER / REAL / TEXT / BLOB in one column,
and mentat's SQL compares `v` directly (`datoms00.v = $v0`, `v < 10`, joins
`e = v`). Choice: **`v` is a DuckDB `UNION(i BIGINT, d DOUBLE, s VARCHAR, b
BLOB)`** with the same mapping SQLite uses (ref/bool/long/instant -> `i`,
double -> `d`, string/keyword -> `s`, uuid/bytes -> `b`).

Measured against SQLite on the same seven values (2026-10-05):

| predicate | SQLite (what mentat relies on) | DuckDB `UNION` as-is | DuckDB with numeric rewrite |
|---|---|---|---|
| `v = 5` (5 and 5.0 stored) | 5, 5.0 | 5 only | 5, 5.0 |
| `v < 10` | -3, 5, 5.0, 9.5 | -3, 5 | -3, 5, 5.0, 9.5 |
| `v = 'abc'`, `v = 2^53+1` | exact | exact | exact |

A `UNION` compares within one member only, so long-vs-double numeric
comparisons differ from SQLite. The DuckDB query builder therefore renders a
`v` column that takes part in a numeric comparison (`<`, `<=`, `>`, `>=`, `=`,
`!=` against a number, which is what `(< ?v 10)` and numeric `:in` bindings
produce) as `mentat_num(v)` (`CASE union_tag(v) WHEN 'i' THEN v.i::DOUBLE WHEN
'd' THEN v.d END`). Exact equality joins on refs (`e = v`) and string/bytes
equality keep comparing the column directly, so they stay exact and indexable.
The fallback, if this doesn't hold up under the full test suite, is
pg_mentat's typed-column layout (`v_long`, `v_text`, …), at the cost of more
SQL rewriting. Decided in M2 by the query-engine tests.

Fulltext (`:db/fulltext`): SQLite FTS4 has no DuckDB equivalent in core.
**Not in this release**: transacting a fulltext attribute stores the value as a
plain string, and `(fulltext …)` in a query returns an error naming the gap.
DuckDB's `fts` extension can back it later.

### 2. Engine: reuse the SQLite engine through a SQL-dialect seam, not a port

`crates/sqlite/{db,transaction,query-*}` are ~37k lines of tested logic: the
transactor (tempids, upserts, cardinality, cas, retractEntity, tx fns), the
algebrizer, the projector and pull. Its coupling to SQLite is narrow and
mechanical: `rusqlite::Connection` (117 uses), `params!` (31), `Value` (30),
and about 115 prepare/execute/query calls, plus a few SQLite-only statements
(`PRAGMA`, `temp.*` search tables, FTS4, `sqlite_master`). The query algebrizer
has **no** SQLite dependency at all.

pg_mentat's engine is the other candidate, but it is ~14k lines written against
SPI, JSONB, partitions and GUCs. Porting it means rewriting most of it.

So: introduce a small storage trait (connection: execute / query rows /
transaction; values: the five SQL value kinds), implement it for rusqlite (the
existing behaviour, byte for byte) and for DuckDB, and route the engine through
it. SQLite-specific statements move behind the trait as dialect methods. The
SQLite CLI, library and SQLite extension keep identical behaviour and pass their
existing suites unchanged; that is the regression gate for the refactor.

The DuckDB side:

- **Connection**: one `duckdb::Connection` cloned from the entrypoint's, per
  DuckDB thread (thread-local, cloned on first use; the spike's global Mutex
  works but serialises all calls).
- **Query SQL**: `SQLiteQueryBuilder` emits backtick identifiers and `$v0`
  named parameters. A `DuckDbQueryBuilder` emits `"…"` identifiers and `$1…`
  positional parameters. Everything else the algebrizer and projector generate
  (subqueries, `UNION ALL`, `NOT EXISTS`, `LIMIT`, `DISTINCT`, aggregates) is
  standard SQL that DuckDB accepts.
- **Transactor**: `BEGIN` / `COMMIT` on the extension's connection; the
  `temp.*` search tables become DuckDB `TEMP` tables. Partial indexes and
  indexes on `v` don't exist in DuckDB, so the DuckDB schema indexes only
  BIGINT columns and relies on hash joins and zone maps for value lookups.
  Point lookups by value will be slower than SQLite's; measured in M4.
- **The seam is a dyn trait, not generics**, at two levels:
  - `mentat_db::MentatStoring` (already a trait, documented as "we might
    consider other SQL engines") becomes the transactor's whole storage
    interface: the transactor holds `&dyn MentatStoring`. SQLite keeps its
    existing implementation, SQL text unchanged. DuckDB gets its own
    implementation with its own SQL in one module.
  - `mentat_sql::SqlConn` (execute, query with a per-row callback, named
    parameters, dialect) and `SqlValue` (Null / Integer / Real / Text / Blob)
    for the query runner, projector and pull. `rusqlite::Connection`
    implements it (keeping `prepare_cached`), so `&conn` coerces to
    `&dyn SqlConn` unchanged.
  - The public `mentat` crate API (`Store`, `Conn`, `InProgress`) stays on
    rusqlite.
- **The DuckDB extension uses the engine crates (`mentat_db`,
  `mentat_transaction` query functions) directly**, not the rusqlite-based
  `mentat` crate.
- **The DuckDB storage crate has no duckdb dependency.**
  `crates/duckdb/store` (`mentat_duckdb_store`) implements `MentatStoring` and
  bootstrap over `&dyn SqlConn` with DuckDB SQL. The `SqlConn` impl for
  `duckdb::Connection` is one small file compiled into both the cdylib
  (`loadable-extension`) and the test harness (`bundled`).
- **DuckDB SQL differences found by testing** (each handled in the DuckDB
  module): `end` is a reserved word (`known_parts.end` -> `"end"`), as are the
  `left`/`right` aliases; named and positional parameters can't be mixed in
  one statement; `sum(BIGINT)` returns HUGEINT; there's no `PRAGMA
  user_version` (a `meta` table instead) and no `sqlite_master`
  (`duckdb_tables()`).
- **Testing outside the extension**: the cdylib's duckdb is built with
  `loadable-extension` (C API through function pointers); a test crate needs a
  normal `bundled` duckdb. Cargo unifies features per build, so the test crate
  is a separate (non-member) workspace under `crates/duckdb/store-tests`.

### 3. SQL surface

The function names and shapes stay the same; the **path argument changes
meaning**. It was a SQLite file path; it is now a **store name**, a namespace
inside the DuckDB database (pg_mentat's model: `mentat.stores` and a `store_id`
column, default store `'default'`). A value that looks like a path (`/` or
`.mentat`) keeps working as a name, so existing calls such as
`edn_t('/tmp/demo.mentat', …)` still run, but they now write to the current
DuckDB database instead of a file. The README and registry description are
deliberately left as they are, per instruction.

| Function | Behaviour |
|---|---|
| `edn_t(store, edn)` | transact into DuckDB tables; returns the same JSON tx-report |
| `edn_q(store, query, options)` | table function, same columns and options JSON |
| `edn_pull(store, pattern, eid)` | same JSON |
| `edn_eval(store, script)` | mino scripts run against the DuckDB-backed store |

**Which database**: the one the extension was loaded into (its default catalog
when the entrypoint ran). A persistent database (`duckdb my.db`) gets persistent
datoms; `:memory:` gets in-memory ones. Writing to a different attached database
is a follow-up (an explicit `ATTACH` name in the store argument).

**Transactions**: `edn_t` commits its own transaction. Calling it inside a
caller's `BEGIN … ROLLBACK` doesn't roll it back (spike finding 2). This matches
how pg_mentat behaves under SPI autocommit per call and is documented in the
CHANGELOG.

### 4. Migration

There is nothing to migrate on the DuckDB side: before this release every
DuckDB-side store was a SQLite file. `CALL mentat_import(store, '/path/x.mentat')`
(a table function, since it writes) reads an existing SQLite store's
`transactions` log and replays it into DuckDB. **Not in the first cut**: it
would put SQLite back into the binary. Users with old files can export them with
the CLI and transact them. A follow-up if anyone asks.

## Milestones (each committed when green)

- **M0** spike — done (findings above).
- **M1** storage seam: trait + rusqlite impl; route `db`, `transaction`,
  `query-projector`, `query-pull` and the `mentat` crate through it. Gate: the
  whole workspace test suite passes unchanged (87 binaries, mino, CLI, FFI,
  SQLite ext smoke).
- **M2** DuckDB backend (outside the cdylib for testing): a library with the
  DuckDB impl of the trait, the DuckDB schema, and `DuckDbQueryBuilder`, tested
  against a regular (non-extension) DuckDB connection. Gate: the engine's
  transact/query/pull/history/as-of test suites run against DuckDB.
- **M3** extension: `crates/duckdb` switches to the DuckDB backend. Gate:
  `test/sql/mentat.test` and `smoke.sh` updated to assert the data lands in
  DuckDB tables and that no SQLite file is created, plus a persistence test
  (restart DuckDB without the extension, `SELECT` from `mentat.datoms`).
- **M4** qualify: workspace + pg16 gates, registry build on the fork (all 5
  platforms, tests on osx_arm64 and windows), benchmarks against 1.10.3
  (scale `s`/`m` read mix, transact throughput).
- **M5** release 1.11.0 (minor: the storage location changes) and bump the
  registry descriptor.

## Risks

- **UNION `v` column performance or semantics under the full suite**: fallback
  is typed columns (§1), decided in M2.
- **DuckDB is OLAP-oriented**: single-row transacts are slower than SQLite's.
  Measured in M4 and reported, not hidden.
- **The `loadable-extension` feature replaces the C API**: the M2 test library
  must use a normal `duckdb` build, and the cdylib a loadable one. Two crates (or
  a feature) keep them apart.
