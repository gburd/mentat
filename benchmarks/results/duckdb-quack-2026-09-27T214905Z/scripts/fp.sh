#!/bin/bash
# A short-lived client: a fresh DuckDB CLI process makes ONE point lookup.
# In-process (LOAD mentat, open store, query) vs over Quack (LOAD quack, quack_query to a warm server).
set -u
cd ~/mx
W=~/data/wc; EXT=$PWD/crates/duckdb/build/release/mentat.duckdb_extension
export MENTAT_QUACK_TOKEN=fp$(od -An -tx1 -N8 /dev/urandom | tr -d ' \n') MENTAT_QUACK_PORT=9494
MENTAT_EXT=$EXT DUCKDB=/tmp/duckdb-bl MENTAT_QUACK_LOG=/tmp/fp.log crates/duckdb/server/serve.sh >/dev/null
Q="[:find ?e ?n :in \$ ?email :where [?e :user/email ?email] [?e :user/name ?n]]"
ms() { local t0=$(date +%s%N); "$@" > /dev/null 2>&1; echo $(( ($(date +%s%N) - t0) / 1000000 )); }
med() { sort -n | awk '{a[NR]=$1} END{print a[int((NR+1)/2)]}'; }
echo "| scale | fresh process, in-process (LOAD mentat + open + query) ms | fresh process, over Quack (LOAD quack + quack_query) ms | fresh process, SELECT 1 only ms |"
echo "|---|---:|---:|---:|"
for sc in s m; do
  st=$W/rr-$sc.db
  SQL="SELECT * FROM edn_q('$st', '$Q', '{\"inputs\": [\"user7@example.com\"]}')"
  ~/duckdb -unsigned -c "LOAD '$EXT'" -c "$SQL" | tail -3 >&2
  a=$(for i in $(seq 15); do ms ~/duckdb -unsigned -c "LOAD '$EXT'" -c "$SQL"; done | med)
  QS=${SQL//\'/\'\'}
  ~/duckdb -c "LOAD quack" -c "SELECT * FROM quack_query('quack:127.0.0.1:9494', '$QS', token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true)" | tail -3 >&2
  b=$(for i in $(seq 15); do ms ~/duckdb -c "LOAD quack" -c "SELECT * FROM quack_query('quack:127.0.0.1:9494', '$QS', token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true)"; done | med)
  c=$(for i in $(seq 15); do ms ~/duckdb -c "SELECT 1"; done | med)
  echo "| $sc | $a | $b | $c |"
done
crates/duckdb/server/stop.sh >/dev/null
