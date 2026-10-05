#!/bin/bash
# A/B: the 1.10.3 DuckDB extension (datoms in a SQLite file) vs this tree's
# (datoms in DuckDB tables): same dataset, same single-client scenarios.
set -u
B=~/mentat/benchmarks/scale; PY=~/py311/bin/python
SC=${SC:-xs}; DATA=~/data/$SC; OUT=~/ab-$SC; mkdir -p $OUT ~/data ~/.trash
[ -f $DATA/meta.json ] || $PY $B/gen.py $SC $DATA > $OUT/gen.log 2>&1
SCENS="point_lookup,ref_traversal,aggregate,predicate_scan,pull,as_of,since,input_bindings"
for v in v1103 mentat; do
  EXT=~/$v/crates/duckdb/build/release/mentat.duckdb_extension
  ST=$OUT/store-$v
  # 1.10.3 keeps its datoms in the SQLite file at ST; give it an in-memory DuckDB.
  [ $v = v1103 ] && export DUCKDB_DB=:memory: || unset DUCKDB_DB
  for f in $ST $ST.duckdb $ST.duckdb.wal $ST-wal $ST-shm; do [ -e $f ] && mv $f ~/.trash/$(basename $f).$$; done
  echo "== $v load"
  DUCKDB_EXT=$EXT /usr/bin/time -f "%e s %M KB" $PY $B/bench.py load duckdb $DATA $ST > $OUT/load-$v.json 2> $OUT/load-$v.time
  cat $OUT/load-$v.json; tail -1 $OUT/load-$v.time
  echo "== $v bench"
  DUCKDB_EXT=$EXT $PY $B/bench.py run duckdb $DATA $ST $SC 3 1 $SCENS 2 10 30 20 > $OUT/bench-$v.csv 2> $OUT/bench-$v.err
  echo "== $v small transacts"
  DUCKDB_EXT=$EXT $PY - $DATA $ST <<'PY' > $OUT/write-$v.txt
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
  cat $OUT/write-$v.txt
  echo "== $v size"; du -cb $ST* 2>/dev/null | tail -1
done
echo AB_DONE
