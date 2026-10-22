#!/usr/bin/env python3
"""The extension loaded into more than one database in one process (#363).

Each check runs in a child Python process with a timeout, so a hang fails the
check instead of the run.

Usage: python3 test/databases.py build/release/mentat.duckdb_extension
(needs the duckdb Python package, 1.5.6 or later; CI runs 1.5.6 and 2.0)."""
import os
import subprocess
import sys
import tempfile
import textwrap

EXT = os.path.abspath(sys.argv[1])
TMP = tempfile.mkdtemp()

PRELUDE = f"""
import duckdb, json, os, sys
EXT = {EXT!r}
TMP = {TMP!r}
def connect(path=":memory:"):
    c = duckdb.connect(path, config={{"allow_unsigned_extensions": "true"}})
    c.load_extension(EXT)
    return c
def schemas(c):
    return sorted(r[0] for r in c.execute(
        "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE 'mentat%'").fetchall())
def names(c, store):
    return sorted(r[0] for r in c.execute(
        "SELECT * FROM edn_q(?, '[:find ?n :where [_ :p/name ?n]]', '{{}}')", [store]).fetchall())
SCHEMA = "[{{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}}]"
"""

CHECKS = {
    # 1.11-1.17.2: every database's calls ran on the FIRST database's pool.
    "two in-memory databases each write their own": """
        a, b = connect(), connect()
        b.execute("SELECT edn_t('s', ?)", [SCHEMA]).fetchall()
        assert schemas(a) == [], f"store created in the wrong database: a has {schemas(a)}"
        assert schemas(b) == ["mentat_s_f60c4582"], schemas(b)
        a.execute("SELECT edn_t('s', ?)", [SCHEMA]).fetchall()
        a.execute("SELECT edn_t('s', '[{:p/name \\"in-a\\"}]')").fetchall()
        b.execute("SELECT edn_t('s', '[{:p/name \\"in-b\\"}]')").fetchall()
        assert names(a, "s") == ["in-a"], names(a, "s")
        assert names(b, "s") == ["in-b"], names(b, "s")
        e = json.loads(b.execute("SELECT edn_t('s', '[{:db/id \\"x\\" :p/name \\"b2\\"}]')").fetchone()[0])["tempids"]["x"]
        assert json.loads(b.execute("SELECT edn_pull('s', '[:p/name]', ?)", [e]).fetchone()[0])[":p/name"] == "b2"
        assert json.loads(a.execute("SELECT edn_pull('s', '[:p/name]', ?)", [e]).fetchone()[0]).get(":p/name") is None
        b.execute("SELECT edn_eval('s', '(mentat.store/transact (mentat.store/open) [{:p/name \\"eval-b\\"}])')").fetchall()
        assert names(a, "s") == ["in-a"], names(a, "s")
        assert names(b, "s") == ["b2", "eval-b", "in-b"], names(b, "s")
    """,
    "two file databases each write their own": """
        a, b = connect(os.path.join(TMP, "a.duckdb")), connect(os.path.join(TMP, "b.duckdb"))
        a.execute("SELECT edn_t('default', ?)", [SCHEMA]).fetchall()
        b.execute("SELECT edn_t('default', ?)", [SCHEMA]).fetchall()
        b.execute("SELECT edn_t('default', '[{:p/name \\"only-b\\"}]')").fetchall()
        assert names(a, "default") == [], names(a, "default")
        assert names(b, "default") == ["only-b"], names(b, "default")
    """,
    "mentat_import into the second database": """
        import sqlite3
        a, b = connect(), connect()
        try:
            b.execute("INSTALL sqlite; LOAD sqlite")
        except duckdb.Error as e:
            if os.environ.get("MENTAT_REQUIRE_SQLITE_EXT") == "1":
                raise
            print(f"SKIPPED (no DuckDB sqlite extension; MENTAT_REQUIRE_SQLITE_EXT=1 fails instead): {e}")
            sys.exit(0)
        # A SQLite-format store (what 1.10.x wrote), copied from a store in a.
        a.execute("SELECT edn_t('old', ?)", [SCHEMA]).fetchall()
        a.execute("SELECT edn_t('old', '[{:p/name \\"imported\\"}]')").fetchall()
        [old] = schemas(a)
        path = os.path.join(TMP, "old.mentat")
        lite = sqlite3.connect(path)
        for t, cols in [("datoms", "e, a, v, tx, value_type_tag, index_avet, index_vaet, index_fulltext, unique_value"),
                        ("timelined_transactions", "e, a, v, tx, added, value_type_tag, timeline"),
                        ("idents", "e, a, v, value_type_tag"), ("schema", "e, a, v, value_type_tag"),
                        ("known_parts", 'part, start, "end", idx, allow_excision')]:
            lite.execute(f"CREATE TABLE {t} ({cols})")
            rows = a.execute(f"SELECT {cols} FROM {old}.{t}").fetchall()
            lite.executemany(f"INSERT INTO {t} VALUES ({', '.join('?' * len(rows[0]))})", rows)
        lite.execute("CREATE TABLE fulltext_values (text, searchid)")
        lite.execute("PRAGMA user_version = 3")
        lite.commit()
        lite.close()
        r = json.loads(b.execute("SELECT mentat_import('imp', ?)", [path]).fetchone()[0])
        assert schemas(a) == [old], f"imported into the wrong database: a has {schemas(a)}"
        assert len(schemas(b)) == 1, schemas(b)
        assert names(b, "imp") == ["imported"], names(b, "imp")
        assert r["datoms"] > 0, r
    """,
}


def run(name, body, timeout=60):
    src = PRELUDE + textwrap.dedent(body)
    try:
        p = subprocess.run([sys.executable, "-c", src], capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return f"{name}: timed out after {timeout} s"
    if p.returncode:
        return f"{name}: exit {p.returncode}\n{p.stdout}{p.stderr}"
    print(p.stdout, end="")  # a skip notice
    return None


errors = [e for e in (run(n, b) for n, b in CHECKS.items()) if e]
print("\n".join(errors) or f"databases OK: {len(CHECKS)} checks")
sys.exit(1 if errors else 0)
