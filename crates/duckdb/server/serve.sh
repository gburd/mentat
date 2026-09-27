#!/usr/bin/env bash
# Run mentat inside a long-lived DuckDB Quack server (see ../README.md,
# "Running as a server (Quack)").
#
#   MENTAT_QUACK_TOKEN=$(openssl rand -hex 24) crates/duckdb/server/serve.sh
#
# Env (all optional except the token):
#   MENTAT_QUACK_TOKEN     shared secret clients must present (>= 4 chars; use 32+)
#   MENTAT_QUACK_HOST      bind address, default 127.0.0.1. Anything else also
#                          needs a TLS reverse proxy in front: Quack speaks plain HTTP.
#   MENTAT_QUACK_PORT      default 9494
#   MENTAT_QUACK_DB        the server's own DuckDB database file, default in-memory
#   MENTAT_EXT             default ../build/release/mentat.duckdb_extension
#   DUCKDB                 DuckDB v1.5.5 CLI, default `duckdb`
#   MENTAT_QUACK_PIDFILE   default ${XDG_RUNTIME_DIR:-/tmp}/mentat-quack-$PORT.pid
#   MENTAT_QUACK_LOG       default ${PIDFILE%.pid}.log (background mode)
#   MENTAT_QUACK_FOREGROUND=1  exec in the foreground (systemd); else daemonize
#   MENTAT_STORE_CACHE     passed through to the extension (stores kept per thread)
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
HOST="${MENTAT_QUACK_HOST:-127.0.0.1}"
PORT="${MENTAT_QUACK_PORT:-9494}"
EXT="${MENTAT_EXT:-$HERE/../build/release/mentat.duckdb_extension}"
DUCKDB="${DUCKDB:-duckdb}"
PIDFILE="${MENTAT_QUACK_PIDFILE:-${XDG_RUNTIME_DIR:-/tmp}/mentat-quack-$PORT.pid}"
LOG="${MENTAT_QUACK_LOG:-${PIDFILE%.pid}.log}"
DB="${MENTAT_QUACK_DB:-}"

MENTAT_QUACK_TOKEN="${MENTAT_QUACK_TOKEN:-}"
die() { echo "serve.sh: $*" >&2; exit 1; }
[ "${#MENTAT_QUACK_TOKEN}" -ge 4 ] || die "set MENTAT_QUACK_TOKEN (>= 4 chars; use 32+)"
[ -f "$EXT" ] || die "extension not found: $EXT (make release in crates/duckdb)"
if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
  die "already running (pid $(cat "$PIDFILE"), $PIDFILE)"
fi

other=false
case "$HOST" in
  127.0.0.1|localhost|::1) ;;
  *) other=true
     echo "serve.sh: binding $HOST: Quack is plain HTTP, put a TLS reverse proxy in front" >&2 ;;
esac

# The token is read with getenv() inside the server, so it is never in argv
# (ps) or on disk. The DuckDB CLI serves while its stdin stays open.
INIT="$(mktemp "${TMPDIR:-/tmp}/mentat-quack.XXXXXX.sql")"
trap 'rm -f "$INIT"' EXIT
esc() { printf "%s" "${1//\'/\'\'}"; }
cat > "$INIT" <<SQL
INSTALL quack;
LOAD quack;
LOAD '$(esc "$EXT")';
SELECT listen_uri, listen_url FROM quack_serve('quack:$(esc "$HOST"):$PORT',
  token => getenv('MENTAT_QUACK_TOKEN'), allow_other_hostname => $other, disable_ssl => true);
SQL
export MENTAT_QUACK_TOKEN
# -unsigned: mentat is not (yet) a signed community extension.
CMD=("$DUCKDB" -unsigned -init "$INIT")
[ -n "$DB" ] && CMD+=("$DB")

if [ "${MENTAT_QUACK_FOREGROUND:-0}" = 1 ]; then
  echo $$ > "$PIDFILE"
  # The init file is read at startup; remove it after, from a subshell.
  ( sleep 5; rm -f "$INIT" ) &
  trap - EXIT
  exec "${CMD[@]}" < <(exec sleep infinity)
fi

# Background: a new session, so the server survives this shell's exit and
# stop.sh can signal the whole group (duckdb + its stdin keeper).
# shellcheck disable=SC2016  # $$/$0/$@ expand in the child bash, on purpose
setsid bash -c 'echo $$ > "$0"; exec "$@" < <(exec sleep infinity)' \
  "$PIDFILE" "${CMD[@]}" > "$LOG" 2>&1 < /dev/null &
for _ in $(seq 100); do
  if (exec 3<>"/dev/tcp/${HOST/localhost/127.0.0.1}/$PORT") 2>/dev/null; then
    echo "mentat quack server: quack:$HOST:$PORT pid $(cat "$PIDFILE") log $LOG"
    exit 0
  fi
  [ -f "$PIDFILE" ] && ! kill -0 "$(cat "$PIDFILE")" 2>/dev/null && break
  sleep 0.1
done
tail -5 "$LOG" >&2
die "server did not start (log: $LOG)"
