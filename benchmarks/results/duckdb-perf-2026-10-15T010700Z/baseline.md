# DuckDB extension performance: baseline (master @ 39f68fb0)

EC2 c7i.8xlarge-class host (32 vCPU Xeon 8488C, 61 GiB, Debian 12), release
extension, DuckDB v1.5.6 Python client, datasets from `benchmarks/scale/gen.py`.
Driver: `perf.sh` (`TAG=before SC=xs|s`), concurrency: `conc.py`. Raw output is
in `before/<scale>/`. Every pass starts from the same loaded store, after one
explicit `CHECKPOINT` (see "WAL replay" below).

## Single client (p50 ms, median of 3 reps)

| scenario | xs (~0.1M datoms) | s (~1M datoms) |
|---|---:|---:|
| point_lookup | 3.33 | 3.75 |
| pull | 3.35 | 3.74 |
| input_bindings | 5.75 | 14.58 |
| ref_traversal | 10.73 | 15.12 |
| aggregate | 5.98 | 20.13 |
| as_of | 23.38 | 27.99 |
| since | 2.37 | 2.80 |
| single-datom transact (tx/s) | 50 | 51 |

## Concurrency (ops/s; Python threads, one cursor each, one database)

| workload | threads | xs ops/s | xs p50 ms | s ops/s | s p50 ms |
|---|---:|---:|---:|---:|---:|
| point_lookup | 1 | 308 | 3.2 | 259 | 3.8 |
| point_lookup | 8 | 357 | 22.4 | 297 | 26.9 |
| point_lookup | 32 | 353 | 90.4 | 290 | 110.0 |
| mix (point/ref/pull) | 1 | 179 | 3.5 | 121 | 4.6 |
| mix | 8 | 183 | 38.6 | 132 | 52.2 |
| mix | 32 | 178 | 167.3 | 131 | 215.3 |

Throughput is flat from 1 to 32 threads: every mentat call takes the
extension's one global lock, on its one connection.

## Where one call's time goes

`crates/duckdb/store/tests-harness/tests/profile.rs` (`call_breakdown`) wraps
the store's `SqlConn` and times every statement, on a copy of the loaded store
(bundled DuckDB 1.5.6, same engine). Per call, xs:

| point_lookup (2.83 ms in the store) | ms |
|---|---:|
| `meta.generation` read (prepared each time) | 0.33 |
| `SET search_path` | 0.10 |
| the Datalog query's SQL, of which prepare is 0.13 | 2.20 |
| mentat in Rust (parse, algebrize, project) | 0.21 |

| pull `[*]` (2.92 ms) | ms |
|---|---:|
| generation read + `SET search_path` | 0.42 |
| `?e` existence probe (`WHERE e = n LIMIT 1`) | 0.53 |
| attribute fetch (`e IN (n) AND a IN (...)`) | 1.73 |
| Rust | 0.25 |

| single-datom transact (16.8 ms) | ms |
|---|---:|
| `meta.generation` read, 3 times | 0.85 |
| drop + create 3 temp tables | 0.95 |
| search `INSERT ... SELECT` (LEFT JOIN datoms) | 3.6 |
| datoms delete + insert, 2 timeline inserts | 4.1 |
| `known_parts` update, 3 times | 1.0 |
| `COMMIT` | 3.7-5.2 |
| partition-map read, BEGIN, the rest | 0.6 |

From `statement_costs`: a fresh prepare of the generation read costs 0.18 ms and
a cached prepared statement 0.04 ms. The point query costs about the same
cached or not (1.80 vs 1.90 ms). `perf record` of a loop of extension point
lookups puts 1.4% of samples in mentat's own code; the rest is DuckDB (scans,
`RLEFilter`, the task scheduler, allocation).

So caching statements and cutting duplicated per-call statements is worth
about 0.4-0.5 ms per call. The rest of a point lookup is DuckDB executing the
query, which this round doesn't change.

## WAL replay (DuckDB bug, found here)

The first baseline ran on stores that had only ever been written by the
extension: the database never checkpoints while mentat is loaded, so the load
was entirely in the WAL (157 MB WAL beside a 12 KB file at s). Opening such a
database once with a plain DuckDB replays the WAL and checkpoints it on close.
After that, `SELECT count(*) FROM datoms WHERE a = 65538` returns 0, while
`a BETWEEN 65538 AND 65538` returns the rows: an `ART` index on `(a)` misses
the replayed rows. `repro5.sh` reproduces it with no mentat at all. This is
upstream duckdb/duckdb#26828; the fix (PR #26814) is unreleased. The lead is
handling the mentat-side mitigation on master. These benchmarks checkpoint the
loaded store once, explicitly (which gives correct results), before any
measurement.
