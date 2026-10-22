#!/usr/bin/env python3
"""End to end: a store written by a REAL 1.10.x extension (a SQLite file; its
store argument is a path) is imported with `mentat_import` by this build and
answers the differential queries exactly as 1.10.x does: now, and as of /
since every transaction. Then the next transaction gets the same tx id and
entid on both.

Every step runs in its own process. One process can't hold both: the two
extensions are both named `mentat`, and (since 1.11) the extension keeps its
connections for the life of the process, so a file database closed in a
process can't be reopened in it.

  python3 test/import-1.10.py OLD_EXT NEW_EXT [OLD_CLI]

OLD_EXT is a 1.10.x mentat.duckdb_extension built for DuckDB v1.5.6 (the
release asset mentat-duckdb-v1.5.6-linux_amd64); OLD_CLI is the v1.5.6 CLI
that loads it (default: duckdb on PATH). This interpreter's duckdb package
loads NEW_EXT and needs the sqlite extension installed (`INSTALL sqlite`).
The queries and tx-data are the store harness's (src/fixtures.rs)."""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

OLD, NEW = sys.argv[1], sys.argv[2]
OLD_CLI = sys.argv[3] if len(sys.argv) > 3 else "duckdb"
HERE = os.path.dirname(os.path.abspath(__file__))
diff = open(os.path.join(HERE, "../store/tests-harness/src/fixtures.rs")).read()
TXS = re.findall(r'r#"(.*?)"#', diff.split("pub const TXS")[1].split("];")[0], re.S)
QS = [json.loads(q) for q in re.findall(r'^\s*("\[.*\]"),$',
                                       diff.split("pub const QUERIES")[1].split("];")[0], re.M)]
assert len(TXS) >= 5 and len(QS) > 40, (len(TXS), len(QS))
# The rest use 1.18.0 tx syntax 1.10.x rejects ([a v] lookup refs, #152).
TXS = TXS[:5]
TMP = tempfile.mkdtemp(prefix="mentat-import-")
OLD_STORE = os.path.join(TMP, "old-1.10.mentat")
NEW_DB = os.path.join(TMP, "new.duckdb")


def lit(s):
    return "'" + s.replace("'", "''") + "'"


def old_cli(sql):
    """Run `sql` under the 1.10.x extension; JSON rows."""
    out = subprocess.run([OLD_CLI, "-unsigned", "-json", ":memory:",
                          "-c", f"LOAD {lit(OLD)};", "-c", sql],
                         capture_output=True, text=True, timeout=120)
    if out.returncode != 0:
        return "ERR " + (out.stderr or out.stdout).strip().split("\n")[0][:80]
    return json.loads(out.stdout or "[]")


def new_py(code):
    """Run `code` under this build in a fresh interpreter; its JSON stdout."""
    prog = (
        "import duckdb, json, sys\n"
        f"c = duckdb.connect({NEW_DB!r}, config={{'allow_unsigned_extensions': 'true'}})\n"
        f"c.load_extension({NEW!r})\n" + code)
    out = subprocess.run([sys.executable, "-c", prog], capture_output=True, text=True,
                         timeout=300)
    if out.returncode != 0:
        sys.exit(f"new build failed:\n{out.stderr}")
    return json.loads(out.stdout)


def queries(txids):
    """(label, query, options) for now, asOf and since every tx."""
    out = [("now", q, {}) for q in QS]
    for t in txids:
        for q in QS[:12]:
            out += [(f"asOf {t}", q, {"asOf": t}), (f"since {t}", q, {"since": t})]
    return out


# 1. The 1.10.x extension writes the store and answers every query.
txids = []
for tx in TXS:
    r = old_cli(f"SELECT edn_t({lit(OLD_STORE)}, {lit(tx)}) AS r;")
    assert not isinstance(r, str), r
    txids.append(json.loads(r[0]["r"])["tx_id"])
print("1.10.x wrote", OLD_STORE, os.path.getsize(OLD_STORE), "bytes; txs", txids)
with open(OLD_STORE, "rb") as f:
    assert f.read(16) == b"SQLite format 3\x00"
QUERIES = queries(txids)


def old_rows(q, opts):
    r = old_cli(f"SELECT * FROM edn_q({lit(OLD_STORE)}, {lit(q)}, {lit(json.dumps(opts))});")
    return r if isinstance(r, str) else sorted(json.dumps(list(row.values())) for row in r)


want = [old_rows(q, o) for _, q, o in QUERIES]

# 2. This build imports it, then (3) a fresh process answers the queries.
ANSWER = (
    "def rows(q, o):\n"
    "    try:\n"
    "        return sorted(json.dumps(list(r)) for r in\n"
    "                      c.execute('SELECT * FROM edn_q(?, ?, ?)', ['imported', q, json.dumps(o)]).fetchall())\n"
    "    except Exception as e:\n"
    "        return 'ERR ' + str(e).split('\\n')[0][:80]\n"
    f"print(json.dumps([rows(q, o) for _, q, o in {QUERIES!r}]))\n")
imp = new_py(
    "c.execute('LOAD sqlite')\n"
    f"print(c.execute('SELECT mentat_import(?, ?)', ['imported', {OLD_STORE!r}]).fetchone()[0])\n")
print("import:", imp)
assert imp["last_tx"] == txids[-1] and imp["transactions"] == len(TXS) + 1, imp
got = new_py(ANSWER)


def norm_err(v):
    # Both engines reject the same queries; their messages differ.
    return "ERR" if isinstance(v, str) else v


# A query 1.10.x can't run (get-else / missing? came later, #335; string
# predicates, #267; min/max over refs, #249; sum over no rows, #237) is
# answered by this build: an engine change, not an import one. Only those
# may differ; tests/import.rs compares them against the current engine.
NEWER = ("no function named get-else", "no function named missing?",
         "no function named like", "no function named ilike",
         "no function named starts-with?", "no function named ends-with?",
         "no function named includes?", "cannot apply projection operation M",
         "pair: (Null, 5)")
newer = [i for i in range(len(QUERIES)) if isinstance(want[i], str)
         and any(m in want[i] for m in NEWER) and not isinstance(got[i], str)]
bad = [(QUERIES[i][:2], want[i], got[i]) for i in range(len(QUERIES))
       if i not in newer and norm_err(want[i]) != norm_err(got[i])]
for k, a, b in bad[:10]:
    print("DIFF", k, "\n  1.10.x:", a, "\n  import:", b)
errs = sum(1 for v in want if isinstance(v, str)) - len(newer)
print(f"compared {len(QUERIES)} query results ({errs} errors on both, {len(newer)} "
      f"unsupported by 1.10.x); {len(bad)} differ")

# 4. The next transaction continues the old store's counters.
NEXT = '[{:db/id "n" :p/name "Next"}]'
a = json.loads(old_cli(f"SELECT edn_t({lit(OLD_STORE)}, {lit(NEXT)}) AS r;")[0]["r"])
b = new_py(f"print(c.execute('SELECT edn_t(?, ?)', ['imported', {NEXT!r}]).fetchone()[0])\n")
print("next tx: 1.10.x", a["tx_id"], a["tempids"], "import", b["tx_id"], b["tempids"])
ok = not bad and a["tx_id"] == b["tx_id"] and a["tempids"] == b["tempids"]
print("IMPORT E2E", "PASS" if ok else "FAIL (files kept in " + TMP + ")")
if ok:
    shutil.rmtree(TMP)
sys.exit(0 if ok else 1)
