#!/usr/bin/env bash
# ALTER EXTENSION pg_mentat UPDATE from 1.18.0 against live writers (#428).
#
# The 1.18.0 -> 1.19.0 script took its locks one by one and deadlocked with a
# mentat.t() that started while it ran. The 1.18.0 -> 1.19.1 script takes them
# all up front, without waiting while it holds one, and fails with SQLSTATE
# 55P03 ("busy") if it can't get them within 30 s.
#
#   seed               make database $TEMPLATE at pg_mentat 1.18.0: a default
#                      store and a named store "acme" with data. Run it while
#                      the 1.18.0 library is installed (the server as a 1.18.0
#                      deploy left it).
#   writers [TO] [N]   N runs (default 10), each on a fresh copy of $TEMPLATE:
#                      one pgbench writer per store in $STORES calls mentat.t()
#                      $RATE times a second; after 1.5 s, ALTER EXTENSION
#                      pg_mentat UPDATE TO 'TO' (default 1.19.1), again after a
#                      busy error (up to 5 tries). Prints one line a run and a
#                      summary: deadlocks, busy errors, successes, writer
#                      errors, the writers' longest stall (a write's latency
#                      from its scheduled start).
#   reader [TO]        a reader holds ACCESS SHARE on mentat.current_ref for
#                      $HOLD s (default 40) while the UPDATE runs, with the
#                      writers. 1.19.1 gives up with the busy error after 30 s
#                      and the writers go on; 1.19.0 stalls them until the
#                      reader ends, then deadlocks or succeeds.
#   paths              pg_extension_update_paths for 1.17.2 / 1.18.0 / 1.19.0.
#
# Run it with the new library and the upgrade scripts installed and the server
# restarted after seed. Environment: PGHOST, PGPORT, PGUSER as for psql;
# TEMPLATE (default upw_tmpl), STORES (default "default acme"), RATE
# (default 50), ROWS (default 20000 entities a store).
set -euo pipefail

TEMPLATE=${TEMPLATE:-upw_tmpl}
STORES=${STORES:-default acme}
RATE=${RATE:-50}
ROWS=${ROWS:-20000}
WORK=$(mktemp -d "${TMPDIR:-/tmp}/upw.XXXXXX")
export PGOPTIONS="-c search_path=mentat,public"

q() { psql -X -qAt -v ON_ERROR_STOP=1 -d "$1" -c "$2"; }

seed() {
    psql -X -q -d postgres -c "DROP DATABASE IF EXISTS $TEMPLATE" -c "CREATE DATABASE $TEMPLATE"
    q "$TEMPLATE" "CREATE EXTENSION IF NOT EXISTS pg_trgm"
    q "$TEMPLATE" "CREATE EXTENSION pg_mentat VERSION '1.18.0'"
    q "$TEMPLATE" "SELECT create_store('acme')" >/dev/null
    for st in default acme; do
        q "$TEMPLATE" "SELECT t('$st', '[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one :db/fulltext true}
            {:db/ident :person/age :db/valueType :db.type/long :db/cardinality :db.cardinality/one}
            {:db/ident :person/tag :db/valueType :db.type/keyword :db/cardinality :db.cardinality/many}]')" >/dev/null
        for ((i = 0; i < ROWS; i += 1000)); do
            q "$TEMPLATE" "SELECT t('$st', (SELECT '[' || string_agg(format('{:person/name \"Person %s\" :person/age %s :person/tag :tag/t%s}', n, n % 90, n % 7), ' ') || ']'
                                      FROM generate_series($i, $i + 999) n))" >/dev/null
        done
    done
    echo "seeded $TEMPLATE: pg_mentat $(q "$TEMPLATE" "SELECT extversion FROM pg_extension WHERE extname = 'pg_mentat'"), \
$(q "$TEMPLATE" "SELECT count(*) FROM mentat.current_text") + $(q "$TEMPLATE" "SELECT count(*) FROM mentat_acme.current_text") current_text rows"
}

# fresh DB: a new copy of $TEMPLATE.
fresh() {
    psql -X -q -d postgres -c "DROP DATABASE IF EXISTS $1 WITH (FORCE)" -c "CREATE DATABASE $1 TEMPLATE $TEMPLATE"
}

# start_writers DB SECONDS: one pgbench a store, in the background.
start_writers() {
    local db=$1 secs=$2 st
    WRITERS=()
    for st in $STORES; do
        cat > "$WORK/w_$st.sql" <<EOF
\set n random(1, 1000000000)
SELECT t('$st', '[{:db/id "w" :person/name "Jane Doe ' || :n || '"}]');
EOF
        (cd "$WORK" && pgbench -n -c 1 -R "$RATE" -T "$secs" -l --log-prefix="log_$st" \
            -f "w_$st.sql" "$db" > "$WORK/out_$st" 2>&1) &
        WRITERS+=($!)
    done
}

# stop_writers: wait for them; sets WERR (failed + aborted) and WMAX, the
# longest write in ms from its scheduled start (pgbench -R: includes the time
# it waited to start behind the one before).
stop_writers() {
    local p st f n
    for p in "${WRITERS[@]}"; do wait "$p" || true; done
    WERR=0
    for st in $STORES; do
        f=$(sed -n 's/^number of failed transactions: \([0-9]*\).*/\1/p' "$WORK/out_$st")
        n=$(grep -c "aborted" "$WORK/out_$st" || true)
        grep -q "actually processed: [1-9]" "$WORK/out_$st" || n=$((n + 1))
        WERR=$((WERR + ${f:-0} + n))
    done
    WMAX=$(cat "$WORK"/log_*.* 2>/dev/null | awk '$3 ~ /^[0-9]+$/ && $3 + 0 > m { m = $3 + 0 } END { printf "%.0f", m / 1000 }')
    rm -f "$WORK"/log_*.*
}

# update DB TO: one ALTER EXTENSION UPDATE; prints ok / busy / deadlock / error.
update() {
    local out
    if out=$(psql -X -qAt -v ON_ERROR_STOP=1 -d "$1" -c "ALTER EXTENSION pg_mentat UPDATE TO '$2'" 2>&1); then
        echo ok
    elif grep -q "deadlock detected" <<<"$out"; then echo deadlock
    elif grep -q "could not lock the stores' tables" <<<"$out"; then echo busy
    else echo "error: $(grep -m1 ERROR <<<"$out")"
    fi
}

writers() {
    local to=${1:-1.19.1} runs=${2:-10} r db res tries t0 secs
    local ok=0 busy=0 dead=0 other=0 werr=0 wmax=0
    echo "UPDATE TO '$to', $runs runs, writers on: $STORES at $RATE/s each"
    for ((r = 1; r <= runs; r++)); do
        db=upw_run
        fresh "$db"
        start_writers "$db" 8
        sleep 1.5
        tries=0
        t0=$(date +%s.%N)
        while :; do
            tries=$((tries + 1))
            res=$(update "$db" "$to")
            if [ "$res" = busy ] && [ "$tries" -lt 5 ]; then busy=$((busy + 1)); continue; fi
            break
        done
        secs=$(echo "$(date +%s.%N) - $t0" | bc)
        stop_writers
        case $res in
            ok) ok=$((ok + 1)) ;;
            busy) busy=$((busy + 1)) ;;
            deadlock) dead=$((dead + 1)) ;;
            *) other=$((other + 1)) ;;
        esac
        werr=$((werr + WERR))
        if [ "$WMAX" -gt "$wmax" ]; then wmax=$WMAX; fi
        printf 'run %2d: %-9s tries %d, %.2f s; now %s; writer errors %d, longest write %d ms\n' \
            "$r" "$res" "$tries" "$secs" "$(q "$db" "SELECT extversion FROM pg_extension WHERE extname = 'pg_mentat'")" \
            "$WERR" "$WMAX"
    done
    echo "TOTAL to $to: success $ok/$runs, deadlock $dead, busy errors $busy, other errors $other;" \
         "writer errors $werr; writers' longest write $wmax ms"
}

reader() {
    local to=${1:-1.19.1} hold=${HOLD:-40} db=upw_run res t0 secs
    fresh "$db"
    psql -X -q -d "$db" -c "BEGIN" -c "SELECT count(*) FROM mentat.current_ref" \
        -c "SELECT pg_sleep($hold)" -c "COMMIT" >/dev/null 2>&1 &
    local rd=$!
    start_writers "$db" $((hold + 15))
    sleep 1.5
    t0=$(date +%s.%N)
    res=$(update "$db" "$to")
    secs=$(echo "$(date +%s.%N) - $t0" | bc)
    wait "$rd" || true
    stop_writers
    printf "reader holds current_ref %d s; UPDATE TO '%s': %s after %.1f s; writer errors %d, longest write %d ms\n" \
        "$hold" "$to" "$res" "$secs" "$WERR" "$WMAX"
}

paths() {
    q "$TEMPLATE" "SELECT source || ' -> ' || target || ': ' || coalesce(path, '(none)') FROM pg_extension_update_paths('pg_mentat')
                    WHERE source IN ('1.17.2', '1.18.0', '1.19.0') AND target IN ('1.19.0', '1.19.1') ORDER BY 1"
}

cmd=${1:-}
shift || true
case $cmd in
    seed) seed ;;
    writers) writers "$@" ;;
    reader) reader "$@" ;;
    paths) paths ;;
    *) sed -n '2,32p' "$0"; exit 2 ;;
esac
rm -r "$WORK"
