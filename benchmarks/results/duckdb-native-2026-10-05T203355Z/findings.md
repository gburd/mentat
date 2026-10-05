# DuckDB extension: datoms in DuckDB (1.11) vs a SQLite file (1.10.3)

Same dataset (benchmarks/scale gen.py), same extension calls, one client,
release builds, DuckDB v1.5.6 Python client, EC2 c7i.8xlarge (32 vCPU, Xeon
8488C, 61 GiB). 1.10.3's extension keeps its datoms in a SQLite file (run
with an in-memory DuckDB); 1.11's keeps them in tables of a persistent DuckDB
database. Driver: `ab.sh` (SC=xs|s); tables: `abtab.py`. Correctness is
checked separately (tests-harness differential test vs SQLite: 0
disagreements), so these are timings only.

## xs (~0.1M datoms)

| scenario (p50 ms, 1 client) | 1.10.3: datoms in a SQLite file | 1.11: datoms in DuckDB | new / old |
|---|---:|---:|---:|
| point_lookup | 0.58 | 2.79 | 4.78 |
| ref_traversal | 2.62 | 6.80 | 2.60 |
| aggregate | 3.02 | 3.68 | 1.22 |
| predicate_scan | 3.47 | 9.97 | 2.87 |
| pull | 0.55 | 2.98 | 5.43 |
| as_of | 8.77 | 19.98 | 2.28 |
| since | 1.26 | 2.34 | 1.85 |
| input_bindings | 0.92 | 4.90 | 5.33 |
| bulk load (s) | 2.3 | 5.7 | 2.51 |
| single-datom transact (tx/s) | 186 | 66 | 0.36 |
| store size (MB) | 21 | 16 | 0.74 |

## s (~1M datoms)

| scenario (p50 ms, 1 client) | 1.10.3: datoms in a SQLite file | 1.11: datoms in DuckDB | new / old |
|---|---:|---:|---:|
| point_lookup | 0.60 | 3.23 | 5.40 |
| ref_traversal | 22.39 | 10.37 | 0.46 |
| aggregate | 30.15 | 5.53 | 0.18 |
| predicate_scan | 28.89 | 40.43 | 1.40 |
| pull | 0.56 | 3.83 | 6.85 |
| as_of | 88.57 | 25.46 | 0.29 |
| since | 5.25 | 2.73 | 0.52 |
| input_bindings | 1.12 | 8.78 | 7.82 |
| bulk load (s) | 177.4 | 62.7 | 0.35 |
| single-datom transact (tx/s) | 182 | 54 | 0.29 |
| store size (MB) | 213 | 157 | 0.74 |

## Reading it

- **Faster on DuckDB at 1M datoms:** queries that touch many datoms.
  Aggregates 5.5x, as-of 3.5x, ref traversal 2.2x, since 1.9x, and bulk load
  2.8x. The store is a quarter smaller. These get better with scale, because
  DuckDB scans columns and the SQLite store probes its indexes row by row.
- **Slower on DuckDB:** point lookups, pull and `:in` bindings, by about
  3-8 ms each, and single-datom transactions (~55 tx/s vs ~180). These are
  per-statement costs. Each call runs several statements (a schema-generation
  check, the query, and for pull the attribute fetch), and DuckDB plans and
  runs each in ~0.2-1 ms, where SQLite answers an indexed lookup in
  microseconds. DuckDB can't index the value column (a UNION) and doesn't use
  its ART indexes for these joins, so a value lookup is a scan of the
  attribute's rows.
- Two optimizations got it here, both measured at xs:
  - Caching each store's schema per thread (per-call overhead 4.4 -> 2.1 ms).
  - Plain typed copies of the value (`v_i`/`v_d`/`v_s`) for filters, joins
    and comparisons. Ref traversal went 12.1 -> 6.8 ms; on the UNION alone,
    DuckDB took 16 ms for a join that takes 3.3 ms on plain columns.
- Not done (possible next steps): batch a call's statements; a connection
  pool so calls run concurrently (they are serialized on one connection
  today); clustering `datoms` by `(a, e)` for zone-map pruning (2.6 vs
  3.3 ms on the ref traversal probe).
