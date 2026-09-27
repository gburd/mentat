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
