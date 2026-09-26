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

## TODO NEXT (after T12 lands, to avoid crates/sqlite/mentat overlap)
- **T7** (mentat-script shared crate): extract crates/script with ScriptBackend
  trait; mentat + pg_mentat both implement it; one model test suite. Plan §1.19/Task 7.
  Can drop the inst/uuid output-only workaround (mino now round-trips them - Task 8).
- Then: full green gate (workspace + pg16 + duckdb + mino), fmt/clippy/deny, commit.
- Decide: does DuckDB extension go into 1.7.1 or a 1.8.0? (new consumer = minor bump).
- TERMINATE EC2.

## Gotchas (from the 1.7.0 session, still apply)
- Don't run pgrx matrix concurrently with other cargo on EC2 (shm/mutex cascade).
- rustfmt ignore=[vendor] is nightly-only (vendored gc gets fmt'd, that's fine).
- EC2 ~/mentat is NOT a git repo (rsync excludes .git); fmt on EC2 then rsync .rs back.
- background cargo over ssh: nohup setsid cmd >log 2>&1 </dev/null &, confirm log grows.
- concurrent agents: NON-delete rsync, git add only own paths.
