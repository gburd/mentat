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

## IN FLIGHT (branch master, floki commits, not pushed)
- **DuckDB extension** (agent 28939d29): crates/duckdb per docs/duckdb-extension-plan.md.
  M0 scaffold (loadable .duckdb_extension via extension-ci-tools footer) + M1
  (mentat_transact scalar + mentat_query table fn over embedded mentat::Store,
  all-VARCHAR). duckdb-rs ~1.10505.0 = DuckDB v1.5.5, USE_UNSTABLE_C_API=1 (version-lock).
  crate mentat_duckdb, cdylib, workspace member NOT default-member (like pg_mentat).
- **T12** (agent 35015d64): SQLite algebrizer history [?e ?a ?v ?tx ?added] via
  DatomsTable::Transactions, as-of/since q, non-scalar :in coll/tuple/rel bindings.
  Removes UnsupportedHistoryPattern/UnsupportedInputBinding/UnsupportedSource gates.
  Oracle: pg_mentat src/{history,temporal,input_parameter,no_history}_tests.rs.

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
