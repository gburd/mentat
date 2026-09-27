#!/usr/bin/env bash
# Stop a server started by serve.sh (same MENTAT_QUACK_PORT / MENTAT_QUACK_PIDFILE).
set -euo pipefail
PORT="${MENTAT_QUACK_PORT:-9494}"
PIDFILE="${MENTAT_QUACK_PIDFILE:-${XDG_RUNTIME_DIR:-/tmp}/mentat-quack-$PORT.pid}"
[ -f "$PIDFILE" ] || { echo "stop.sh: no pidfile $PIDFILE" >&2; exit 1; }
pid="$(cat "$PIDFILE")"
# serve.sh made the server a session leader: signal the group (duckdb and the
# `sleep` holding its stdin open). SQLite commits are atomic, so a TERM mid-call
# loses at most that call.
kill -TERM -- "-$pid" 2>/dev/null || kill -TERM "$pid" 2>/dev/null || true
for _ in $(seq 50); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
kill -0 "$pid" 2>/dev/null && kill -KILL -- "-$pid" 2>/dev/null || true
rm -f "$PIDFILE"
echo "stopped $pid"
