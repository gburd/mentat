#!/bin/bash
# One measurement pass of this tree's extension: load (once per scale, kept in
# ~/stores), single-client scenario p50s, single-datom transacts, concurrency.
# Usage: TAG=before SC=xs perf.sh
set -u
B=~/mentat/benchmarks/scale; PY=~/v/bin/python; R=~/mentat/benchmarks/results/duckdb-perf-2026-10-15T010700Z
SC=${SC:-xs}; TAG=${TAG:-run}; DATA=~/data/$SC; OUT=~/perf/$TAG-$SC; mkdir -p $OUT ~/stores ~/.trash
export DUCKDB_EXT=${DUCKDB_EXT:-~/mentat/crates/duckdb/build/release/mentat.duckdb_extension}
ST=~/stores/$SC
if [ ! -f $ST.load.json ] || [ -n "${RELOAD:-}" ]; then
  for f in $ST.duckdb $ST.duckdb.wal $ST.entids $ST.load.json; do [ -e $f ] && mv $f ~/.trash/$(basename $f).$$; done
  echo "== load"; $PY $B/bench.py load duckdb $DATA $ST > $OUT/load.json 2> $OUT/load.err
  cat $OUT/load.json
  # With mentat loaded the database never checkpoints (see findings.md), so
  # the load is all WAL. Checkpoint it explicitly, once: a plain open would
  # replay and checkpoint it, and on v1.5.6 that loses the ART index's view
  # of the replayed rows (repro5.sh).
  ~/duckdb $ST.duckdb -c CHECKPOINT > /dev/null
  cp $ST.duckdb $ST.pristine
fi
# Every pass starts from the freshly loaded, checkpointed store.
cp $ST.pristine $ST.duckdb; [ -e $ST.duckdb.wal ] && mv $ST.duckdb.wal ~/.trash/wal.$$
echo "== check"
# bench.py check runs as_of in a child process, which can't open the database
# this process holds; run it inline instead.
$PY - $DATA $ST <<'PY' | tail -1
import os, sys
sys.path.insert(0, os.path.expanduser("~/mentat/benchmarks/scale"))
import bench
data, st = sys.argv[1], sys.argv[2]
meta = bench.jload(f"{data}/meta.json")
b = bench.Ext("duckdb", st, meta)
bench.timed_one = lambda backend, d, t, scen, arg, tmo: (bench.one(b, scen, arg), 0.0)
fails = bench.check(b, meta, False, "duckdb", data, st)
print("check:", "OK" if not fails else "FAILED " + ",".join(fails))
PY
echo "== bench"
$PY $B/bench.py run duckdb $DATA $ST $SC 3 1 point_lookup,pull,input_bindings,ref_traversal,aggregate,as_of,since 2 10 30 20 > $OUT/bench.csv 2> $OUT/bench.err
$PY - $OUT/bench.csv <<'PY'
import csv, statistics, sys
d = {}
for r in csv.reader(open(sys.argv[1])):
    if len(r) > 8 and r[5] == "read": d.setdefault(r[0], []).append(float(r[7]))
for k, x in d.items(): print(f"p50 {k:16s} {statistics.median(x):8.3f} ms")
PY
echo "== single-datom transacts"
$PY - $DATA $ST <<'PY' | tee $OUT/write.txt
import json, os, random, sys, time
sys.path.insert(0, os.path.expanduser("~/mentat/benchmarks/scale"))
import bench
data, st = sys.argv[1], sys.argv[2]
meta = bench.jload(f"{data}/meta.json")
b = bench.Ext("duckdb", st, meta)
rng = random.Random(7); lat = []
for _ in range(300):
    e = b.entid(rng.randrange(meta["n_issues"]))
    t = time.perf_counter(); b.t(f"[[:db/add {e} :issue/state :state/{rng.choice(['open','closed','resolved'])}]]")
    lat.append((time.perf_counter() - t) * 1e3)
lat.sort(); print(json.dumps({"n": len(lat), "p50_ms": lat[len(lat)//2], "p95_ms": lat[int(len(lat)*.95)], "tx_per_s": len(lat) / (sum(lat) / 1e3)}))
PY
echo "== concurrency"
for wl in point mix; do $PY $R/conc.py $DATA $ST 1,8,32 ${CONC_S:-8} $wl 2>>$OUT/conc.err | tee -a $OUT/conc.jsonl; done
echo PERF_DONE
