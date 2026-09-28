# Operations: Throughput, Bloat, and the Live Projection

This page covers the operational concerns that matter when pg_mentat is
the identity / knowledge-graph backbone of a production service: how to
make `mentat.t()` ingest fast, how to keep the narrow datom tables from
bloating, and how to read the "current value" of an attribute cheaply.

It is written against real production feedback (an 82 GB store used as a
community-stats identity backbone). The version that introduced the
accessors and autovacuum defaults described here is **1.4.0**.

## 1. `mentat.t()` throughput

### What costs time per call

Each `mentat.t()` call does, regardless of batch size:

1. Allocate a transaction id (`nextval` on the tx sequence) and insert
   the `mentat.transactions` row + the `:db/txInstant` datom.
2. Parse the EDN, resolve idents / tempids, validate constraints.
3. Per **cardinality-one** datom, look up the current value to decide
   assert / replace / skip.
4. Batch-insert the new datom rows (one INSERT per touched type table).

Step 3 used to run a **9-way `UNION ALL`** probe per datom. As of 1.4.0
it is a single indexed lookup on the one narrow table matching the
value's type — a measured **~1.8× speedup** on a cardinality-one
re-assertion workload (6.2 s → 3.4 s for 2000 calls in the project's
microbenchmark).

The residual per-call floor (~1–2 ms in that benchmark; higher under
production latency and replication) is **tx allocation + the txInstant
datom + savepoint setup**. It is *fixed per call*, so the way to amortize
it is fewer, larger transactions — not smaller batches.

### Make backfills fast: one tx, many facts

Batch as many assertions as possible into a **single** `mentat.t()`
call. The per-call overhead is paid once; the per-datom cost scales
linearly and is cheap. A 250k-fact backfill batched at, say, 5,000
facts/tx is 50 calls — not 1,250.

```sql
-- Good: 5000 facts, ONE tx, ONE tx-allocation overhead.
SELECT mentat.t($edn$[
  {:db/id "c1" :contribution/key "..." :contribution/kind :commit ...}
  {:db/id "c2" ...}
  ... 4998 more ...
]$edn$);
```

The larger the batch, the closer you get to the per-datom floor. There
is no fixed upper bound other than statement memory; batches of several
thousand facts are routinely fine.

### Idempotent re-assertion is already a no-op for the data

If your sync re-asserts a cardinality-one fact whose value already
matches the current value, pg_mentat takes the **Skip** path: no new
datom is written, no retraction is written, the datom table is not
churned. You still pay the per-call tx overhead, so the same advice
applies — batch the no-op re-assertions into few large transactions and
the cost disappears into the per-call floor.

> **Tip.** If your nightly mirror is mostly no-ops (idempotent by a
> `:contribution/key`-style natural key), the cheapest thing you can do
> is widen the batch. The Skip path means the table doesn't grow; the
> only cost left is the per-`t()` tx allocation, which batching
> amortizes.

## 2. Autovacuum and bloat

### The default-scale-factor trap

PostgreSQL's default `autovacuum_vacuum_scale_factor = 0.2` means a
table is vacuumed only after 20% of its rows are dead. On a 50M-row
narrow table that is **10M dead tuples** of slack — autovacuum
effectively never fires, and the table (especially its PK and EAVT
index) bloats without bound. The instant-typed table is the worst case
because monotonic attributes (`:first-seen`, `:last-seen`,
`:observed-at`) are re-asserted on every sync, each generating a
retraction + assertion.

### What 1.4.0 ships

`CREATE EXTENSION pg_mentat` (and the 1.3.0→1.4.0 upgrade) now sets, on
**all nine** `datoms_*_new` tables and on `mentat.transactions`:

```
autovacuum_vacuum_scale_factor  = 0
autovacuum_vacuum_threshold     = 50000
autovacuum_analyze_scale_factor = 0
autovacuum_analyze_threshold    = 50000
```

Scale-factor 0 + a fixed 50k-dead-tuple threshold means autovacuum
fires on a **constant** amount of dead tuples regardless of table size.
High-churn deployments can lower the threshold further per table:

```sql
ALTER TABLE mentat.datoms_instant_new
  SET (autovacuum_vacuum_threshold = 10000);
```

### Reclaiming existing bloat

Storage params change *future* triggering; they do not shrink a table
that is already bloated. To reclaim:

```sql
-- Online, no exclusive lock, needs pg_repack installed:
pg_repack -t mentat.datoms_instant_new -d yourdb

-- Or, during a maintenance window (takes an ACCESS EXCLUSIVE lock):
VACUUM FULL mentat.datoms_instant_new;
```

Schedule a periodic VACUUM via [pg_cron](./pg_cron.md):

```sql
SELECT mentat.cron_schedule_vacuum_datoms('0 4 * * *');
```

### Monitoring before it bites

`mentat.attribute_health()` reports live datom counts and the
dead-tuple % of each backing table:

```sql
SELECT * FROM mentat.attribute_health() ORDER BY dead_pct DESC;
```

```
   attr_ident    | value_type |    backing_table      | live_datoms | dead_pct
-----------------+------------+-----------------------+-------------+----------
 :person/seen    | instant    | mentat.datoms_instant_new | 124032  |     31.4
 :person/email   | string     | mentat.datoms_text_new    | 41200   |      2.1
 ...
```

Alert on `dead_pct > 25` to catch bloat before it costs you query
latency or disk.

> **Note on `:last-seen`-style attributes.** Keeping full history of a
> value that changes every sync is inherently bloat-generating: each
> change is a retraction + an assertion. If you do not need the history
> of a monotonic timestamp, mark the attribute **`:db/noHistory true`**
> (see below) — noHistory attributes keep only the current value, so
> they generate no history trail and cannot bloat.

### `:db/noHistory` — non-historical attributes

As of 1.5.0 the datom log is **append-only**: a retraction is a new
immutable datom, never an in-place flip of the prior assertion. That
makes history exact, but it also means an attribute whose value changes
every sync (the `:observed-at` / `:last-seen` class) accumulates one
assert + one retract datom per change — unbounded growth.

Mark such an attribute `:db/noHistory true` to opt out of history:

```clojure
{:db/ident :host/last-seen
 :db/valueType :db.type/instant
 :db/cardinality :db.cardinality/one
 :db/noHistory true}
```

For a `:db/noHistory` attribute, each assertion **physically replaces**
the prior value in the log (and the projection) instead of appending a
retraction + assertion. The log holds exactly the current value:

```sql
-- After 10 updates to a noHistory :host/last-seen:
SELECT count(*) FROM mentat.datoms_instant_new
  WHERE e = :host AND a = mentat.attr_id(':host/last-seen');
-- => 1   (a normal attribute would have ~20 rows: 10 asserts + 10 retracts)
```

Semantics:

- **Current-time queries are unchanged** — `[?h :host/last-seen ?t]`
  returns the current value exactly as for a normal attribute.
- **`:as-of` / history queries see only the current value**, because no
  prior versions are retained. This is the deliberate trade: you give
  up time-travel on that attribute in exchange for zero bloat.
- **Per-attribute, not global** — a noHistory attribute and a
  full-history attribute on the same entity each behave correctly.
- This is Datomic-compatible: Datomic's `:db/noHistory` has the same
  "keep only the current value" meaning.

Use it for high-churn, history-irrelevant values (heartbeats,
last-seen timestamps, observed counters). Do **not** use it for
attributes whose history you audit or time-travel.

## 3. The live projection: `mentat.current()` and `mentat.attr_id()`

### The problem with DISTINCT ON / LATERAL views

A view that resolves "the latest value of attribute A for entity E"
typically looks like:

```sql
SELECT DISTINCT ON (e) e, v
FROM mentat.datoms_text_new
WHERE a = <attr> AND added
ORDER BY e, tx DESC
```

…with a `LEFT JOIN LATERAL (... ORDER BY tx DESC LIMIT 1)` per extra
attribute. That recomputes the latest-per-`(e, a)` on every read and
fans out one lateral per attribute — the dominant cost when refreshing a
materialized view that joins many attributes.

### `mentat.current(e, a)`

`mentat.current()` returns the current value of one attribute for one
entity as text, with a single indexed lookup on the
`(store_id, e, a, tx DESC) WHERE added` covering index. It dispatches on
the attribute's declared value type so only one narrow table is touched.

```sql
-- By attribute keyword (resolves the entid for you):
SELECT mentat.current(12345, ':person/canonical-email');

-- Or by attribute entid, if you already have it:
SELECT mentat.current(12345, mentat.attr_id(':person/canonical-email'));
```

Use it in a view to replace the DISTINCT ON / LATERAL machinery:

```sql
CREATE VIEW community.persons AS
SELECT
    e AS person_id,
    mentat.current(e, ':person/canonical-email') AS email,
    mentat.current(e, ':person/name')            AS name,
    mentat.current(e, ':person/employer')        AS employer
FROM (
    -- the set of person entities (one row per entity)
    SELECT DISTINCT e FROM mentat.datoms_text_new
    WHERE a = mentat.attr_id(':person/canonical-email') AND added
) p;
```

Each `mentat.current()` call is an index lookup; the planner folds the
STABLE function once per row. This is dramatically cheaper than a
per-attribute LATERAL on a large fan-out.

### A maintained current-state index

For the absolute hottest read paths, back `mentat.current()` with a
covering index per attribute type (most projections read text/keyword):

```sql
-- Already shipped: the *_tx covering index supports the lookup:
--   (store_id, tx DESC) INCLUDE (e, a, v) WHERE added
-- For a per-attribute hot path, add a partial index:
CREATE INDEX idx_person_email_current
  ON mentat.datoms_text_new (e, tx DESC)
  WHERE a = <:person/canonical-email entid> AND added;
```

A fully maintained "current datoms" table (kept in sync on `t()`) is on
the roadmap; today the covering-index + `mentat.current()` combination
gives index-backed reads without it.

### `mentat.attr_id()`

`mentat.attr_id(':ns/name')` resolves an attribute keyword to its entid
for use in SQL and view definitions, so generated viewdefs read
`a = mentat.attr_id(':person/name')` instead of an opaque
`a = 1308861`. It is STABLE, so the planner evaluates it once.

## 4. Health dashboard

| Function | Returns |
|:---|:---|
| `mentat.attr_id(':ns/n')` | The attribute's entid (BIGINT), or NULL. |
| `mentat.current(e, a)` | Current value of attribute for entity, as TEXT. |
| `mentat.attribute_health()` | Per-attribute live datom count + backing-table dead %. |
| `mentat.storage()` | Per-table size + row estimates (pre-existing). |
| `mentat.stats()` | Query-execution statistics (pre-existing). |

A minimal alerting query:

```sql
SELECT attr_ident, live_datoms, dead_pct
FROM mentat.attribute_health()
WHERE dead_pct > 25
ORDER BY dead_pct DESC;
```

## What is NOT solved by auto-indexing

A common first instinct for "pg_mentat is slow" is *missing indexes*.
The shipped set is EAVT / AEVT / VAET / tx covering indexes on the
history tables, AEV + AVET (1.10.0) on the current-state projection, and
an FTS GIN index -- so equality lookups, ref traversal and current-state
value ranges are already index probes. The costs that actually hurt in
production are:

- **per-`t()` transaction overhead** -- solved by batching, not indexing;
- **history-resolution on reads** -- solved by the current-state
  projection (current-time queries read it directly);
- **dead-tuple bloat** -- solved by autovacuum tuning + scheduled
  vacuum, not by indexing.

The one gap the automatic index manager (section 7) fills is value
ranges on one attribute in *temporal* queries. Beyond that, extra indexes
will not move these numbers and will slow `t()` further (every index is
maintained on write).

## 5. Reading from a hot standby (read replica)

The Datalog read path (`mentat.q`, `mentat.pull`, `mentat.entity`,
`:as-of` / `:since` time-travel, `mentat.lookup_by_ident`, and the
`mentat.has_<ext>()` extension detectors) runs on a PostgreSQL hot standby
(streaming-replication read replica). As of 1.5.3–1.5.4 these paths use
read-only SPI and set their resource-limit GUCs via `set_config_option`
rather than a SQL `SET`, so they never try to assign a transaction id
during recovery.

What this means operationally:

- Serve read APIs from the replica. A query such as
  `SELECT mentat.q('[:find ?e :where [?e :db/ident :db/ident]]')` runs on
  the standby with no error.
- Writes (`mentat.t`, excision, entid allocation, schema changes) are
  primary-only by nature — they take a mutable transaction and cannot run
  on a standby. That is expected; route writes to the primary.

No configuration is required; it works whenever the extension is installed
on both hosts (as it must be for replication).

## 6. Entity-id partitions and collision repair

pg_mentat allocates entity ids from three disjoint, bounded per-partition
sequences:

| partition | band | source sequence |
|---|---|---|
| `db.part/db`   | `[0, 1e6)`       | `partition_db_seq`   (schema / bootstrap) |
| `db.part/user` | `[1000001, 1e12)`| `partition_user_seq` (data entities)      |
| `db.part/tx`   | `[1e12, 2e12)`   | `partition_tx_seq`   (transactions)       |

Since 1.5.6 each sequence carries a `MAXVALUE` at its band ceiling, so an
exhausted partition **fails loud** (`nextval: reached maximum value of
sequence`) instead of silently issuing ids that collide with the next
partition's space. `db.part/tx` consumes one id per `mentat.t`; its band
(`~1e12` ids) is effectively unbounded at any realistic write rate.

### Pre-1.5.6 stores: overflow and collisions

Stores created before the bands were bounded ran their sequences
**unbounded to bigint-max**. A long-lived, write-heavy store can therefore
have overflowed the old `[1e4,1e6)` user band and `[1e6,2e6)` tx band into
one another, producing entids used as BOTH a transaction and a user/schema
entity — one integer, two logical entities, so `mentat.entity(E)` returns
the union of both.

Check for this after upgrading:

```sql
SELECT mentat.entid_collision_count();          -- 0 == healthy
SELECT * FROM mentat.entid_collision_report();   -- one row per colliding entid
```

`entid_collision_report()` lists each colliding entid, how many non-tx
datoms it carries, whether it is used as an attribute, and its incoming ref
count.

### Repairing collisions

`mentat.repair_entid_collisions(dry_run BOOLEAN DEFAULT true, store BIGINT
DEFAULT 0)` renumbers the colliding **non-transaction** entities into fresh
user-band ids — rewriting `e`, `a`, and incoming ref `v` across the nine
log tables and nine current-projection tables, plus the schema/idents
catalogs. The **transaction keeps its id**: a tx id is woven through every
datom's `tx` column and anchors `basis-t` / `:as-of` monotonicity, so it
must not move.

The repair is destructive. It defaults to a dry run. The safe procedure:

```sql
-- 1. See how many would be remapped, changing nothing:
SELECT mentat.repair_entid_collisions(true);

-- 2. Back up the database.

-- 3. In your own transaction, perform the repair:
BEGIN;
SELECT mentat.repair_entid_collisions(false);
SELECT mentat.entid_collision_count();   -- confirm 0 before COMMIT
COMMIT;
```

On a store with many collisions the repair rewrites every datom of each
colliding entity, so run it in a maintenance window and expect it to scale
with the number of affected datoms, not with total store size.

### The 1.5.6 upgrade caveat (fixed in 1.5.7)

The 1.5.5→1.5.6 migration bounded the sequences with a fixed `MAXVALUE`,
which **aborts** if a sequence has already run past that ceiling
(`RESTART value cannot be greater than MAXVALUE`). Upgrade **directly to
1.5.7 or later**: it ships a direct `1.5.5→1.5.7` path that bounds each
sequence to `GREATEST(intended_ceiling, current_head)` — never below the
live head — so an overflowed store keeps working. Do not stop at 1.5.6 on
a store whose sequences may have overflowed.

## 7. Automatic index management

`mentat_tune_indexes(dry_run BOOLEAN DEFAULT true)` adds indexes where
query evidence says they help and drops the ones it added once they stop
being used. It is small on purpose: one kind of index, two rules, one
registry.

```sql
SELECT * FROM mentat_tune_indexes();        -- what would change (default: dry run)
SELECT * FROM mentat_tune_indexes(false);   -- do it
SELECT * FROM mentat.managed_indexes;       -- what it owns, and why
```

Each row returned is `(action, index_name, table_name, reason)` with
`action` one of `create`, `drop`, `forget` (a managed index someone
dropped by hand), or `skip` (see below). Every create / drop is also
`LOG`ged with its reason. `dry_run` changes no index and no registry row.

**Evidence.** Every compiled query with a range predicate (`<`, `<=`,
`>`, `>=`) on the value of a constant-attribute pattern that reads a
history table (as-of / since / history queries) counts one hit for
(store, attribute, table). Hits are per backend and flushed to
`mentat.index_evidence` every `mentat.auto_index_every_n_tx` `edn_t`
calls and by `mentat_tune_indexes()` itself.

**Create rule.** An attribute with at least `mentat.auto_index_min_queries`
hits and at least `mentat.auto_index_min_rows` rows in its history table
gets

```sql
CREATE INDEX mentat_auto_datoms_<type>_new_a<entid>
  ON mentat.datoms_<type>_new (store_id, v, e, tx) WHERE a = <entid> AND added;
```

Why only this: current-time queries read the projection, whose AVET
index `(store_id, a, v, e)` already turns a range on one attribute into
an index range scan (measured flat from 1M to 10M datoms). The history
tables' VAET index leads with `v`, not `a`, so a temporal range query on
one attribute otherwise scans all of that attribute's values through
AEVT with a `Filter` (4.9 ms vs 23.6 ms for a 25% range of 200k values).
The predicate carries only literals (`store_id` is a bound parameter in
the generated SQL, so it is a key column), and the generated SQL pushes
the attribute down as a literal `a = <entid>`, so the planner can match
it. Caveat: `edn_q` caches prepared statements, and after five runs
PostgreSQL may switch to a generic plan, which assumes `v >= $n` matches
a third of the rows and can prefer AEVT's `e` order for the as-of
anti-join. If the index is then never scanned, the vestigial rule
removes it.

**Vestigial rule.** A managed index whose `idx_scan` has not grown since
it was last seen in use, for at least `mentat.auto_index_idle_window`
(default 7 days), while its table took writes, is dropped. Hysteresis:
nothing is dropped within one window of its creation, and any scan
restarts the clock. A read-only table never loses its indexes (an idle
index there costs nothing).

**Only its own.** The drop rule reads only `mentat.managed_indexes`,
whose `CHECK` pins names to `mentat_auto_*`: primary keys, the shipped
indexes, M1's AVET indexes and user indexes are never candidates.

**Modes** (`mentat.auto_index`, see [configuration](configuration.md)):
`schema` (default) collects evidence and acts only when you call
`mentat_tune_indexes`; `adaptive` also runs it every
`mentat.auto_index_every_n_tx` transactions of a backend, with
`mentat.auto_index_lock_timeout` (default 100 ms) on the `CREATE INDEX`
lock: if it would wait longer the run is skipped and `LOG`ged
(`mentat auto_index: tuning skipped (55P03 ...)`) and retried at the next tick (the evidence is kept), and
the triggering `edn_t` succeeds regardless. Note that the adaptive build
is a plain `CREATE INDEX` inside the caller's transaction: it blocks
writes to that table for the build (seconds per million rows). On a
large store, prefer `schema` mode and run `mentat_tune_indexes(false)` in
a maintenance window, or create the reported index yourself `CONCURRENTLY`
under the reported name and insert its registry row.

## 8. Scaling notes: large results and big stores

Measured on a 32-vCPU box, PostgreSQL 16.15, current-time queries
(`benchmarks/scale`, 1M and 10M datoms; see
`benchmarks/results/pg-autoindex-*` for the full tables).

- **Point lookups and ref traversal are flat in store size.** A
  value-bound pattern (`[?e :user/email ?x]`) is an AVET index probe on
  the current-state projection: ~0.19 ms at 1M and at 10M datoms.
- **Aggregates scale with the attribute, not the store.** `(count ?x)`
  whose tuple is a key of the join (a single cardinality-one pattern, or a
  chain of them) runs as `COUNT(*)` with parallel workers (10M datoms, 1.3M
  values: ~55 ms). When the tuple is not a key (a cardinality-many join, a
  non-unique value) it is de-duplicated first with `SELECT DISTINCT` +
  HashAggregate -- correct, but proportional to the join's size.
- **Results are bounded by `mentat.max_result_rows` (100k).** `edn_q`
  builds its answer as one JSONB value in memory (PostgreSQL caps a
  value at 1 GB). For large results use `edn_q_rows`, which returns one
  row per result and can be consumed by `COPY (SELECT ...) TO STDOUT` or
  a client-side cursor, together with `SET LOCAL mentat.max_result_rows =
  0` -- or paginate with `{"limit": N, "offset": M}` inputs (offsets are
  O(offset); prefer a range predicate on a key for deep pages).
- **Spills.** Large DISTINCT / sort steps spill to disk under
  `work_mem`; `mentat.temp_file_limit` (1 GB) bounds that per query and
  the error names it. Raising `work_mem` for the session (or
  `mentat.default_work_mem` with optimizer hints on) keeps them in memory.
- **Temporal range queries** (as-of / since / history with `<`/`>` on a
  value) are the one access path the shipped indexes do not cover
  per attribute; the automatic index manager (section 7) adds those
  indexes from observed use.

