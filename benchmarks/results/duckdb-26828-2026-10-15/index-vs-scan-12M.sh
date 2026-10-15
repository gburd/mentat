#!/bin/bash
set -u; D=$HOME/duckdb; W=$(mktemp -d /tmp/plan.XXXX); cd $W
mk="CREATE TABLE d(e BIGINT, a BIGINT, v_s VARCHAR, tx BIGINT); INSERT INTO d SELECT 65536000 + (hash(range) % 6000000)::BIGINT, 65536 + (range % 2), 'p' || range, 268435456 + (range // 2000) FROM range(12000000);"
$D i.db -c "SET threads=4; $mk CREATE INDEX ea ON d(e, a); CREATE INDEX ia ON d(a); CHECKPOINT;" >/dev/null
$D n.db -c "SET threads=4; $mk CHECKPOINT;" >/dev/null
E=$($D -noheader -csv n.db -c "SELECT e FROM d LIMIT 1 OFFSET 7777777")
echo "--- full EXPLAIN (indexed), e = $E ---"; $D i.db -c "EXPLAIN SELECT * FROM d WHERE e = $E" | grep -E 'SCAN|Type|Table|Filters' | sed 's/[│┌┐└┘─]//g' | tr -s ' ' | head -6
for f in i n; do echo "== $f e= x4: $($D $f.db -c ".timer on" -c "SELECT count(*) FROM d WHERE e = $E" -c "SELECT count(*) FROM d WHERE e = $E" -c "SELECT count(*) FROM d WHERE e = $E AND a = 65537" -c "SELECT count(*) FROM d WHERE e = $E AND a = 65537" 2>&1 | grep 'Run Time' | awk '{printf "%s ", $5}')"; done
mv $W ~/.trash/
