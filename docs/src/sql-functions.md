# SQL Function Reference

All pg_mentat functions live in the `mentat` schema. After `CREATE EXTENSION pg_mentat`, they are accessible as `mentat.function_name()`.

## Function Naming Convention

pg_mentat provides two sets of function names:

**Convenience aliases** (recommended for everyday use in the default `mentat` schema):

```sql
SELECT mentat.t('[{:person/name "Alice"}]');
SELECT mentat.q('[:find ?e :where [?e :person/name "Alice"]]');
SELECT mentat.pull('[*]', 10001);
```

**Core functions** use the cross-backend `edn_*` names shared with the SQLite
and DuckDB extensions (since 1.9.0):

```sql
SELECT edn_t('[{:person/name "Alice"}]');
SELECT edn_q('[:find ?e :where [?e :person/name "Alice"]]', '{}');
SELECT edn_pull('[*]', 10001);
SELECT edn_eval('(+ 1 2)');   -- only in builds with the `script` feature
```

**Deprecated names.** Before 1.9.0 these were `mentat_transact`, `mentat_query`,
`mentat_pull` and `mentat_eval`. The old names still work: they are thin SQL
wrappers over the `edn_*` functions (`COMMENT`ed as deprecated) and will be
removed in a future major release. Other functions keep their `mentat_` prefix.

### Quick Reference

| Convenience alias | Full function | Description |
|-------------------|--------------|-------------|
| `mentat.t(edn)` | `edn_t(edn)` | Transact EDN data |
| `mentat.q(query, inputs)` | `edn_q(query, inputs)` | Run a Datalog query |
| -- | `edn_q_rows(query, inputs)` | Run a query, one JSONB row per result |
| `mentat.pull(pattern, eid)` | `edn_pull(pattern, eid)` | Pull entity attributes |
| `mentat.pull_many(pattern, eids)` | `mentat_pull_many(pattern, eids)` | Pull multiple entities |
| `mentat.entity(eid)` | `mentat_entity(eid)` | All attributes as JSON |
| `mentat.schema()` | `mentat_schema()` | Current schema |
| `mentat.explain(query)` | `mentat_explain(query)` | Show generated SQL |
| `mentat.stats()` | `mentat_query_stats()` | Execution statistics |
| `mentat.storage()` | `mentat_storage_stats()` | Storage statistics |
| `mentat.cache_stats()` | `mentat_stmt_cache_stats()` | Statement cache info |
| `mentat.cache_clear()` | `mentat_stmt_cache_clear()` | Clear statement cache |

---

## Transaction Functions

### `mentat.t(edn)` / `edn_t(edn)`

Execute a transaction. Returns a JSON transaction report:

```json
{"db-before": {"basis-t": 1000000000001}, "db-after": {"basis-t": 1000000000002},
 "tx-data": [[e, a, v, tx, added], ...], "tempids": {"tempid-1": 1000003},
 "schema_changed": false, "datoms_asserted": 2, "datoms_retracted": 0}
```

`datoms_asserted` / `datoms_retracted` count the datoms the transaction wrote,
leaving out its `:db/txInstant`; both are 0 when the transaction changed
nothing (e.g. it re-asserted values already there). The embedded engine's
`TxReport` and the DuckDB extension's `edn_t` report the same counts.

`edn_t` runs in the caller's transaction, at its isolation level. Under READ
COMMITTED (the default) and SERIALIZABLE, concurrent transactions keep a
cardinality-one attribute to one live value, `:db/cas` is atomic, a
`:db/unique` value stays on one entity, and a schema alteration to
`:db.cardinality/one` or `:db/unique` can't race a write: a second writer of
the same value waits for the first to commit, then replaces its value (or its
cas fails). Under SERIALIZABLE it may get a serialization failure instead;
retry. Avoid REPEATABLE READ. There a racing replacement or cas fails with a
serialization error, but two first assertions of one attribute on one entity,
two new entities claiming one unique value, or a write racing a many-to-one
alteration can both commit and leave two live values. See "Transaction
isolation" in `crates/pg/pg_mentat/sql/README.md`.

```sql
SELECT mentat.t('[
  {:db/id "tempid-1"
   :person/name "Alice"
   :person/age 30}
]');
```

The `t` alias transacts against the default store. Use the full function with a store argument for named stores:

```sql
SELECT mentat.mentat_transact_store('analytics', '[{:event/type "click"}]');
```

### `mentat_with(edn)` — Speculative Transaction

Execute a transaction without persisting it. Returns the same report format, but writes nothing. Useful for validation or "what-if" analysis.

```sql
SELECT mentat.mentat_with('[
  {:person/name "Test" :person/age 99}
]');
```

---

## Query Functions

### `mentat.q(query, inputs)` / `edn_q(query, inputs)`

Execute a Datalog query. Returns JSONB with `columns` and `results`.

```sql
-- Simple query
SELECT mentat.q('
  [:find ?name ?age
   :where [?e :person/name ?name]
          [?e :person/age ?age]
          [(> ?age 21)]]
');

-- With input bindings (positional, matching :in clause order)
SELECT mentat.q('
  [:find ?name
   :in $ ?min-age
   :where [?e :person/name ?name]
          [?e :person/age ?age]
          [(>= ?age ?min-age)]]
', '[25]');
```

The `inputs` parameter is a JSON value:
- Simple array for positional bindings: `'[25]'`
- Empty for no inputs: `'{}'` or `'[]'`
- An object with the [query options](datalog.md#query-options): `inputs`,
  `asOf`, `since`, `history`, `limit`, `offset`. Since 1.18.0 any other key,
  or a non-integer `asOf` / `since`, is an error; before, it was ignored and
  the query read current state (`{"as_of": T}` returned today's values).

### `edn_q_rows(query, inputs DEFAULT '{}')` -- streamed rows

`RETURNS SETOF JSONB`: one JSON array per result row, in `:find` order,
with the same value encoding and the same `inputs` (`:in` bindings,
`asOf` / `since` / `history`, `limit` / `offset`) as `edn_q`. Use it when
the result is large -- `edn_q` returns one JSONB value, which PostgreSQL
caps at 1 GB and which must be built in memory before the first row
reaches the client -- or when you want to join / filter / aggregate the
rows in SQL:

```sql
SELECT r->>0 AS name, (r->>1)::int AS age
FROM edn_q_rows('[:find ?n ?a :where [?e :person/name ?n] [?e :person/age ?a]]') r
WHERE (r->>1)::int > 21;
```

Rows are read from a cursor in batches of 1000 into a tuplestore, which
PostgreSQL keeps in memory up to `work_mem` and spills to a temp file past
it, so backend memory does not grow with the result. `mentat.max_result_rows`
applies exactly as for `edn_q` (more rows than the limit is an error, not
a silent truncation), so both functions give the same answer for the
same settings; `SET LOCAL mentat.max_result_rows = 0` for an unbounded
stream. Default store only (like `edn_q`).

### `mentat_tune_indexes(dry_run DEFAULT true)` -- automatic indexes

Report (dry run) or apply the automatic index manager's creates / drops:
`RETURNS TABLE(action, index_name, table_name, reason)`. See
[Operations: automatic index management](operations.md#7-automatic-index-management).

### `mentat.explain(query)` / `mentat_explain(query)`

Show the generated SQL and PostgreSQL's EXPLAIN output without executing the query.

```sql
SELECT mentat.explain('[:find ?name :where [?e :person/name ?name]]');
```

### `mentat_query_sql(query)` — Generated SQL

Return only the generated SQL string (no execution, no EXPLAIN).

```sql
SELECT mentat.mentat_query_sql('[:find ?name :where [?e :person/name ?name]]');
```

### `mentat_query_view(name, query)` — Create SQL VIEW from Datalog

Create a PostgreSQL VIEW backed by a Datalog query:

```sql
SELECT mentat.mentat_query_view('people_over_30', '
  [:find ?name ?age ?email
   :where [?e :person/name ?name]
          [?e :person/age ?age]
          [?e :person/email ?email]
          [(> ?age 30)]]
');

-- Now use it like any SQL view
SELECT * FROM mentat.people_over_30 WHERE name LIKE 'A%';
```

---

## Pull Functions

### `mentat.pull(pattern, eid)` / `edn_pull(pattern, eid)`

Pull attributes for a single entity. Returns a nested JSON document.

```sql
-- Pull everything
SELECT mentat.pull('[*]', 10001);

-- Pull specific attributes with nested refs
SELECT mentat.pull('[
  :person/name
  :person/age
  {:person/friends [:person/name :person/age]}
]', 10001);

-- Reverse lookup: who has this entity as a friend?
SELECT mentat.pull('[:person/name :person/_friends]', 10001);

-- With modifiers
SELECT mentat.pull('[
  :person/name
  {(:person/friends :limit 5 :as :top-friends) [:person/name]}
  {(:person/_friends :as :admirers) [:person/name]}
]', 10001);
```

### `edn_pull(pattern, eid, options)`

Pull on a historical basis (since 1.18.0). `options` takes `edn_q`'s temporal
keys: `{"asOf": T}` (the state after transaction T), `{"since": S}` (only the
current datoms transacted after S), or both (the assertions with S < tx <= T
not retracted by T). `'{}'` is the current state. Any other key is an error;
`"history"` does not apply to pull.

```sql
SELECT edn_pull('[:person/name :person/age]', 10001, '{"asOf": 1000}');
```

On a named store, `pull(store, pattern, eid, options)` takes the same
options; `q(store, ...)` uses it for a `(pull ?e [...])` find under `asOf` /
`since`.

### `mentat.pull_many(pattern, eids)` / `mentat_pull_many(pattern, eids)`

Pull the same pattern for multiple entities. Returns a JSON array.

```sql
SELECT mentat.pull_many('[:person/name :person/age]', ARRAY[10001, 10002, 10003]);
```

### `mentat.entity(eid)` / `mentat_entity(eid)`

Return all current attributes for an entity as a flat JSON map (equivalent to `pull('[*]', eid)`).

```sql
SELECT mentat.entity(10001);
```

---

## Schema Functions

### `mentat.schema()` / `mentat_schema()`

Return the full schema as JSON, keyed by attribute ident.

```sql
SELECT mentat.schema();
```

---

## Store Management

### `mentat.create_store(name, description)`

Create a new isolated store with its own schema, tables, and indexes.

```sql
SELECT mentat.create_store('analytics', 'Event tracking store');
```

### `mentat.drop_store(name)`

Drop a store and all its data (irreversible).

### `mentat.list_stores()`

List all stores with metadata.

### `mentat.rename_store(old_name, new_name)`

Rename an existing store.

---

## Time Travel Functions

### `mentat.log(store, from_tx, to_tx)`

Return the transaction log for a range of transactions.

```sql
SELECT mentat.log('default', 1000001, 1000010);
```

### `mentat.diff(store, from_tx, to_tx)`

Compute the diff between two points in time — what was added and retracted.

```sql
SELECT mentat.diff('default', 1000003, 1000007);
```

### Time-travel via query parameters

Pass `as_of_tx` or `since_tx` to query functions:

```sql
-- Query the database as of transaction 1000005
SELECT mentat.q('
  [:find ?name :where [?e :person/name ?name]]
', '[]', 1000005, NULL);
```

---

## Excision Functions

### `mentat_excise(entity_ids, store, reason)`

Permanently remove entities from the database, including all history. This is the only operation that truly deletes data (GDPR compliance). The entities' partition must have `allow_excision = true`, and no other entity may still refer to them.

```sql
-- Remove all data for two entities of the default store
SELECT mentat_excise(ARRAY[1000042, 1000043]::BIGINT[]);

-- On a named store, with a reason for mentat.excision_log
SELECT mentat_excise(ARRAY[1000042]::BIGINT[], 'acme', 'GDPR request 17');
```

Excision deletes the entities' datoms from the log and from the current-state
projection, their text from the store's fulltext table, their rows in the
default store's vector and geometry tables, and an excised enum entity's
`:db/ident`, all in the calling transaction. A materialized view
(`materialize`, or any `CREATE MATERIALIZED VIEW` over the store) keeps the
rows it computed until it is refreshed: run `refresh(store, view)` for each
one after excising. Before 1.19.0 excision left the current-state projection
in place, so queries, `pull` and `entity` still returned the excised values.

PostgreSQL itself can still hold copies of excised values for a while. The
deleted rows stay on disk as dead tuples until `VACUUM` reclaims them, and
planner statistics (`pg_stats`: most common values, histogram bounds) keep
sampled values until the next `ANALYZE`. Roles that can read `pg_stats` for
those tables can see them. To clear them at once, run `VACUUM ANALYZE` on the
store's `datoms_<type>_new`, `current_<type>` and fulltext tables after
excising. WAL and existing backups keep their copies until they expire.

Excision sends no subscription notification. A `subscribe` listener keeps the
last payload it received, which can include the excised values, until the
next transaction on the store sends a new one.

---

## Subscription Functions

### `mentat.subscribe(store, name, query [, token_only])`

Subscribe to changes matching a Datalog query pattern. Uses PostgreSQL `LISTEN`/`NOTIFY`:
the channel is `mentat_<name>`. After each write to the store whose result
differs from the last one this session sent, the payload is the query
result as JSON. PostgreSQL limits a payload to just under 8000 bytes; a
larger result is sent as a token instead (`{"tx": T, "hash": H,
"too_large": true}`, see below), so the write still succeeds and the
listener re-runs the query.

```sql
SELECT mentat.subscribe('default', 'new_people',
  '[:find ?e :where [?e :person/name]]');

-- In another session:
LISTEN mentat_new_people;
```

**Who receives it.** PostgreSQL has no access control on `LISTEN`: every
role that can connect to the database can `LISTEN mentat_<name>` and gets
every payload, whatever its privileges on the store. So the query result
reaches roles that have no `USAGE` on the store's schema. To keep it to
the roles that can read the store, subscribe with `token_only => true`:

```sql
SELECT mentat.subscribe('acme', 'payroll', '[:find ?n :where [_ :p/name ?n]]',
  token_only => true);
-- payload: {"tx": 1000000000105, "hash": "9a0364b9e99bb480dd25e1f0284c8555"}
```

The payload is then `{"tx": T, "hash": H}`: `tx` is the id of the
transaction's last `t()` / `edn_t()` on this store, `hash` the md5 of the
result's JSON text. `tx` is null when the write was a plain statement on
the store's `datoms` view in a transaction that called neither on this
store. The listener re-runs the query itself, with its
own privileges. The channel name, and so the subscription name, is still
visible to every listener.

### `mentat.unsubscribe(store, name)`

Remove a subscription.

---

## Materialized View Functions

### `mentat.materialize(store, name, query)`

Create a materialized view from a Datalog query for faster repeated access.

```sql
SELECT mentat.materialize('default', 'active_users',
  '[:find ?e ?name :where [?e :person/name ?name] [?e :person/active true]]');
```

### `mentat.refresh(store, name)`

Refresh a materialized view with current data.

---

## Statistics & Monitoring

### `mentat.stats()` / `mentat_query_stats()`

Query execution statistics: call counts, timing, cache hit rates.

### `mentat.storage()` / `mentat_storage_stats()`

Storage statistics: row counts, table sizes, index sizes.

### `mentat.cache_stats()` / `mentat_stmt_cache_stats()`

Prepared statement cache statistics.

### `mentat.cache_clear()` / `mentat_stmt_cache_clear()`

Clear the statement cache.

### `mentat_health_check()`

Extension health check (returns JSON with status, version, store count).

### `mentat_slow_queries(threshold_ms)`

Return recently logged slow queries exceeding the given threshold.

---

## EDN Helper Functions

These operate on EDN-formatted text values and are installed in the `public` schema for convenience.

| Function | Description |
|----------|-------------|
| `edn_get(edn, key)` | Extract a value from an EDN map |
| `edn_nth(edn, index)` | Extract Nth element from an EDN vector |
| `edn_count(edn)` | Count elements in an EDN collection |
| `edn_keys(edn)` | Keys of an EDN map as a vector |
| `edn_values(edn)` | Values of an EDN map as a vector |
| `edn_contains(edn, key)` | Check if a map contains a key |
| `edn_type(edn)` | Type of an EDN value |
| `edn_pretty(edn, width)` | Pretty-print EDN with indentation |

---

## Bootstrap Functions

### `mentat.bootstrap_schema()`

Re-run the bootstrap schema installation. Called automatically during `CREATE EXTENSION` but can be invoked to repair a corrupted schema.
