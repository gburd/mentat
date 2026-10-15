#!/bin/bash
# DuckDB 1.5.6 index-after-WAL-replay bug (duckdb/duckdb#26828) through mentat.
# Through mentat only, from nothing. 1) one Python process loads the xs dataset
# via edn_t and exits normally; 2) a plain CLI session opens the file; 3) mentat queries.
set -u
E=~/ext-base/mentat.duckdb_extension; ST=/tmp/e2e/xs; mkdir -p /tmp/e2e; rm -f $ST*
DUCKDB_EXT=$E ~/v/bin/python ~/mentat/benchmarks/scale/bench.py load duckdb ~/data/xs $ST > /dev/null
ls -la $ST.duckdb $ST.duckdb.wal | awk '{print "after load:", $5, $9}'
cp $ST.duckdb $ST.ctl.duckdb; cp $ST.duckdb.wal $ST.ctl.duckdb.wal   # control: never opened by plain CLI
~/duckdb $ST.duckdb -c "SELECT 1" > /dev/null
ls -la $ST.duckdb* | awk '{print "after plain open:", $5, $9}'
for db in $ST.duckdb $ST.ctl.duckdb; do
  echo "== $db"
  ~/v/bin/python - $db $E $ST <<'PY'
import duckdb, sys
c = duckdb.connect(sys.argv[1], config={"allow_unsigned_extensions": "true"}); c.load_extension(sys.argv[2])
q = lambda edn, o="{}": c.execute("SELECT * FROM edn_q(?, ?, ?)", [sys.argv[3], edn, o]).fetchall()
print(" [?e :user/email _] count (expect 200):", q("[:find (count ?e) . :where [?e :user/email _]]"))
print(" [?i :issue/title _] count (expect 13000):", q("[:find (count ?i) . :where [?i :issue/title _]]"))
print(" point lookup by email:", q("[:find ?e ?name :in $ ?email :where [?e :user/email ?email] [?e :user/name ?name]]", '{"inputs": ["user7@example.com"]}'))
PY
done
