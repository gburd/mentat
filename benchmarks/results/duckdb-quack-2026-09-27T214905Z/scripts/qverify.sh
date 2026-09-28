#!/bin/bash
# Quack verification on the gate build: serve.sh, remote edn_t + edn_q, token
# rejection, ATTACH behaviour, stop.sh.
set -u
cd ~/mentat
export DUCKDB=~/duckdb MENTAT_QUACK_PORT=9700 MENTAT_QUACK_TOKEN=$(od -An -tx1 -N24 /dev/urandom | tr -d ' \n')
export MENTAT_QUACK_PIDFILE=/tmp/gate-quack.pid MENTAT_QUACK_LOG=/tmp/gate-quack.log
U=quack:127.0.0.1:9700; ST=$(mktemp -u /tmp/gate_quack.XXXXXX.db)
c() { echo "\$ duckdb: $1" | sed "s/$MENTAT_QUACK_TOKEN/<token>/g"; ~/duckdb -noheader -list -c "LOAD quack;" -c "$1" 2>&1 | sed "s/$MENTAT_QUACK_TOKEN/<token>/g"; echo "(duckdb exit ${PIPESTATUS[0]})"; }
echo "== serve.sh"; crates/duckdb/server/serve.sh; ss -ltn "( sport = :9700 )" | tail -1
echo "== remote edn_t"
c "SELECT * FROM quack_query('$U', \$\$SELECT edn_t('$ST', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]') AS r\$\$, token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);"
c "SELECT * FROM quack_query('$U', \$\$SELECT edn_t('$ST', '[{:person/name \"Alice\"} {:person/name \"Bob\"}]') AS r\$\$, token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);"
echo "== remote edn_q"
c "SELECT * FROM quack_query('$U', \$\$SELECT * FROM edn_q('$ST', '[:find ?n :where [_ :person/name ?n]]', '{}') ORDER BY 1\$\$, token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true);"
echo "== remote edn_q, joined with a client-local table (client has no mentat loaded)"
c "CREATE TABLE ages AS SELECT * FROM (VALUES ('Alice', 30), ('Bob', 41)) t(name, age); SELECT q.*, a.age FROM quack_query('$U', \$\$SELECT * FROM edn_q('$ST', '[:find ?n :where [_ :person/name ?n]]', '{}')\$\$, token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true) q JOIN ages a ON a.name = q.\"?n\" ORDER BY 1;"
echo "== wrong token"
c "SELECT * FROM quack_query('$U', 'SELECT 1', token => 'wrong-token-1234', disable_ssl => true);"
echo "== no token"
c "SELECT * FROM quack_query('$U', 'SELECT 1', disable_ssl => true);"
echo "== ATTACH, wrong token"
c "ATTACH '$U' AS r (TOKEN 'wrong-token-1234', DISABLE_SSL true);"
echo "== ATTACH, right token: tables yes, mentat functions no"
c "SELECT * FROM quack_query('$U', 'CREATE OR REPLACE TABLE t AS SELECT 42 AS k', token => getenv('MENTAT_QUACK_TOKEN'), disable_ssl => true); ATTACH '$U' AS r (TOKEN getenv('MENTAT_QUACK_TOKEN'), DISABLE_SSL true); SELECT * FROM r.t;"
c "ATTACH '$U' AS r (TOKEN getenv('MENTAT_QUACK_TOKEN'), DISABLE_SSL true); SELECT * FROM r.edn_q('$ST', '[:find ?n :where [_ :person/name ?n]]', '{}');"
echo "== server log"; sed 's/\x1b\[[0-9;]*m//g' /tmp/gate-quack.log
echo "== stop.sh"; crates/duckdb/server/stop.sh; ss -ltn "( sport = :9700 )" | tail -n +2 | wc -l
rm -f "$ST" "$ST"-wal "$ST"-shm
