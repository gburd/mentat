#!/bin/bash
# WAL growth during a 120 s sustained run (32 readers + 1 writer) over Quack,
# from a checkpointed m store; then single-client ref_traversal on the result.
set -u
cd ~/mx
W=~/data/wc; R=~/results/quack-rerun; PY=~/py311/bin/python
cp $W/m-v2.db $W/sw-m.db; cp $W/m-v2.db.entids $W/sw-m.db.entids; cp $W/m-v2.db.load.json $W/sw-m.db.load.json
export MENTAT_QUACK_TOKEN=walrun$(od -An -tx1 -N8 /dev/urandom | tr -d ' \n') MENTAT_QUACK_PORT=9494 QUACK_URI=quack:127.0.0.1:9494
MENTAT_EXT=$PWD/crates/duckdb/build/release/mentat.duckdb_extension DUCKDB=~/duckdb MENTAT_QUACK_LOG=$R/server-wal.log crates/duckdb/server/serve.sh
( t0=$(date +%s); while [ $(( $(date +%s) - t0 )) -lt 130 ]; do echo "$(( $(date +%s) - t0 )) $(stat -c %s $W/sw-m.db-wal 2>/dev/null || echo 0)"; sleep 10; done ) > $R/wal-growth.txt &
$PY benchmarks/scale/bench.py mixed duckdb-quack ~/data/m $W/sw-m.db m 1 32 120 sustained > $R/sustained-120.csv 2> $R/sustained-120.err
wait
cat $R/wal-growth.txt; cat $R/sustained-120.csv
cat > /tmp/rt1.py <<'EOF'
import os, sys, time, random
sys.path.insert(0, os.path.expanduser("~/mx/benchmarks/scale"))
import bench
meta = bench.jload(os.path.expanduser("~/data/m/meta.json"))
b = bench.Ext("duckdb-quack", sys.argv[1], meta); b.load = bench.jload(sys.argv[1] + ".load.json")
rng = random.Random(7); ts = []
for _ in range(3):
    a = bench._iter_arg("ref_traversal", meta, rng); t = time.perf_counter(); bench.one(b, "ref_traversal", a); ts.append((time.perf_counter() - t) * 1e3)
print("ref_traversal ms after the run:", " ".join(f"{x:.0f}" for x in ts))
EOF
$PY /tmp/rt1.py $W/sw-m.db | tee -a $R/wal-growth.txt
crates/duckdb/server/stop.sh
echo "WAL after server stop: $(stat -c %s $W/sw-m.db-wal 2>/dev/null || echo gone)" | tee -a $R/wal-growth.txt
echo SW-DONE
