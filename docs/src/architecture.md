# Architecture

Mentat is one repository that builds two storage backends behind a single
Datalog/EDN front-end. This chapter covers the front-end/backend split, the
crate map, and which features live on which backend, then the internals of the
PostgreSQL backend (storage layout, query compilation, transactions, pull).

## Front-end / backend split

A query, transaction, or schema definition is EDN text. Both backends parse it
with the same three shared crates:

- **`edn`** — the EDN reader and the Datalog grammar. It produces a `ParsedQuery`
  (find spec, where clauses, `:in` bindings, `:with`, rules) and parsed
  transaction entities, independent of any backend. It caps nesting depth before
  parsing so deeply nested input is rejected with an error rather than
  overflowing the stack.
- **`core-traits`** — the fundamental value model: `ValueType`, `TypedValue`,
  `ValueTypeSet`, and the type/value mappings.
- **`core`** (`mentat_core`) — conversions between EDN and the value model,
  reusable keyword constants, schema traits, and shared utilities.

Because a query is parsed by one grammar, it means the same thing on either
backend. The backends diverge only below the parsed representation, where each
turns it into storage-specific operations:

- The **embedded** backend algebrizes the parsed query into SQL over SQLite's
  `datoms` table (`query-algebrizer` → `query-projector` → `query-sql`) and
  transacts through its own SQLite transactor (`db`, `transaction`).
- The **PostgreSQL** backend compiles the same parsed query into SQL over its
  narrow per-type tables and executes it through PostgreSQL's SPI
  (`crates/pg/pg_mentat`).

## Crate map

```
crates/
├── edn/                 shared: EDN reader + Datalog grammar
├── core-traits/         shared: ValueType, TypedValue, ValueTypeSet
├── core/                shared (mentat_core): EDN↔value conversions, schema traits
├── mino/                the mino-rs interpreter (mentat.store/* prims) + tests/corpus/
├── sqlite/              the embedded backend
│   ├── mentat/          the `mentat` crate: Store, Conn, InProgress, scripting glue
│   ├── db/ db-traits/   SQLite storage: bootstrap, transactor, attribute cache
│   ├── transaction/     in-progress transaction wrappers
│   ├── sql/ sql-traits/  SQL text generation for SQLite
│   ├── query-algebrizer{,-traits}/   parsed query → algebraic query
│   ├── query-projector{,-traits}/    algebraic query → output shaping
│   ├── query-pull{,-traits}/         pull execution
│   ├── query-sql/       algebraic query → SQL
│   ├── ffi/             C ABI over the embedded store (mentat_ffi)
│   └── cli/             embedded CLI (mentat_cli)
└── pg/
    ├── pg_mentat/       the pgrx extension (SQL functions, narrow tables, SPI)
    └── mentatd/         HTTP/WebSocket server over tokio-postgres
```

A plain `cargo build` at the repo root builds only the SQLite side and the
shared front-end — no `pg_config`, libclang, or PostgreSQL required. The
extension builds with `cargo pgrx` from `crates/pg/pg_mentat`; `mentatd` builds
with `cargo build -p mentatd`.

## Features by backend

| Feature | Embedded (`mentat`) | PostgreSQL (`pg_mentat`) | DuckDB (`mentat_duckdb`) |
|---|---|---|---|
| Embedded EAV store, Datalog `q`, pull, schema | yes | yes | `q` + `transact` (embeds the SQLite store) |
| `:db.fn/cas`, `:db/retractEntity` | yes | yes | via the embedded store |
| C ABI (`ffi`) | yes | — | — |
| `?added` / history patterns in `:where` | yes | yes | via the embedded store |
| Historical `q` (as-of / since) | yes | yes | via the embedded store |
| `:in` collection / tuple / relation bindings | yes | yes | scalar `:in` only (planned) |
| Join Datalog results against native SQL tables | — | — | yes |
| Reactive subscriptions (LISTEN/NOTIFY) | — | yes | — |
| BM25 full-text, pgvector, pg_trgm, PostGIS, rum | — | yes | — |
| Materialized / virtual views, multi-store, excision | — | yes | — |
| `mentatd` HTTP/WebSocket server | — | yes | — |

`:db.fn/cas`, `:db/retractEntity`, `?added` history patterns, historical
(`as-of`/`since`) Datalog `q`, and non-scalar `:in` bindings are on **both** the
embedded and PostgreSQL backends as of 1.8.0. Historical `q` on the embedded
side is `Store::q_once_as_of(tx, …)` / `q_once_since(tx, …)` (and the `Conn`
equivalents), which evaluate a query against the transaction log at, or since, a
transaction.

The **DuckDB** backend (`mentat_duckdb`, added 1.8.0) is a loadable DuckDB
extension that embeds the SQLite store and exposes it through `mentat_transact`
(scalar) and `mentat_query` (table function), so Datalog results join against
native DuckDB tables. It inherits whatever the embedded store supports; a
DuckDB-native storage backend, typed result columns, `mentat_pull`, and
`mentat_eval` are planned. See `duckdb-extension-plan.md`.

---

# PostgreSQL backend internals

pg_mentat implements the Datomic data model within PostgreSQL using pgrx (Rust PostgreSQL extension framework). The rest of this chapter describes its storage layout, query compilation pipeline, transaction processing, and pull architecture.

## Storage Model

### Narrow Per-Type Tables

pg_mentat stores datoms (facts) in nine type-specific narrow tables. Each table stores values of exactly one PostgreSQL native type, enabling the query optimizer to use type-appropriate comparison operators and indexes without casts.

| Table | Value Column Type | Datomic Type |
|-------|-------------------|--------------|
| `mentat.datoms_ref_new` | `BIGINT` | `:db.type/ref` |
| `mentat.datoms_boolean_new` | `BOOLEAN` | `:db.type/boolean` |
| `mentat.datoms_long_new` | `BIGINT` | `:db.type/long` |
| `mentat.datoms_double_new` | `DOUBLE PRECISION` | `:db.type/double` |
| `mentat.datoms_text_new` | `TEXT` | `:db.type/string` |
| `mentat.datoms_keyword_new` | `TEXT` | `:db.type/keyword` |
| `mentat.datoms_instant_new` | `TIMESTAMPTZ` | `:db.type/instant` |
| `mentat.datoms_uuid_new` | `UUID` | `:db.type/uuid` |
| `mentat.datoms_bytes_new` | `BYTEA` | `:db.type/bytes` |

Each table has columns: `e` (entity), `a` (attribute entid), `v` (typed value), `tx` (transaction ID), `added` (boolean: assert or retract).

### Compatibility View

A `mentat.datoms` view is provided as a compatibility shim over the nine narrow tables with `INSTEAD OF` triggers. This allows legacy code to `INSERT INTO mentat.datoms` or `DELETE FROM mentat.datoms` -- the triggers route to the appropriate narrow table. New code should use the narrow tables directly.

### Covering Indexes

Each narrow table has four covering indexes, mirroring Datomic's index model:

| Index | Column Order | Use Case |
|-------|-------------|----------|
| EAVT | `(e, a, v, tx)` | Entity lookup -- "what do I know about entity E?" |
| AEVT | `(a, e, v, tx)` | Attribute scan -- "all entities with attribute A" |
| AVET | `(a, v, e, tx)` | Value lookup -- "who has name = 'Alice'?" |
| VAET | `(v, a, e, tx)` | Reverse ref -- "who points to entity V?" (ref tables only) |

This is 4 indexes per table x 9 tables = 36 indexes total. The VAET index is only useful on `datoms_ref_new` but is maintained on all tables for uniformity.

### Supporting Tables

| Table | Purpose |
|-------|---------|
| `mentat.schema` | Attribute definitions (entid, ident, value_type, cardinality, etc.) |
| `mentat.idents` | Keyword-to-entid bidirectional mapping |
| `mentat.partitions` | Entity ID allocation ranges per partition |
| `mentat.transactions` | Transaction log (tx ID, timestamp) |
| `mentat.cache_generation` | Cross-backend schema cache invalidation counter |

## Query Compilation Pipeline

A Datalog query flows through four stages:

```
EDN text --> Parse --> Compile --> SQL text --> SPI Execute --> JSON result
```

### Stage 1: EDN Parse

The `edn` crate (workspace member) parses the EDN query string into a `ParsedQuery` struct containing:
- `find` -- find specification (relation, collection, tuple, scalar)
- `where_clauses` -- pattern matches, predicates, function calls, not/or clauses
- `in_bindings` -- input parameter bindings (scalar, collection, tuple, relation)
- `with` -- additional grouping variables
- `rules` -- named rule definitions

### Stage 2: Datalog Compile

The query compiler (`pg_mentat/src/functions/query.rs`) transforms the parsed query into SQL:

1. **Schema resolution** -- resolve keyword attributes (`:person/name`) to entid integers via the schema cache
2. **Variable binding** -- assign SQL aliases to each pattern variable (`?e` -> `t0.e`)
3. **Table selection** -- route each pattern to the correct narrow table based on the attribute's value type
4. **Join generation** -- unify shared variables across patterns via `JOIN ... ON t0.e = t1.e`
5. **Predicate translation** -- convert Datalog predicates to SQL `WHERE` conditions with type guards
6. **NOT/OR compilation** -- `not` becomes `NOT EXISTS (subquery)`, `or` becomes `UNION`
7. **Aggregate wrapping** -- wrap the base query in `GROUP BY` with aggregate functions
8. **Find spec projection** -- shape the output (array of arrays, single value, etc.)

All string constants use parameterized bindings (`$1`, `$2`, ...) via the `SqlBuilder` to prevent SQL injection.

### Stage 3: SQL Execution

The generated SQL is executed through PostgreSQL's SPI (Server Programming Interface). Results are collected into a `serde_json::Value` and returned as `JSONB`.

### Stage 4: Result Shaping

The JSON result is shaped according to the find specification:
- **Relation** `[:find ?x ?y]` -- array of arrays: `[[1, "Alice"], [2, "Bob"]]`
- **Collection** `[:find [?x ...]]` -- flat array: `[1, 2, 3]`
- **Tuple** `[:find [?x ?y]]` -- single array: `[1, "Alice"]`
- **Scalar** `[:find ?x .]` -- single value: `1`

## Transaction Pipeline

Transaction processing (`pg_mentat/src/functions/transact.rs`) follows these steps:

```
EDN tx data --> Parse --> Schema detect --> Tempid allocate -->
  Type route --> Upsert resolve --> Insert/Retract --> Commit
```

### Steps in Detail

1. **Parse** -- EDN transaction data is parsed into assertion/retraction operations (`:db/add`, `:db/retract`, map forms, `:db/retractEntity`, `:db.fn/cas`)

2. **Schema detection** -- operations targeting schema attributes (`:db/ident`, `:db/valueType`, etc.) are identified; these will update the schema table and invalidate caches

3. **Transaction ID allocation** -- a new transaction ID is obtained from the `mentat.transactions` sequence

4. **Tempid resolution** -- string tempids (`"tempid-1"`) are mapped to freshly allocated entity IDs from the partition sequence

5. **Lookup ref resolution** -- lookup refs like `[:person/email "alice@example.com"]` are resolved to entity IDs

6. **Upsert handling** -- for attributes with `:db/unique :db.unique/identity`, existing entities are found and reused rather than creating duplicates

7. **Type routing** -- each datom is inserted into the appropriate narrow table based on the attribute's value type

8. **CAS validation** -- `:db.fn/cas` operations verify the current value matches the expected value before asserting the new value

9. **Commit** -- the transaction record is written to `mentat.transactions`; if schema changed, the cache generation counter is bumped

### Speculative Transactions

`mentat_with()` wraps the transaction in a savepoint and rolls back after computing the result. This lets you see what a transaction *would* do without persisting it.

## Pull Architecture

The pull API (`pg_mentat/src/functions/pull.rs`) implements Datomic's declarative entity retrieval:

```
Pull pattern --> Parse --> Recursive fetch --> Cycle detection --> JSON assembly
```

Key implementation details:

- **Recursive navigation** -- forward refs (`:person/friends`) and reverse refs (`:person/_friends`) are followed to arbitrary depth
- **Cycle detection** -- a visited-entity set prevents infinite loops in cyclic reference graphs
- **Bounded recursion** -- `{:person/friends 3}` limits traversal depth
- **Component auto-expansion** -- attributes marked `:db/isComponent true` are automatically pulled recursively
- **Wildcard** -- `[*]` retrieves all attributes for an entity
- **Default values** -- `(default :person/age 0)` provides fallbacks for missing attributes
- **Limits** -- `(limit :person/email 5)` caps cardinality-many results

## Schema Cache

pg_mentat maintains a process-local LRU schema cache (`SchemaCache`) per store that maps attribute keywords to their metadata (entid, value type, cardinality, etc.). This avoids repeated schema table lookups during query compilation.

Cache invalidation uses a generation counter stored in `mentat.cache_generation`:
- After a schema-affecting transaction, the counter is bumped
- Before query compilation, the local cache checks its generation against the table
- If stale, the cache is reloaded from `mentat.schema`

This ensures cross-backend consistency when multiple PostgreSQL backends modify schema concurrently.

## Multi-Store Architecture

pg_mentat supports multiple isolated stores within a single PostgreSQL database. Each store gets its own PostgreSQL schema (e.g., `mentat_mystore`) containing independent copies of all tables and indexes. The default store uses the `mentat` schema.

```sql
SELECT mentat.create_store('analytics');
SELECT mentat.t('analytics', '[{:db/ident :event/type ...}]');
SELECT mentat.q('analytics', '[:find ?e :where [?e :event/type "click"]]', '{}');
```
