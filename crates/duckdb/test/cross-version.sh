#!/usr/bin/env bash
# One mentat.duckdb_extension, loaded into DuckDB v1.5.6 (its C API floor)
# and DuckDB 2.0 (v2.0.0 at d54cbbe3, what the Community Extensions registry
# builds against). See README.md "Pinned versions".
#
#   EXT=build/release/mentat.duckdb_extension CLIS=<dir> test/cross-version.sh
#
# CLIS is the directory scripts/duckdb-clis.sh fills. Runs: the footer check
# (C_STRUCT, not C_STRUCT_UNSTABLE), test/smoke.sh under both CLIs, and when
# configure/venv exists (make configure) the SQLLogicTest + concurrency test
# under the duckdb 1.5.6 wheel and the 2.0 dev wheel.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
EXT="$(cd "$(dirname "${EXT:?set EXT}")" && pwd)/$(basename "$EXT")"
CLIS="${CLIS:?set CLIS (see scripts/duckdb-clis.sh)}"
cd "$HERE"

# The footer is the last 512 bytes: NUL-padded 32-byte fields; the 4th is
# the ABI type, the 6th the DuckDB version (for C_STRUCT, the C API floor).
field() { tail -c 512 "$EXT" | head -c $(($1 * 32)) | tail -c 32 | tr -d '\0'; }
[ "$(field 4)" = C_STRUCT ] \
  || { echo "FAIL: $EXT footer ABI is '$(field 4)', not C_STRUCT (the stable C API)"; exit 1; }
echo "footer: $(field 4), floor $(field 6), extension $(field 5)"

for v in v1.5.6 v2.0; do
  echo "== smoke.sh under the DuckDB $v CLI"
  EXT="$EXT" DUCKDB="$CLIS/$v/duckdb" bash test/smoke.sh
done

if [ -x configure/venv/bin/python3 ]; then
  echo "== SQLLogicTest + concurrency under duckdb $(configure/venv/bin/python3 -c 'import duckdb; print(duckdb.__version__)')"
  configure/venv/bin/python3 -m duckdb_sqllogictest --test-dir test/sql --external-extension "$EXT"
  configure/venv/bin/python3 test/concurrency.py "$EXT"

  v20=configure/venv-v20
  if [ ! -x "$v20/bin/python3" ]; then
    python3 -m venv "$v20"
    # The newest 2.0 dev wheel at the registry's d54cbbe3 pin (it is built
    # from b32b1368, 13 commits earlier; duckdb_extension.h is identical).
    "$v20/bin/pip" install -q --pre "duckdb==2.0.0.dev2610192346" packaging \
      "git+https://github.com/duckdb/duckdb-sqllogictest-python@2ac8dbc012ddbd96a57dca37784fd8ee3c0eb021"
  fi
  echo "== SQLLogicTest + concurrency under duckdb $("$v20/bin/python3" -c 'import duckdb; print(duckdb.__version__)')"
  "$v20/bin/python3" -m duckdb_sqllogictest --test-dir test/sql --external-extension "$EXT"
  "$v20/bin/python3" test/concurrency.py "$EXT"
fi
echo "PASS: one binary, DuckDB v1.5.6 and v2.0"
