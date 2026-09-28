# pg_mentat 1.10.0: AVET, count, auto-index, edn_q_rows -- results

c7i.8xlarge (32 vCPU Xeon 8488C, 61.8 GB), Amazon Linux 2023, PostgreSQL
16.15 release build (`shared_buffers` 16GB, `work_mem` 64MB,
`max_parallel_workers_per_gather` 4), `benchmarks/scale` with
`PHASE=bench SCALES="s m" BACKENDS=pg EXTRA=concurrency_sweep CLIENTS="1 8 32"
REPS=3`. s = 984,803 datoms (1,600 users, 130,000 issues), m = 9,850,258
(16,000 users, 1.3M issues).

Three runs of the same suite over the same datasets:

- **v1.9.0** -- `~/results/before-190` (the 1.9.0 install);
- **after M1** -- `~/results/after-m1` (3a1d1cd8, AVET indexes);
- **final** -- `scale/` here (46b47573 plus the auto-index skip rule),
  all 36 correctness checks pass.

`compare.txt` has `benchmarks/scale/compare.py` for final against both
baselines (p50, p99, throughput): **0 regressions at 10%** against either.

## Summary (p50 ms at 1 client; concurrency_sweep in ops/s)

| scenario | scale | clients | v1.9.0 | after M1 | final | unit |
|---|---|---:|---:|---:|---:|---|
| aggregate (q3) | m | 1 | 1427.821 | 1445.204 | 57.659 | p50 ms |
| aggregate (q3) | s | 1 | 111.918 | 34.551 | 9.172 | p50 ms |
| as_of | m | 1 | 2.022 | 2.032 | 1.962 | p50 ms |
| as_of | s | 1 | 1.713 | 1.653 | 1.643 | p50 ms |
| concurrency_sweep | m | 1 | 570.600 | 1204.900 | 1262.500 | ops/s |
| concurrency_sweep | m | 8 | 4401.200 | 9508.700 | 9067.000 | ops/s |
| concurrency_sweep | m | 32 | 12524.000 | 28538.600 | 30718.200 | ops/s |
| concurrency_sweep | s | 1 | 1176.000 | 1297.200 | 1277.600 | ops/s |
| concurrency_sweep | s | 8 | 9599.500 | 10379.800 | 10234.700 | ops/s |
| concurrency_sweep | s | 32 | 27560.900 | 31113.900 | 32569.000 | ops/s |
| input_bindings | m | 1 | 2.697 | 0.926 | 0.922 | p50 ms |
| input_bindings | s | 1 | 1.078 | 0.834 | 0.814 | p50 ms |
| point_lookup (q1) | m | 1 | 1.654 | 0.190 | 0.191 | p50 ms |
| point_lookup (q1) | s | 1 | 0.425 | 0.185 | 0.191 | p50 ms |
| predicate_scan (q4) | m | 1 | 764.220 | 619.823 | 612.583 | p50 ms |
| predicate_scan (q4) | s | 1 | 79.116 | 71.149 | 48.908 | p50 ms |
| pull | m | 1 | 0.845 | 0.875 | 0.876 | p50 ms |
| pull | s | 1 | 0.824 | 0.854 | 0.845 | p50 ms |
| ref_traversal (q2) | m | 1 | 2.542 | 1.164 | 1.131 | p50 ms |
| ref_traversal (q2) | s | 1 | 1.104 | 0.955 | 0.915 | p50 ms |
| since | m | 1 | 3.098 | 3.171 | 3.047 | p50 ms |
| since | s | 1 | 1.920 | 1.915 | 1.890 | p50 ms |

## M2: count (q3 `[:find ?state (count ?i) :where [?i :issue/state ?state]]`)

1445 ms -> 58 ms at 10M datoms (25x), 35 -> 9 ms at 1M.
`explain/before-190-m-q3.txt` vs `explain/final-m-q3.txt`:

- before: `GroupAggregate` over a `Sort` of all 1.3M (value, e) pairs
  (`external merge, Disk: 45296kB`) for `count(DISTINCT e)`;
- after: `(?state, ?i)` is a key of the join (?i is the entity of a
  cardinality-one pattern), so `COUNT(*)`, grouped on the raw keyword:
  `Finalize GroupAggregate <- Gather Merge (3 workers) <- Partial
  HashAggregate <- Parallel Seq Scan current_keyword`. At 1M the planner
  instead streams `idx_current_keyword_avet` in value order into a
  `GroupAggregate`.

Parts of the gain (m): COUNT(*) instead of COUNT(DISTINCT) 1491 -> 214 ms
(serial); aggregate queries skipping the SPI plan cache so they can run
parallel (SPI-prepared plans never are) 214 -> 65 ms; grouping on the raw
typed column instead of the decoded text 65 -> 55 ms.

A non-key aggregate (`[:find ?state (count ?u) :where [?i :issue/state
?state] [?i :issue/assignee ?u]]`, distinct assignees per state) takes the
de-duplicating path `SELECT DISTINCT` -> `HashAggregate`: 406 ms at m
(`m2-count/m-nk.*`), against 762 ms for the old `COUNT(DISTINCT)` form of
the same query (probe in psql).

## M3: automatic index management

`explain/autoindex-*.txt`: a synthetic store (8 long attributes x 200k
entities, so each is 1/8 of `datoms_long_new`), as-of range queries on one
attribute, then `mentat_tune_indexes(false)`:

- wide range (25% of the values): the planner scanned all of the
  attribute's values through `idx_datoms_long_new_aevt` with
  `Filter: v >= ...`; after, `Parallel Index Scan using
  mentat_auto_datoms_long_new_a1000004`. edn_q p50 162 -> 141 ms (-13%).
- narrow range (0.5%): the VAET index already served it; 2.8 -> 2.6 ms.
- the scale suite's `:issue/priority` (100% of `datoms_long_new`):
  655 -> 606 ms for a 62 MB index. This led to the skip rule: an
  attribute holding at least half its table is reported `skip`, not
  indexed.

So the manager is a modest, targeted win for hot temporal range queries.
The large wins in this release are M1 (AVET) and M2 (count).

## Regression caught and fixed (46b47573)

Between after-m1 and the first final run, `input_bindings` at s went
0.834 -> 1.131 ms. A/B of the release `.so` of each commit on the same DB
(pgbench, 100-email `[?email ...]` input, `-c1 -T10` x2): 3a1d1cd8 0.89,
bd14ee6d 0.88, **c19fe955 1.18**, HEAD 1.18. The literal `a = <entid>`
pushdown (c19fe955) changed edn_q's plan-cache choice for a statement
with one parameter per collection value. Fix: bind the collection as one
array (`v = ANY($n)`) -> 0.87 ms; final run 0.814 ms.

## Files

- `scale/` -- the final run (`timings.csv`, `raw.csv`, `summary.md`,
  `plans/`, `logs/`, `env.txt`, `checks.txt`);
- `compare.txt` -- compare.py vs v1.9.0 and vs after-M1;
- `explain/` -- q1/q3/q4 plans for v1.9.0, after-M1 and final, and the
  auto-index before/after plans;
- `m2-count/` -- the M2 q3 / non-key / :with / many-attr probes (SQL,
  plan, result, timings) at s and m.
