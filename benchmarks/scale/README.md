# benchmarks/scale: multi-backend scale and load suite

This suite runs one workload against all three ways to deploy the mentat core,
up to hundreds of millions of datoms. It is meant to be re-run to validate
future releases.

| backend | what runs | how the suite drives it |
|---|---|---|
| `embedded` | the `mentat` Rust crate (`Store`) | `runner/`: the `mentat-scale` binary, one `Store` per thread |
| `sqlite-ext` | `crates/sqlite/ext`, `libmentat_sqlite.so` in a host SQLite | `bench.py`: Python `sqlite3` + `load_extension`, one process per client |
| `pg` | `crates/pg/pg_mentat` in PostgreSQL 16 | `pgbench -f pgbench/*.sql` (`\set` random params), `psql` for load |
| `duckdb` | `crates/duckdb`, `mentat.duckdb_extension` in DuckDB v1.5.5 | `bench.py`: Python `duckdb==1.5.5` (needs Python >= 3.10), one process per client |

The workload is phase2's issue tracker, the same `schema.edn`, SEED and value
distributions as `../phase2/gen_dataset.py`. `gen.py` imports phase2's
constants and shards the output, so it can stream hundreds of millions of
datoms to disk in parallel.

## Layout

```
gen.py            dataset generator (scales xs/s/m/l/xl); writes pg/*.sql, store/*.edn, meta.json (truth)
queries/*.edn     the Datalog queries every backend runs (q1..q4, since, inputs, pull)
pgbench/*.sql     pgbench scripts for the same queries (+ write_state.sql writer)
runner/           Rust crate `mentat_scale_runner` -> `mentat-scale` (embedded backend)
bench.py          correctness checks; sqlite-ext/duckdb load+bench; pgbench log parsing; medians
run.sh            the driver
report.py         timings.csv + loads.csv -> summary.md tables (+ findings.md if present)
compare.py        diff two result dirs, flag regressions
ec2/bootstrap.sh  fresh AL2023 host: NVMe RAID-0, toolchains, PG 16 (no cassert), duckdb CLI + venv
ec2/tune.sh       kernel + postgresql.conf tuning (shared_buffers = 85% RAM, hugepages...)
```

## Scales

| name | users | issues | labels | ~datoms |
|---|---:|---:|---:|---:|
| xs | 200 | 13,000 | 50 | 0.1M (local smoke) |
| s | 1,600 | 130,000 | 200 | 1M |
| m | 16,000 | 1,300,000 | 500 | 10M |
| l | 160,000 | 13,000,000 | 1,000 | 100M |
| xl | 500,000 | 40,000,000 | 2,000 | 300M |

Each issue has 6 cardinality-one attributes and 0 to 3 labels. About 5% of
issues get one later `:issue/state` change in a *history phase* after the
initial load. The loader records two tx ids: `t_mid`, the last tx of the
initial load, and `t_since`, the last tx before the final history file.
Those make `as_of` and `since` meaningful and checkable. `meta.json` holds the
exact expected answers.

## Scenarios

| scenario | what it measures | check (fails the run if wrong) |
|---|---|---|
| `bulk_load` | load from empty. Wall time and datoms/s. PG adds ANALYZE and VACUUM time and `pg_database_size` vs `shared_buffers`. | the datom count follows from the checks below |
| `point_lookup` | q1: user by unique `:user/email`, random user | exactly 1 row, the right name |
| `ref_traversal` | q2: issues assigned to a random user (email -> ref -> title, state) | the set of (issue, state) pairs matches truth |
| `aggregate` | q3: count by state (full scan of one attribute) | counts sum to n_issues and equal the per-state truth |
| `predicate_scan` | q4: open issues with priority >= 4 | row count equals truth |
| `pull` | `edn_pull('[*]', e)` on a random issue (embedded: `(pull ?e [*])`) | title, state and priority of fixed issues |
| `as_of` | q2 with `{"asOf": t_mid}` | returns the **pre-update** states |
| `since` | `[:find ?i :where [?i :issue/state _]]` with `{"since": t_since}` | distinct ?i equals the number of updates in the last history file |
| `input_bindings` | `:in $ [?email ...]` with 100 random emails | the right names for a fixed set |
| `write_mixed` | 1 writer (state update or label add, 1 datom per tx) plus MIXED_READERS readers doing q1/q2 | a post-write check: point lookup, aggregate sum, as_of, input bindings |
| `concurrency_sweep` | read mix (q1, q2, pull in equal parts) at CLIENTS = 1 8 32 64 128 | (the checks above) |
| `cold_vs_warm` | restart (PG: `pg_ctl restart`; embedded: new process + `Store::open`), drop the OS page cache, then the first call vs the next 30 | |
| `sustained` | SUSTAINED_S seconds of read mix at SUSTAINED_CLIENTS plus 1 writer on the largest scale. RSS, iostat, pg_stat_io/bgwriter/database sampled every 10 s. 10-second latency windows for drift. | |

Concurrency model:

- **embedded**: threads, one `Store` per thread on the same file (`Store` is
  `!Sync`). Exactly one writer, because each `Store` keeps its own partition
  map and two writers would allocate the same tx id.
- **sqlite-ext/duckdb**: processes, each with its own host connection. The
  extensions open the mentat store *on every call* (`Store::open`), and that
  cost is part of what they measure. DuckDB is in-process with a single writer,
  so the sweep runs parallel reader processes.
- **pg**: `pgbench -c N -j min(N, nproc) -M simple`. Prepared/extended mode would rewrite the `:keywords` inside the Datalog literals into `$N` parameters.

## Output

Results go to `benchmarks/results/scale-<UTC>/`:

- `raw.csv`: every (scenario, backend, scale, clients, op, rep) row.
- `timings.csv`: the median over reps, with the schema
  `scenario,backend,scale,n_datoms,clients,op,count,p50_ms,p95_ms,p99_ms,max_ms,throughput_ops_s,errors,reps`.
  `op` is `read`, `write`, `ceiling` (the first call exceeded PROBE_S, so the
  scenario was skipped; the latency shown is that first call), `open`, or
  `cold_*`/`warm_*`.
- `loads.csv`: bulk-load rows. `ceiling=1` means LOAD_MAX_S was hit, and the
  row carries the datoms/s reached.
- `checks.txt`: every correctness check.
- `sizes.txt`: the PG DB size vs `shared_buffers`.
- `plans/`: `mentat_explain` of q1 to q4 per scale.
- `logs/`: pgbench output, pg_stat_statements, sampler/iostat, load logs.
- `env.txt`: hardware, kernel, tuning, the full non-default `pg_settings`, versions, git SHA, every knob.
- `summary.md`: generated tables plus the hand-written `findings.md`.

Methodology: 3 warm-up calls are discarded. Each point runs at least MIN_S
(10) seconds and MIN_N (30) samples, capped at MAX_S (60) s. There are REPS
(3) repetitions, and timings.csv reports the median. The seeds are fixed. The
run exits non-zero if any check fails, so a fast wrong answer never looks like
a win.

## Run it

### Locally, at a small scale (a quick check that nothing is broken)

```bash
cargo build --release -p mentat_scale_runner -p mentat_sqlite_ext
# PG: a pg16 with pg_mentat installed (cargo pgrx install --release) and
# pg_stat_statements preloaded. PGHOST/PGPORT pointing at it.
SCALES=xs BACKENDS="embedded sqlite-ext pg" REPS=1 MIN_S=2 MAX_S=10 MIXED_S=5 \
  CLIENTS="1 4" benchmarks/scale/run.sh
# duckdb: add it to BACKENDS and set PY=/path/to/python3.11-venv/bin/python
# (with duckdb==1.5.5) and DUCKDB_EXT=crates/duckdb/build/release/mentat.duckdb_extension
```

### On EC2 at full scale (what produced `results/scale-*`)

```bash
# r6id.metal (128 vCPU, 1 TiB, 4x1.9 TB NVMe), AL2023, us-east-2.
rsync -a --exclude .git --exclude target ./ HOST:/nvme/mentat/   # + crates/duckdb/extension-ci-tools/
ssh HOST 'bash /nvme/mentat/benchmarks/scale/ec2/bootstrap.sh'   # RAID-0 /nvme, PG 16, toolchains
ssh HOST 'cd /nvme/mentat && cargo build --release -p mentat_sqlite_ext -p mentat_scale_runner &&
          (cd crates/pg/pg_mentat && cargo pgrx install --release --pg-config /nvme/pg16/bin/pg_config) &&
          (cd crates/duckdb && make configure PYTHON_BIN=python3.11 && make release)'
ssh HOST 'bash /nvme/mentat/benchmarks/scale/ec2/tune.sh'        # hugepages, s_b=85%, start PG
ssh HOST 'cd /nvme/mentat && PGHOST=/tmp PGBIN=/nvme/pg16/bin PGDATA=/nvme/pgdata PY=/nvme/venv/bin/python \
          DATA_ROOT=/nvme/data SCALES="s m l" SUSTAINED_S=1800 MENTAT_GIT=<sha> \
          nohup setsid benchmarks/scale/run.sh > /nvme/run.log 2>&1 &'
```

PG's database must stay **smaller than shared_buffers**, and `sizes.txt`
records the ratio at each scale. On r6id.metal, s_b is about 870 GiB.

### Knobs (env vars, all recorded in env.txt)

- `SCALES`, `BACKENDS`: select what runs.
- `SCENARIOS`: the single-client query scenarios.
- `EXTRA`: `write_mixed concurrency_sweep cold_vs_warm`.
- `CLIENTS`, `REPS`, `MIN_S`, `MIN_N`, `MAX_S`: sampling.
- `PROBE_S`: the per-call ceiling (default 20 s).
- `LOAD_MAX_S`: the per-backend bulk-load cap (default 5400 s). A backend
  that cannot load a scale in time gets a `ceiling` row with its achieved
  rate, and its scenarios for that scale are skipped.
- `MIXED_S`, `MIXED_READERS`, `SUSTAINED_S`, `SUSTAINED_CLIENTS`.
- `EXT_SCENARIO_FILTER`: scenarios to skip on sqlite-ext and duckdb.
- `PG_LOAD_JOBS`: parallel psql loaders.
- `DATA_ROOT`, `WORK`, `OUT`, `PY`, `RUNNER`, `SQLITE_EXT`, `DUCKDB_EXT`, `PGBIN`, `PGDATA`, `MENTAT_GIT`.

## Compare two runs

```bash
benchmarks/scale/compare.py benchmarks/results/scale-OLD benchmarks/results/scale-NEW --threshold 10
```

Rows are matched on (scenario, backend, scale, clients, op). The script flags
p50/p99 increases and throughput drops beyond the threshold. It also flags new
errors, a scenario that became a ceiling, and rows missing from NEW. It exits
1 on any regression, so it can gate a release. Compare runs from the same
instance type only.

## Known limits of the suite (read before citing numbers)

- **The embedded bulk load must use transactions under 5461 datoms.**
  `crates/sqlite/db/src/db.rs` `insert_non_fts_searches` asserts
  `6 * n < 32766`, and 5461 datoms trip it. `gen.py` batches 500 issues
  (about 3.8K datoms) per tx.
- **Every sqlite-ext and duckdb call is a full `Store::open`.** That call reads
  the partition map through the `parts` view, which is a GROUP BY over all of
  `timelined_transactions`. The per-call cost therefore grows with history
  (about 60 ms at 0.1M datoms). The ext backends measure this deployment as it
  ships.
- **Embedded as_of is slow.** It uses a correlated `NOT EXISTS` over
  `timelined_transactions`, which has no (e, a, v) index. It usually shows up
  as a `ceiling`.
- **pg_mentat errors when a result has more than `mentat.max_result_rows`
  rows** (default 100000). q4 and `since` pass that cap from scale m up, so
  `run.sh` sets `ALTER DATABASE … SET mentat.max_result_rows = 0` (override
  with `PG_MAX_RESULT_ROWS`).
- **An interrupted embedded query panics.** The PROBE_S watchdog uses
  `sqlite3_interrupt`, and mentat's projector unwraps the row iterator
  (`query-projector/src/projectors/simple.rs`), which poisons the Store's
  mutex. The runner catches the unwind and reopens its stores.
- The pg loader uses explicit entids (bands of 1e10), because pg_mentat
  accepts caller-chosen ids. The embedded, sqlite-ext and duckdb loaders use
  tempids and lookup-refs, because embedded mentat only accepts entids it
  allocated. The datoms are the same, but the transaction shape differs.
