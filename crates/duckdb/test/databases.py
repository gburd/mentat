#!/usr/bin/env python3
"""The extension loaded into more than one database in one process, and a
database closed and reopened in one process (#363).

Each check runs in a child Python process with a timeout, so a hang fails the
check instead of the run. The `capi:` checks drive the wheel's own libduckdb
through ctypes: `duckdb_open` doesn't go through DuckDB's instance cache, as
the CLI's `.open`, duckdb-rs and Go don't.

Usage: python3 test/databases.py build/release/mentat.duckdb_extension
(needs the duckdb Python package, 1.5.6 or later; CI runs 1.5.6 and 2.0)."""
import os
import subprocess
import sys
import tempfile
import textwrap

EXT = os.path.abspath(sys.argv[1])
TMP = tempfile.mkdtemp()

# Helpers for the child processes (a plain string: no f-string escaping).
PRELUDE = r'''
import ctypes, duckdb, json, os, subprocess, sys, time
import _duckdb

SCHEMA = "[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]"
# The DuckDB schema of store 's' (mentat_duckdb_store::schema_for).
S = "mentat_s_f60c4582"


def connect(path=":memory:", **config):
    c = duckdb.connect(path, config={"allow_unsigned_extensions": "true", **config})
    c.load_extension(EXT)
    return c


def schemas(c):
    return sorted(r[0] for r in c.execute(
        "SELECT schema_name FROM information_schema.schemata WHERE schema_name LIKE 'mentat%'").fetchall())


def names(c, store):
    return sorted(r[0] for r in c.execute(
        "SELECT * FROM edn_q(?, '[:find ?n :where [_ :p/name ?n]]', '{}')", [store]).fetchall())


def threads():
    return int([l for l in open("/proc/self/status") if l.startswith("Threads:")][0].split()[1])


def other_process_writes(path):
    # Whether another process can open `path` for writing; it must not while
    # this process has it open.
    prog = "import duckdb, sys; duckdb.connect(sys.argv[1]).execute('CREATE TABLE other(x INT)')"
    return subprocess.run([sys.executable, "-c", prog, path], capture_output=True).returncode == 0


lib = ctypes.CDLL(_duckdb.__file__)
for f in ("duckdb_open_ext", "duckdb_connect", "duckdb_query", "duckdb_create_config", "duckdb_set_config"):
    getattr(lib, f).restype = ctypes.c_int
lib.duckdb_result_error.restype = ctypes.c_char_p
lib.duckdb_value_varchar.restype = ctypes.c_void_p
lib.duckdb_value_varchar.argtypes = [ctypes.c_void_p, ctypes.c_uint64, ctypes.c_uint64]


def capi_open(path):
    cfg, db, err = ctypes.c_void_p(), ctypes.c_void_p(), ctypes.c_char_p()
    lib.duckdb_create_config(ctypes.byref(cfg))
    lib.duckdb_set_config(cfg, b"allow_unsigned_extensions", b"true")
    assert lib.duckdb_open_ext(path.encode(), ctypes.byref(db), cfg, ctypes.byref(err)) == 0, err.value
    lib.duckdb_destroy_config(ctypes.byref(cfg))
    return db


def capi_connect(db):
    con = ctypes.c_void_p()
    assert lib.duckdb_connect(db, ctypes.byref(con)) == 0
    return con


def capi_query(con, sql):
    # (first value or None, error or None)
    res = (ctypes.c_byte * 128)()  # a duckdb_result (48 bytes in v1.5.6)
    if lib.duckdb_query(con, sql.encode(), res):
        err = lib.duckdb_result_error(res).decode()
        lib.duckdb_destroy_result(res)
        return None, err
    p = lib.duckdb_value_varchar(ctypes.addressof(res), 0, 0)
    v = ctypes.string_at(p).decode() if p else None
    if p:
        lib.duckdb_free(ctypes.c_void_p(p))
    lib.duckdb_destroy_result(res)
    return v, None
'''

# Exit with a database still open, or right after closing one (a release may
# be under way). argv: path, ext, tag, keep|close, delay.
EXIT_PROG = r'''
import duckdb, sys, time
path, ext, tag, how, delay = sys.argv[1:]
cfg = {"allow_unsigned_extensions": "true"}
c = duckdb.connect(path, config=cfg); c.load_extension(ext)
c.execute("SELECT edn_t('s', ?)", ["[{:db/ident :p/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]"]).fetchall()
c.execute("SELECT edn_t('s', ?)", ['[{:p/name "%s"}]' % tag]).fetchall()
m = duckdb.connect(":memory:", config=cfg); m.load_extension(ext)
if how == "close":
    c.close(); m.close(); time.sleep(float(delay))
'''

CHECKS = {
    # 1.11-1.17.1: every database's calls ran on the FIRST database's pool.
    "two in-memory databases each write their own": r'''
        a, b = connect(), connect()
        b.execute("SELECT edn_t('s', ?)", [SCHEMA]).fetchall()
        assert schemas(a) == [], f"store created in the wrong database: a has {schemas(a)}"
        assert schemas(b) == [S], schemas(b)
        a.execute("SELECT edn_t('s', ?)", [SCHEMA]).fetchall()
        a.execute("""SELECT edn_t('s', '[{:p/name "in-a"}]')""").fetchall()
        b.execute("""SELECT edn_t('s', '[{:p/name "in-b"}]')""").fetchall()
        assert names(a, "s") == ["in-a"], names(a, "s")
        assert names(b, "s") == ["in-b"], names(b, "s")
        r = b.execute("""SELECT edn_t('s', '[{:db/id "x" :p/name "b2"}]')""").fetchone()[0]
        e = json.loads(r)["tempids"]["x"]
        pull = lambda c: json.loads(c.execute("SELECT edn_pull('s', '[:p/name]', ?)", [e]).fetchone()[0])
        assert pull(b)[":p/name"] == "b2", pull(b)
        assert ":p/name" not in pull(a), pull(a)
    ''',
    "two file databases each write their own": r'''
        a, b = connect(os.path.join(TMP, "a.duckdb")), connect(os.path.join(TMP, "b.duckdb"))
        a.execute("SELECT edn_t('default', ?)", [SCHEMA]).fetchall()
        b.execute("SELECT edn_t('default', ?)", [SCHEMA]).fetchall()
        b.execute("""SELECT edn_t('default', '[{:p/name "only-b"}]')""").fetchall()
        assert names(a, "default") == [], names(a, "default")
        assert names(b, "default") == ["only-b"], names(b, "default")
    ''',
    # 1.11-1.17.1: the pool's connections kept the closed database alive, and
    # DuckDB's instance cache waited for it forever (at 100% CPU).
    "close and reopen the same file": r'''
        path = os.path.join(TMP, "reopen.duckdb")
        for i in range(3):
            c = connect(path)
            c.execute("SELECT edn_t('s', ?)", [SCHEMA]).fetchall()
            c.execute("SELECT edn_t('s', ?)", ['[{:p/name "n%d"}]' % i]).fetchall()
            c.close()
        c = duckdb.connect(path)
        n = c.execute(f"SELECT count(*) FROM {S}.datoms WHERE v_s LIKE 'n_'").fetchone()[0]
        assert n == 3, n
    ''',
    # A closed database is freed: its DuckDB worker threads end.
    "a closed database is freed": r'''
        def cycle():
            c = connect(threads=8)
            c.execute("SELECT edn_t('s', ?)", [SCHEMA]).fetchall()
            c.close()
        cycle(); time.sleep(0.5)
        before = threads()
        for _ in range(5):
            cycle()
        time.sleep(0.5)
        assert threads() <= before + 1, f"{threads() - before} threads more after 5 closed databases"
    ''',
    "MENTAT_DUCKDB_RELEASE_IDLE=0 keeps the database open": r'''
        os.environ["MENTAT_DUCKDB_RELEASE_IDLE"] = "0"
        connect(threads=8).close(); time.sleep(0.5)
        before = threads()
        connect(threads=8).close(); time.sleep(0.5)
        assert threads() >= before + 7, "the database was freed"
    ''',
    "exit without closing, and right after closing": r'''
        path = os.path.join(TMP, "exit.duckdb")
        runs = [("keep", 0)] + [("close", d / 100) for d in range(0, 30, 3)]
        for i, (how, delay) in enumerate(runs):
            t = time.time()
            p = subprocess.run([sys.executable, "-c", EXIT_PROG, path, EXT, f"x{i}", how, str(delay)],
                               capture_output=True, text=True, timeout=30)
            assert p.returncode == 0, (how, delay, p.returncode, p.stderr[-500:])
            assert time.time() - t < 10, (how, delay, time.time() - t)
        c = duckdb.connect(path)
        n = c.execute(f"SELECT count(*) FROM {S}.datoms WHERE v_s LIKE 'x%'").fetchone()[0]
        assert n == len(runs), (n, len(runs))
    ''',
    # A database still open with no connection (the C API, Go and R allow it):
    # its mentat connections are closed, and a later call says why.
    "capi: a database with no connections left": r'''
        for release in ("1", "0"):
            os.environ["MENTAT_DUCKDB_RELEASE_IDLE"] = release
            db = capi_open(":memory:")
            con = capi_connect(db)
            assert capi_query(con, f"LOAD '{EXT}'")[1] is None
            lib.duckdb_disconnect(ctypes.byref(con))
            time.sleep(0.5)
            con = capi_connect(db)
            v, err = capi_query(con, f"SELECT edn_t('s', '{SCHEMA}')")
            if release == "1":
                assert err and "close and reopen the database" in err, (v, err)
            else:
                assert err is None and "tx_id" in v, (v, err)
            lib.duckdb_disconnect(ctypes.byref(con))
            lib.duckdb_close(ctypes.byref(db))
    ''',
    # Close a file and open it again without the instance cache: the old
    # instance must not be freed while the new one has the file open (closing
    # its descriptor would drop this process's lock on the file).
    "capi: reopen without the instance cache keeps the file locked": r'''
        path = os.path.join(TMP, "capi.duckdb")
        db = capi_open(path)
        con = capi_connect(db)
        assert capi_query(con, f"LOAD '{EXT}'")[1] is None
        assert capi_query(con, f"SELECT edn_t('s', '{SCHEMA}')")[1] is None
        lib.duckdb_disconnect(ctypes.byref(con))
        lib.duckdb_close(ctypes.byref(db))
        for delay in (0, 0.05):
            time.sleep(delay)
            db = capi_open(path)
            con = capi_connect(db)
            time.sleep(0.5)
            assert not other_process_writes(path), f"another process opened the file for writing (delay {delay})"
            v, err = capi_query(con, f"SELECT count(*) FROM {S}.datoms")
            assert err is None and int(v) > 0, (v, err)
            lib.duckdb_disconnect(ctypes.byref(con))
            lib.duckdb_close(ctypes.byref(db))
    ''',
}


def run(name, body, timeout=90):
    src = f"EXT = {EXT!r}\nTMP = {TMP!r}\nEXIT_PROG = {EXIT_PROG!r}\n" + PRELUDE + textwrap.dedent(body)
    env = {k: v for k, v in os.environ.items() if not k.startswith("MENTAT_DUCKDB_")}
    try:
        p = subprocess.run([sys.executable, "-c", src], capture_output=True, text=True, timeout=timeout, env=env)
    except subprocess.TimeoutExpired:
        return f"{name}: timed out after {timeout} s"
    if p.returncode:
        return f"{name}: exit {p.returncode}\n{p.stdout}{p.stderr}"
    return None


only = sys.argv[2:]  # optional: run only the checks whose names contain these
checks = {n: b for n, b in CHECKS.items() if not only or any(o in n for o in only)}
errors = [e for e in (run(n, b) for n, b in checks.items()) if e]
print("\n".join(errors) or f"databases OK: {len(checks)} checks")
sys.exit(1 if errors else 0)
