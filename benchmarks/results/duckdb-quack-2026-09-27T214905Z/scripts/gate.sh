#!/bin/bash
# Gate: build + smoke + clippy on the synced floki tree (~/mentat).
set -u
cd ~/mentat
echo "tree: floki master 43d0d8fd (+ uncommitted CHANGELOG.md), rsynced $(date -u +%FT%TZ)" > ~/results/gate-tree.txt
G=~/results/gate; mkdir -p $G
st() { echo "$1 exit=$2" | tee -a $G/status.txt; }
: > $G/status.txt
cargo build --release -p mentat_sqlite_ext > $G/build-sqlite-ext.log 2>&1; st "cargo build --release -p mentat_sqlite_ext" $?
(cd crates/duckdb && make release) > $G/make-release.log 2>&1; st "cd crates/duckdb && make release" $?
EXT=$PWD/target/release/libmentat_sqlite bash crates/sqlite/ext/test/smoke.sh > $G/smoke-sqlite.log 2>&1; st "EXT=target/release/libmentat_sqlite crates/sqlite/ext/test/smoke.sh" $?
(cd crates/duckdb && DUCKDB=~/duckdb bash test/smoke.sh) > $G/smoke-duckdb.log 2>&1; st "DUCKDB=~/duckdb crates/duckdb/test/smoke.sh" $?
cargo clippy -p mentat_sqlite_ext -p mentat_duckdb -- -D warnings > $G/clippy.log 2>&1; st "cargo clippy -p mentat_sqlite_ext -p mentat_duckdb -- -D warnings" $?
echo GATE-DONE
