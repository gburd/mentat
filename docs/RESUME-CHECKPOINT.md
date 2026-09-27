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
