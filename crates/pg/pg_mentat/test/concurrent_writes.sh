#!/usr/bin/env bash
# Concurrent-writer races on pg_mentat (#376), with two real sessions.
#
# Each case runs session A's transaction up to (not including) COMMIT, starts
# session B on the same data, waits until B blocks on a lock or finishes,
# commits A, then checks the invariant:
#   replace  two replacements of one cardinality-one (e, a)        -> one live value
#   empty    two first assertions of one cardinality-one (e, a)    -> one live value
#   alter    :db.cardinality many->one (#286) against a new value  -> one live value
#   cas      two [:db/cas e a 5 _] from the same old value          -> one live value
#   unique   two new entities asserting one :db.unique/identity    -> one entity holds it
#
# Usage: test/concurrent_writes.sh [isolation ...]
#   isolation levels default to: "read committed" "repeatable read" "serializable"
# Environment: PGHOST, PGPORT, PGUSER as for psql. RACE_DB (default
# pg_mentat_race) is dropped and recreated; the server must have pg_mentat
# installed. Example against a scratch cluster on port 55472:
#   PGHOST=/tmp PGPORT=55472 crates/pg/pg_mentat/test/concurrent_writes.sh
#
# Under REPEATABLE READ a transaction keeps its first snapshot, so a lock
# can't show it a value committed while it waited: "empty", "alter" and
# "unique" can still break there. Those are reported as KNOWN, not FAIL.
# Exit status: 0 if no case FAILs.
set -euo pipefail

DB=${RACE_DB:-pg_mentat_race}
export PGOPTIONS="-c search_path=mentat,public"
if [ $# -gt 0 ]; then LEVELS=("$@"); else
    LEVELS=("read committed" "repeatable read" "serializable"); fi

q() { psql -X -qAt -v ON_ERROR_STOP=1 -d "$DB" -c "$1"; }
tx() { q "SELECT edn_t('$1')"; }
tempid() { q "SELECT edn_t('$1')::jsonb -> 'tempids' ->> '$2'"; }
entid() { q "SELECT entid FROM mentat.idents WHERE ident = ':$1'"; }
# Run edn_t and print "asserted/retracted" (or psql's ERROR line).
report_sql() { echo "SELECT (r->>'datoms_asserted') || '/' || (r->>'datoms_retracted') FROM (SELECT edn_t('$1')::jsonb AS r) s;"; }

psql -X -q -d postgres -c "DROP DATABASE IF EXISTS $DB" -c "CREATE DATABASE $DB"
q "CREATE EXTENSION pg_mentat" >/dev/null
echo "pg_mentat $(q "SELECT extversion FROM pg_extension WHERE extname = 'pg_mentat'") on $(q 'SHOW server_version')"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/race.XXXXXX")
fails=0
n=0

# race ISO A_EDN B_EDN: run the two sessions; sets WAITED, A_OUT, B_OUT.
race() {
    local iso=$1 a=$2 b=$3
    rm -f "$WORK/a" && mkfifo "$WORK/a"
    PGAPPNAME=race_a psql -X -qAt -d "$DB" < "$WORK/a" > "$WORK/a.out" 2>&1 &
    local apid=$!
    exec 3> "$WORK/a"
    echo "BEGIN ISOLATION LEVEL $iso;" >&3
    report_sql "$a" >&3
    local i
    for i in $(seq 200); do  # until A's edn_t is done (its transaction is still open)
        [ -n "$(q "SELECT 1 FROM pg_stat_activity WHERE application_name = 'race_a' \
                   AND state LIKE 'idle in transaction%'")" ] && break
        sleep 0.05
    done
    PGAPPNAME=race_b psql -X -qAt -v ON_ERROR_STOP=1 -d "$DB" \
        -c "BEGIN ISOLATION LEVEL $iso" -c "$(report_sql "$b")" -c "COMMIT" \
        > "$WORK/b.out" 2>&1 &
    local bpid=$!
    WAITED=no
    for i in $(seq 100); do  # until B waits on a lock or finishes (5 s)
        WAITED=$(q "SELECT wait_event FROM pg_stat_activity WHERE application_name = 'race_b' \
                    AND wait_event_type = 'Lock'")
        [ -n "$WAITED" ] && break
        WAITED=no
        kill -0 $bpid 2>/dev/null || break
        sleep 0.05
    done
    echo "COMMIT;" >&3
    exec 3>&-
    wait $apid || true
    wait $bpid || true
    A_OUT=$(outcome "$WORK/a.out")
    B_OUT=$(outcome "$WORK/b.out")
}

# outcome FILE: a session's first ERROR (its edn_t's or its COMMIT's), else
# its edn_t's datoms asserted/retracted.
outcome() {
    { grep -m1 'ERROR' "$1" || grep -m1 -E '^[0-9]+/[0-9]+$' "$1" || echo '?'; } |
        sed -e 's/^psql:[^E]*ERROR: */ERROR: /' -e 's/:db.error\/[a-z-]* //' | cut -c1-60
}

# verdict CASE ISO OK LIVE: print the line and count failures.
verdict() {
    local case=$1 iso=$2 ok=$3 live=$4 v
    if [ "$ok" = 1 ]; then v=PASS
    elif [ "$iso" = "repeatable read" ] && [[ " empty alter unique " == *" $case "* ]]; then v=KNOWN
    else v=FAIL; fails=$((fails + 1)); fi
    printf '%-8s %-16s B waited: %-13s A: %-24s B: %-44s live: %-12s %s\n' \
        "$case" "$iso" "$WAITED" "$A_OUT" "$B_OUT" "$live" "$v"
}

for iso in "${LEVELS[@]}"; do
    # replace: e has n = 1; A sets 2, B sets 3.
    n=$((n + 1)); ns=r$n
    tx "[{:db/ident :$ns/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]" >/dev/null
    e=$(tempid "[{:db/id \"e\" :$ns/n 1}]" e); a=$(entid $ns/n)
    race "$iso" "[[:db/add $e :$ns/n 2]]" "[[:db/add $e :$ns/n 3]]"
    live=$(q "SELECT string_agg(v::text, ',' ORDER BY v) FROM mentat.current_long WHERE e = $e AND a = $a")
    verdict replace "$iso" "$([[ $live != *,* ]] && echo 1)" "$live"

    # empty: e has no n; A asserts 2, B asserts 3.
    n=$((n + 1)); ns=r$n
    tx "[{:db/ident :$ns/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
         {:db/ident :$ns/tag :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]" >/dev/null
    e=$(tempid "[{:db/id \"e\" :$ns/tag \"t\"}]" e); a=$(entid $ns/n)
    race "$iso" "[[:db/add $e :$ns/n 2]]" "[[:db/add $e :$ns/n 3]]"
    live=$(q "SELECT string_agg(v::text, ',' ORDER BY v) FROM mentat.current_long WHERE e = $e AND a = $a")
    verdict empty "$iso" "$([[ $live != *,* ]] && echo 1)" "$live"

    # alter: t is cardinality many and e has "x"; A alters t to one, B adds "y".
    n=$((n + 1)); ns=r$n
    tx "[{:db/ident :$ns/t :db/valueType :db.type/string :db/cardinality :db.cardinality/many}]" >/dev/null
    e=$(tempid "[{:db/id \"e\" :$ns/t [\"x\"]}]" e); a=$(entid $ns/t)
    race "$iso" "[[:db/add :$ns/t :db/cardinality :db.cardinality/one]]" "[[:db/add $e :$ns/t \"y\"]]"
    live=$(q "SELECT string_agg(v, ',' ORDER BY v) FROM mentat.current_text WHERE e = $e AND a = $a")
    card=$(q "SELECT cardinality FROM mentat.schema WHERE entid = $a")
    verdict alter "$iso" "$([[ $card == many || $live != *,* ]] && echo 1)" "$live ($card)"

    # cas: e has k = 5; A cas 5 -> 7, B cas 5 -> 6.
    n=$((n + 1)); ns=r$n
    tx "[{:db/ident :$ns/k :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]" >/dev/null
    e=$(tempid "[{:db/id \"e\" :$ns/k 5}]" e); a=$(entid $ns/k)
    race "$iso" "[[:db/cas $e :$ns/k 5 7]]" "[[:db/cas $e :$ns/k 5 6]]"
    live=$(q "SELECT string_agg(v::text, ',' ORDER BY v) FROM mentat.current_long WHERE e = $e AND a = $a")
    verdict cas "$iso" "$([[ $live != *,* ]] && echo 1)" "$live"

    # unique: two new entities assert :email "u" (identity); B should upsert into A's.
    n=$((n + 1)); ns=r$n
    tx "[{:db/ident :$ns/email :db/valueType :db.type/string :db/cardinality :db.cardinality/one
          :db/unique :db.unique/identity}
         {:db/ident :$ns/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]" >/dev/null
    a=$(entid $ns/email)
    race "$iso" "[{:db/id \"a\" :$ns/email \"u\" :$ns/n 1}]" "[{:db/id \"b\" :$ns/email \"u\" :$ns/n 2}]"
    live=$(q "SELECT count(*) FROM mentat.current_text WHERE a = $a AND v = 'u'")
    verdict unique "$iso" "$([[ $live == 1 ]] && echo 1)" "$live holder(s)"
done

mv "$WORK" "${TMPDIR:-/tmp}/race-last" 2>/dev/null || true
echo "failures: $fails"
[ "$fails" = 0 ]
