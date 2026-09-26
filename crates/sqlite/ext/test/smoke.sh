#!/usr/bin/env bash
# Smoke test for the mentat SQLite loadable extension, against a real `sqlite3`
# CLI (the host SQLite). Asserts on output so it fails loudly.
#
# Usage: [SQLITE3=sqlite3] [EXT=target/debug/libmentat_sqlite] crates/sqlite/ext/test/smoke.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../../.." && pwd)"
EXT="${EXT:-$ROOT/target/debug/libmentat_sqlite}"
SQLITE3="${SQLITE3:-sqlite3}"
DB="$(mktemp -u /tmp/mentat_sqlite_smoke.XXXXXX.db)"
HOST="$(mktemp -u /tmp/mentat_sqlite_host.XXXXXX.db)"
trap 'rm -f "$DB" "$DB"-wal "$DB"-shm "$HOST"' EXIT

[ -f "$EXT.so" ] || { echo "FAIL: $EXT.so not found (cargo build -p mentat_sqlite_ext)"; exit 1; }
"$SQLITE3" --version

run() { "$SQLITE3" -bail -noheader -list "$HOST" ".load $EXT" "$1" 2>&1; }

out="$("$SQLITE3" -bail -noheader -list "$HOST" <<SQL
.load $EXT
SELECT 'schema=' || (json_extract(edn_t('$DB', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                                                 {:db/ident :person/age  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}]'), '\$.tx_id') > 0);
SELECT 'data=' || (edn_t('$DB', '[{:person/name "Alice"} {:person/name "Bob"}]') LIKE '%"tx_id":%');

-- edn_q basic rows; the shape is pg_mentat's {"columns":[..],"results":[..]}.
SELECT 'cols=' || json_extract(edn_q('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', '{}'), '\$.columns');
SELECT 'q=' || group_concat(json_extract(value, '\$[1]'), ',')
  FROM (SELECT value FROM json_each(edn_q('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', '{}'), '\$.results')
        ORDER BY json_extract(value, '\$[1]'));

-- JOIN json_each(edn_q(...)) against a native SQLite table.
CREATE TABLE ages(name TEXT, age INT);
INSERT INTO ages VALUES ('Alice', 30), ('Bob', 25);
SELECT 'join=' || group_concat(n || '=' || age, ',') FROM (
  SELECT json_extract(m.value, '\$[1]') AS n, a.age
    FROM json_each(edn_q('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', NULL), '\$.results') m
    JOIN ages a ON a.name = json_extract(m.value, '\$[1]')
   ORDER BY a.age);

-- :in scalar and collection inputs.
SELECT 'in_scalar=' || (json_extract(edn_q('$DB', '[:find ?e . :in ?name :where [?e :person/name ?name]]',
                                           '{"inputs":["Alice"]}'), '\$.result') > 0);
SELECT 'in_coll=' || json_extract(edn_q('$DB', '[:find [?n ...] :in [?name ...] :where [?e :person/name ?name] [?e :person/name ?n]]',
                                        '{"inputs":[["Alice","Zed"]]}'), '\$.result');

-- asOf / since: tx1 sets Alice's age to 30, a later tx changes it to 31.
CREATE TEMP TABLE v AS SELECT json_extract(edn_q('$DB', '[:find ?e . :where [?e :person/name "Alice"]]', ''), '\$.result') AS alice;
CREATE TEMP TABLE t1 AS SELECT json_extract(edn_t('$DB', '[[:db/add ' || alice || ' :person/age 30]]'), '\$.tx_id') AS tx1 FROM v;
SELECT 'tx2=' || (edn_t('$DB', '[[:db/add ' || alice || ' :person/age 31]]') LIKE '%tx_id%') FROM v;
SELECT 'now=' || json_extract(edn_q('$DB', '[:find ?a . :in ?e :where [?e :person/age ?a]]',
                                    json_object('inputs', json_array(alice))), '\$.result') FROM v;
SELECT 'asof=' || json_extract(edn_q('$DB', '[:find ?a . :in ?e :where [?e :person/age ?a]]',
                                     json_object('inputs', json_array(alice), 'asOf', tx1)), '\$.result') FROM v, t1;
SELECT 'since=' || group_concat(json_extract(value, '\$[0]'), ',') FROM t1,
  json_each(edn_q('$DB', '[:find ?a ?added :where [?e :person/age ?a ?tx ?added]]',
                  json_object('since', tx1)), '\$.results')
 WHERE json_extract(value, '\$[1]') = 1;

-- edn_pull: pg_mentat's JSON shape (":ns/attr" keys + ":db/id").
SELECT 'pull=' || (edn_pull('$DB', '[*]', alice) LIKE '%":person/name":"Alice"%') FROM v;
SELECT 'pull_id=' || (json_extract(edn_pull('$DB', '[*]', alice), '\$.":db/id"') = alice) FROM v;
SELECT 'pull_attrs=' || json_extract(edn_pull('$DB', '[:person/age]', alice), '\$.":person/age"') FROM v;

-- edn_eval: (mentat.store/open) with no args opens db_path; the write is
-- visible to a later edn_q.
SELECT 'eval=' || edn_eval('$DB', '(def c (mentat.store/open))
  (mentat.store/transact c [{:person/name "Carol"}])
  (count (mentat.store/q (mentat.store/db c) (quote [:find ?n :where [_ :person/name ?n]])))');
SELECT 'after_eval=' || group_concat(value, ',') FROM (SELECT value FROM
  json_each(edn_q('$DB', '[:find [?n ...] :where [_ :person/name ?n]]', ''), '\$.result') ORDER BY value);

-- NULL in -> NULL out.
SELECT 'null=' || (edn_q(NULL, '[:find ?e :where [?e _ _]]', '{}') IS NULL);
SQL
)"

echo "$out"
expect() { grep -qxF -- "$1" <<<"$out" || { echo "FAIL: expected line '$1'"; exit 1; }; }
expect "schema=1"
expect "data=1"
expect 'cols=["?e","?name"]'
expect "q=Alice,Bob"
expect "join=Bob=25,Alice=30"
expect "in_scalar=1"
expect 'in_coll=["Alice"]'
expect "tx2=1"
expect "now=31"
expect "asof=30"
expect "since=31"
expect "pull=1"
expect "pull_id=1"
expect "pull_attrs=31"
expect "eval=3"
expect "after_eval=Alice,Bob,Carol"
expect "null=1"

# Error paths: each must fail with a recognisable SQLite error (not a crash).
expect_err() {
  local sql="$1" pat="$2" o
  if o="$(run "$sql")"; then echo "FAIL: expected error for: $sql"; echo "$o"; exit 1; fi
  grep -q -- "$pat" <<<"$o" || { echo "FAIL: error for '$sql' lacks '$pat':"; echo "$o"; exit 1; }
  echo "ok (error): $pat"
}
expect_err "SELECT edn_eval('$DB', '(slurp \"/etc/passwd\")');" "unbound symbol: slurp"
expect_err "SELECT edn_q('$DB', '[:find ?n :where [_ :person/name ?n]]', '{\"bogus\":1}');" 'unknown option "bogus"'
expect_err "SELECT edn_q('$DB', '[:find ?n :where [_ :person/name ?n]]', '{nope');" "not valid JSON"
expect_err "SELECT edn_q('$DB', '[:find ?n :where', '{}');" "edn_q:"
expect_err "SELECT edn_pull('$DB', '[*]] :where [(x)]', 1);" "must be an EDN vector"
expect_err "SELECT edn_pull('$DB', '[*]', 'x');" "entity must be an INTEGER"
expect_err "SELECT edn_t('$HOST', '[]');" "host's own database"
# SQLITE_DIRECTONLY: not callable from a view (untrusted-schema defence).
expect_err "CREATE VIEW vv AS SELECT edn_q('$DB', '[:find ?e :where [?e _ _]]', '{}') AS r; SELECT * FROM vv;" "unsafe use of edn_q"

echo "PASS: mentat SQLite extension smoke test"
