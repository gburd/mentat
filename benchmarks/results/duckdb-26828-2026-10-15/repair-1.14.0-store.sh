#!/bin/bash
set -u; D=$HOME/duckdb; NEW=$HOME/mentat/crates/duckdb/build/debug/mentat.duckdb_extension
mkdir -p ~/rel114 && tar xzf ~/rel114.tgz -C ~/rel114; OLD=$(ls ~/rel114/*/mentat.duckdb_extension)
W=$(mktemp -d /tmp/rep.XXXX); cd $W
SCHEMA='[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/unique :db.unique/identity :db/index true} {:db/ident :p/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]'
python3 - > load.sql <<P
print("LOAD '$OLD';"); print("SELECT edn_t('default', '$SCHEMA');")
for b in range(0, 13000, 500):
    print("SELECT edn_t('default', \$\$[" + " ".join('{:p/name "p%d" :p/n %d}' % (i, i%6) for i in range(b, b+500)) + "]\$\$);")
P
$D -unsigned m.db < load.sql > load.out 2>&1; echo "1.14.0 load errs=$(grep -ci error load.out) wal=$(stat -c %s m.db.wal 2>/dev/null || echo none)"
$D m.db -c "SELECT 1" >/dev/null 2>&1
Q="SELECT count(*) FROM edn_q('default', '[:find ?e :where [?e :p/name _]]', '{}');"
echo "indexes: $($D -noheader -csv m.db -c "SELECT count(*) FROM duckdb_indexes() WHERE schema_name='mentat'")"
cp m.db old.db
echo "1.14.0 ext after replay: $($D -unsigned -noheader -csv old.db -c "LOAD '$OLD'; $Q" 2>&1 | tail -1)  (0 = hit)"
echo "fixed ext, same file:    $($D -unsigned -noheader -csv m.db -c "LOAD '$NEW'; $Q" 2>&1 | tail -1)  (expect 13000)"
echo "indexes after fixed open: $($D -noheader -csv m.db -c "SELECT count(*) FROM duckdb_indexes() WHERE schema_name='mentat'")"
echo "retractEntity after:     $($D -unsigned m.db -c "LOAD '$NEW'; SELECT edn_t('default', '[[:db/retractEntity [:p/name \"p5\"]]]') IS NOT NULL;" 2>&1 | grep -oE 'true|FATAL.*|Error.*' | head -1 | cut -c1-120)"
echo "count after retract:     $($D -unsigned -noheader -csv m.db -c "LOAD '$NEW'; $Q" 2>&1 | tail -1)  (expect 12999)"
mv $W ~/.trash/ 2>/dev/null
