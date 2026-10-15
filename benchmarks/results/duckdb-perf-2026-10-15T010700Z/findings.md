# DuckDB extension performance: connection pool and per-call overhead

Branch `duckdb-perf` on top of master a377c4d7 (stores without secondary
indexes). Same host, datasets and drivers as `baseline.md`: a 32-vCPU Xeon
8488C, Debian 12, release extension, DuckDB v1.5.6 Python client, `gen.py`
xs (~0.1M datoms) and s (~1M). `perf.sh` measures single-client p50s
(`bench.py run`, median of 3 reps), single-datom transacts and `conc.py`
throughput (Python threads, each with its own `duckdb` cursor on ONE
database). `writers.py` measures concurrent writes and `ab.sh` + `loop.py`
interleave two builds call by call.

Columns:

- **before**: a377c4d7, the index-free layout (`before-noidx/`). The
  `baseline.md` numbers were taken on the indexed 1.14.0 layout. The two
  differ by less than 0.2 ms per read and ~5 tx/s, so the before/after below
  uses the index-free run.
- **stmt**: 9e43b7b7 + 63c86ce0, statement-overhead changes (`m2-fixed/`).
- **pool**: 42094d96, connection pool + per-store write lock (`m3/`).

## Concurrency (ops/s at 1 / 8 / 32 client threads)

| workload | scale | before | stmt | pool |
|---|---|---:|---:|---:|
| point_lookup | xs | 280 / 321 / 315 | 280 / 322 / 317 | 285 / 1510 / 1574 |
| mixed reads (point, ref, pull) | xs | 154 / 167 / 166 | 154 / 171 / 171 | 160 / 980 / 1039 |
| point_lookup | s | 254 / 294 / 295 | 259 / 299 / 293 | 258 / 1310 / 1262 |
| mixed reads | s | 125 / 132 / 128 | 121 / 132 / 130 | 120 / 598 / 606 |

Point-lookup p50 at 32 threads: xs 101 -> 20 ms, s 108 -> 25 ms. Before the
pool, throughput was flat from 1 to 32 threads (one connection, one global
lock). With the default pool of 8 connections it scales about 5x and then
flattens. Past 8 concurrent calls, the extra calls wait for a connection.
`MENTAT_DUCKDB_POOL` raises the limit.

Writes (`m3/writers.txt`, single-datom `edn_t`, tx/s):

| layout | threads | stmt (serialized) | pool |
|---|---:|---:|---:|
| one store | 1 | 71 | 72 |
| one store | 8 | 69 | 73 |
| a store per thread | 8 | 65 | 291 |

Writes to one store take turns by design (see "Writes" below); writes to
different stores now run in parallel.

## Single client (p50 ms)

| scenario | xs before | xs pool | s before | s pool |
|---|---:|---:|---:|---:|
| point_lookup | 3.47 | 3.43 | 3.80 | 3.79 |
| pull | 3.52 | 3.46 | 3.85 | 3.80 |
| input_bindings | 5.64 | 5.58 | 6.37 | 6.18 |
| ref_traversal | 11.04 | 10.91 | 15.54 | 14.77 |
| aggregate | 4.99 | 4.93 | 6.40 | 6.10 |
| as_of | 24.16 | 24.12 | 28.21 | 28.60 |
| since | 2.40 | 2.40 | 2.82 | 2.80 |
| single-datom transact (tx/s) | 55 | 58 | 50 | 54 |

Interleaved per-call A/B on xs (`m2-fixed/ab-xs.log`, 3 rounds): point
lookup 3.50 -> 3.47 ms, pull 3.54 -> 3.54, transact 18.50 -> 17.22 ms.

Single-client latency barely moved, and that is what the profile predicts. Of a
2.8 ms in-store point lookup, 2.2 ms is DuckDB executing the query's SQL: two
filtered scans of `datoms` and a hash join. Only ~0.13 ms of that is
preparing it, and mentat's Rust is ~0.2 ms. In `perf record` of a loop of
extension point lookups, mentat's own code is 1.4% of the samples.

## What changed

1. **Connection pool** (`crates/duckdb/src/lib.rs`). The entrypoint opens
   `MENTAT_DUCKDB_POOL` connections (default one per core, at most 8; the
   database handle is only valid during the entrypoint, so they can't be
   opened later). `try_clone()` repeated N times inside the entrypoint works.
   Each call leases one and returns it on drop; with all leased, a call
   waits on a condvar.
2. **Writes: a per-store lock** (`DuckStore::transact`, store creation, the
   legacy-index drop). Without it, the harness's 8 concurrent writers fail on
   their first transaction with "TransactionContext Error: Conflict on
   update!": every transaction updates `known_parts`, and DuckDB's optimistic
   concurrency lets one of two concurrent updaters win. Retrying would also
   work. The lock is simpler, and a single store's writes can't overlap anyway.
   It is per process; another process can't open the database for writing.
3. **Schema cache** is per thread, revalidated against `meta.generation` on
   every call. It doesn't depend on which connection a call gets, so it is
   unchanged.
4. **One generation read fewer per transact** (`open` returns the generation it
   validated; `transact` compares against it inside the transaction), and
   `known_parts` is updated only for partitions whose index moved (1 UPDATE
   instead of 3 for an ordinary transaction).
5. **Prepared-statement cache, opt-in.** `SqlConn::query_cached` /
   `execute_cached` keep a statement prepared. The store uses them only for
   single-table statements on its own bookkeeping tables, with every varying
   value bound: the generation read, the `known_parts` read and the
   `known_parts` UPDATE. Engine queries, joins and the transact DML are
   prepared fresh.

## The prepared-statement bug (and why engine queries aren't cached)

9e43b7b7 cached every statement. That broke queries: with two stores on one
connection, a count on the second store kept returning 0 after it had
datoms. On DuckDB 1.5.x a reused prepared plan keeps the column statistics it
was planned with. A filter with a constant outside them (the engine inlines
entids: `datoms.a = 65536` before that attribute has datoms) keeps matching
nothing after rows arrive. `repro-stale-prepared.sql` shows it in plain SQL.
A bound parameter (`a = $1`) is not affected there; the join-statistics
variant is. Upstream: duckdb/duckdb#23704 / #21207, fixed by PR #24696
(merged to main 2026-08-14, in no 1.5.x release); joins: #24182. 63c86ce0
restricts caching as above. Two harness tests fail on 9e43b7b7 and pass
since. Caching engine queries can be reconsidered once the pinned DuckDB
includes #24696 and #24182; it would save ~0.1-0.2 ms per call.

## Not done

- **Faster single-client reads.** The cost is DuckDB scanning `datoms`. Things
  tried and measured:
  - A sorted copy of `datoms` cut the join from 2.4 to 1.8 ms (`probes/layout.py`).
  - Pre-filtering the transact search join by entity made it slower
    (2.4 -> 3.4 ms, `probes/search.py`).
  - Disabling statistics propagation changed nothing (`probes/statcost.py`).

  Clustering `datoms` by `(a, e)` on checkpoint is the most promising lead.
- **Transact cost** (17 ms) is mostly DuckDB too: COMMIT 3.5 ms, the search
  `INSERT ... SELECT` 3.4 ms, and recreating three temp tables per transaction
  (0.9 ms). Reusing the temp tables with `DELETE` measured the same
  (`probes/cine.py`).
- **The WAL replay index bug** (duckdb/duckdb#26828) was found here and is
  handled on master (a377c4d7); `repro5.sh` and `repro-mentat.sh` reproduce it.

## Gate (on 42094d96, the perf box)

| check | result |
|---|---|
| `cargo fmt --all --check` (+ tests-harness) | pass |
| `cargo clippy -p mentat_duckdb -p mentat_duckdb_store -- -D warnings` | pass, 0 warnings |
| `cargo test --manifest-path crates/duckdb/store/tests-harness/Cargo.toml` | pass: differential (0 disagreements vs SQLite), store 10, script_model 15, concurrency 1 |
| `make debug && DUCKDB=~/duckdb bash test/smoke.sh` | PASS |
| `python test/concurrency.py build/debug/mentat.duckdb_extension` | OK |
| SQLLogicTest (`duckdb==1.5.6`, duckdb-sqllogictest-python@2ac8dbc) | SUCCESS, 1/1 |
| workspace `cargo test` (default members) | pass: 1769 passed, 0 failed, 30 ignored |
