#!/bin/bash
# Same store, back to back, 1 client: in-process duckdb (b) vs Quack with the
# 4096 backlog shim (c'), all eight scenarios. Isolates protocol+serialization cost.
set -u
cd ~/mx
W=~/data/wc; R=~/results/quack-rerun; PY=~/py311/bin/python
export DUCKDB_EXT=$PWD/crates/duckdb/build/release/mentat.duckdb_extension MENTAT_EXT=$PWD/crates/duckdb/build/release/mentat.duckdb_extension
export MENTAT_QUACK_TOKEN=ab$(od -An -tx1 -N8 /dev/urandom | tr -d ' \n') MENTAT_QUACK_PORT=9494 QUACK_URI=quack:127.0.0.1:9494
S=point_lookup,ref_traversal,aggregate,predicate_scan,pull,as_of,since,input_bindings
for sc in s m; do
  cat $W/rr-$sc.db > /dev/null
  $PY benchmarks/scale/bench.py run duckdb ~/data/$sc $W/rr-$sc.db $sc 1 1 $S 10 60 30 20 > $R/ab-inproc-$sc.csv 2> $R/ab-inproc-$sc.err
  MENTAT_QUACK_LOG=/tmp/ab-$sc.log DUCKDB=/tmp/duckdb-bl crates/duckdb/server/serve.sh
  $PY benchmarks/scale/bench.py run duckdb-quack ~/data/$sc $W/rr-$sc.db $sc 1 1 $S 10 60 30 20 > $R/ab-quack-$sc.csv 2> $R/ab-quack-$sc.err
  crates/duckdb/server/stop.sh
done
echo AB-DONE
