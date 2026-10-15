#!/bin/bash
# The agent's trigger: the store is CREATED inside a session that never
# checkpoints (everything in the WAL), clean exit; a PLAIN open replays it;
# then mentat queries an attribute with a bare [?e :attr _] (filter a = X only).
set -u; D=$HOME/duckdb; EXT=$HOME/mentat/crates/duckdb/build/debug/mentat.duckdb_extension
W=$(mktemp -d /tmp/mwal.XXXX); cd $W; LOAD="LOAD '$EXT';"
SCHEMA='[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true} {:db/ident :p/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]'
python3 - > load.sql <<P
print("LOAD '$EXT';")
print("SELECT edn_t('default', '$SCHEMA');")
for b in range(0, 13000, 500):
    print("SELECT edn_t('default', \$\$[" + " ".join('{:p/name "p%d" :p/n %d}' % (i, i%6) for i in range(b, b+500)) + "]\$\$);")
P
$D -unsigned m.db < load.sql >/dev/null 2>&1
echo "after the only mentat session: db=$(stat -c %s m.db) wal=$(stat -c %s m.db.wal 2>/dev/null || echo none)"
cp m.db c.db; cp m.db.wal c.db.wal   # control: never opened plainly
QS="SELECT count(*) FROM edn_q('default', '[:find ?e :where [?e :p/name _]]', '{}');"
$D m.db -c "SELECT 1" >/dev/null 2>&1   # plain CLI open
echo "plain reopen -> wal=$(stat -c %s m.db.wal 2>/dev/null || echo none) db=$(stat -c %s m.db)"
echo "[?e :p/name _] after plain reopen: $($D -unsigned -noheader -csv m.db -c "$LOAD $QS" 2>&1 | tail -1)  (expect 13000)"
echo "[?e :p/name _] control:            $($D -unsigned -noheader -csv c.db -c "$LOAD $QS" 2>&1 | tail -1)"
mv $W ~/.trash/ 2>/dev/null
