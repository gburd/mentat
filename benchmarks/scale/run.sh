#!/usr/bin/env bash
# benchmarks/scale/run.sh: multi-backend scale/load benchmark driver.
#
#   SCALES="s m" BACKENDS="embedded sqlite-ext pg duckdb" benchmarks/scale/run.sh
#
# For each scale: generate the dataset (cached under $DATA_ROOT), then for each
# backend bulk-load it from empty, run the correctness checks (the run fails on
# a wrong answer), then run every scenario REPS times. Results go to
# benchmarks/results/scale-<UTC>/ (or $OUT): raw.csv (every rep),
# timings.csv (median of reps), per-scenario logs, env.txt, plans/.
# See README.md for every knob. All knobs are env vars and all are recorded in env.txt.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
STAMP="$(date -u +%Y-%m-%dT%H%M%SZ)"
OUT="${OUT:-$REPO/benchmarks/results/scale-$STAMP}"
DATA_ROOT="${DATA_ROOT:-/tmp/mentat-scale-data}"   # datasets (regenerable, never committed)
WORK="${WORK:-$DATA_ROOT/work}"                    # store files
SCALES="${SCALES:-s}"
BACKENDS="${BACKENDS:-embedded sqlite-ext pg duckdb}"
SCENARIOS="${SCENARIOS-point_lookup ref_traversal aggregate predicate_scan pull as_of since input_bindings}"
EXTRA="${EXTRA-write_mixed concurrency_sweep cold_vs_warm}"   # + sustained (SUSTAINED_S>0)
CLIENTS="${CLIENTS:-1 8 32 64 128}"               # concurrency_sweep points
REPS="${REPS:-3}"
MIN_S="${MIN_S:-10}"        # per point: run at least MIN_S seconds AND MIN_N samples...
MIN_N="${MIN_N:-30}"
MAX_S="${MAX_S:-60}"        # ...but stop at MAX_S (slow scenarios: fewer samples, noted in count)
PROBE_S="${PROBE_S:-20}"    # first call slower than this => op=ceiling row, scenario skipped
MIXED_S="${MIXED_S:-60}"    # write_mixed duration
MIXED_READERS="${MIXED_READERS:-8}"
SUSTAINED_S="${SUSTAINED_S:-0}"    # >0: run `sustained` on the largest scale for this long
SUSTAINED_CLIENTS="${SUSTAINED_CLIENTS:-32}"
LOAD_MAX_S="${LOAD_MAX_S:-5400}"   # per-backend bulk-load cap => ceiling record
EXT_LOAD_MAX_S="${EXT_LOAD_MAX_S:-$LOAD_MAX_S}"   # same, for sqlite-ext/duckdb (edn_t per tx)
PHASE="${PHASE:-all}"              # all | load (load+check only) | bench (reuse loaded stores/DBs) | sustained (only)
PG_LOAD_JOBS="${PG_LOAD_JOBS:-32}"
EXT_SCENARIO_FILTER="${EXT_SCENARIO_FILTER:-}"   # scenarios to skip on sqlite-ext/duckdb (space list)
export LOAD_MAX_S

PGBIN="${PGBIN:-$(dirname "$(command -v pg_config || echo /usr/bin/pg_config)")}"
export PATH="$PGBIN:$PATH"
export PGOPTIONS="--client-min-messages=warning"   # PGDATABASE is mentat_<scale>
PY="${PY:-python3}"             # needs duckdb==1.5.5 for the duckdb backend
RUNNER="${RUNNER:-$REPO/target/release/mentat-scale}"
SQLITE_EXT="${SQLITE_EXT:-$REPO/target/release/libmentat_sqlite}"
DUCKDB_EXT="${DUCKDB_EXT:-$REPO/crates/duckdb/build/release/mentat.duckdb_extension}"
export RUNNER SQLITE_EXT DUCKDB_EXT

mkdir -p "$OUT/logs" "$OUT/plans" "$DATA_ROOT" "$WORK"
RAW="$OUT/raw.csv"
[ -f "$RAW" ] || echo "scenario,backend,scale,n_datoms,clients,op,count,p50_ms,p95_ms,p99_ms,max_ms,throughput_ops_s,errors,rep" > "$RAW"
LOADS="$OUT/loads.csv"
[ -f "$LOADS" ] || echo "backend,scale,n_datoms,load_s,datoms_per_s,analyze_s,vacuum_s,store_bytes,ceiling,note" > "$LOADS"
CHECKS="$OUT/checks.txt"
FAILED=0

log() { echo "[$(date -u +%H:%M:%S)] $*" | tee -a "$OUT/run.log" >&2; }
has() { [[ " $1 " == *" $2 "* ]]; }
psql1() { psql -X -qAt -v ON_ERROR_STOP=1 "$@"; }

# ---------------------------------------------------------------- env.txt
knobs_txt() {
  {
    echo
    echo "invocation $(date -u +%FT%TZ): PHASE=$PHASE SCALES='$SCALES' BACKENDS='$BACKENDS' cpu_affinity=$(taskset -pc $$ | sed 's/.*: //')"
    echo "  SCENARIOS='$SCENARIOS' EXTRA='$EXTRA' CLIENTS='$CLIENTS' REPS=$REPS MIN_S=$MIN_S MIN_N=$MIN_N MAX_S=$MAX_S"
    echo "  PROBE_S=$PROBE_S MIXED_S=$MIXED_S MIXED_READERS=$MIXED_READERS SUSTAINED_S=$SUSTAINED_S SUSTAINED_CLIENTS=$SUSTAINED_CLIENTS"
    echo "  LOAD_MAX_S=$LOAD_MAX_S EXT_LOAD_MAX_S=$EXT_LOAD_MAX_S PG_LOAD_JOBS=$PG_LOAD_JOBS EXT_SCENARIO_FILTER='$EXT_SCENARIO_FILTER'"
    echo "  warmup=3 calls (discarded) seed=20260509"
  } >> "$OUT/env.txt"
}

env_txt() {
  [ -f "$OUT/env.txt" ] && return 0
  {
    echo "date_utc:        $(date -u +%FT%TZ)"
    echo "host:            $(hostname)"
    echo "instance_type:   $(curl -s -m 2 -H "X-aws-ec2-metadata-token: $(curl -s -m 2 -X PUT http://169.254.169.254/latest/api/token -H 'X-aws-ec2-metadata-token-ttl-seconds: 60')" http://169.254.169.254/latest/meta-data/instance-type 2>/dev/null || echo n/a)"
    echo "kernel:          $(uname -r)"
    echo "cpu_model:       $(grep -m1 'model name' /proc/cpuinfo | sed 's/^[^:]*: //')"
    echo "cpus:            $(nproc)"
    echo "sockets/numa:    $(lscpu | awk -F: '/Socket\(s\)|NUMA node\(s\)/{gsub(/ /,"",$2); printf "%s ", $2}')"
    echo "mem_total_gib:   $(awk '/MemTotal/{printf "%.1f", $2/1048576}' /proc/meminfo)"
    echo "hugepages:       $(awk '/HugePages_Total|HugePages_Free|Hugepagesize/{printf "%s=%s ", $1, $2}' /proc/meminfo)"
    echo "thp:             $(cat /sys/kernel/mm/transparent_hugepage/enabled 2>/dev/null)"
    echo "governor:        $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo n/a)"
    echo "numa_balancing:  $(cat /proc/sys/kernel/numa_balancing 2>/dev/null)"
    echo "overcommit:      $(cat /proc/sys/vm/overcommit_memory)"
    echo "pg_numactl:      ${PG_NUMACTL:-numactl --interleave=all} (postmaster)"
    echo "storage:";  lsblk -o NAME,SIZE,TYPE,MODEL,MOUNTPOINT | sed 's/^/  /'
    [ -e /proc/mdstat ] && { echo "mdstat:"; sed 's/^/  /' /proc/mdstat; }
    echo "data_fs:         $(df -hT "$WORK" | tail -1)"
    echo "mentat_git:      ${MENTAT_GIT:-$(git -C "$REPO" rev-parse HEAD 2>/dev/null || echo n/a)}"
    echo "rustc:           $(rustc --version 2>/dev/null || echo n/a)"
    echo "python:          $($PY --version 2>&1)"
    echo "duckdb:          $($PY -c 'import duckdb; print(duckdb.__version__)' 2>/dev/null || echo n/a)"
    echo "sqlite(host py): $($PY -c 'import sqlite3; print(sqlite3.sqlite_version)')"
    echo "postgres:        $(postgres --version 2>/dev/null || echo n/a)"
    echo "pg_configure:    $(pg_config --configure 2>/dev/null || echo n/a)"
    if psql1 -d postgres -c 'SELECT 1' >/dev/null 2>&1; then
      echo; echo "postgresql.conf (non-default settings):"
      psql1 -d postgres -F ' = ' -c "SELECT name, setting || COALESCE(unit,'') FROM pg_settings WHERE source NOT IN ('default','override') ORDER BY 1" | sed 's/^/  /'
    fi
  } > "$OUT/env.txt"
}

# ---------------------------------------------------------------- data
gen() {  # gen SCALE -> dataset dir
  local d="$DATA_ROOT/$1"
  if [ ! -f "$d/meta.json" ]; then
    log "gen $1"
    $PY "$HERE/gen.py" "$1" "$d" >&2
  fi
  echo "$d"
}
mget() { $PY -c "import json,sys; print(json.load(open('$1/meta.json'))['$2'])"; }
lget() { $PY -c "import json,sys; print(json.load(open('$1'))['$2'])"; }

# ---------------------------------------------------------------- pg
pg_load() {  # pg_load DATA SCALE
  local d=$1 sc=$2 n; n=$(mget "$d" n_datoms)
  log "pg: reset + load $sc"
  psql1 -d postgres -c "DROP DATABASE IF EXISTS $PGDATABASE" -c "CREATE DATABASE $PGDATABASE"
  # pg_mentat caps results at mentat.max_result_rows (default 100000) and
  # ERRORS past it. q4/since return more rows than that from scale m up, so the
  # bench lifts the cap (0 = unlimited). Other backends have no cap.
  psql1 -d postgres -c "ALTER DATABASE $PGDATABASE SET mentat.max_result_rows = ${PG_MAX_RESULT_ROWS:-0}"
  # Likewise mentat.temp_file_limit (default 1GB, SET LOCAL per query): q3's
  # COUNT(DISTINCT) sort spills past it at xl (300M datoms).
  psql1 -d postgres -c "ALTER DATABASE $PGDATABASE SET mentat.temp_file_limit = '${PG_TEMP_FILE_LIMIT:-100GB}'"
  # pg_mentat logs every query slower than mentat.slow_query_threshold_ms
  # (default 100) as a WARNING carrying 500 chars of SQL, sent to the client
  # and to the server log. PG_SLOW_QUERY_MS=0 turns that off. The default is
  # to keep the shipped behaviour (the scale-2026-09-27 run used it).
  psql1 -d postgres -c "ALTER DATABASE $PGDATABASE SET mentat.slow_query_threshold_ms = ${PG_SLOW_QUERY_MS:-100}"
  psql1 -c "CREATE EXTENSION pg_mentat" -c "CREATE EXTENSION IF NOT EXISTS pg_stat_statements" \
        -c "CREATE SCHEMA bench" -c "CREATE TABLE bench.kv (key text primary key, value text)"
  local t0 t1 ta tv; t0=$(date +%s.%N)
  psql1 -c "SELECT 1 FROM edn_t(\$e\$$(cat "$d/schema.edn")\$e\$)" >/dev/null
  psql1 -f "$d/pg/base.sql" >/dev/null
  # Issues: shards in parallel (explicit entids, no tempid contention).
  if ! ls "$d"/pg/issues-*.sql | timeout "$LOAD_MAX_S" xargs -P "$PG_LOAD_JOBS" -I{} \
        psql -X -q -v ON_ERROR_STOP=1 -o /dev/null -f {} 2>"$OUT/logs/pg-load-$sc.err"; then
    local el; el=$(echo "$(date +%s.%N) - $t0" | bc)
    log "pg: load of $sc hit LOAD_MAX_S or failed after ${el}s (see logs/pg-load-$sc.err)"
    echo "pg,$sc,$n,$el,,,,,1,load cap ${LOAD_MAX_S}s or error" >> "$LOADS"
    return 1
  fi
  psql1 -c "INSERT INTO bench.kv VALUES ('t_mid', (SELECT max(tx) FROM mentat.transactions))"
  local hist; hist=$(ls "$d"/pg/hist-*.sql)
  local last; last=$(echo "$hist" | tail -1)
  for f in $(echo "$hist" | head -n -1); do psql1 -o /dev/null -f "$f"; done
  psql1 -c "INSERT INTO bench.kv VALUES ('t_since', (SELECT max(tx) FROM mentat.transactions))"
  psql1 -o /dev/null -f "$last"
  t1=$(date +%s.%N)
  psql1 -c "ANALYZE"; ta=$(date +%s.%N)
  psql1 -c "VACUUM"; tv=$(date +%s.%N)
  local ls as vs bytes
  ls=$(echo "$t1 - $t0" | bc); as=$(echo "$ta - $t1" | bc); vs=$(echo "$tv - $ta" | bc)
  bytes=$(psql1 -c "SELECT pg_database_size(current_database())")
  echo "pg,$sc,$n,$ls,$(echo "$n / $ls" | bc),$as,$vs,$bytes,0," >> "$LOADS"
  psql1 -c "INSERT INTO bench.kv VALUES ('db_bytes', '$bytes')"
  local sb; sb=$(psql1 -c "SELECT pg_size_bytes(current_setting('shared_buffers'))")
  log "pg: $sc loaded in ${ls}s, db=$(numfmt --to=iec "$bytes") vs shared_buffers=$(numfmt --to=iec "$sb")"
  echo "pg $sc: pg_database_size=$bytes ($(numfmt --to=iec "$bytes")) shared_buffers=$sb ($(numfmt --to=iec "$sb")) ratio=$(echo "scale=3; $bytes / $sb" | bc)" >> "$OUT/sizes.txt"
  psql1 -c "SELECT relname, pg_size_pretty(pg_total_relation_size(c.oid)) total, pg_size_pretty(pg_indexes_size(c.oid)) idx
            FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
            WHERE n.nspname = 'mentat' AND c.relkind = 'r' AND pg_total_relation_size(c.oid) > 1048576
            ORDER BY pg_total_relation_size(c.oid) DESC" > "$OUT/logs/pg-sizes-$sc.txt"
}

pg_vars() {  # -D flags for the pgbench scripts
  local d=$1
  echo "-D n_users=$(mget "$d" n_users) -D n_issues=$(mget "$d" n_issues) -D n_labels=$(mget "$d" n_labels)" \
       "-D i0=$(mget "$d" I0) -D l0=$(mget "$d" L0)" \
       "-D t_mid=$(psql1 -c "SELECT value FROM bench.kv WHERE key='t_mid'")" \
       "-D t_since=$(psql1 -c "SELECT value FROM bench.kv WHERE key='t_since'")"
}

# pgbench one point. pgbench_point DATA SCALE REP SCEN CLIENTS SECS SCRIPT...
# -M simple, not prepared/extended: those modes rewrite EVERY `:name` in the
# script into $N, including the `:find`/`:where` keywords inside the Datalog
# string literals. Simple mode substitutes only defined variables.
pgbench_point() {
  local d=$1 sc=$2 rep=$3 scen=$4 c=$5 secs=$6; shift 6
  local lp="$WORK/pgb-$scen-$sc-$c-$rep"; rm -f "$lp".*
  local j=$(( c < $(nproc) ? c : $(nproc) ))
  # shellcheck disable=SC2046
  pgbench -n -M simple -c "$c" -j "$j" -T "$secs" $(pg_vars "$d") --log --log-prefix="$lp" \
     "$@" > "$OUT/logs/pg-$scen-$sc-c$c-r$rep.txt" 2>&1 || { log "pgbench $scen failed"; tail -5 "$OUT/logs/pg-$scen-$sc-c$c-r$rep.txt" >&2; }
}

pg_bench_scen() {  # DATA SCALE SCEN
  local d=$1 sc=$2 scen=$3 n; n=$(mget "$d" n_datoms)
  local S="$HERE/pgbench"
  # Probe: one call timed; too slow => ceiling.
  local t0 t1 ms
  local vars; vars=$(pg_vars "$d")
  t0=$(date +%s%N)
  # shellcheck disable=SC2086
  PGOPTIONS="$PGOPTIONS -c statement_timeout=${PROBE_S}s" pgbench -n -t 1 -c 1 $vars -f "$S/$scen.sql" >/dev/null 2>&1 || true
  t1=$(date +%s%N); ms=$(( (t1 - t0) / 1000000 ))
  if [ "$ms" -ge $((PROBE_S * 1000)) ]; then
    echo "$scen,pg,$sc,$n,1,ceiling,1,$ms,$ms,$ms,$ms,0,0,1" >> "$RAW"; log "pg $scen: ceiling (${ms}ms)"; return
  fi
  local secs=$MIN_S; [ "$ms" -gt 300 ] && secs=$MAX_S
  for rep in $(seq 1 "$REPS"); do
    pgbench_point "$d" "$sc" "$rep" "$scen" 1 "$secs" -f "$S/$scen.sql"
    $PY "$HERE/bench.py" pglog "$scen" "$sc" "$n" 1 read "$rep" "$secs" "$WORK/pgb-$scen-$sc-1-$rep" >> "$RAW"
  done
}

pg_explain() {  # plans of the generated SQL for each query
  local sc=$1 u=user0@example.com
  for pair in "q1:{\"inputs\":[\"$u\"]}" "q2:{\"inputs\":[\"$u\"]}" "q3:{}" "q4:{\"inputs\":[4]}"; do
    local qn=${pair%%:*} js=${pair#*:}
    local edn; edn=$(sed -n 2p "$HERE/queries/$qn.edn")
    psql1 -c "SELECT mentat_explain(\$q\$$edn\$q\$, '$js'::jsonb)::text" > "$OUT/plans/$sc-$qn.json" 2>&1 || true
  done
  psql1 -c "SELECT round(mean_exec_time::numeric,3) mean_ms, calls, round(total_exec_time::numeric) total_ms,
            left(regexp_replace(query, '\s+', ' ', 'g'), 160) q FROM pg_stat_statements
            ORDER BY total_exec_time DESC LIMIT 25" > "$OUT/logs/pg-stat-statements-$sc.txt" 2>&1 || true
}

# A store/DB that write_mixed or sustained has written to no longer matches
# the generator's truth for state-dependent checks: those runs leave a
# marker, and later checks of that target use --post-write.
pw() { [ -f "$WORK/mutated-$1" ] && echo --post-write; true; }
mutated() { touch "$WORK/mutated-$1"; }

run_pg() {  # DATA SCALE
  local d=$1 sc=$2 n; n=$(mget "$d" n_datoms)
  if [ "$PHASE" != bench ]; then pg_load "$d" "$sc" && rm -f "$WORK/mutated-pg-$sc" || return 0; fi
  log "pg: check $sc"
  # shellcheck disable=SC2046
  if ! $PY "$HERE/bench.py" check pg "$d" - $(pw "pg-$sc") >> "$CHECKS" 2>&1; then FAILED=1; log "pg: CHECK FAILED"; return 0; fi
  [ "$PHASE" = load ] && return 0
  psql1 -c "SELECT pg_stat_statements_reset()" >/dev/null
  for scen in $SCENARIOS; do log "pg $sc $scen"; pg_bench_scen "$d" "$sc" "$scen"; done
  [ -n "$SCENARIOS" ] && pg_explain "$sc"
  local S="$HERE/pgbench"
  if has "$EXTRA" concurrency_sweep; then
    for rep in $(seq 1 "$REPS"); do for c in $CLIENTS; do
      log "pg $sc concurrency_sweep c=$c rep=$rep"
      pgbench_point "$d" "$sc" "$rep" concurrency_sweep "$c" "$MIN_S" -f "$S/point_lookup.sql" -f "$S/ref_traversal.sql" -f "$S/pull.sql"
      $PY "$HERE/bench.py" pglog concurrency_sweep "$sc" "$n" "$c" read "$rep" "$MIN_S" "$WORK/pgb-concurrency_sweep-$sc-$c-$rep" >> "$RAW"
    done; done
  fi
  if has "$EXTRA" write_mixed; then
    for rep in $(seq 1 "$REPS"); do
      log "pg $sc write_mixed rep=$rep"
      pgbench_point "$d" "$sc" "$rep" wm_read "$MIXED_READERS" "$MIXED_S" -f "$S/point_lookup.sql" -f "$S/ref_traversal.sql" &
      mutated "pg-$sc"
      pgbench_point "$d" "$sc" "$rep" wm_write 1 "$MIXED_S" -f "$S/write_state.sql"
      wait
      $PY "$HERE/bench.py" pglog write_mixed "$sc" "$n" "$MIXED_READERS" read "$rep" "$MIXED_S" "$WORK/pgb-wm_read-$sc-$MIXED_READERS-$rep" >> "$RAW"
      $PY "$HERE/bench.py" pglog write_mixed "$sc" "$n" 1 write "$rep" "$MIXED_S" "$WORK/pgb-wm_write-$sc-1-$rep" >> "$RAW"
    done
    $PY "$HERE/bench.py" check pg "$d" - --post-write >> "$CHECKS" 2>&1 || { FAILED=1; log "pg: POST-WRITE CHECK FAILED"; }
  fi
  if has "$EXTRA" cold_vs_warm; then pg_cold_warm "$d" "$sc"; fi
}

pg_cold_warm() {  # restart PG, drop the OS page cache, time the first and the next 30 calls.
  local d=$1 sc=$2 n; n=$(mget "$d" n_datoms)
  [ -n "${PGDATA:-}" ] || { log "pg cold_vs_warm: PGDATA unset, skipped"; return; }
  log "pg $sc cold_vs_warm (restart + drop_caches)"
  # Restart under the same NUMA policy the postmaster was started with.
  pg_ctl -D "$PGDATA" -m fast -w stop >/dev/null
  ${PG_NUMACTL:-numactl --interleave=all} pg_ctl -D "$PGDATA" -w -t 900 start -l "$PGDATA/../pg.log" >/dev/null
  sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null || true
  for scen in point_lookup ref_traversal pull aggregate; do
    local lp="$WORK/pgb-cw-$scen-$sc"; rm -f "$lp".*
    # shellcheck disable=SC2046
    # 1 cold + 30 warm calls, bounded to ~MAX_S of warm time (aggregate at l is ~20 s/call).
    pgbench -n -c 1 -t 1 $(pg_vars "$d") --log --log-prefix="$lp.c" -f "$HERE/pgbench/$scen.sql" > /dev/null 2>&1
    local wt=$(( MIN_S * 2 ))
    pgbench -n -c 1 -T "$wt" $(pg_vars "$d") --log --log-prefix="$lp.w" -f "$HERE/pgbench/$scen.sql" > /dev/null 2>&1
    $PY - "$lp" "$scen" "$sc" "$n" >> "$RAW" <<'EOF'
import glob, sys
sys.path.insert(0, __import__("os").environ["HERE"])
from bench import row
lp, scen, sc, n = sys.argv[1:]
rd = lambda p: [int(l.split()[2]) / 1e3 for f in glob.glob(p + "*") for l in open(f)][:30]  # noqa: E731
cold, warm = rd(lp + ".c"), rd(lp + ".w")
print(row("cold_vs_warm", "pg", sc, n, 1, f"cold_{scen}", cold, cold[0] / 1e3, 0, 1))
print(row("cold_vs_warm", "pg", sc, n, 1, f"warm_{scen}", warm, sum(warm) / 1e3, 0, 1))
EOF
  done
}

pg_sustained() {  # DATA SCALE: long mixed run + 10s samplers
  local d=$1 sc=$2 n; n=$(mget "$d" n_datoms) S="$HERE/pgbench"
  log "pg $sc sustained ${SUSTAINED_S}s c=$SUSTAINED_CLIENTS"
  sampler pg "$sc" &
  local sp=$!
  local lp="$WORK/pgb-sus-$sc"; rm -f "$lp".* "$lp"w.*
  mutated "pg-$sc"
  # shellcheck disable=SC2046
  pgbench -n -M simple -c "$SUSTAINED_CLIENTS" -j "$SUSTAINED_CLIENTS" -T "$SUSTAINED_S" -P 10 $(pg_vars "$d") \
     --log --log-prefix="$lp" -f "$S/point_lookup.sql" -f "$S/ref_traversal.sql" -f "$S/pull.sql" \
     > "$OUT/logs/pg-sustained-$sc.txt" 2>&1 &
  local rp=$!
  # shellcheck disable=SC2046
  pgbench -n -M simple -c 1 -T "$SUSTAINED_S" -P 10 $(pg_vars "$d") --log --log-prefix="${lp}w" \
     -f "$S/write_state.sql" > "$OUT/logs/pg-sustained-write-$sc.txt" 2>&1
  wait $rp || true
  kill $sp 2>/dev/null || true
  $PY "$HERE/bench.py" pglog sustained "$sc" "$n" "$SUSTAINED_CLIENTS" read 1 "$SUSTAINED_S" "$lp." >> "$RAW"
  $PY "$HERE/bench.py" pglog sustained "$sc" "$n" 1 write 1 "$SUSTAINED_S" "${lp}w." >> "$RAW"
  $PY "$HERE/bench.py" pgwindows "$OUT/logs/pg-sustained-windows-$sc.csv" "$lp." "${lp}w."
}

sampler() {  # BACKEND SCALE: every 10s -> logs/sampler-*.log (RSS, iostat, pg_stat_io/bgwriter)
  local be=$1 sc=$2 f="$OUT/logs/sampler-$1-$2.log"
  iostat -x -m 10 > "$OUT/logs/iostat-$be-$sc.log" 2>&1 &
  local ip=$!
  trap 'kill $ip 2>/dev/null; exit 0' TERM
  while true; do
    {
      echo "== $(date -u +%FT%TZ)"
      grep -E 'MemFree|^Cached|Dirty|HugePages_Free' /proc/meminfo | tr -s ' ' | tr '\n' ' '; echo
      ps -eo rss,comm | awk '/postgres|mentat-scale|python/{r[$2]+=$1} END{for(k in r) printf "rss_kib[%s]=%d ", k, r[k]; print ""}'
      if [ "$be" = pg ]; then
        psql1 -c "SELECT 'bgwriter', buffers_clean, maxwritten_clean, buffers_backend, buffers_alloc, checkpoints_timed, checkpoints_req FROM pg_stat_bgwriter" 2>/dev/null
        psql1 -c "SELECT 'io', backend_type, object, context, reads, writes, extends, hits, evictions FROM pg_stat_io WHERE reads>0 OR writes>0 OR hits>0" 2>/dev/null
        psql1 -c "SELECT 'db', xact_commit, blks_read, blks_hit, temp_bytes, deadlocks FROM pg_stat_database WHERE datname=current_database()" 2>/dev/null
      fi
    } >> "$f"
    sleep 10
  done
}

# ---------------------------------------------------------------- embedded / ext
run_embedded() {  # DATA SCALE
  local d=$1 sc=$2 st="$WORK/embedded-$2.db" n; n=$(mget "$d" n_datoms)
  if [ "$PHASE" != bench ]; then
    log "embedded: load $sc"
    local rc=0
    "$RUNNER" load "$d" "$st" > "$OUT/logs/embedded-load-$sc.json" 2> "$OUT/logs/embedded-load-$sc.log" || rc=$?
    ext_load_row embedded "$sc" "$n" "$st" || return 0
    [ $rc -eq 0 ] || return 0
    rm -f "$WORK/mutated-embedded-$sc"
  fi
  log "embedded: check $sc"
  # shellcheck disable=SC2046
  if ! $PY "$HERE/bench.py" check embedded "$d" "$st" $(pw "embedded-$sc") >> "$CHECKS" 2>&1; then FAILED=1; log "embedded: CHECK FAILED"; return 0; fi
  [ "$PHASE" = load ] && return 0
  local scens=${SCENARIOS// /,}
  [ -n "$scens" ] && "$RUNNER" bench "$st" "$d" "$sc" "$REPS" 1 "$scens" "$MIN_S" "$MAX_S" "$MIN_N" "$PROBE_S" >> "$RAW" 2>> "$OUT/logs/embedded-$sc.log"
  if has "$EXTRA" concurrency_sweep; then
    log "embedded $sc concurrency_sweep"
    "$RUNNER" bench "$st" "$d" "$sc" "$REPS" "${CLIENTS// /,}" concurrency_sweep "$MIN_S" "$MAX_S" "$MIN_N" "$PROBE_S" >> "$RAW" 2>> "$OUT/logs/embedded-$sc.log"
  fi
  if has "$EXTRA" cold_vs_warm; then
    sync; echo 3 | sudo tee /proc/sys/vm/drop_caches >/dev/null || true
    "$RUNNER" coldwarm "$st" "$d" "$sc" point_lookup,ref_traversal,pull,aggregate >> "$RAW" 2>> "$OUT/logs/embedded-$sc.log"
  fi
  if has "$EXTRA" write_mixed; then
    log "embedded $sc write_mixed"
    mutated "embedded-$sc"
    "$RUNNER" mixed "$st" "$d" "$sc" "$REPS" "$MIXED_READERS" "$MIXED_S" >> "$RAW" 2>> "$OUT/logs/embedded-$sc.log"
    $PY "$HERE/bench.py" check embedded "$d" "$st" --post-write >> "$CHECKS" 2>&1 || { FAILED=1; log "embedded: POST-WRITE CHECK FAILED"; }
  fi
}

ext_load_row() {  # BACKEND SCALE N STORE: loads.csv row from STORE.load.json
  local be=$1 sc=$2 n=$3 st=$4
  [ -f "$st.load.json" ] || { echo "$be,$sc,$n,,,,,,1,load crashed (see logs)" >> "$LOADS"; log "$be: load failed"; return 1; }
  $PY - "$st.load.json" "$be" "$sc" "$n" >> "$LOADS" <<'EOF'
import json, sys
j = json.load(open(sys.argv[1])); be, sc, n = sys.argv[2:]
if j.get("ceiling"):
    print(f"{be},{sc},{n},{j['elapsed_s']:.1f},{j['datoms_per_s']:.0f},,,,1,"
          f"load cap hit after ~{j['datoms_loaded_est']:.0f} datoms ({j['txs']} txs)")
else:
    print(f"{be},{sc},{n},{j['load_s']:.1f},{int(n) / j['load_s']:.0f},,,{j['store_bytes']},0,")
EOF
  ! grep -q '"ceiling"' "$st.load.json"
}

run_ext() {  # BACKEND DATA SCALE
  local be=$1 d=$2 sc=$3 st="$WORK/$1-$3.db" n; n=$(mget "$d" n_datoms)
  if [ "$PHASE" != bench ]; then
    log "$be: load $sc"
    LOAD_MAX_S=$EXT_LOAD_MAX_S $PY "$HERE/bench.py" load "$be" "$d" "$st" > "$OUT/logs/$be-load-$sc.json" 2> "$OUT/logs/$be-load-$sc.log" || true
    if ! ext_load_row "$be" "$sc" "$n" "$st"; then
      # The load through the extension hit its cap. The on-disk store format
      # is the same mentat crate, so measure the READ path on a copy of the
      # store the embedded loader built (logged; loads.csv keeps the ceiling).
      local e="$WORK/embedded-$sc.db"
      [ -f "$e.load.json" ] && ! grep -q '"ceiling"' "$e.load.json" || return 0
      log "$be: load capped; benchmarking reads on a copy of $e"
      cp "$e" "$st"; cp "$e.entids" "$st.entids"; cp "$e.load.json" "$st.load.json"
      [ -f "$WORK/mutated-embedded-$sc" ] && mutated "$be-$sc"
      echo "$be $sc: reads measured on a copy of the embedded-built store (ext bulk load capped at ${EXT_LOAD_MAX_S}s)" >> "$OUT/sizes.txt"
    fi
  fi
  # Each ext call re-opens the store (O(history)). If one call already takes
  # longer than PROBE_S, the checks would take ~20x that: record ceilings only.
  if ! $PY "$HERE/bench.py" probe "$be" "$d" "$st" "$PROBE_S" >> "$CHECKS" 2>&1; then
    log "$be $sc: one call > ${PROBE_S}s; checks skipped, recording ceilings"
    echo "check $be ($sc): SKIP (per-call cost above PROBE_S=${PROBE_S}s)" >> "$CHECKS"
    local s2; for s2 in $SCENARIOS concurrency_sweep; do
      echo "$s2,$be,$sc,$n,1,ceiling,1,$((PROBE_S * 1000)),$((PROBE_S * 1000)),$((PROBE_S * 1000)),$((PROBE_S * 1000)),0,0,1" >> "$RAW"
    done
    return 0
  fi
  log "$be: check $sc"
  # shellcheck disable=SC2046
  if ! $PY "$HERE/bench.py" check "$be" "$d" "$st" $(pw "$be-$sc") >> "$CHECKS" 2>&1; then FAILED=1; log "$be: CHECK FAILED"; return 0; fi
  [ "$PHASE" = load ] && return 0
  local scens="" s
  for s in $SCENARIOS; do has "$EXT_SCENARIO_FILTER" "$s" || scens="$scens,$s"; done
  [ -n "${scens#,}" ] && $PY "$HERE/bench.py" run "$be" "$d" "$st" "$sc" "$REPS" 1 "${scens#,}" "$MIN_S" "$MAX_S" "$MIN_N" "$PROBE_S" >> "$RAW" 2>> "$OUT/logs/$be-$sc.log"
  if has "$EXTRA" concurrency_sweep; then
    log "$be $sc concurrency_sweep"
    $PY "$HERE/bench.py" run "$be" "$d" "$st" "$sc" "$REPS" "${CLIENTS// /,}" concurrency_sweep "$MIN_S" "$MAX_S" "$MIN_N" "$PROBE_S" >> "$RAW" 2>> "$OUT/logs/$be-$sc.log"
  fi
  if has "$EXTRA" write_mixed; then
    log "$be $sc write_mixed"
    mutated "$be-$sc"
    $PY "$HERE/bench.py" mixed "$be" "$d" "$st" "$sc" "$REPS" "$MIXED_READERS" "$MIXED_S" >> "$RAW" 2>> "$OUT/logs/$be-$sc.log"
    $PY "$HERE/bench.py" check "$be" "$d" "$st" --post-write >> "$CHECKS" 2>&1 || { FAILED=1; log "$be: POST-WRITE CHECK FAILED"; }
  fi
}

# ---------------------------------------------------------------- main
export HERE
env_txt
knobs_txt
log "results -> $OUT"
LAST=""
for sc in $SCALES; do
  d=$(gen "$sc"); LAST=$sc
  [ "$PHASE" = sustained ] && continue
  export PGDATABASE=mentat_$sc
  grep -q "^$sc: " "$OUT/sizes.txt" 2>/dev/null || echo "$sc: $(mget "$d" n_datoms) datoms ($(mget "$d" n_users) users, $(mget "$d" n_issues) issues, $(mget "$d" n_hist) history updates)" >> "$OUT/sizes.txt"
  for be in $BACKENDS; do
    case $be in
      pg) run_pg "$d" "$sc" ;;
      embedded) run_embedded "$d" "$sc" ;;
      sqlite-ext|duckdb) run_ext "$be" "$d" "$sc" ;;
      *) log "unknown backend $be" ;;
    esac
  done
  rm -f "$WORK"/pgb-*-"$sc"-* "$WORK"/pgb-cw-*-"$sc".*   # this scale only (concurrent invocations)
done

if [ "$SUSTAINED_S" -gt 0 ] && [ -n "$LAST" ] && [ "$PHASE" != load ]; then
  d=$(gen "$LAST"); export PGDATABASE=mentat_$LAST
  if has "$BACKENDS" pg; then pg_sustained "$d" "$LAST"; fi
  if has "$BACKENDS" embedded && [ -f "$WORK/embedded-$LAST.db" ]; then
    log "embedded $LAST sustained ${SUSTAINED_S}s"
    mutated "embedded-$LAST"
    sampler embedded "$LAST" & sp=$!
    "$RUNNER" mixed "$WORK/embedded-$LAST.db" "$d" "$LAST" 1 "$SUSTAINED_CLIENTS" "$SUSTAINED_S" \
       "$OUT/logs/embedded-sustained-windows-$LAST.csv" >> "$RAW" 2>> "$OUT/logs/embedded-$LAST.log"
    kill $sp 2>/dev/null || true
  fi
fi

$PY "$HERE/bench.py" medians "$RAW" "$OUT/timings.csv"
$PY "$HERE/report.py" "$OUT" > "$OUT/summary.md" || log "report.py failed"
for f in "$OUT"/logs/*.log "$OUT"/logs/*.txt; do [ -s "$f" ] && [ "$(stat -c %s "$f")" -gt 1048576 ] && gzip -f "$f"; done
log "done: $OUT (checks: $(grep -c PASS "$CHECKS" 2>/dev/null || echo 0) pass, $(grep -c FAIL "$CHECKS" 2>/dev/null || echo 0) fail)"
exit $FAILED
