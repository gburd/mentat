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

## DONE 2026-10-15 — sparsemap evaluated, not adopted (docs/design/sparsemap-evaluation.md)
(Original TODO, added 2026-09-27, kept for context:)
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

## v1.10.1 RELEASED (2026-09-28)
- Tag v1.10.1 = 3e1100e3 (1.10.0 + test-script fix 5d5b1573, empty upgrade edge
  pg_mentat--1.10.0--1.10.1.sql). CI green on it (all jobs, pg13-18).
- Release run 36429089526: attempts 1-2 failed build-duckdb-ext on a rustup
  mid-build component install ("detected conflict: bin/cargo-fmt|cargo-clippy");
  fixed in abe1f605 (release.yml installs rustfmt+clippy with the toolchain);
  attempt 3 green. GitHub Release has 6 assets; PGXN shows pg_mentat 1.10.1; the
  pg tarball ships 1.9.0->1.9.1->1.10.0->1.10.1.
- v1.10.0 stays as a tag with no release (documented in CHANGELOG 1.10.1).
- Registry PR #2812 head 8bda202a: version 1.10.1, ref 3e1100e3. Still awaits a
  maintainer to approve its build workflows. COMMUNITY_EXT_PAT is unset, so
  release.yml's registry job skips (bumps are manual on branch mentat-1.8.0).

## v1.10.3 RELEASED (2026-10-02) — DuckDB registry fixes
- PR #2812 at v1.10.2 (e5613091, cut 2026-09-30 by another session; release run
  cancelled, no pg/META bump) still failed macOS + Windows. Two causes:
  (1) crates/duckdb/Makefile `TARGET_INFO=-p mentat_duckdb` replaced ci-tools'
  macOS `--target <triple>` -> now `+=`; (2) registry DUCKDB_LATEST_STABLE moved
  to v1.5.6 and our unstable-C-API build was v1.5.5 -> duckdb-rs ~1.10506.0,
  TARGET_DUCKDB_VERSION/CI/release/docs v1.5.6.
- v1.10.3 = a835c85e. Verified BEFORE touching the PR by running the registry's
  build.yml on fork branch mentat-test-1.10.3 (deleted after): all 5 platforms
  green, osx_arm64 + windows ran SQLLogicTest (linux_amd64/arm64 and osx_amd64
  skip tests). Local ubuntu:24.04: make test_release SUCCESS (venv duckdb 1.5.6),
  smoke PASS with v1.5.6 CLI, v1.5.5 CLI refuses the binary.
- mentat CI green on a835c85e; release run green first try: GitHub Release 6
  assets, PGXN pg_mentat 1.10.3, pg edge pg_mentat--1.10.1--1.10.3.sql.
- PR #2812 head 0aaca0f0 (version 1.10.3, ref a835c85e); comment posted correcting
  the 1.10.2 claim. Its CI is "action_required" again (each push to the PR needs
  maintainer approval for fork workflows).
- Treadmill: every DuckDB patch release (registry stable bump) needs a mentat
  release while on the unstable C API. See docs/registry-publishing.md.

## IN PROGRESS: native DuckDB storage for the DuckDB extension (2026-10-05)
Design: docs/duckdb-native-storage-plan.md. User: "fix the DuckDB extension so that it uses
DuckDB storage, test, qualify and re-release it. Don't change the README and registry description."
- M0 spike done. M1 (storage seam) DONE, all on master, unpushed:
  8692a552 SqlConn/SqlValue seam + SqlValue codec; 01b8bc91 transactor via &dyn MentatStoring;
  780b0820 projector/pull/cache via the seam + run_algebrized_query_on/q_once_on + DuckDB identifier quoting.
  Gate: workspace 87/87, mino/CLI/sqlite-ext features, clippy clean.
- M2 DONE b2bc60a1/76266cf5: crates/duckdb/store (mentat_duckdb_store) + tests-harness (separate
  workspace, bundled duckdb): 7/7. M3 DONE 5a6b850b/7258fca3: extension stores in DuckDB schemas;
  SQLLogicTest SUCCESS (duckdb 1.5.6 in ubuntu docker), smoke PASS.
  Gotchas: entrypoint db handle only valid during init -> one connection made there, calls
  serialized; CREATE TYPE in non-main schema breaks WAL replay on 1.5.6 -> inline UNION type;
  `end` reserved; no partial indexes / no index on UNION; named+positional params can't mix.
- NEXT M4 qualify: mino model suite on DuckDB backend, broader Datalog cases, perf vs 1.10.3,
  registry build on fork (5 platforms). M5: release 1.11.0 + registry PR (new PR; #2812 merged).
- EC2 dev box (account 170848442262 = hotdog profile now): /tmp/mentat_duck.env
  (DEV_ID i-0d9deacbbbb1b24bd, DEV_IP 3.21.158.53, key mentat-duck-20261005-135857-key, SG sg-03e8945a1022cb82d).
  Sync with /tmp/sync.sh (never --delete, excludes submodule/target/.cargo registry).
- M4 (2026-10-05, in progress): differential test vs SQLite (60 queries x now/asOf/since)
  PASSES and is proven to catch regressions; model suite 12/12 on DuckDB. Perf A/B at xs
  (1.10.3 SQLite-in-ext vs new DuckDB storage, single client, median ms): point 0.58 vs 2.95,
  ref_traversal 2.54 vs 12.1, aggregate 2.97 vs 3.67, predicate 3.41 vs 11.4, pull 0.55 vs 3.0,
  as_of 8.7 vs 25.4, since 1.23 vs 2.33, inputs 0.90 vs 4.9; small tx 185 vs 69 tx/s;
  load 2.3 s vs 5.6 s. ROOT CAUSE: the UNION v column (q2 on typed columns 3.3 ms vs 16.4 ms
  on UNION; q1 1.5 vs 2.4). NEXT: add typed shadow columns v_i BIGINT/v_d DOUBLE/v_s VARCHAR
  next to UNION v (written on insert), render typed-constant equality, e=v joins, numeric
  compares, aggregates and known-type projections on them in the DuckDB dialect; re-run A/B
  at xs and s; then registry fork build, CHANGELOG, 1.11.0.
  Box scripts: ~/ab.sh (SC=xs|s), ~/abtab.py, /tmp/plan4.py (q2 layout probe).
- 2026-10-05 21:15Z: 1.11.0 prepared and pushed (master 89288a08, NOT tagged). EC2 release gate
  ALL GREEN (fmt, clippy ws+exts, 89 test bins, features, deny, both ext smokes, SQLLogicTest
  on ubuntu+duckdb 1.5.6, harness 21, pg16+script 1916/0/1, pg16 1871/0). Perf A/B committed
  (95bf16b4, benchmarks/results/duckdb-native-*/findings.md). BLOCKED on GitHub Actions major
  outage: mentat CI (run 37371816400) and the fork registry build (gburd/community-extensions
  branch mentat-test-1.11.0, run 37371906132) were cancelled unrun. NEXT when GitHub recovers:
  re-run both; if green tag v1.11.0 at 89288a08, watch release.yml + PGXN, open a NEW registry PR
  (version 1.11.0, ref = tag commit; #2812 already merged), delete fork test branch, terminate
  EC2 i-0d9deacbbbb1b24bd (+ key mentat-duck-20261005-135857-key, SG sg-03e8945a1022cb82d).

## v1.11.0 RELEASED (2026-10-05) — DuckDB extension stores its datoms in DuckDB
- Tag v1.11.0 = 89288a08. mentat CI green (all jobs incl. pg13-18, test-duckdb + store harness);
  release run green: GitHub Release 6 assets (duckdb ext 2.7 MB tarball), PGXN pg_mentat 1.11.0.
  Release asset verified in a clean ubuntu container: datoms land in DuckDB, no file written.
- Registry: NEW PR duckdb/community-extensions#2935 (branch gburd:mentat-1.11.0, version 1.11.0,
  ref 89288a08). Same descriptor passed the registry build on the fork (5 platforms; SQLLogicTest
  on osx_arm64 + windows). #2812 (initial listing) was merged 2026-10-05.
- README / registry description intentionally NOT updated (user instruction); they still say
  "embeds the mentat SQLite store". CHANGELOG lists this under Known gaps.
- EC2 (account 170848442262) terminated; key + SG deleted.
- Follow-ups: connection pool (calls serialized), batch per-call statements (point lookups
  ~3 ms), fulltext on DuckDB, ATTACHed-database stores, migration from 1.10.x SQLite files.
- 2026-10-05 later: README / extension README / registry description DID get updated (c3010c5c,
  user request); registry PR #2935 MERGED 2026-10-06.

## v1.11.1 RELEASED (2026-10-07) — Nix derivations named after the real version
- Report from solnix.io: `nix build github:gburd/mentat/<tag>#pg_mentat-pg18` named the
  derivation pg_mentat-pg18-1.6.2 on every tag since v1.9.1 (flake.nix hardcoded extVersion
  1.6.2 / cargoVersion 1.6.1). Fixed: 20cbe2b9 (flake reads pg_mentat.control default_version
  + Cargo.toml [workspace.package].version; checks.<sys>.version-sync), d70100f7 (CI nix-build
  builds version-sync and asserts pg16/pg18 .version == control; release.yml gate requires
  Cargo.toml, pg_mentat.control, pg_mentat/Cargo.toml, META.json == tag; README sandbox note).
- Tag v1.11.1 = 065bf94b (no-op pg_mentat--1.11.0--1.11.1.sql). EC2 gate green (same set as
  1.11.0 + 1.11.0->1.11.1 in-place upgrade + pg_dump/restore). Local `nix build
  --option sandbox relaxed` of pg_mentat-pg18-1.11.1 loads in nixpkgs PG 18.3.
  `nix eval github:gburd/mentat/v1.11.1#packages.x86_64-linux.pg_mentat-pg18.name` =
  pg_mentat-pg18-1.11.1. Release run green: 6 assets, PGXN 1.11.1.
- DuckDB extension code unchanged since 1.11.0, so no registry bump (registry stays 1.11.0).
- EC2 i-02fa4111dc910ed11 + SG + key deleted.

## v1.12.0 RELEASED (2026-10-11) — embedded engine published to crates.io
- Tag v1.12.0 = b8b72e74. 22 SQLite-engine crates published to crates.io for
  the first time, under free package names (library names unchanged, so
  `use mentat::`/`use edn::`/`use gc::` still compile):
  mentat->mentatdb(lib mentat), edn->mentat_edn(lib edn),
  vendored gc/gc_derive->mino_gc/mino_gc_derive(libs gc/gc_derive),
  core_traits/db_traits/sql_traits/public_traits/query_*_traits->mentat_*.
  All at 1.12.0 except mino_gc 0.5.1 / mino_gc_derive 0.5.0 (upstream).
  publish=false: pg_mentat, mentatd, SQLite+DuckDB extensions, mentat_ffi,
  benchmark runner.
- GitHub Release v1.12.0: 6 assets. PGXN pg_mentat 1.12.0. Registry PR opened
  by release.yml (DuckDB ext code unchanged since 1.11.0). pg upgrade edge
  pg_mentat--1.11.1--1.12.0.sql is a no-op.
- Qualified in a clean ubuntu:24.04 Docker container (AWS hotdog creds were
  gone again, so no EC2): cargo metadata (22@1.12.0), fmt, clippy -D warnings
  (ws+sqlite ext), cargo test 89 ok/0 fail, features (mino/script/mino-rs/ffi),
  cargo deny, cargo publish --dry-run, SQLite ext smoke. Verified a clean
  external consumer builds with just `mentatdb = "1.12"` and runs a query.
- Follow-up commits after the tag (included in the published crates, not the
  tag): bd84c46a add repository to 16 crates; 2bec3f0d version the mino->gc
  dep; 24b5fed9 drop version on the circular projector_traits dev-dep;
  ea242e58 add mentatdb description. See docs/crates-io-publishing.md for the
  runbook + the 5 gotchas (rate limit, dep versions, circular versioned
  dev-deps, required description, cargo-deny self-dev-dep panic).
- NO EC2 used (hotdog creds absent). Publish token: ~/.cargo/credentials.toml;
  CARGO_HOME must be unset (repo .cargo/ has no creds).

## v1.13.0 RELEASED (2026-10-14) — PostgreSQL ergonomics from a Datomic-team review
- Tag v1.13.0 = 87564a17. Came out of a Datomic/Clojure/Postgres-expert review
  of pg_mentat (report at /tmp/pg_mentat-review-2026-10-14.md). Filed 8 Codeberg
  issues #325-332 (GitHub has issues DISABLED; it's a mirror). This release
  closed #329, #331, #332; verified #327 on PG. Deferred (open, need design):
  #325 user-defined txfns, #326 retractEntity ref-cascade, #328 scripting-q over
  history, #330 explain collection :in.
- Changes (no engine/storage/semantics change):
  - #329 deprecation WARNING (one/session) on mentat_transact/query/pull/eval;
    README says edn_* is canonical.
  - #331 trusted=true injected into the PACKAGED control file by release.yml +
    flake.nix (pgrx 0.17 rejects it in source; PG accepts it). Verified in the
    shipped pg16 tarball.
  - #332 optional pg_fts (BM25) + pg_turbovec (vector) helpers sql/28,29, soft
    has_<ext>() pattern. Follow-up (open): wire (bm25-match)/(turbovec-near)
    where-fns in functions/query.rs.
  - #327 cas/retractEntity lookup refs already work on PG (resolve_entity_place);
    added test; retitled #327 to SQLite-only scope.
  - deps: rustls 0.23.45->0.23.46 (RUSTSEC-2026-0346..0349); fixed flaky
    mentatd test_touch_resets_expiry (50ms TTL race -> 500ms).
- Qualified on EC2 Debian 12 (KIWI account 830118047484, hotdog creds still
  absent): pgrx pg16 1872/0, pg16+script 1917/0/1; local SQLite 89 bins, fmt,
  clippy, deny clean; EC2 DuckDB ext build+smoke+harness 21; CI green pg13-18 +
  nix-build; release.yml green (6 assets, PGXN 1.13.0). crates.io: all 20
  changed crates published at 1.13.0 (gc crates stay 0.5.x) — NO rate limiting
  on version bumps of existing crates (unlike the 1.12.0 new-crate burst).
  DuckDB registry: skipped (no PAT; ext unchanged since 1.11.0).
- Announcement: GitHub Release v1.13.0 body (edited to a human-facing summary).
- EC2 i-058dc2f42a620ade0 + SG + key deleted.

## v1.14.0 RELEASED (2026-10-14) — rest of the Datomic-team review
- Tag v1.14.0 = caa7a0cc. Commits: ec68db6e (#332 bm25-match / turbovec-near
  where-fns + fix attach_turbovec_attribute; #330 EXPLAIN/query_sql/query_view
  honour collection :in), feaca8b9 (#326 retractEntity retracts inbound refs on
  all 3 backends + pg reads current_datoms not the log; #327 lookup refs in tx
  fns on SQLite/DuckDB; #328 scripting q on as-of/since), 9112c8fc (#325 user
  tx-functions :db/fn, pg script builds, functions/txfn.rs pre-pass,
  mentat.txfn_max_depth, bootstrap entid 20), e1cd344f (2nd flaky mentatd
  session test), af978018 (docs/design/*.md), caa7a0cc (release bump +
  pg_mentat--1.13.0--1.14.0.sql).
- Issues #325-#332 all closed on Codeberg; follow-up #333 = tx-functions on
  the embedded SQLite/DuckDB backends.
- Qualified on EC2 Debian 12 (kiwi 830118047484): pgrx PG16+PG17 1880/0,
  +script 1933/0/1 (PG17 with pg_fts 1.12.0 + pg_turbovec 2.12.1 built and
  installed), DuckDB ext build+smoke+SQLLogicTest+harness, upgrade 1.13.0->1.14.0
  == fresh (204 fns prosrc md5 + bootstrap rows), tx-fn on upgraded db,
  pg_dump/restore; local SQLite 89 bins, fmt, clippy, deny. Each intermediate
  commit compiles (no-script and script). CI + installcheck green on caa7a0cc.
- Released: GitHub Release (6 assets, trusted control, notes edited),
  PGXN 1.14.0, crates.io 1.14.0 (20 crates; gc stays 0.5.x). A clean consumer
  on mentatdb =1.14.0 verified cas-via-lookup-ref + inbound-ref retraction.
- EC2 i-02397aec9cd81a2ef + SG + key deleted.

## master after v1.14.0 (2026-10-15) — unreleased, CHANGELOG "Unreleased" is current
- fa516c92 #333: tx-functions on embedded SQLite (mino) + DuckDB, shared
  `mentat_script::txfn`; `:db/fn` defined lazily (no store migration). #333 closed.
- 7db12005 sparsemap evaluated, not adopted (docs/design/sparsemap-evaluation.md).
- a377c4d7 DuckDB: duckdb/duckdb#26828 (1.4.0-1.5.6: replayed WAL rows lost
  from secondary ART indexes at the next checkpoint, so `a = X` returns 0 and
  DELETE fails FATAL). Stores now have no secondary indexes, and `open` drops
  the 1.11-1.14 ones, which also repairs a store that was already hit.
  Verified on a store corrupted by the released 1.14.0 extension.
- 29f136fb..9fabfaf2 DuckDB perf: `MENTAT_DUCKDB_POOL` connection pool plus a
  per-store write lock. Point lookups at 8 threads went ~320 -> ~1500 ops/s.
  Single-client latency is unchanged (DuckDB scan-bound; next lead: cluster
  datoms by (a, e)). Only 3 parameterized bookkeeping statements are cached:
  in DuckDB 1.5.x, a reused plan keeps stale statistics
  (duckdb/duckdb#24696/#24182, fixed on main, not released).
  Findings: benchmarks/results/duckdb-perf-2026-10-15T010700Z/findings.md.
- External:
  - DuckDB registry PR duckdb/community-extensions#3072 (1.14.0) is open.
  - Codeberg PR #324 is closed.
  - The Quack backlog issue is duckdb/duckdb-quack#318.
- EC2 (kiwi): every mentat-* instance, SG and key is deleted. The hotdog
  profile is gone; use kiwi.
- (superseded: 1.15.0 is released, see below)

## v1.15.0 RELEASED (2026-10-15) — tag 88ee547f
- Gate (EC2 c6i.4xlarge, kiwi):
  - pgrx PG16/17 1880/0, +script 1934/0/1.
  - 1.14.0->1.15.0 upgrade == fresh (202 functions, 327 objects, idents and
    schema rows).
  - DuckDB: harness, smoke, SQLLogicTest, concurrency, the #26828 repro, and
    repair of a 1.14.0-corrupted store.
- GitHub Release has 6 assets (shipped control file has `trusted = true`).
  PGXN and crates.io are at 1.15.0; a clean consumer ran a tx-fn on
  mentatdb =1.15.0.
- DuckDB registry PR duckdb/community-extensions#3072 is retitled "Update
  mentat to 1.15.0" (ref 88ee547f, 1.11.0 -> 1.15.0). The fork build passed
  on all 5 platforms. It waits on the DuckDB maintainers.
- EC2: all mentat-* instances, SGs and keys are deleted.

## master after v1.15.0 (2026-10-17): PostgreSQL 19 support (unreleased)
- 3d0e1608: pgrx 0.17 -> 0.19.3 (pgrx 0.17 has no PG19 bindings, which was the
  user's PG19rc1 build failure). Toolchain 1.90 -> 1.96 everywhere; the
  pgrx_embed bin is gone. CI test-pg matrix 13..19, where pg19 builds 19rc1
  from source because pgrx's download fetches 19beta4. duckdb 1.10506.1
  (zip 9, RUSTSEC-2026-0367..0369).
- Verified:
  - EC2: pgrx +script on 13..19rc1, 1934/0/1 each; plain pg19/pg16 1880/0;
    DuckDB gate.
  - Local: Rust 1.96 gate.
  - nix pg18 smoke.sql.
  - GitHub CI green, including test-pg (pg19).
- AWS: the kiwi burner was REPLACED on 2026-10-17. kiwi is now account
  505435643998. The old 830118047484 is gone, along with an orphaned
  mentat-pg19 box that went with it. The new account has others' instances
  (libumem-*, pgtv-*, undo-*, pcompress-dev, pgprop-*); never touch them.
  Every mentat-* resource in the new account is deleted.
- (superseded: 1.16.0 is released, see below)

## v1.16.0 RELEASED (2026-10-17): tag 28817a03, PostgreSQL 19 support
- release.yml builds pg15..pg19. The pg19 leg adds the PGDG component "19"
  (19~rc1).
- GitHub Release has 8 assets (pg15-19, CLI, SQLite ext, DuckDB ext).
  PGXN, and crates.io (20 crates), are at 1.16.0.
- Verified the released tarballs on Ubuntu 24.04 + PGDG 15.19, 16.15, 17.11,
  18.6 and 19rc1:
  - smoke.sql PASS on each;
  - a real upgrade from the 1.15.0 tarballs (pg15/16/17) via ALTER EXTENSION
    UPDATE keeps data written under 1.15.0;
  - upgrade == fresh on PG16/17.
- pgrx +script 1934/0/1 against PGDG pg18/pg19.
- flake: no pg_mentat-pg19, because the locked nixpkgs lacks postgresql_19
  (nixos-unstable has 19beta4). Add it when nixpkgs ships 19.0.
- #331 REOPENED: non-superuser CREATE EXTENSION never worked ("permission
  denied for language c"). PG ignores trusted=true unless superuser=true.
  The fix needs superuser=true (pgrx accepts trusted then) plus GRANTs on
  the mentat schema, which is a security-model decision for the user. Noted
  as a Known issue in the 1.16.0 CHANGELOG and release notes.
- DuckDB registry PR #3072 is now "Update mentat to 1.16.0" (ref 28817a03).
  The fork build passed on all 5 platforms; waiting on maintainers.
- EC2 (kiwi 505435643998): all mentat-* resources deleted.
