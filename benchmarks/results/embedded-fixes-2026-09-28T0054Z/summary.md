# Embedded mentat fixes (1.10.0): v1.9.0 vs 6fed515a

The scale suite (`benchmarks/scale/run.sh`), `BACKENDS=embedded SCALES="s m"`,
on the same EC2 c7i.8xlarge (us-east-2, AL2023 kernel 6.12, rustc 1.90), pinned
to cores 0-23. Defaults everywhere: REPS=3, MIN_S=10, MAX_S=60, PROBE_S=20.

- `before/`: `mentat_git 72ee3765`. The engine is identical to v1.9.0; it
  differs only in docs and lockfile lines. Run 2026-09-27T13:43Z.
- `after/`: `mentat_git 6fed515a` (master, not pushed). Run 2026-09-28T01:08Z.
  52 of 52 correctness checks pass (`after/checks.txt`), as before.
- `compare.txt`: `benchmarks/scale/compare.py before after` (threshold 10%).

Default configuration throughout (`AutoIndex::Schema`, no environment knobs).

## Headline (p50, 1 client unless noted)

| scenario | s before | s after | m before | m after |
|---|---:|---:|---:|---:|
| `Store::open` (cold_vs_warm) | 1621 ms | 45 ms | 16499 ms | 50 ms |
| point_lookup (q1, unique email) | 0.070 ms | 0.019 ms | 0.515 ms | 0.019 ms |
| ref_traversal (q2) | 42.8 ms | 22.6 ms | 556 ms | 290 ms |
| aggregate (q3, `(count ?i)` by state) | 102 ms | 31.2 ms | 1352 ms | 372 ms |
| as_of (q2 as of t_mid) | 20769 ms | 88 ms | >20 s (ceiling) | 1040 ms |
| since | 48.4 ms | 4.2 ms | 485 ms | 52.5 ms |
| predicate_scan (q4) | 25.6 ms | 24.0 ms | 280 ms | 275 ms |
| pull | 0.042 ms | 0.036 ms | 0.048 ms | 0.044 ms |
| input_bindings (100-email `:in` collection) | 0.378 ms | 0.383 ms | 2.03 ms | 2.13 ms |
| write_mixed writer | 3.87 ms | 3.39 ms | 4.17 ms | 3.41 ms |
| write_mixed, 8 readers, reads/s | 9.7 | 577 | 0.7 | 36.9 |

### Concurrency sweep: read mix (q1/q2/pull), ops/s

| clients | 1 | 8 | 32 | 64 | 128 |
|---|---:|---:|---:|---:|---:|
| s before | 29 | 6.9 | 6.8 | ceiling | not run |
| s after | 137 | 996 | 2095 | 2093 | 2101 |
| m before | 1.6 | ceiling | not run | not run | not run |
| m after | 10.6 | 72 | 157 | 161 | 160 |

("ceiling": the first call took over PROBE_S = 20 s, so the suite skipped the
remaining points.)

## What changed (commits on master)

| commit | fix |
|---|---|
| `60c4a966` | transactions of ≥5461 datoms no longer panic |
| `cac32747` | an interrupted query returns an error and leaves the Store usable |
| `5072ef13` | as-of/since use a covering history index (`idx_transactions_aevt`); `Store::open` reads the persisted partition marks and no longer scans the log; schema v2 upgrade |
| `7feb42c1` | readers no longer serialize on SQLite's global page-cache mutex: `.cargo/config.toml` `LIBSQLITE3_FLAGS=-USQLITE_ENABLE_MEMORY_MANAGEMENT -DSQLITE_DEFAULT_MEMSTATUS=0`, plus per-connection `mmap_size` (`MENTAT_MMAP_SIZE`, default 1 GiB) |
| `bf26af3e` | adaptive per-attribute value indexes (`AutoIndex::Adaptive`, `Store::tune_indexes`) |
| `7ff0a0e0` | `(count ?x)` drops the inner DISTINCT when it is provably redundant, else uses `count(DISTINCT ?x)`; `temp_store` 2 → 1 (`MENTAT_TEMP_STORE`) |
| `da2241e2` | shared `mentat::options_from_json` (SQLite and DuckDB extensions, CLI) |
| `266006f6` | CLI: `.q` options, `.pull`, `.eval`, `.tune`, batch mode |
| `8dd332b8` | scripting: `(q db query & inputs)`, plus model tests for history, cas and retractEntity |
| `b08f3219` | `:db/unique` attributes, and refs marked `:db/index`, get a usable value index in the default mode; schema v3 upgrade |
| `6fed515a` | stale statistics on those value indexes are refreshed on open |

## Flagged by compare.py (16 rows) and why

- **`write_mixed` 8-reader read p50** (s 2.1 → 10.2 ms, m 4.2 → 6.3 ms).
  Throughput went up 59× (s) and 53× (m). Before, the readers were serialized
  by the page-cache mutex: few reads finished, and those were mostly cheap
  point lookups. After, all eight readers run the full q1/q2/pull mix
  concurrently with a writer, so the median read includes q2. p99 fell 97%
  (s) and 93% (m).
- **`write_mixed` m writer p99** (5.6 → 8.6 ms). p50 improved from 4.17 to
  3.41 ms and writes/s rose 10%. The tail comes from the eight readers that
  now actually run alongside the writer.
- **`cold_vs_warm` rows** (cold_aggregate m, warm_pull, a few p99s). These are
  one-shot or 30-sample measurements taken right after dropping the OS page
  cache. cold_aggregate at m includes paging in a 2.1 GB store (up from 1.7 GB)
  through mmap. The steady-state rows for the same queries all improved or held:
  pull is 0.042 → 0.036 ms (s) and 0.048 → 0.044 ms (m), and warm_aggregate is
  1333 → 372 ms (m).
- **MISSING / NEW**: the before run's ceilings (as_of m; concurrency s ≥64,
  m ≥8) are now measured points.

## Costs

- **Store size**: s 166 → 213 MB, m 1.69 → 2.15 GB (+28%). The additions are
  the history index (as-of/since) and five schema value indexes (unique
  email/label name, assignee/reporter/label refs).
- **Bulk load**: s 66 → 87 s, m 1482 → 2041 s (6646 → 4825 datoms/s). The same
  indexes are maintained on every insert.
- **One-time upgrade on first open of an older store**: about 21 s at m.

## Not changed / known gaps

- The schema's partial `idx_datoms_avet` / `idx_datoms_unique_value` still
  never match mentat's SQL. They're kept because they back the transactor's
  uniqueness checks.
- `:db/index` on a scalar (an enum or a number) gets no automatic index. At 1M
  datoms, q4 went 28 → 55 ms when SQLite started the join at such an index.
  `AutoIndex::Adaptive` decides these per workload.
