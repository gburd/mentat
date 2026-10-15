#!/bin/bash
# Realistic: index already checkpointed; a later session's inserts are only in the WAL.
D=${1:-/tmp/r5.duckdb}; rm -f $D $D.wal
~/duckdb $D -c "CREATE TABLE t (e BIGINT NOT NULL, a BIGINT NOT NULL); CREATE INDEX i ON t (a); INSERT INTO t VALUES (0, 1); CHECKPOINT;"
~/duckdb $D -c "SET checkpoint_threshold='10GB'; INSERT INTO t SELECT i, 65538 + (i % 6) FROM range(78000) r(i);" -c "SELECT pg_sleep(100)" >/dev/null 2>&1 &
sleep 2; kill -9 %1 2>/dev/null; wait 2>/dev/null
~/duckdb $D -c "SELECT 1" >/dev/null
echo "index pre-checkpointed, data in WAL: a=65538 -> $(~/duckdb $D -noheader -list -c 'SELECT count(*) FROM t WHERE a = 65538') (expect 13000)"
