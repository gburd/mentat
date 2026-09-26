# Resume checkpoint — post-1.7.0: T7, T12, DuckDB extension

## Released: mentat 1.7.0 is on Codeberg (master @ a9c5a459, tag v1.7.0).
User is handling: GitHub mirror enable + pg_mentat archive/pointer.

## NEW EC2 (relaunched 2026-09-26 for T7/T12/DuckDB work)
- AWS_PROFILE=hotdog, us-east-2, acct 373102893032.
- Instance i-0370864337eef216f (c7i.8xlarge, 150GB gp3), Rust 1.90 via rustup.
- Key ~/.ssh/mentat-ci-20260926-110310-key.pem, SG sg-099437a586cc7e4f4.
- Env: /tmp/mentat_ec2.env; ssh string: /tmp/mentat_ssh.txt; PUBIP in env.
- Connect: ssh -i KEY -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null ec2-user@PUBIP
- `. ~/.cargo/env` first. Repo at ~/mentat (rsync from floki, .git excluded).
- **TERMINATE when done**: AWS_PROFILE=hotdog aws ec2 terminate-instances --region us-east-2 --instance-ids i-0370864337eef216f ; then delete SG + key pair.
- deps installed: gcc/clang15/cmake/openssl/sqlite/python3. Workspace builds green.

## DONE (branch master, floki commits, not pushed)
- **DuckDB extension DONE** (90b082c2 M0, 4fa10af5 M1): crates/duckdb, crate mentat_duckdb,
  cdylib, loadable .duckdb_extension (DuckDB v1.5.5, USE_UNSTABLE_C_API version-lock, footer
  via extension-ci-tools submodule). mentat_transact scalar + mentat_query table fn over
  embedded mentat::Store (per-call open), all-VARCHAR cols; JOIN against native DuckDB proven.
  extension-ci-tools is a git submodule. Test: crates/duckdb/test/smoke.sh (needs DuckDB v1.5.5
  CLI - EC2 python3.9 wheel is 1.4.5, too old; used standalone CLI). M2 (pull+typed cols) /
  M3 (session db-path, mentat_eval, store cache) + :in bindings for mentat_query = follow-ups.
  Found+fixed 2 bugs (scalar double-eval -> volatile()=true; workspace-member build).
- **T12 DONE** (8a0d1758): all 3 sub-features on SQLite. History [?e ?a ?v ?tx ?added] via
  DatomsTable::Transactions + EvolvedPattern.added. As-of/since q: NEW API Store::q_once_as_of/
  q_once_since + Conn variants + TemporalBound::{AsOf,Since} (mentat takes plain tx arg, not
  JSON). Non-scalar :in: QueryInputs::with_collection/tuple/relation, VALUES join via ground
  machinery (were silently dropped before). Removed UnsupportedHistoryPattern; kept
  UnsupportedSource. 9 tests in crates/sqlite/mentat/tests/history_and_inputs.rs. UnsupportedInputBinding
  never existed (plan premise wrong). Combined green: 79 test binaries.

## ALL THREE DONE + CERTIFIED (branch master, floki, NOT pushed)
- **T7 DONE** (74fa9ad8/774b2cb2/432f4eeb + fmt): crates/script (mentat_script) extracted.
  ScriptBackend trait, install(), DbRef, value builders. mentat impl over Store (script.rs
  1018->638), pg_mentat impl over engine (759->489). One model suite (10 tests) runs on
  fake + SQLite + pg backends. pg Task-1c sandbox PRESERVED (6 sandbox tests green). inst/uuid
  builders emit real round-tripping values (dropped workaround). tx-report now carries
  :mentat.store/db-after on both backends.
- **T12 DONE** (8a0d1758): history/asof-q/coll-bindings on SQLite (above).
- **DuckDB DONE** (90b082c2/4fa10af5): crates/duckdb extension, above.
- **CERTIFIED on EC2**: workspace 83 test binaries green; pg16 with script 1888 pass 1 ignored;
  DuckDB extension rebuilt (v1.5.5) + smoke test PASS (load, transact, query, JOIN vs native);
  fmt clean; clippy --workspace --exclude pg_mentat --exclude mentat_duckdb -D warnings 0 errors;
  cargo deny advisories/bans/licenses/sources ok (added CDLA-Permissive-2.0 for webpki-roots).
  Fixed the extension-ci-tools submodule on floki (was rsync-polluted; now proper gitlink 20bad04c).

## REMAINING
- User handles: GitHub mirror + pg_mentat archive/pointer (1.7.0).
- Decide with user: release these as 1.7.1 (bugfix+features) or 1.8.0 (DuckDB = new consumer,
  arguably minor bump). NOT yet version-bumped or released - these are on master unpushed.
- TERMINATE EC2 i-0370864337eef216f + delete SG sg-099437a586cc7e4f4 + key when user says done.

## Gotchas (from the 1.7.0 session, still apply)
- Don't run pgrx matrix concurrently with other cargo on EC2 (shm/mutex cascade).
- rustfmt ignore=[vendor] is nightly-only (vendored gc gets fmt'd, that's fine).
- EC2 ~/mentat is NOT a git repo (rsync excludes .git); fmt on EC2 then rsync .rs back.
- background cargo over ssh: nohup setsid cmd >log 2>&1 </dev/null &, confirm log grows.
- concurrent agents: NON-delete rsync, git add only own paths.
