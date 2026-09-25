# Resume checkpoint — mentat/pg_mentat merge + mino hardening

Written 2026-09-25 at ~91% context. Read this + the plan
`~/ws/mentat/docs/superpowers/plans/2026-09-24-merge-pg-mentat.md` to resume.

## Where work happens
- **All build/test/benchmark on EC2**, NOT floki (user directive). Host:
  AWS profile `hotdog` (acct 373102893032), us-east-2, instance
  `i-0ef5dddfe262f499e` (c7i.8xlarge). Conn string in `/tmp/mentat_ssh.txt`,
  env in `/tmp/mentat_ec2.env`. `. "$HOME/.cargo/env"` first; rust 1.90.
  Repos at `~/mentat` (branch pg) and `~/pg_mentat` (branch deps-1.6.3),
  rsync'd from floki (`.git` excluded). PG 13-18 all `cargo pgrx init`'d.
  Build cmd: `$SSH '. ~/.cargo/env; cd ~/mentat && cargo test --workspace'`.
  pg test: `cd ~/pg_mentat/pg_mentat && cargo pgrx test --no-default-features --features pg16 pg16`.
- Git history lives on floki. Workflow: edit on floki, rsync up, test on EC2,
  commit on floki. **TERMINATE at the very end**:
  `AWS_PROFILE=hotdog aws ec2 terminate-instances --region us-east-2 --instance-ids i-0ef5dddfe262f499e`
  then delete SG `sg-0fb5d9c96b7fcfe8f` + key pair `mentat-ci-20260925-102527-key`.

## DONE (committed on floki, not pushed except where noted)
- **pg_mentat 1.6.2** (branch main, PUSHED to codeberg + tag v1.6.2, mirrored to
  GitHub): edn nesting-depth cap (server-crash fix), non-superuser mentat_query
  fix (set_local_guc PGC_SUSET-if-superuser), instant UTC fix. All PG13-18 green.
- **mentat edn nesting cap**: commit 5313594f (edn/src/depth.rs MAX_NESTING=256,
  wraps parse:: entry points).
- **mino moved in-tree**: `~/ws/mentat/mino/` is the mino-rs crate (workspace
  member; mentat dep `path="mino"`; corpus vendored at mino/tests/corpus;
  no local-fs paths). Commits 230e4cea, 69879911.
- **mino sandbox + limits** (commits 0e789fd7..620a2a2b): Interpreter::sandboxed(),
  Limits{steps,heap_bytes,depth}, set_check_hook, take_output; uncatchable limit
  trips; heap charging in bulk prims; mentat src/script.rs uses sandboxed()
  steps 10M/heap 64MiB/depth 1000. Tests mino/tests/sandbox.rs (11).
- **mino tail calls** (b707f571, 4b503190): Value::TailCall, apply_closure
  trampoline; dotimes/while/self+mutual recursion constant-stack.
  mino/tests/tail_calls.rs (9).
- **mino iterative GC** (f02423b9, 9e867d96): vendored rust-gc v0.5.1 at
  mino/vendor/gc + gc_derive (MPL-2.0, VENDORED.md/CHANGES.md), marking uses a
  worklist (GcBoxHeader.this + MARK_STACK). mino dep gc = path vendor/gc.
  Workspace members include mino/vendor/gc + gc_derive. Deep flat lists +
  200k-nested data no longer crash. mino/vendor/gc/tests/deep_chains.rs (4).
- **pg_mentat deps** (branch deps-1.6.3, commit a008770): lru 0.18, prometheus
  0.14->protobuf 3.7, h2 0.4.19, rustls 0.23.45, chacha20 0.10.2. cargo audit +
  deny clean (only paste/serde_cbor unmaintained-via-pgrx ignores). deny.toml +
  audit.yml updated. Validated pg16 1846 pass (before the reconcile).
- **Task 2 reconcile** (mentat d447e1f7+75d6cf3d branch pg; pg_mentat eec7abb
  branch deps-1.6.3): edn/core/core-traits byte-identical between repos.
  5-place Pattern, :in src vars->in_sources + bindings, :rules/:with rules,
  plain-keyword values, enumset, Limit::Unlimited, itertools 0.15.
  mentat workspace 631 pass; pg_mentat pg16 1846 pass. Both VALIDATED on EC2.
- **README** (e348e7bd): EDN/Datalog learning links from /tmp/edn.

## REMAINING (in order)
1. **mino deep-data recursion**: DONE (commits 41b153d4, c321edfc). reader cap
   (MAX_DATA_DEPTH=512), printer depth-cap + cycle detection + print_str_checked,
   iterative eq_val, depth-capped hash32/default_cmp. EC2-verified:
   112+12+7+11+9 mino tests, workspace 638, mino_script 11 green. This completes
   Task 1b entirely (mino is now safe for untrusted mentat_eval input).
2. **Task 1c**: open mentat_eval to PUBLIC on the hardened interpreter, pg_mentat
   1.6.3.

   *** BLOCKER / DECISION (found 2026-09-25): pg_mentat pins `mino-rs` at git tag
   v0.1.0 (standalone repo, codeberg.org/gregburd/mino-rs), which has NONE of the
   hardening. The hardened mino lives ONLY in ~/ws/mentat/mino (in-tree, not
   pushed; version 0.14.0). Task 1c needs the hardened mino. Options:
     (a) push hardened mino to the standalone repo as v0.2.0, pg_mentat deps on it
         — but plan 1.25 retires the standalone repo, so this is a throwaway.
     (b) pg_mentat deps via the mentat repo git tag — needs mentat pushed (it isn't).
     (c) DEFER 1c until after the repo merge (Tasks 4-5), when pg_mentat's mino-rs
         dep becomes a workspace path. Then 1c is trivial. mentat_eval is
         off-by-default until the merged repo ships, so there is NO live exposure
         to race — nothing forces 1c before the merge. RECOMMENDED: option (c),
         i.e. do Tasks 3-5 (merge) first, then 1c falls out.
   RESOLVED 2026-09-25: user chose merge-first. Doing Tasks 3-6 (merge) then
   Task 1c falls out when pg_mentat's mino-rs dep becomes a workspace path.
   The 1c mechanics themselves (below) are ready: to PUBLIC on the hardened interpreter, pg_mentat
   1.6.3. GUCs mentat.script_max_{steps,heap_bytes,depth} (PGC_SUSET) in
   _PG_init (pg_mentat/src/planner/hooks.rs has the define_int_guc pattern);
   pg_mentat/src/functions/script.rs build_interpreter() -> sandboxed()+limits
   +check_hook(check_for_interrupts + stack_is_too_deep). NO REVOKE (user: leave
   open). Tests as ordinary role. Then tag pg_mentat 1.6.3 (deps + reconcile +
   1c all ship together), push to codeberg.
3. **Task 3 DONE** (mentat 5406c30a, pg_mentat 3cf4a75): toolchain pinned 1.90
   (plan wanted 1.98, EC2 has 1.90 — ponytail note in both rust-toolchain.toml),
   workspace clippy lints table, release+release-dev profiles, edn/core/core-traits
   opt into lints. Both green on EC2. NOTE: this broke the byte-identical property
   of the 3 shared crates (added [lints] to their Cargo.toml) — Task 4/5 re-handles.
4. **Tasks 3-6 DONE** on branch merge/pg-mentat (single unified workspace):
   - Task 4 (9051419a merge + 1307b091 drop): imported pg_mentat's 1265 commits under
     crates/pg/, tags v1.2.1..v1.6.2, source byte-identical to canonical edn/core/core-traits.
   - Task 5 (8c4f9a73 moves + dc861f83 wire): restructured into crates/{edn,core-traits,
     core,mino,sqlite/*,pg/{pg_mentat,mentatd}}. Single workspace, Cargo.lock committed,
     workspace.dependencies table added (per-crate .workspace=true conversion DEFERRED).
     pg_mentat now deps mino-rs via path=../../mino (the HARDENED in-repo mino) -> UNBLOCKS
     Task 1c. EC2 green: cargo test default, -p mentat --features mino, -p mino-rs (112+12+7+11+9),
     pgrx pg16 1846, mentatd builds w/o pg_config. fixtures at crates/sqlite/fixtures (workspace
     exclude); tools/mentatweb stub left in tools/.
   - Task 6 (cd207eb3 flake, f0844f54 delete dead weight, 0ec7d223 forgejo gate,
     30751bb6 github publish workflows, d6176259 Makefile, 8c4b37b7 CHANGELOG,
     43c6fb58 mdBook, cd030488 fmt): one flake.nix (pgrx + sqlite devShells,
     packages mentat-cli/mentatd/pg_mentat-pg{14-18}, rust 1.90, nix flake check
     passed on floki), deleted sdks/automation/_/vscode/dead-CI, .forgejo/ci.yml
     gate + .github publish workflows (release.yml gated >=v1.7.0 + mentat-cli
     binary), root Makefile, CHANGELOG.md canonical (was pg_mentat's), one mdBook
     at docs/ (Jekyll dropped, superpowers/RESUME preserved). cargo fmt --all
     clean workspace-wide (vendored gc reformatted, VENDORED.md notes it).
   - EC2 GREEN after all: cargo test default, -p mentat --features mino,
     -p mino-rs (112+12+7+11+9), pgrx pg16 1846, fmt --check clean, mentatd builds.
   - **Task 1c DONE** (5d7db9e5 + 2b534591): mentat_eval sandboxed() + 3 SUSET GUCs
     (script_max_steps/heap_bytes/depth) + check hook; 7 security pg_tests; 1884 pg16
     with 'script', 1846 without. NO REVOKE, not SECURITY DEFINER.
   - **Task 8 DONE** (f1bf4213..8a3d9345, 11 commits): mino refreshed to upstream 9c65bb50.
     #uuid->Value::Uuid + prints #uuid"..", #inst prints constructor form, read-string,
     classed/keyword catch, regex lookahead, delay type, store backend seam (byte-identical
     file format), new core.clj prims. Corpus 14/14 green. BigDec DEFERRED (ponytail note +
     #[ignore]d test bigdec_literal_reads in reader.rs). uuid dep added.
   - **Task 10 DONE** (faff7919 algebrizer, 4f4b6761 projector, 8b807064 db, 4f6d8d9c pull,
     da65b3c9 edn test clippy): 15 unimplemented!()/todo! sites -> typed errors (new
     AlgebrizerError::UnsupportedBigInteger/UnsupportedSource, DbErrorKind::NotYetImplemented)
     or justified unreachable!(); dead resolve_argument fn deleted; found+guarded a genuinely
     reachable fulltext arm. [lints] workspace=true on query-algebrizer(-traits), query-projector
     (-traits), db(-traits), query-pull(-traits). edn test files clippy-clean.
   - **Combined green** (a543c311): workspace 77 binaries, mino 116+14+7+11+9, pgrx pg16 1846.
     Fixed 1 stale mentat test (inst/uuid round-trip now real values).
   - TODO: Task 7 (mentat-script crate - can now drop inst/uuid workaround),
     11 (SQLite cas/retractEntity), 12 (SQLite history/asof q + coll bindings -> removes
     UnsupportedHistoryPattern/UnsupportedInputBinding gates), 13 (README/docs),
     14 (release 1.7.0 + retire pg_mentat). Then benchmark + prod-readiness.
   - Remaining old note (superseded): workspace lints/profile/toolchain; import
   pg_mentat history into mentat under crates/pg/ (git filter-repo, plan Task 4);
   restructure into crates/ (Task 5); one flake + CI + delete dead weight (Task 6).
4. **Task 7**: mentat-script crate (shared scripting layer, ScriptBackend trait).
5. **Task 8**: refresh mino from upstream 9c65bb50 (real #uuid/#inst/read-string,
   classed catch, regex lookahead, bigdec, delay, store backend seam).
6. **Task 10-12**: SQLite unimplemented!() -> errors; cas/retractEntity in the
   SQLite transactor; history/as-of q + :in coll bindings (turns on the
   UnsupportedHistoryPattern / non-scalar in_binding gaps).
7. **Task 13-14**: docs; release 1.7.0, merge to master (--no-ff), tag, retire
   pg_mentat repos read-only.

## Gotchas
- rsync single files: keep the subdir (`edn/tests/x` not `edn/x`).
- gio trash fails on /tmp (mv to ~/.local/share/Trash/files/ instead of rm -rf).
- pg_mentat needs its repo-local cargo-pgrx (on EC2, the rustup-installed 0.17).
- background subagents keep dying on connection errors ~25-90min in; keep tasks
  small, commit after each step, and steer if idle >30min with no writes.
- `git fetch <other-repo>` imports its tags unless --no-tags.
