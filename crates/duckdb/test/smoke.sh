#!/usr/bin/env bash
# M0 + M1 smoke test for the mentat DuckDB extension, using a standalone
# DuckDB v1.5.5 CLI (matches the extension target). Use this when the
# SQLLogicTest venv duckdb doesn't match v1.5.5 (e.g. host Python 3.9 caps at
# duckdb 1.4.5). Asserts on output so it fails loudly if the logic breaks.
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

out="$("$DUCKDB" -unsigned -noheader -list <<SQL
LOAD '$EXT';
SELECT * FROM mentat_hello();
SELECT mentat_transact('$DB', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]') LIKE '%tx_id%';
SELECT mentat_transact('$DB', '[{:person/name "Alice"} {:person/name "Bob"}]') LIKE '%tx_id%';
SELECT string_agg(name, ',' ORDER BY name)
  FROM mentat_query('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', '{}') AS m(e, name);
CREATE TABLE ages(name VARCHAR, age INT);
INSERT INTO ages VALUES ('"Alice"', 30), ('"Bob"', 25);
SELECT string_agg(m.name || '=' || a.age, ',' ORDER BY a.age)
  FROM mentat_query('$DB', '[:find ?e ?name :where [?e :person/name ?name]]', '{}') AS m(e, name)
  JOIN ages a ON a.name = m.name;
SQL
)"

echo "$out"
grep -qx "mentat duckdb extension loaded" <<<"$out" || { echo "FAIL: mentat_hello"; exit 1; }
grep -qx "true" <<<"$out"                            || { echo "FAIL: transact report"; exit 1; }
grep -qx '"Alice","Bob"' <<<"$out"                   || { echo "FAIL: mentat_query rows"; exit 1; }
grep -qx '"Bob"=25,"Alice"=30' <<<"$out"             || { echo "FAIL: JOIN"; exit 1; }
echo "PASS: mentat DuckDB extension M0+M1 smoke test"
