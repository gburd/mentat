#!/bin/bash
# E4: duckdb over Quack. (b) in-process+cache comes from ext-cache (E2).
set -u
export PY=~/py311/bin/python DATA_ROOT=~/data WORK=~/data/wc MENTAT_GIT="d34e0177 + duckdb-quack backend"
export SQLITE_EXT=~/mx/target/release/libmentat_sqlite DUCKDB_EXT=~/mx/crates/duckdb/build/release/mentat.duckdb_extension
export DUCKDB_CLI=~/duckdb REPS=1 EXTRA="write_mixed concurrency_sweep" OUT=~/results/duckdb-quack
cd ~/mx
SCALES=s BACKENDS=duckdb-quack bash benchmarks/scale/run.sh
cp $WORK/m-v2.db $WORK/duckdb-quack-m.db; cp $WORK/m-v2.db.entids $WORK/duckdb-quack-m.db.entids; cp $WORK/m-v2.db.load.json $WORK/duckdb-quack-m.db.load.json
echo "m: duckdb-quack reads a copy of the embedded-built store (as ext-cache did)" >> $OUT/sizes.txt
PHASE=bench SCALES=m BACKENDS=duckdb-quack SUSTAINED_S=300 bash benchmarks/scale/run.sh
PHASE=sustained SCALES=m BACKENDS=duckdb SUSTAINED_S=300 bash benchmarks/scale/run.sh
echo E4-DONE
