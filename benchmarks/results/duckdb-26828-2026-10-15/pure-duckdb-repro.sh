#!/bin/bash
# Pure DuckDB 1.5.6. Writer session never checkpoints (killed), so all rows are
# WAL-only; a plain reopen replays + checkpoints on close; then query.
set -u; D=$HOME/duckdb; W=$(mktemp -d /tmp/pure.XXXX); cd $W
for idx in yes no; do for pre in yes no; do
  f=i$idx-p$pre.db
  IDX=""; [ $idx = yes ] && IDX="CREATE INDEX i ON t(a);"
  if [ $pre = yes ]; then $D $f -c "CREATE TABLE t(e BIGINT, a BIGINT); $IDX CHECKPOINT;" >/dev/null; SQL="INSERT INTO t SELECT range, 65536 + (range % 6) FROM range(78000);";
  else SQL="CREATE TABLE t(e BIGINT, a BIGINT); $IDX INSERT INTO t SELECT range, 65536 + (range % 6) FROM range(78000);"; fi
  ( echo "SET checkpoint_threshold='10GB'; $SQL"; sleep 20 ) | $D $f >/dev/null 2>&1 &
  P=$!; sleep 3; kill -9 $(pgrep -f "duckdb $f") 2>/dev/null; wait $P 2>/dev/null
  w=$(stat -c %s $f.wal 2>/dev/null || echo none)
  $D $f -c "SELECT 1" >/dev/null
  echo "[index=$idx precheckpointed=$pre] wal=$w eq=$($D -noheader -csv $f -c "SELECT count(*) FROM t WHERE a = 65538") between=$($D -noheader -csv $f -c "SELECT count(*) FROM t WHERE a BETWEEN 65538 AND 65538")"
done; done
mv $W ~/.trash/
