#!/usr/bin/env bash
# Smoke test for the mentat DuckDB extension (edn_t / edn_q / edn_pull /
# edn_eval), storing in DuckDB tables, using a standalone DuckDB v1.5.6 CLI
# (matches the extension target). Use this when the SQLLogicTest venv duckdb doesn't match v1.5.6
# (e.g. host Python 3.9 caps at duckdb 1.4.5). Asserts on output so it fails
# loudly if the logic breaks.
#
# Usage:
#   DUCKDB=/path/to/duckdb-v1.5.6 crates/duckdb/test/smoke.sh
# Assumes `make debug` has produced build/debug/mentat.duckdb_extension.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT="${EXT:-$HERE/build/debug/mentat.duckdb_extension}"
DUCKDB="${DUCKDB:-duckdb}"
# The datoms live in DuckDB: a persistent DuckDB database file, and a store
# NAME (the old file-path argument still works as a name; see the second store).
DUCK="$(mktemp -u /tmp/mentat_smoke.XXXXXX.duckdb)"
DB="default"
OLD="$(mktemp -u /tmp/mentat_smoke_old.XXXXXX.mentat)"
OTHER="$(mktemp /tmp/mentat_duck_other.XXXXXX)"
trap 'rm -f "$DUCK" "$DUCK".wal "$OLD" "$OTHER" "$OTHER".sql' EXIT

[ -f "$EXT" ] || { echo "FAIL: $EXT not found (run 'make debug' first)"; exit 1; }

run() { "$DUCKDB" -unsigned -noheader -list "$DUCK" -c "LOAD '$EXT';" -c "$1" 2>&1; }

# A SECOND connection on the same database (another DuckDB connection in a
# separate session after this one closes): adds an attribute and an entity,
# which this session must then see.
cat > "$OTHER.sql" <<SQL
LOAD '$EXT';
SELECT 'other=' || (edn_t('$DB', '[{:db/ident :person/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]') LIKE '%tx_id%');
SELECT 'other_e=' || (edn_t('$DB', '[{:db/id "o" :person/name "Olga" :person/email "o@x"}]')::JSON->>'\$.tempids.o');
SQL

out="$("$DUCKDB" -unsigned -noheader -list "$DUCK" <<SQL
LOAD '$EXT';
SELECT 'schema=' || (edn_t('$DB', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                                   {:db/ident :person/age  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}]') LIKE '%tx_id%');
SELECT 'data=' || (edn_t('$DB', '[{:person/name "Alice"} {:person/name "Bob" :person/age 25}]') LIKE '%tx_id%');

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
-- mixed scalar + collection inputs (QueryInputs::merge): Bob is 25, Alice isn't.
SELECT 'in_mixed=' || string_agg(n, ',' ORDER BY n)
  FROM edn_q('$DB', '[:find ?n :in ?age [?name ...] :where [?e :person/name ?name] [?e :person/age ?age] [?e :person/name ?n]]',
             '{"inputs":[25, ["Alice","Bob"]]}') AS t(n);

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

-- The datoms are DuckDB rows: native SQL sees them, in the store's schema.
SELECT 'native=' || count(*) FROM mentat.datoms d JOIN mentat.idents i ON i.e = d.a
 WHERE union_extract(i.v, 's') = ':person/name';
-- ...and nothing was written outside DuckDB.
SELECT 'tables=' || count(*) FROM duckdb_tables() WHERE schema_name = 'mentat'
   AND table_name IN ('datoms', 'timelined_transactions', 'idents', 'schema', 'known_parts');
-- A path-like name is a separate store (a DuckDB schema), not a file.
SELECT 'old_name=' || (edn_t('$OLD', '[{:db/ident :x/y :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]') LIKE '%tx_id%');
SELECT 'old_schema=' || count(*) FROM duckdb_schemas() WHERE schema_name LIKE 'mentat_tmp_mentat_smoke_old_%';
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
expect "in_mixed=Bob"
expect "tx2=true"
expect "now=31"
expect "asof=30"
expect "since=31"
expect "pull=true"
expect "pull_id=true"
expect "pull_attrs=31"
expect "eval=3"
expect "after_eval=Alice,Bob,Carol"
expect "native=3"
expect "tables=5"
expect "old_name=true"
expect "old_schema=1"
[ ! -e "$OLD" ] || { echo "FAIL: a file was written at $OLD"; exit 1; }

# Persistence + a second connection: a new session on the same database file
# sees the store, writes to it, and a third session without the extension
# reads the rows with plain SQL.
other="$("$DUCKDB" -unsigned -noheader -list "$DUCK" < "$OTHER.sql" 2>&1)"
echo "$other"
grep -qx "other=true" <<<"$other" || { echo "FAIL: second session"; exit 1; }
again="$(run "SELECT 'mine_e=' || (edn_t('$DB', '[{:db/id \"m\" :person/name \"Mona\" :person/email \"m@x\"}]')::JSON->>'\$.tempids.m');
SELECT 'emails=' || string_agg(m, ',' ORDER BY m) FROM edn_q('$DB', '[:find ?m :where [_ :person/email ?m]]', '') AS t(m);")"
echo "$again"
grep -qx "emails=m@x,o@x" <<<"$again" || { echo "FAIL: emails"; exit 1; }
oe="$(sed -n 's/^other_e=//p' <<<"$other")"; me="$(sed -n 's/^mine_e=//p' <<<"$again")"
[ -n "$oe" ] && [ -n "$me" ] && [ "$oe" != "$me" ] || { echo "FAIL: entids other=$oe mine=$me"; exit 1; }
plain="$("$DUCKDB" -noheader -list "$DUCK" -c "SELECT 'plain=' || count(DISTINCT e) FROM mentat.datoms WHERE a = (SELECT e FROM mentat.idents WHERE union_extract(v, 's') = ':person/name');" 2>&1)"
echo "$plain"
grep -qx "plain=5" <<<"$plain" || { echo "FAIL: plain SQL read"; exit 1; }

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
