#!/bin/bash
# Same store content, WAL left by the sustained run vs checkpointed: ref_traversal, in-process duckdb.
set -u
W=~/data/wc; PY=~/py311/bin/python
cp $W/duckdb-quack-m.db $W/wal-m.db; cp $W/duckdb-quack-m.db-wal $W/wal-m.db-wal
cp $W/duckdb-quack-m.db.entids $W/wal-m.db.entids; cp $W/duckdb-quack-m.db.load.json $W/wal-m.db.load.json
cat $W/wal-m.db $W/wal-m.db-wal > /dev/null
q() {
DUCKDB_EXT=~/mx/crates/duckdb/build/release/mentat.duckdb_extension $PY - "$1" <<'EOF'
import os, sys, time, random
sys.path.insert(0, os.path.expanduser("~/mx/benchmarks/scale"))
import bench
meta = bench.jload(os.path.expanduser("~/data/m/meta.json"))
b = bench.Ext("duckdb", sys.argv[1], meta); b.load = bench.jload(sys.argv[1] + ".load.json")
rng = random.Random(7); ts = []
for _ in range(4):
    a = bench._iter_arg("ref_traversal", meta, rng); t = time.perf_counter(); bench.one(b, "ref_traversal", a); ts.append((time.perf_counter() - t) * 1e3)
print("ref_traversal ms:", " ".join(f"{x:.0f}" for x in ts))
EOF
}
echo "WAL $(stat -c %s $W/wal-m.db-wal) bytes (as left by the 300 s sustained run):"; q $W/wal-m.db
$PY -c "import sqlite3,sys; print('checkpoint(TRUNCATE):', sqlite3.connect(sys.argv[1]).execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone())" $W/wal-m.db
echo "WAL $(stat -c %s $W/wal-m.db-wal) bytes:"; q $W/wal-m.db
