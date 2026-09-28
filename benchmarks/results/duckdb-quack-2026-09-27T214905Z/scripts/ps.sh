#!/bin/bash
# Single client, large result (predicate_scan at m, ~104k rows/call): listen backlog 5 vs 4096.
set -u
cd ~/mx
W=~/data/wc; R=~/results/quack-rerun; PY=~/py311/bin/python
export MENTAT_QUACK_TOKEN=ps$(od -An -tx1 -N8 /dev/urandom | tr -d ' \n') MENTAT_QUACK_PORT=9494 QUACK_URI=quack:127.0.0.1:9494
export MENTAT_EXT=$PWD/crates/duckdb/build/release/mentat.duckdb_extension
ov() { nstat -az TcpExtListenOverflows | awk 'NR>1{print $2}'; }
for cfg in A B; do
  case $cfg in A) D=~/duckdb;; B) D=/tmp/duckdb-bl;; esac
  MENTAT_QUACK_LOG=/tmp/ps-$cfg.log DUCKDB=$D crates/duckdb/server/serve.sh
  a=$(ov)
  $PY benchmarks/scale/bench.py run duckdb-quack ~/data/m $W/rr-m.db m 1 1 predicate_scan,since 10 60 30 20 > $R/single-$cfg-m.csv
  echo "$cfg backlog=$(ss -ltn '( sport = :9494 )' | awk 'NR==2{print $3}') overflows=$(( $(ov) - a ))" | tee -a $R/single-overflows.txt
  crates/duckdb/server/stop.sh
done
cat $R/single-*-m.csv
echo PS-DONE
