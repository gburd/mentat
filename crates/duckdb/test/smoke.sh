#!/usr/bin/env bash
# Smoke test for the mentat DuckDB extension (edn_t / edn_q / edn_pull /
# edn_eval), using a standalone DuckDB v1.5.5 CLI (matches the extension
# target). Use this when the SQLLogicTest venv duckdb doesn't match v1.5.5
# (e.g. host Python 3.9 caps at duckdb 1.4.5). Asserts on output so it fails
# loudly if the logic breaks.
#
# Usage:
#   DUCKDB=/path/to/duckdb-v1.5.5 crates/duckdb/test/smoke.sh
# Assumes `make debug` has produced build/debug/mentat.duckdb_extension.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT="$HERE/build/debug/mentat.duckdb_extension"
DUCKDB="${DUCKDB:-duckdb}"
DB="$(mktemp -u /tmp/mentat_smoke.XXXXXX.sqlite)"
trap 'rm -f "$DB"' EXIT

[ -f "$EXT" ] || { echo "FAIL: $EXT not found (run 'make debug' first)"; exit 1; }

run() { "$DUCKDB" -unsigned -noheader -list -c "LOAD '$EXT';" -c "$1" 2>&1; }

out="$("$DUCKDB" -unsigned -noheader -list <<SQL
LOAD '$EXT';
SELECT 'schema=' || (edn_t('$DB', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                                   {:db/ident :person/age  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}]') LIKE '%tx_id%');
SELECT 'data=' || (edn_t('$DB', '[{:person/name "Alice"} {:person/name "Bob"}]') LIKE '%tx_id%');

-- edn_q basic; strings come back RAW (no EDN quotes).
SELECT 'q=' || string_agg(name, ',' ORDER BY name)
  FROM edn_q('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', '{}') AS m(e, name);

-- JOIN against a native table holding PLAIN strings.
CREATE TABLE ages(name VARCHAR, age INT);
INSERT INTO ages VALUES ('Alice', 30), ('Bob', 25);
SELECT 'join=' || string_agg(m.name || '=' || a.age, ',' ORDER BY a.age)
  FROM edn_q('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', '{}') AS m(e, name)
  JOIN ages a ON a.name = m.name;

-- :in scalar and collection inputs.
SELECT 'in_scalar=' || (e::BIGINT > 0)
  FROM edn_q('$DB', '[:find ?e . :in ?name :where [?e :person/name ?name]]', '{"inputs":["Alice"]}') AS t(e);
SELECT 'in_coll=' || string_agg(n, ',' ORDER BY n)
  FROM edn_q('$DB', '[:find ?n :in [?name ...] :where [?e :person/name ?name] [?e :person/name ?n]]',
             '{"inputs":[["Alice","Zed"]]}') AS t(n);

-- asOf / since: tx1 sets Alice's age to 30, a later tx changes it to 31.
SET VARIABLE alice = (SELECT e FROM edn_q('$DB', '[:find ?e . :where [?e :person/name "Alice"]]', NULL) AS t(e));
SET VARIABLE tx1 = (SELECT edn_t('$DB', '[[:db/add ' || getvariable('alice') || ' :person/age 30]]')->>'tx_id');
SELECT 'tx2=' || (edn_t('$DB', '[[:db/add ' || getvariable('alice') || ' :person/age 31]]') LIKE '%tx_id%');
SELECT 'now=' || a FROM edn_q('$DB', '[:find ?a . :in ?e :where [?e :person/age ?a]]',
                              '{"inputs":[' || getvariable('alice') || ']}') AS t(a);
SELECT 'asof=' || a FROM edn_q('$DB', '[:find ?a . :in ?e :where [?e :person/age ?a]]',
                               '{"inputs":[' || getvariable('alice') || '], "asOf": ' || getvariable('tx1') || '}') AS t(a);
SELECT 'since=' || string_agg(a, ',' ORDER BY a)
  FROM edn_q('$DB', '[:find ?a ?added :where [?e :person/age ?a ?tx ?added]]',
             '{"since": ' || getvariable('tx1') || '}') AS t(a, added)
 WHERE added = 'true';

-- edn_pull: pg_mentat's JSON shape (":ns/attr" keys + ":db/id").
SELECT 'pull=' || (edn_pull('$DB', '[*]', getvariable('alice')::BIGINT) LIKE '%":person/name":"Alice"%');
SELECT 'pull_id=' || ((edn_pull('$DB', '[*]', getvariable('alice')::BIGINT)::JSON->>':db/id') = getvariable('alice'));
SELECT 'pull_attrs=' || (edn_pull('$DB', '[:person/age]', getvariable('alice')::BIGINT)::JSON->>':person/age');

-- edn_eval: (mentat.store/open) with no args opens db_path; the write is
-- visible to a subsequent edn_q.
SELECT 'eval=' || edn_eval('$DB', '(def c (mentat.store/open))
  (mentat.store/transact c [{:person/name "Carol"}])
  (count (mentat.store/q (mentat.store/db c) (quote [:find ?n :where [_ :person/name ?n]])))');
SELECT 'after_eval=' || string_agg(n, ',' ORDER BY n)
  FROM edn_q('$DB', '[:find ?n :where [_ :person/name ?n]]', '') AS t(n);
SQL
)"

echo "$out"
expect() { grep -qx -- "$1" <<<"$out" || { echo "FAIL: expected line '$1'"; exit 1; }; }
expect "schema=true"
expect "data=true"
expect "q=Alice,Bob"
expect "join=Bob=25,Alice=30"
expect "in_scalar=true"
expect "in_coll=Alice"
expect "tx2=true"
expect "now=31"
expect "asof=30"
expect "since=31"
expect "pull=true"
expect "pull_id=true"
expect "pull_attrs=31"
expect "eval=3"
expect "after_eval=Alice,Bob,Carol"

# Error paths: each must fail with a recognisable message.
expect_err() {
  local sql="$1" pat="$2" o
  if o="$(run "$sql")"; then echo "FAIL: expected error for: $sql"; echo "$o"; exit 1; fi
  grep -q -- "$pat" <<<"$o" || { echo "FAIL: error for '$sql' lacks '$pat':"; echo "$o"; exit 1; }
  echo "ok (error): $pat"
}
expect_err "SELECT edn_eval('$DB', '(slurp \"/etc/passwd\")');" "unbound symbol: slurp"
expect_err "SELECT * FROM edn_q('$DB', '[:find ?n :where [_ :person/name ?n]]', '{\"bogus\":1}');" 'unknown option "bogus"'
expect_err "SELECT * FROM edn_q('$DB', '[:find ?n :where [_ :person/name ?n]]', '{nope');" "not valid JSON"
expect_err "SELECT * FROM mentat_hello();" "mentat_hello does not exist"
expect_err "SELECT mentat_transact('$DB', '[]');" "mentat_transact does not exist"
expect_err "SELECT * FROM mentat_query('$DB', '[:find ?e :where [?e _ _]]', '{}');" "mentat_query does not exist"

echo "PASS: mentat DuckDB extension smoke test"
