# Resume checkpoint — after 1.9.0 + first scale benchmark run

## State
- v1.9.0 released (Codeberg + GitHub, auto-mirror works). master 33ae0613.
  GitHub Release v1.9.0 has: pg_mentat pg15/16/17, mentat-cli, sqlite-ext .so,
  duckdb ext (v1.5.5 linux_amd64).
- One SQL surface on all backends: edn_t / edn_q / edn_pull / edn_eval.
  PG keeps mentat_* as deprecated wrappers; 1.8.0->1.9.0 upgrade tested.
- New: crates/sqlite/ext (SQLite loadable ext), QueryInputs::merge (mixed :in).
- CI green on master (ci + installcheck). v1.9.0 tag's own CI run had one red job
  (test-duckdb curl/unzip into missing ~/.local/bin) — fixed on master 8c6c8816.
- PGXN: first publish FAILED on META.json (PostgreSQL prereq "13.0" not semver);
  fixed on master (238c7926) but v1.9.0 tag predates it -> PGXN has nothing yet.
  Secrets PGXN_USERNAME/PASSWORD ARE set on gburd/mentat.
- DuckDB registry PR https://github.com/duckdb/community-extensions/pull/2812 —
  DRAFT, ref 5f03696b (1.9.0). Root Makefile now forwards configure_ci/release/
  test_* to crates/duckdb (registry clones repo root). Their CI workflow run needs
  maintainer approval (first-time contributor); none has run a build yet.
- EC2: ALL mentat instances terminated, SGs + keys deleted.

## Scale suite: benchmarks/scale/ (results: benchmarks/results/scale-2026-09-27T010840Z/)
r6id.metal, s_b 857GiB (85%), 1M..303M datoms, DB <=28% of s_b.
Bottlenecks to fix (in priority order):
1. pg: no AVET index on current_* -> q1/q2/:in/as_of O(n). Add (store_id,a,v) INCLUDE (e).
2. sqlite-ext + duckdb: Store::open per call scans parts view -> O(history) per call,
   quadratic bulk load. Cache Store / partition map per path.
3. embedded as_of: NOT EXISTS over timelined_transactions has no (e,a,v) index.
4. embedded: tx >= 5461 datoms panics (db.rs insert_non_fts_searches off-by-one).
5. embedded: interrupted query panics in projectors/simple.rs, poisons lock.
6. embedded: many-thread readers collapse (likely SQLite global allocator mutex).
7. pg: (count ?i) -> COUNT(DISTINCT e) + full sort; edn_q JSONB 256MB result cap;
   max_result_rows / temp_file_limit defaults bite at scale (document).

## IN FLIGHT (1.10.0) — started 2026-09-27
User asked: CLI + Datomic layer updated/tested? DuckDB registry ran? Quack server +
benchmark; fix all 5 benchmark issues; auto index create/drop; optimize count.
- Answer given: CLI was NOT updated for 1.8/1.9 (no inputs/as-of/pull/eval, only
  parser unit tests); model suite lacked :in/history/cas coverage. Registry: 3 runs
  "action_required" (fork CI needs a DuckDB maintainer; we can't approve - 403).
  PR #2812 marked READY for review 2026-09-27.
- Dev boxes (c7i.8xlarge each): /tmp/mentat_dev.env  PG_ID/EMBEDDED_ID/EXT_ID + IPs,
  KEY=mentat-dev-20260927-132950-key SG=sg-0265c9d1805273c66. TERMINATE ALL 3 when done.
- Agents: PG 7e8d9f6a (auto-index, count, edn_q_rows, limits; version->1.10.0)
          EMBEDDED 9e024292 (5 engine bugs, auto-index, count, CLI, model tests, shared inputs helper)
          EXT ecb272fa (per-path Store cache in both exts, DuckDB Quack server + benchmark)

## TODO — review sparsemap v5.7.0 and incorporate as needed (added 2026-09-27)
sparsemap = the maintainer's compressed bitmap library in C (sm.c/sm.h, MIT;
~/ws/sparsemap, codeberg.org/gregburd/sparsemap). Other projects vendor sm.c/sm.h
via contrib/*_sync.sh (e.g. pg_tre). ~/ws/sparsemap/rust/ has only packaged
crates up to 5.5.1 in target/ — no current Rust crate source, so using it from Rust
means either publishing a 5.7.0 crate or vendoring sm.c behind a -sys binding.
mentat does NOT use it yet. 5.7.0: small-set mode (<1024 bits stored as a bare uint64 array,
PostgreSQL Bitmapset layout, RLE-aware promote/demote), fixes to sm_equals/sm_hash/
sm_compare on differently-built equal maps, sm_split invalid output, sm_offset
overflow; zero-warning strict-flag build; wire format v2 unchanged.
Evaluate where it would help mentat, e.g.:
- entity-id sets in the query engine (VALUES joins for coll :in inputs, NOT EXISTS
  / as-of retraction sets, `pull` visited sets);
- the auto-index work (per-attribute usage tracking) and partition/tx high-water maps;
- pg_mentat: tx/entity bitmaps for history/as-of scans (it's PG-Bitmapset-compatible
  in small-set mode).
Deliverable: a short evaluation note (docs/) + a benchmark on a real hot path before
adding any dependency (pin by crates.io version or git tag, never a local path).

## Restart 2026-09-27 ~18:00 UTC (third wave; earlier agents timed out/stopped)
Committed so far: 60c4a966 (>=5461 tx panic), cac32747 (interrupted query), 3a1d1cd8 (pg AVET + 1.10.0 bump).
Uncommitted on floki: embedded issue-3 WIP (q_explain_temporal, scale_regressions.rs).
Agents: PG bed19273 (M2 count, M3 auto-index, M4 edn_q_rows, M5 gate)
        EXT ad3f21cd (E1 cache, E2 measure, E3 Quack packaging, E4 Quack bench, E5 gate)
        EMBEDDED 4cffaed8 (M3 as_of idx+migration+O(1) open, M4 sqlite flags+mmap, M5 auto-index,
                           M6 count, M7 shared options helper, M8 CLI, M9 model tests, M10 gate)
Decided: commit root .cargo/config.toml [env] LIBSQLITE3_FLAGS=-USQLITE_ENABLE_MEMORY_MANAGEMENT
  -DSQLITE_DEFAULT_MEMSTATUS=0 (8 clients: 20 -> 542 ops/s) + runtime mmap_size.
DuckDB registry #2812: marked ready for review; fork CI runs "action_required" (needs a
  DuckDB maintainer to approve; we got 403). Mergers are mostly sebastiaan-dev.

## 1.10.0 — DONE on master (unpushed), 2026-09-28
33 commits since origin/master (72ee3765..8583d5bf). Final gates on the committed tree:
fmt clean, clippy -D warnings 0 (workspace + both exts), 87 test binaries, mino/script/cli
features, cargo deny ok, SQLite + DuckDB smoke PASS, pg16+script 1916/0/1, pg16 1871/0,
mentatd builds. Not yet: version tag v1.10.0 / push (awaiting user), pg13-18 matrix on CI.
Registry PR #2812: ready for review; fork CI still "action_required" (DuckDB maintainer
must approve; we get 403). Its ref is 1.9.0 (5f03696b) — bump to the v1.10.0 commit when tagged.
Open follow-ups: WAL restart could also run on Store drop; process-wide ext store cache for
the Quack server; Quack backlog=5 upstream; retractEntity doesn't retract incoming refs;
cas/retractEntity lookup refs; pg 5-place pattern inside not/rules; edn_q_rows collects in
one call; sparsemap v5.7.0 evaluation (TODO above).

## 1.10.0 tagged + pushed (2026-09-28)
- Merged origin's v1.9.1 (pg_dump fix) first: upgrade edge is now
  pg_mentat--1.9.1--1.10.0.sql (also registers managed_indexes/index_evidence for
  pg_dump). EC2-verified: 1.9.0 -> 1.9.1 -> 1.10.0 in place, pg_dump/restore, 37
  dump-registered tables fresh == upgraded; pg16+script 1916/0/1; workspace green.
- v1.10.0 = 3d4e73a2. Its release run FAILED: build-sqlite-ext / build-duckdb-ext
  smoke "store not reused" (fd check assumed bash's .system exec; Ubuntu's dash
  forks). No GitHub Release, no PGXN. Fixed in 5d5b1573 (test-only; CI ext jobs
  green). Decision pending: v1.10.1 at 5d5b1573 (recommended; no tag move).
- Registry PR #2812 head 25eabfde: version 1.10.0, ref 3d4e73a2 (ext code identical
  at 5d5b1573). Workflows still await maintainer approval; comment posted.
- EC2: all mentat instances/keys/SGs deleted.
