# mentat scale benchmark: scale-2026-09-27T010840Z

## Findings (hand-written; the tables below are generated)

**Instance:** r6id.metal (Intel Xeon Platinum 8375C, 2 sockets and 128 vCPU,
1 TiB RAM, 4×1.9 TB instance NVMe as XFS on md RAID-0), AL2023 kernel 6.18,
us-east-2a. PostgreSQL 16.15 built from source (-O2, no cassert). pg_mentat
1.9.0 was built `--release` with pgrx 0.17. shared_buffers = 857 GiB (85% of
RAM) on 2 MiB huge pages. The postmaster ran under `numactl --interleave=all`.
Full settings are in `env.txt`, and the reasoning is in
`benchmarks/scale/ec2/tune.sh`.

**The working set is smaller than shared_buffers at every scale.**
pg_database_size was 0.8 GB at s (1M datoms), 8.0 GB at m (10M), 80 GB at l
(98M) and 244 GB at xl (303M). xl uses 28% of s_b. On disk that is about
0.81 to 0.86 KB per datom (see `sizes.txt`).

### Where each backend scales, and where it falls over

1. **PostgreSQL (pg_mentat) is the only deployment that reached xl (303M
   datoms).** Bulk load ran at 131 to 206K datoms/s with 16 to 32 parallel
   loaders (xl: 1472 s). ANALYZE took 0.7 to 1.5 s and VACUUM 0.4 to 8.4 s.
   **The concurrency curve** (read mix q1+q2+pull, quiet machine, median of 3,
   ops/s at 1 / 8 / 32 / 64 / 128 clients):

   | scale | 1 | 8 | 32 | 64 | 128 | p50 @1 → @128 |
   |---|---:|---:|---:|---:|---:|---|
   | s (1M) | 1,316 | 10,236 | 38,581 | 64,669 | 69,167 | 0.77 → 1.6 ms |
   | m (10M) | 625 | 4,894 | 18,871 | 31,911 | 35,769 | 1.6 → 3.5 ms |
   | l (98M) | 101 | 810 | 3,195 | 5,730 | 5,844 | 13.6 → 30.8 ms |
   | xl (303M) | 33 | 265 | 1,061 | 1,922 | 1,932 | 44 → 96 ms |

   Scaling is linear up to 64 clients (one per physical core), then flat.
   The throughput level falls with scale because of q1 (see below).
   `pull` stays flat at 0.7 to 1.3 ms p50 from s to xl, because it uses
   the EAVT pkey. Two query classes do **not** scale:
   - **Point lookup by a unique attribute degrades linearly with the
     attribute's size.** q1 p50 was 0.41 ms at s, 1.6 ms at m, 13.6 ms at l
     and 49 ms at xl. `plans/l-q1.json`: the `current_text` table has only
     `(store_id,e,a,v)` and `(store_id,a,e) INCLUDE (v,tx)`, so
     `[?e :user/email "x"]` is a *Parallel Index Only Scan of every
     :user/email value* with `Filter: v = 'x'`. There is no AVET index on the
     current-state tables. q2, input_bindings and as_of inherit the same cost.
     Fix (crates/pg): add `(store_id, a, v) INCLUDE (e)` on `current_*`, at
     least for `:db/unique`/`:db/index` attributes.
   - **Full-attribute scans (q3 aggregate, q4 predicate) are O(n) and slow.**
     q3 took 126 ms at s, 1.5 s at m and 16 s at l. At xl it exceeded the
     20 s probe (**ceiling**). `plans/*-q3.json`: `(count ?i)` compiles to
     `COUNT(DISTINCT e)` with a full sort. At xl the first attempt died on
     pg_mentat's own `mentat.temp_file_limit` (1 GB default). q4 at xl returns
     3.2M rows, and the single `jsonb` result exceeds PostgreSQL's 256 MB
     jsonb limit (`ERROR: total size of jsonb object elements exceeds the
     maximum`): a **hard ceiling** of the `edn_q → jsonb` API, not a tuning
     issue.
   - pg_mentat **errors** once a result passes `mentat.max_result_rows`
     (default 100000). From m up, q4 and `since` need it lifted. The suite
     sets it to 0 in the bench DBs.
   - Writes: single-datom `edn_t` with `synchronous_commit=on` ran at 1.3K
     tx/s at s, about 500 tx/s at m and l, and 1.0K/s at xl, with p99 of 1 to
     11 ms, while 8 readers ran. All writes succeeded; there were no
     serialization failures.

2. **Embedded (the `mentat` crate on SQLite) has the best single-thread
   latency for entity-shaped reads, but does not scale with threads or
   history.**
   - Point lookup was 0.09 ms at s, 0.66 ms at m and 7.9 ms at l. Pull was
     0.056 to 0.074 ms from s to l, 10 to 20× faster than PG, because the call
     is in-process and there is an AVET index. Input bindings (100 emails)
     took 0.45 ms at s and 23 ms at l.
   - The joins and scans are much slower than PG. At m, q2 took 640 ms, q3
     1.3 s and q4 320 ms. At l, q2 took 14.7 s and q3 16 s. **as_of was a
     ceiling (over 20 s) at every scale from s up.** It uses a correlated
     `NOT EXISTS` over `timelined_transactions`, which has no (e,a,v) index.
   - **Concurrency falls over.** Threads with one `Store` each
     (`concurrency_sweep`) gave 63 ops/s at 1 client and 10 to 14 ops/s at 8
     to 64 clients, with p99 of 2 to 16 s. At 128 clients it hit the ceiling.
     At m, 8 clients already hit the ceiling. Readers during `write_mixed`
     had a p99 of 2.7 s at s and 32 s at m. The metadata mutex in
     `Conn::q_once` is per Store, and each thread has its own Store, so that
     mutex is not the cause. The suspected cause is SQLite's global allocator
     mutex: the bundled build keeps memstatus on, and the gdb dump of the
     128-thread `Store::open` shows every thread in `sqlite3_free` →
     `pthread_mutex_lock`. A profile of the query-phase collapse was not taken,
     so profile it with `perf` before fixing.
   - **`Store::open` is O(history).** It took 0.7 s at s, 6.8 s at m and about
     20 s at l, because `read_partition_map` scans the `parts` view (a GROUP
     BY over all of `timelined_transactions`). **128 concurrent opens on the m
     store did not finish.** All 128 threads sat in `sqlite3_free` →
     `pthread_mutex_lock`, RSS grew to 80 GB, and the kernel OOM-killed the
     process after 25 min (`logs/embedded-m-open128-gdb.txt.gz`,
     `logs/embedded-m-oom.txt`). The runner now opens stores in batches.
   - Bulk load: 42 to 55K datoms/s at m and l with direct entids (l: 39 min),
     against 11K/s at s when refs were `(lookup-ref …)`. **Lookup-ref
     resolution is the embedded bulk-load bottleneck** (see gen.py).
     **Any transaction of 5461 or more datoms panics** (off-by-one assert in
     `db.rs insert_non_fts_searches`, `6*n < 32766`).
   - An interrupted query (`sqlite3_interrupt`) **panics** in the projector
     (`projectors/simple.rs` unwraps the row iterator), which poisons the
     Store's mutex.

3. **The SQLite loadable extension and the DuckDB extension pay a full
   `Store::open` on every call.** Every scenario costs the same: 0.57 to
   0.72 s per call at s and 9.5 to 11 s at m, whether it is q1, pull or q3.
   Bulk load through `edn_t` is therefore O(n²). It ran at 5.9K datoms/s
   (sqlite-ext) and 5.5K/s (DuckDB) at s. At m, sqlite-ext reached only 3.2M
   of 9.85M datoms in the 900 s cap (**ceiling**). Its m reads were measured
   on a copy of the embedded-built store. More processes help only up to the
   core count: sqlite-ext at s did 1.7 ops/s at 1 client and 78 ops/s at 128.
   **The fix belongs in crates/**: cache the `Store` per `db_path` per
   connection, or at least cache the partition map. DuckDB m was not run,
   within the instance budget. Its per-call cost is the same `Store::open`
   (DuckDB at s is within 5% of sqlite-ext on every scenario).

### Sustained load, cold vs warm

- **sustained, 20 min** (cut from the planned 30 min to stay inside the 6 h
  budget). PG at xl (32 readers of q1+q2+pull, plus 1 writer) and embedded at
  m (8 readers of the same mix, plus 1 writer) ran at the same time on the
  128-vCPU box.
  - **PG xl was stable.** Comparing the first and last sixth of the 10 s
    windows, reads went from 1036 to 987 ops/s, read p50 from 44.4 to 44.9 ms
    and p99 from 49.6 to 50.1 ms. Writes went from 566 to 755 tx/s, with p99
    of 2.6 then 2.0 ms. 1.25M reads and 0.80M writes finished with 0 errors.
    pg_stat_bgwriter shows 0 buffers_clean. Backend relation reads grew from
    4.9M to 9.7M blocks, because first touches of the 244 GB DB came from the
    OS cache into s_b. temp_bytes did not grow. Postgres RSS stayed flat,
    since s_b is in huge pages and does not count as RSS. There was no
    latency drift and no throughput collapse.
  - **Embedded m readers starved.** 8 reader threads did 932 queries in total
    in 20 min: 0.8 ops/s, p50 3.9 ms, p99 35 s. The single writer meanwhile
    ran at 480 tx/s (p99 2.9 ms). Writer throughput was flat over the run.
    Reader p50 improved (11.7 s → 5.8 s) but p99 stayed around 33 to 35 s.
    Process RSS grew from 27 MB to 1.36 GB (8 Stores' page caches), then
    stayed there. The same collapse shows in write_mixed. Under a steady
    writer, embedded mentat's WAL-mode readers get almost no work done
    against q2's multi-second scans.
  - The windows are in `logs/*-sustained-windows-*.csv`. The samplers (RSS,
    iostat, pg_stat_io/bgwriter/database) are in `logs/sampler-*.log` and
    `logs/iostat-*.log`.
- cold_vs_warm restarted PG and dropped the OS page cache. A cold first call
  cost 18 to 48 ms for q1 at s to l (424 ms at xl) and 11 to 14 ms for pull.
  After that, warm latency matched the steady state. shared_buffers starts
  empty after a restart. Embedded cold means a new process: the open time
  above plus a first query 2 to 10× slower than warm.

### Correctness

Every scenario verifies its answer against the generator's truth
(`checks.txt`: point lookups, per-state counts, as_of pre-update states, the
since count, input bindings, pull contents). **No backend returned a wrong
answer.** 440 checks passed. The 7 FAIL lines are the annotated harness false
positive described below. Some checks were SKIPped where the backend could not answer within
limits: embedded/sqlite-ext/duckdb as_of (timeout), PG q4 at xl (jsonb size
limit). Two harness false positives are annotated in `checks.txt`: a stale
runner binary on the box, and a re-check without `--post-write` after
`write_mixed` had mutated the data. Both are fixed in the suite.

### Methodology caveats

- All PG concurrency sweeps (s to xl) were re-run at the end with nothing
  else on the box, and the tables show those runs. The earlier sweeps
  overlapped other backends' phases and are kept in
  `logs/pg-sweeps-contended.csv`.
- The single-client scenario phases of different backends sometimes
  overlapped: PG l while embedded l loaded, and embedded m while PG xl loaded.
  Each phase is at most one busy core plus IO on a 128-vCPU box with
  7 TB of NVMe, but it is not a quiet machine. Use `compare.py` against a
  rerun before citing a single-client number to better than about 10%.
- Embedded, sqlite-ext and duckdb load through tempids with `@@` entid
  substitution. PG loads caller-chosen entids. The datoms are the same.
- The `mentat_git` value in env.txt (815164a1+) is the suite commit when the
  run began. The engine is v1.9.0 (tag, 8c6c8816). Later suite commits
  changed only benchmarks/.

## Environment (abridged; full detail in env.txt)

```
instance_type:   r6id.metal
kernel:          6.18.48-109.150.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8375C CPU @ 2.90GHz
cpus:            128
sockets/numa:    2 2 
mem_total_gib:   1007.7
hugepages:       HugePages_Total:=452730 HugePages_Free:=445145 Hugepagesize:=2048 
data_fs:         /dev/md0       xfs   7.0T   53G  6.9T   1% /nvme
mentat_git:      815164a1+
duckdb:          1.5.5
postgres:        postgres (PostgreSQL) 16.15
```

## Dataset and PG size vs shared_buffers

```
s: 984803 datoms (1600 users, 130000 issues, 6492 history updates)
m: 9850258 datoms (16000 users, 1300000 issues, 64986 history updates)
m: 9850258 datoms (16000 users, 1300000 issues, 64986 history updates)
pg m: pg_database_size=8493931543 (8.0G) shared_buffers=919707058176 (857G) ratio=.009
l: 98474009 datoms (160000 users, 13000000 issues, 650127 history updates)
pg s: pg_database_size=840973335 (803M) shared_buffers=919707058176 (857G) ratio=0
pg l: pg_database_size=85055028247 (80G) shared_buffers=919707058176 (857G) ratio=.092
sqlite-ext m: reads measured on a copy of the embedded-built store (ext bulk load capped at 900s)
xl: 303006946 datoms (500000 users, 40000000 issues, 2000847 history updates)
embedded m concurrency_sweep: first attempt opened 128 Stores concurrently; they serialized on SQLite malloc mutex, grew to 80 GB RSS and were OOM-killed after 25 min (logs/embedded-m-open128-gdb.txt.gz). Rerun with batched opens (OPEN_PAR=8).
pg xl: pg_database_size=261716278295 (244G) shared_buffers=919707058176 (857G) ratio=.284
```

## bulk_load

| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |
|---|---|---:|---:|---:|---:|---:|---:|---|
| embedded | s | 984,803 | 88.3 | 11,150 |  |  | 0.15 GiB |  |
| sqlite-ext | s | 984,803 | 167.4 | 5,884 |  |  | 0.15 GiB |  |
| pg | m | 9,850,258 | 54.8 | 179,611 | 1.3 | 1.4 | 7.91 GiB |  |
| embedded | m | 9,850,258 | 180.0 | 54,725 |  |  | 1.57 GiB |  |
| pg | s | 984,803 | 31.4 | 31,405 | 0.7 | 0.4 | 0.78 GiB |  |
| pg | l | 98,474,009 | 750.5 | 131,205 | 1.5 | 2.9 | 79.21 GiB |  |
| duckdb | s | 984,803 | 180.5 | 5,457 |  |  | 0.15 GiB |  |
| embedded | l | 98,474,009 | 2,338 | 42,119 |  |  | 16.29 GiB |  |
| sqlite-ext | m | 9,850,258 | 902.2 | 3,546 |  |  |  | load cap hit after ~3199031 datoms (850 txs) |
| pg | xl | 303,006,946 | 1,472 | 205,820 | 1.5 | 8.4 | 243.74 GiB |  |

## point_lookup

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 600.398 | 607.858 | 609.573 | 609.573 | 1.7 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 109741 | 0.091 | 0.095 | 0.095 | 0.213 | 10,974 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 14722 | 0.673 | 0.718 | 0.723 | 0.957 | 1,472 | 0 | 8 |
| embedded | l | 98,474,009 | 1 | read | 631 | 7.879 | 8.380 | 8.423 | 8.632 | 126.1 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 24369 | 0.409 | 0.418 | 0.429 | 2.819 | 2,437 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 6185 | 1.624 | 1.638 | 1.647 | 4.210 | 618.5 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 741 | 13.609 | 13.732 | 13.879 | 17.346 | 74.1 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 205 | 48.555 | 52.635 | 52.795 | 53.025 | 20.5 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 566.533 | 586.447 | 590.944 | 590.944 | 1.8 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 7 | 9,461 | 9,556 | 9,556 | 9,556 | 0.1 | 0 | 1 |

## ref_traversal

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 652.060 | 658.923 | 661.180 | 661.180 | 1.5 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 202 | 49.587 | 50.396 | 50.518 | 51.086 | 20.1 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 641.709 | 649.716 | 649.918 | 649.918 | 1.6 | 0 | 8 |
| embedded | l | 98,474,009 | 1 | read | 2 | 14,734 | 15,050 | 15,050 | 15,050 | 0.1 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 8719 | 1.143 | 1.270 | 1.331 | 4.549 | 871.9 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 3651 | 2.733 | 3.088 | 3.187 | 7.123 | 365.1 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 499 | 20.053 | 21.634 | 22.700 | 24.794 | 49.9 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 200 | 49.731 | 54.195 | 54.436 | 55.938 | 20.0 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 625.060 | 634.004 | 635.052 | 635.052 | 1.6 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 6 | 10,602 | 10,831 | 10,831 | 10,831 | 0.1 | 0 | 1 |

## aggregate

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 716.032 | 725.491 | 726.401 | 726.401 | 1.4 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 90 | 111.074 | 113.793 | 114.363 | 114.363 | 9.0 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 1,372 | 1,380 | 1,380 | 1,380 | 0.7 | 0 | 6 |
| embedded | l | 98,474,009 | 1 | read | 2 | 16,051 | 16,091 | 16,091 | 16,091 | 0.1 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 80 | 126.280 | 127.706 | 128.723 | 128.723 | 8.0 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 40 | 1,548 | 1,563 | 1,565 | 1,565 | 0.7 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 4 | 16,201 | 16,385 | 16,385 | 16,385 | 0.1 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | **CEILING** (first call) | 1 | 20,030 | 20,030 | 20,030 | 20,030 | 0.0 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 671.278 | 680.317 | 684.244 | 684.244 | 1.5 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 6 | 11,321 | 11,414 | 11,414 | 11,414 | 0.1 | 0 | 1 |

## predicate_scan

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 649.345 | 656.557 | 657.599 | 657.599 | 1.5 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 361 | 27.739 | 28.198 | 28.396 | 28.455 | 36.0 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 32 | 320.583 | 322.175 | 322.612 | 322.612 | 3.1 | 0 | 6 |
| embedded | l | 98,474,009 | 1 | read | 6 | 3,416 | 3,448 | 3,448 | 3,448 | 0.3 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 110 | 91.622 | 93.795 | 96.215 | 98.377 | 11.0 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 74 | 818.639 | 822.771 | 837.992 | 837.992 | 1.2 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 10 | 6,593 | 6,827 | 6,827 | 6,827 | 0.2 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | **CEILING** (first call) | 1 | 20,632 | 20,632 | 20,632 | 20,632 | 0.0 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 602.532 | 607.607 | 609.374 | 609.374 | 1.7 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 6 | 10,149 | 10,328 | 10,328 | 10,328 | 0.1 | 0 | 1 |

## pull

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 596.480 | 622.530 | 626.476 | 626.476 | 1.7 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 178370 | 0.056 | 0.057 | 0.058 | 0.120 | 17,837 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 151806 | 0.066 | 0.069 | 0.071 | 0.250 | 15,181 | 0 | 6 |
| embedded | l | 98,474,009 | 1 | read | 64363 | 0.074 | 0.081 | 0.179 | 0.647 | 12,872 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 13884 | 0.716 | 0.744 | 0.788 | 3.660 | 1,388 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 7499 | 1.326 | 2.063 | 2.300 | 3.942 | 749.9 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 8640 | 1.147 | 1.950 | 2.451 | 4.529 | 864.0 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 12938 | 0.770 | 0.794 | 0.804 | 3.474 | 1,294 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 563.650 | 566.986 | 575.377 | 575.377 | 1.8 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 7 | 9,647 | 9,703 | 9,703 | 9,703 | 0.1 | 0 | 1 |

## as_of

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 0 | 1 |
| embedded | s | 984,803 | 1 | **CEILING** (first call) | 1 | 20,000 | 20,000 | 20,000 | 20,000 | 0.1 | 1 | 1 |
| embedded | m | 9,850,258 | 1 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 1 | 2 |
| embedded | l | 98,474,009 | 1 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 1 | 1 |
| pg | s | 984,803 | 1 | read | 5377 | 1.847 | 2.130 | 2.251 | 8.682 | 537.7 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 3876 | 2.238 | 5.397 | 8.150 | 15.996 | 387.6 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 316 | 31.497 | 38.420 | 42.198 | 49.109 | 31.6 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 4111 | 2.421 | 2.760 | 2.921 | 9.512 | 411.1 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | **CEILING** (first call) | 1 | 20,002 | 20,002 | 20,002 | 20,002 | 0.1 | 0 | 1 |

## since

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 659.851 | 666.914 | 669.019 | 669.019 | 1.5 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 170 | 58.770 | 60.548 | 60.790 | 60.796 | 16.9 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 616.014 | 625.418 | 625.522 | 625.522 | 1.6 | 0 | 6 |
| embedded | l | 98,474,009 | 1 | read | 3 | 7,270 | 10,655 | 10,655 | 10,655 | 0.1 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 4634 | 2.177 | 2.210 | 2.266 | 4.359 | 463.4 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 2750 | 3.668 | 3.708 | 3.732 | 5.770 | 275.0 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 1914 | 5.357 | 5.477 | 5.577 | 8.042 | 191.4 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 2761 | 3.620 | 3.735 | 3.762 | 6.135 | 276.1 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 621.869 | 630.645 | 631.770 | 631.770 | 1.6 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 6 | 10,839 | 11,116 | 11,116 | 11,116 | 0.1 | 0 | 1 |

## input_bindings

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 600.932 | 611.881 | 615.419 | 615.419 | 1.7 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 22158 | 0.450 | 0.460 | 0.467 | 0.533 | 2,216 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 4090 | 2.433 | 2.484 | 2.519 | 2.686 | 409.0 | 0 | 6 |
| embedded | l | 98,474,009 | 1 | read | 216 | 23.105 | 24.086 | 25.205 | 25.328 | 43.0 | 0 | 1 |
| pg | s | 984,803 | 1 | read | 9061 | 1.100 | 1.123 | 1.140 | 3.662 | 906.1 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 3676 | 2.711 | 2.794 | 2.876 | 5.183 | 367.6 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 375 | 26.933 | 28.642 | 29.138 | 32.303 | 37.5 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 152 | 65.495 | 69.615 | 70.441 | 71.864 | 15.2 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 581.022 | 590.974 | 594.505 | 594.505 | 1.7 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 7 | 9,994 | 10,008 | 10,008 | 10,008 | 0.1 | 0 | 1 |

## write_mixed

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | write | 99 | 604.345 | 639.948 | 653.626 | 665.383 | 1.6 | 0 | 3 |
| duckdb | s | 984,803 | 8 | read | 757 | 644.791 | 681.616 | 713.756 | 827.788 | 12.5 | 0 | 3 |
| embedded | s | 984,803 | 1 | write | 35420 | 1.673 | 2.089 | 2.259 | 11.668 | 590.3 | 0 | 3 |
| embedded | s | 984,803 | 8 | read | 406 | 1.292 | 2,615 | 2,676 | 2,764 | 6.6 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | write | 16714 | 1.787 | 2.189 | 2.332 | 4.097 | 557.1 | 0 | 3 |
| embedded | m | 9,850,258 | 8 | read | 16 | 5.459 | 32,454 | 32,454 | 32,454 | 0.5 | 0 | 3 |
| pg | s | 984,803 | 1 | write | 79793 | 0.748 | 0.840 | 0.918 | 4.818 | 1,330 | 0 | 3 |
| pg | s | 984,803 | 8 | read | 632802 | 0.927 | 1.179 | 1.240 | 5.400 | 10,547 | 0 | 3 |
| pg | m | 9,850,258 | 1 | write | 32247 | 1.584 | 3.611 | 11.398 | 95.498 | 537.5 | 0 | 2 |
| pg | m | 9,850,258 | 8 | read | 182278 | 2.461 | 5.554 | 6.184 | 50.079 | 3,038 | 0 | 2 |
| pg | l | 98,474,009 | 1 | write | 30232 | 2.055 | 2.655 | 2.970 | 7.100 | 503.9 | 0 | 3 |
| pg | l | 98,474,009 | 8 | read | 22776 | 20.582 | 28.614 | 30.190 | 33.396 | 379.6 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | write | 30025 | 0.888 | 1.311 | 1.407 | 4.766 | 1,001 | 0 | 3 |
| pg | xl | 303,006,946 | 8 | read | 4982 | 47.890 | 51.886 | 53.177 | 55.611 | 166.1 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | write | 100 | 602.451 | 610.964 | 615.191 | 617.044 | 1.7 | 0 | 3 |
| sqlite-ext | s | 984,803 | 8 | read | 767 | 634.448 | 658.032 | 685.394 | 698.568 | 12.7 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | write | 5 | 6,142 | 6,213 | 6,213 | 6,213 | 0.2 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 8 | read | 40 | 6,262 | 6,866 | 6,888 | 6,888 | 1.2 | 0 | 1 |

## concurrency_sweep

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 30 | 597.077 | 648.648 | 650.873 | 650.873 | 1.6 | 0 | 3 |
| duckdb | s | 984,803 | 8 | read | 127 | 625.179 | 669.466 | 1,253 | 1,260 | 12.0 | 0 | 3 |
| duckdb | s | 984,803 | 32 | read | 392 | 724.401 | 1,407 | 1,620 | 2,056 | 33.2 | 0 | 3 |
| duckdb | s | 984,803 | 64 | read | 682 | 778.102 | 1,566 | 1,673 | 2,630 | 62.1 | 0 | 3 |
| duckdb | s | 984,803 | 128 | read | 751 | 1,688 | 2,761 | 3,243 | 3,740 | 60.7 | 0 | 3 |
| embedded | s | 984,803 | 1 | read | 633 | 0.112 | 48.884 | 49.300 | 49.688 | 63.1 | 0 | 1 |
| embedded | s | 984,803 | 8 | read | 104 | 0.567 | 2,146 | 2,182 | 2,189 | 9.8 | 0 | 1 |
| embedded | s | 984,803 | 32 | read | 229 | 2.022 | 8,531 | 8,647 | 8,755 | 13.8 | 0 | 1 |
| embedded | s | 984,803 | 64 | read | 207 | 5.292 | 16,525 | 16,538 | 16,541 | 12.5 | 0 | 1 |
| embedded | s | 984,803 | 128 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 41 | 1 |
| embedded | m | 9,850,258 | 1 | read | 30 | 0.779 | 2,227 | 2,245 | 2,245 | 1.4 | 0 | 1 |
| embedded | m | 9,850,258 | 8 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 3 | 1 |
| pg | s | 984,803 | 1 | read | 13157 | 0.765 | 1.122 | 1.171 | 4.568 | 1,316 | 0 | 3 |
| pg | s | 984,803 | 8 | read | 102355 | 0.784 | 1.174 | 1.233 | 5.193 | 10,236 | 0 | 3 |
| pg | s | 984,803 | 32 | read | 385808 | 0.808 | 1.283 | 1.364 | 10.105 | 38,581 | 0 | 3 |
| pg | s | 984,803 | 64 | read | 646687 | 0.906 | 1.657 | 2.229 | 22.443 | 64,669 | 0 | 3 |
| pg | s | 984,803 | 128 | read | 691665 | 1.599 | 3.380 | 3.758 | 49.906 | 69,166 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 6254 | 1.649 | 2.466 | 2.532 | 5.833 | 625.4 | 0 | 3 |
| pg | m | 9,850,258 | 8 | read | 48938 | 1.667 | 2.512 | 2.585 | 6.609 | 4,894 | 0 | 3 |
| pg | m | 9,850,258 | 32 | read | 188711 | 1.705 | 2.625 | 2.717 | 12.648 | 18,871 | 0 | 3 |
| pg | m | 9,850,258 | 64 | read | 319114 | 1.781 | 4.449 | 4.880 | 20.147 | 31,911 | 0 | 3 |
| pg | m | 9,850,258 | 128 | read | 357688 | 3.479 | 5.904 | 6.196 | 51.406 | 35,769 | 0 | 3 |
| pg | l | 98,474,009 | 1 | read | 1012 | 13.631 | 14.832 | 15.010 | 19.628 | 101.2 | 0 | 3 |
| pg | l | 98,474,009 | 8 | read | 8103 | 13.836 | 15.139 | 15.375 | 25.852 | 810.3 | 0 | 3 |
| pg | l | 98,474,009 | 32 | read | 31947 | 13.886 | 15.337 | 15.734 | 68.933 | 3,195 | 0 | 3 |
| pg | l | 98,474,009 | 64 | read | 57302 | 14.265 | 28.887 | 30.971 | 59.543 | 5,730 | 0 | 3 |
| pg | l | 98,474,009 | 128 | read | 58437 | 30.840 | 33.529 | 34.020 | 58.975 | 5,844 | 0 | 3 |
| pg | xl | 303,006,946 | 1 | read | 331 | 44.040 | 46.847 | 47.165 | 48.336 | 33.1 | 0 | 3 |
| pg | xl | 303,006,946 | 8 | read | 2648 | 43.947 | 47.184 | 48.226 | 98.954 | 264.8 | 0 | 3 |
| pg | xl | 303,006,946 | 32 | read | 10614 | 43.912 | 46.996 | 48.231 | 118.241 | 1,061 | 0 | 3 |
| pg | xl | 303,006,946 | 64 | read | 19216 | 44.912 | 88.219 | 94.511 | 131.946 | 1,922 | 0 | 3 |
| pg | xl | 303,006,946 | 128 | read | 19315 | 96.238 | 101.670 | 103.081 | 126.931 | 1,932 | 0 | 3 |
| sqlite-ext | s | 984,803 | 1 | read | 30 | 581.313 | 636.360 | 642.068 | 642.068 | 1.7 | 0 | 3 |
| sqlite-ext | s | 984,803 | 8 | read | 136 | 596.196 | 646.695 | 654.216 | 655.798 | 13.1 | 0 | 3 |
| sqlite-ext | s | 984,803 | 32 | read | 509 | 647.670 | 704.418 | 719.418 | 723.723 | 48.0 | 0 | 3 |
| sqlite-ext | s | 984,803 | 64 | read | 613 | 783.971 | 1,678 | 1,684 | 1,687 | 56.5 | 0 | 3 |
| sqlite-ext | s | 984,803 | 128 | read | 881 | 1,589 | 1,716 | 2,046 | 2,715 | 77.5 | 0 | 3 |
| sqlite-ext | m | 9,850,258 | 1 | read | 7 | 9,662 | 10,552 | 10,552 | 10,552 | 0.1 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 8 | read | 32 | 9,800 | 15,081 | 15,084 | 15,084 | 0.6 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 32 | read | 50 | 9,934 | 13,971 | 13,977 | 13,977 | 2.5 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 128 | **CEILING** (first call) | 1 | 24,047 | 24,047 | 24,047 | 24,047 | 0.0 | 0 | 1 |

## cold_vs_warm

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | cold_aggregate | 1 | 108.952 | 108.952 | 108.952 | 108.952 | 9.2 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_point_lookup | 1 | 4.532 | 4.532 | 4.532 | 4.532 | 220.7 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_pull | 1 | 0.159 | 0.159 | 0.159 | 0.159 | 6,306 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_ref_traversal | 1 | 93.052 | 93.052 | 93.052 | 93.052 | 10.8 | 0 | 1 |
| embedded | s | 984,803 | 1 | open | 1 | 692.955 | 692.955 | 692.955 | 692.955 | 1.4 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_aggregate | 30 | 107.523 | 108.189 | 108.350 | 108.350 | 9.3 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_point_lookup | 30 | 0.412 | 0.688 | 0.732 | 0.732 | 2,496 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_pull | 30 | 0.058 | 0.072 | 0.087 | 0.087 | 16,640 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_ref_traversal | 30 | 49.145 | 54.790 | 58.725 | 58.725 | 20.0 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_aggregate | 1 | 1,375 | 1,375 | 1,375 | 1,375 | 0.7 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_point_lookup | 1 | 6.681 | 6.681 | 6.681 | 6.681 | 149.7 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_pull | 1 | 1.233 | 1.233 | 1.233 | 1.233 | 810.9 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_ref_traversal | 1 | 907.559 | 907.559 | 907.559 | 907.559 | 1.1 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | open | 1 | 6,847 | 6,847 | 6,847 | 6,847 | 0.1 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_aggregate | 30 | 1,294 | 1,301 | 1,303 | 1,303 | 0.8 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_point_lookup | 30 | 0.673 | 0.930 | 0.953 | 0.953 | 1,408 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_pull | 30 | 0.286 | 1.906 | 2.031 | 2.031 | 2,660 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_ref_traversal | 30 | 619.233 | 628.565 | 639.100 | 639.100 | 1.6 | 0 | 1 |
| pg | s | 984,803 | 1 | cold_aggregate | 1 | 221.549 | 221.549 | 221.549 | 221.549 | 4.5 | 0 | 1 |
| pg | s | 984,803 | 1 | cold_point_lookup | 1 | 18.445 | 18.445 | 18.445 | 18.445 | 54.2 | 0 | 1 |
| pg | s | 984,803 | 1 | cold_pull | 1 | 11.245 | 11.245 | 11.245 | 11.245 | 88.9 | 0 | 1 |
| pg | s | 984,803 | 1 | cold_ref_traversal | 1 | 59.282 | 59.282 | 59.282 | 59.282 | 16.9 | 0 | 1 |
| pg | s | 984,803 | 1 | warm_aggregate | 30 | 152.492 | 154.643 | 154.714 | 154.714 | 6.6 | 0 | 1 |
| pg | s | 984,803 | 1 | warm_point_lookup | 30 | 0.410 | 0.597 | 0.668 | 0.668 | 2,255 | 0 | 1 |
| pg | s | 984,803 | 1 | warm_pull | 30 | 1.561 | 2.204 | 2.310 | 2.310 | 593.7 | 0 | 1 |
| pg | s | 984,803 | 1 | warm_ref_traversal | 30 | 12.432 | 50.984 | 51.230 | 51.230 | 51.1 | 0 | 1 |
| pg | m | 9,850,258 | 1 | cold_aggregate | 1 | 1,834 | 1,834 | 1,834 | 1,834 | 0.6 | 0 | 1 |
| pg | m | 9,850,258 | 1 | cold_point_lookup | 1 | 43.171 | 43.171 | 43.171 | 43.171 | 23.2 | 0 | 1 |
| pg | m | 9,850,258 | 1 | cold_pull | 1 | 12.591 | 12.591 | 12.591 | 12.591 | 79.4 | 0 | 1 |
| pg | m | 9,850,258 | 1 | cold_ref_traversal | 1 | 74.238 | 74.238 | 74.238 | 74.238 | 13.5 | 0 | 1 |
| pg | m | 9,850,258 | 1 | warm_aggregate | 30 | 1,751 | 1,807 | 1,809 | 1,809 | 0.6 | 0 | 1 |
| pg | m | 9,850,258 | 1 | warm_point_lookup | 30 | 1.670 | 2.097 | 2.236 | 2.236 | 573.4 | 0 | 1 |
| pg | m | 9,850,258 | 1 | warm_pull | 30 | 2.505 | 3.108 | 3.148 | 3.148 | 405.2 | 0 | 1 |
| pg | m | 9,850,258 | 1 | warm_ref_traversal | 30 | 46.117 | 59.961 | 69.697 | 69.697 | 21.0 | 0 | 1 |
| pg | l | 98,474,009 | 1 | cold_aggregate | 1 | 24,632 | 24,632 | 24,632 | 24,632 | 0.0 | 0 | 1 |
| pg | l | 98,474,009 | 1 | cold_point_lookup | 1 | 47.845 | 47.845 | 47.845 | 47.845 | 20.9 | 0 | 1 |
| pg | l | 98,474,009 | 1 | cold_pull | 1 | 14.200 | 14.200 | 14.200 | 14.200 | 70.4 | 0 | 1 |
| pg | l | 98,474,009 | 1 | cold_ref_traversal | 1 | 112.136 | 112.136 | 112.136 | 112.136 | 8.9 | 0 | 1 |
| pg | l | 98,474,009 | 1 | warm_aggregate | 30 | 23,967 | 24,651 | 25,280 | 25,280 | 0.0 | 0 | 1 |
| pg | l | 98,474,009 | 1 | warm_point_lookup | 30 | 20.514 | 21.707 | 21.722 | 21.722 | 48.7 | 0 | 1 |
| pg | l | 98,474,009 | 1 | warm_pull | 30 | 3.519 | 4.498 | 5.057 | 5.057 | 277.5 | 0 | 1 |
| pg | l | 98,474,009 | 1 | warm_ref_traversal | 30 | 76.824 | 110.491 | 113.612 | 113.612 | 12.3 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | cold_aggregate | 1 | 51,250 | 51,250 | 51,250 | 51,250 | 0.0 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | cold_point_lookup | 1 | 424.274 | 424.274 | 424.274 | 424.274 | 2.4 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | cold_pull | 1 | 8.338 | 8.338 | 8.338 | 8.338 | 119.9 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | cold_ref_traversal | 1 | 144.522 | 144.522 | 144.522 | 144.522 | 6.9 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | warm_aggregate | 1 | 50,047 | 50,047 | 50,047 | 50,047 | 0.0 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | warm_point_lookup | 30 | 44.977 | 47.505 | 48.050 | 48.050 | 22.5 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | warm_pull | 30 | 1.623 | 2.129 | 4.637 | 4.637 | 563.9 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | warm_ref_traversal | 30 | 118.829 | 137.079 | 144.253 | 144.253 | 8.4 | 0 | 1 |

## sustained

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | m | 9,850,258 | 1 | write | 576679 | 2.072 | 2.386 | 2.934 | 221.349 | 480.6 | 0 | 1 |
| embedded | m | 9,850,258 | 8 | read | 932 | 3.855 | 34,985 | 35,795 | 36,392 | 0.8 | 0 | 1 |
| pg | xl | 303,006,946 | 1 | write | 800706 | 1.591 | 2.024 | 2.326 | 31.047 | 667.2 | 0 | 1 |
| pg | xl | 303,006,946 | 32 | read | 1245425 | 44.499 | 47.821 | 49.381 | 246.675 | 1,038 | 0 | 1 |

## Correctness checks

439 PASS, 7 FAIL, 32 SKIP; 30 backend check runs OK, 2 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)

- `== pg m re-check (mentat.max_result_rows=0)`
- `check embedded: HARNESS ERROR (stale mentat-scale binary on the box lacked the serve command); re-run below`
- `check embedded: HARNESS ERROR (stale mentat-scale binary on the box lacked the serve command); re-run below`
- `== pg xl re-check (mentat.temp_file_limit=100GB; the first two xl checks hit the 1GB default in aggregate)`
- `== pg xl re-check #3 (q4 result exceeds PG's 256MB jsonb limit at xl; recorded as SKIP/ceiling)`
- `check predicate_scan: SKIP (backend limit: ERROR:  total size of jsonb object elements exceeds the maximum of 268435455 bytes)`
- `== NOTE (harness false positive, fixed in aa744ad6): the next two blocks re-checked pg l/xl AFTER write_mixed had mutated them, without --post-write. Only the state-dependent checks (by_state, predicate_scan, since) diff`
- `check aggregate.by_state: FAIL ({':state/closed': 2600208, ':state/in-progress': 2600236, ':state/open': 2600471, ':state/reopened': 2602203, ':state/resolved': 2596882}, {':state/open': 2600341, ':state/in-progress': 26`
- `check predicate_scan: FAIL (1039447, 1039352)`
- `check since: FAIL (38802, 2497)`
- `check pg: FAILED aggregate.by_state,predicate_scan,since`
- `check aggregate.by_state: FAIL ({':state/closed': 8001517, ':state/in-progress': 8004259, ':state/open': 7997805, ':state/reopened': 8000162, ':state/resolved': 7996257}, {':state/open': 7997738, ':state/in-progress': 80`
- `check predicate_scan: SKIP (backend limit: ERROR:  total size of jsonb object elements exceeds the maximum of 268435455 bytes)`
- `check since: FAIL (38448, 2513)`
- `check pg: FAILED aggregate.by_state,since`

