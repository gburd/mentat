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
