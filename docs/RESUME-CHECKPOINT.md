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
1. **mino deep-data recursion** (plan 1.24 tail; IN PROGRESS — background agent
   2788cfd5 is on it as of 2026-09-25; the reader cap is already applied
   UNCOMMITTED in mino/src/{depth.rs (MAX_DATA_DEPTH=512),reader.rs (ReadError::TooDeep,
   depth field + read_form guard),lib.rs (pub mod depth)}; the agent is also
   editing printer.rs/error.rs/hashing.rs/prim/collections.rs and will commit
   depth.rs/reader.rs/lib.rs as part of its work. If the agent died: check
   `git status mino`, its brief covered printer depth-cap+cycle, iterative-or-capped
   eq_val/hash32/default_cmp, print_str_checked wired to pr-str/str/prn/println/print,
   and mino/tests/deep_data.rs. Build/test on EC2 only.) Original NOT-done note — the background
   agent for it died with no commits). Fix Rust-stack recursion, all crash in a
   DEBUG build on a 2 MB stack (verified): reader (>=2000 nested `[`), printer
   pr-str (>=2000 nested vec), hash (>=2000), eq (>=20000), compare (>=20000),
   self-referencing atom pr-str (always). Fix = MAX_DATA_DEPTH cap in reader
   (prescan like edn/src/depth.rs), iterative eq_val/hash_val/compare + printer
   depth-cap/cycle-detect. See mino/src/{reader.rs,printer.rs,collections/hashing.rs,
   prim/collections.rs::compare}. Add mino/tests/deep_data.rs. A detailed brief
   was written for subagent 3d836d5b (died) — reuse it.
2. **Task 1c**: open mentat_eval to PUBLIC on the hardened interpreter, pg_mentat
   1.6.3. GUCs mentat.script_max_{steps,heap_bytes,depth} (PGC_SUSET) in
   _PG_init (pg_mentat/src/planner/hooks.rs has the define_int_guc pattern);
   pg_mentat/src/functions/script.rs build_interpreter() -> sandboxed()+limits
   +check_hook(check_for_interrupts + stack_is_too_deep). NO REVOKE (user: leave
   open). Tests as ordinary role. Then tag pg_mentat 1.6.3 (deps + reconcile +
   1c all ship together), push to codeberg.
3. **Task 3-6**: workspace lints, release profile, pinned toolchain; import
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
