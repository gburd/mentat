#!/bin/bash
# Short rerun: concurrency_sweep c=32/64/128 at s and m over Quack, per server config.
#   A stock: listen backlog 5, threads 32   B: backlog 4096 (LD_PRELOAD shim)
#   C: backlog 4096 + SET threads=64        D: backlog 5 + SET threads=64
set -u
cd ~/mx
W=~/data/wc; R=~/results/quack-rerun; mkdir -p $R
export PY=~/py311/bin/python MENTAT_QUACK_TOKEN=rerun$(od -An -tx1 -N8 /dev/urandom | tr -d ' \n') MENTAT_QUACK_PORT=9494
export QUACK_URI=quack:127.0.0.1:9494 MENTAT_EXT=$PWD/crates/duckdb/build/release/mentat.duckdb_extension
printf '#!/bin/sh\nexec /home/ec2-user/duckdb -cmd "SET threads=64" "$@"\n' > /tmp/duckdb-t64
printf '#!/bin/sh\nLD_PRELOAD=/tmp/backlog.so exec /home/ec2-user/duckdb -cmd "SET threads=64" "$@"\n' > /tmp/duckdb-bl-t64
chmod +x /tmp/duckdb-t64 /tmp/duckdb-bl-t64
# fresh copies, WAL checkpointed (the originals carry the write phases' WAL)
for sc in s m; do
  src=$W/duckdb-quack-$sc.db; [ $sc = m ] && src=$W/m-v2.db
  cp $src $W/rr-$sc.db; [ -f $src-wal ] && cp $src-wal $W/rr-$sc.db-wal
  cp $src.entids $W/rr-$sc.db.entids; cp $src.load.json $W/rr-$sc.db.load.json
  $PY -c "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); print(sys.argv[1], c.execute('PRAGMA wal_checkpoint(TRUNCATE)').fetchone())" $W/rr-$sc.db
  cat $W/rr-$sc.db > /dev/null
done
ov() { nstat -az TcpExtListenOverflows TcpExtTCPSynRetrans | awk 'NR>1{printf "%s ", $2}'; }
for cfg in A B C D; do
  case $cfg in A) D=~/duckdb;; B) D=/tmp/duckdb-bl;; C) D=/tmp/duckdb-bl-t64;; D) D=/tmp/duckdb-t64;; esac
  for sc in s m; do
    MENTAT_QUACK_LOG=$R/server-$cfg-$sc.log DUCKDB=$D crates/duckdb/server/serve.sh
    p=$(pgrep -f "^/home/ec2-user/duckdb .*-init /tmp/mentat-quack" | head -1)
    bl=$(ss -ltn "( sport = :9494 )" | awk 'NR==2{print $3}'); th=$(ps -o nlwp= -p $p)
    a=$(ov); t0=$(date +%s)
    $PY benchmarks/scale/bench.py run duckdb-quack ~/data/$sc $W/rr-$sc.db $sc 1 32,64,128 concurrency_sweep 10 60 30 20 \
      > $R/$cfg-$sc.csv 2> $R/$cfg-$sc.err
    z=$(ov)
    echo "$cfg $sc backlog=$bl nlwp=$th secs=$(( $(date +%s) - t0 )) overflows+syn_retrans: before=[$a] after=[$z]" | tee -a $R/overflows.txt
    crates/duckdb/server/stop.sh
  done
done
echo RR-DONE
