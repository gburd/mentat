#!/usr/bin/env python3
"""benchmarks/scale helper: correctness checks, sqlite-ext/duckdb load+bench,
pgbench log parsing, rep medians.

  bench.py check    BACKEND DATA TARGET [--post-write]   exit 1 on any wrong answer
  bench.py load     BACKEND DATA STORE                   bulk load through the extension
  bench.py run      BACKEND DATA STORE SCALE REPS CLIENTS,.. SCEN,.. MIN_S MAX_S MIN_N PROBE_S
  bench.py mixed    BACKEND DATA STORE SCALE REPS READERS SECS
  bench.py pglog    SCEN SCALE N_DATOMS CLIENTS OP REP WALL_S LOGPREFIX...
  bench.py pgwindows OUT_CSV READ_LOGPREFIX WRITE_LOGPREFIX
  bench.py medians  RAW_CSV OUT_CSV

BACKEND is embedded | sqlite-ext | duckdb | pg. TARGET is the store file for
the first three and ignored for pg (psql uses PG* env). EXT is the extension
path ($SQLITE_EXT / $DUCKDB_EXT). The embedded backend shells out to
$RUNNER (the mentat-scale binary) for checks.
"""
import glob
import json
import multiprocessing as mp
import os
import random
import statistics
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
Q = {n: open(os.path.join(HERE, "queries", f"{n}.edn")).read().split("\n", 1)[1].strip()
     for n in ("q1", "q2", "q3", "q4", "since", "inputs")}
SCEN_Q = {"point_lookup": "q1", "ref_traversal": "q2", "as_of": "q2", "aggregate": "q3",
          "predicate_scan": "q4", "since": "since", "input_bindings": "inputs"}
READ_MIX = ["point_lookup", "ref_traversal", "pull"]   # concurrency_sweep
READ_MIX2 = ["point_lookup", "ref_traversal"]          # write_mixed readers
COLS = "scenario,backend,scale,n_datoms,clients,op,count,p50_ms,p95_ms,p99_ms,max_ms,throughput_ops_s,errors"


def jload(p):
    with open(p) as f:
        return json.load(f)


def pct(xs, p):
    if not xs:
        return 0.0
    i = int(-(-(p / 100.0) * len(xs) // 1))
    return xs[min(max(i, 1), len(xs)) - 1]


def row(scen, backend, scale, n_datoms, clients, op, lat, wall, errors, rep):
    lat = sorted(lat)
    return (f"{scen},{backend},{scale},{n_datoms},{clients},{op},{len(lat)},{pct(lat, 50):.3f},"
            f"{pct(lat, 95):.3f},{pct(lat, 99):.3f},{(lat[-1] if lat else 0):.3f},"
            f"{len(lat) / max(wall, 1e-9):.2f},{errors},{rep}")


def email(u):
    return f"user{u}@example.com"


# --------------------------------------------------------------------------
# Backends: one query -> list of rows (python values). Same argument meaning
# for every backend: users/issues are the generator's 0-based indexes.
# --------------------------------------------------------------------------
class Ext:
    """sqlite-ext / duckdb: both take the store path first."""

    def __init__(self, backend, store, meta):
        self.backend, self.store, self.meta = backend, store, meta
        self.load = jload(store + ".load.json") if os.path.exists(store + ".load.json") else {}
        self.entids = None
        if backend == "sqlite-ext":
            import sqlite3
            self.db = sqlite3.connect(":memory:", isolation_level=None)
            self.db.enable_load_extension(True)
            self.db.load_extension(os.environ["SQLITE_EXT"])
        else:
            import duckdb
            self.db = duckdb.connect(config={"allow_unsigned_extensions": "true"})
            self.db.load_extension(os.environ["DUCKDB_EXT"])

    def entid(self, idx):
        if self.entids is None:
            import array
            a = array.array("q")
            with open(self.store + ".entids", "rb") as f:
                a.frombytes(f.read())
            self.entids = a
        return self.entids[idx]

    def t(self, edn):
        return self.db.execute("SELECT edn_t(?, ?)", [self.store, edn]).fetchone()[0]

    def q(self, query, opts):
        o = json.dumps(opts)
        if self.backend == "duckdb":
            return [list(r) for r in self.db.execute(
                "SELECT * FROM edn_q(?, ?, ?)", [self.store, query, o]).fetchall()]
        r = json.loads(self.db.execute("SELECT edn_q(?, ?, ?)", [self.store, query, o]).fetchone()[0])
        return r.get("results", r.get("result"))

    def pull(self, idx):
        s = self.db.execute("SELECT edn_pull(?, '[*]', ?)", [self.store, self.entid(idx)]).fetchone()[0]
        return json.loads(s)


class Pg:
    def __init__(self, meta):
        self.meta = meta
        self.load = {"t_mid": int(self.sql("SELECT value FROM bench.kv WHERE key='t_mid'")),
                     "t_since": int(self.sql("SELECT value FROM bench.kv WHERE key='t_since'"))}

    @staticmethod
    def sql(s):
        return subprocess.run(["psql", "-X", "-qAt", "-v", "ON_ERROR_STOP=1", "-c", s],
                              check=True, capture_output=True, text=True).stdout.strip()

    def q(self, query, opts):
        s = "SELECT edn_q($q$" + query + "$q$, $j$" + json.dumps(opts) + "$j$::jsonb)::text"
        r = json.loads(self.sql(s))
        return r.get("results", r.get("result"))

    def pull(self, idx):
        return json.loads(self.sql(f"SELECT edn_pull('[*]', {self.meta['I0'] + idx})::text"))


class Embedded:
    def __init__(self, store, data, meta):
        self.store, self.data, self.meta = store, data, meta
        self.load = jload(store + ".load.json")

    def run(self, scen, arg=None):
        cmd = [os.environ["RUNNER"], "query", self.store, self.data, scen] + ([str(arg)] if arg is not None else [])
        return json.loads(subprocess.run(cmd, check=True, capture_output=True, text=True).stdout)


def one(b, scen, arg=None):
    """Run scenario `scen` once with a fixed argument; returns rows."""
    if isinstance(b, Embedded):
        if scen == "pull":
            r = b.run(scen, arg)
            return r[0][0] if r and r[0] else {}
        return b.run(scen, arg if not isinstance(arg, list) else ",".join(map(str, arg)))
    L = b.load
    if scen in ("point_lookup", "ref_traversal"):
        return b.q(Q[SCEN_Q[scen]], {"inputs": [email(arg)]})
    if scen == "as_of":
        return b.q(Q["q2"], {"inputs": [email(arg)], "asOf": L["t_mid"]})
    if scen == "aggregate":
        return b.q(Q["q3"], {})
    if scen == "predicate_scan":
        return b.q(Q["q4"], {"inputs": [arg]})
    if scen == "since":
        return b.q(Q["since"], {"since": L["t_since"]})
    if scen == "input_bindings":
        return b.q(Q["inputs"], {"inputs": [[email(u) for u in arg]]})
    if scen == "pull":
        return b.pull(arg)
    raise SystemExit(f"unknown scenario {scen}")


def issue_idx(title):
    return int(title.split(":")[0].split()[1])


def check(b, meta, post_write):
    """Every scenario's answer, once, against the generator's truth."""
    t, fails = meta["truth"], []

    def expect(name, ok, detail):
        print(f"check {name}: {'PASS' if ok else 'FAIL'}" + ("" if ok else f" {detail}"))
        if not ok:
            fails.append(name)

    probes = [u for u in meta["probe_users"] if str(u) in t["probe"]][:3]
    for u in (0, meta["n_users"] // 2, meta["n_users"] - 1):
        r = one(b, "point_lookup", u)
        expect(f"point_lookup[{u}]", len(r) == 1 and r[0][1] == f"User {u}", r)
    r = one(b, "aggregate")
    got = {x[0]: int(x[1]) for x in r}
    expect("aggregate.sum", sum(got.values()) == meta["n_issues"], got)
    if not post_write:
        expect("aggregate.by_state", got == t["final"], (got, t["final"]))
    for u in probes:
        rows = t["probe"][str(u)]
        r = one(b, "as_of", u)
        exp = sorted((i, s0) for i, s0, _ in rows)
        expect(f"as_of[{u}]", sorted((issue_idx(x[1]), x[2]) for x in r) == exp, (r[:3], exp[:3]))
        if not post_write:
            r = one(b, "ref_traversal", u)
            exp = sorted((i, s1) for i, _, s1 in rows)
            expect(f"ref_traversal[{u}]", sorted((issue_idx(x[1]), x[2]) for x in r) == exp, (r[:3], exp[:3]))
    users = [1, 2, meta["n_users"] - 1]
    r = one(b, "input_bindings", users)
    expect("input_bindings", sorted(x[1] for x in r) == sorted(f"User {u}" for u in users), r)
    if not post_write:
        r = one(b, "predicate_scan", 4)
        expect("predicate_scan", len(r) == t["open_p_ge"]["4"], (len(r), t["open_p_ge"]["4"]))
        r = one(b, "since")
        expect("since", len({x[0] for x in r}) == t["since_hist"], (len(r), t["since_hist"]))
        for idx, (title, _s0, s1, prio) in list(t["titles"].items())[:4]:
            p = one(b, "pull", int(idx))
            ok = p.get(":issue/title") == title and p.get(":issue/state") == s1 and p.get(":issue/priority") == prio
            expect(f"pull[{idx}]", ok, p)
    return fails


def mk(backend, data, target, meta):
    if backend == "pg":
        return Pg(meta)
    if backend == "embedded":
        return Embedded(target, data, meta)
    return Ext(backend, target, meta)


# --------------------------------------------------------------------------
# Extension load + bench (processes, one host connection each)
# --------------------------------------------------------------------------
def ext_load(backend, data, store, max_s=float("inf")):
    """Bulk load through edn_t (one call per tx), sequential: one writer.
    Exits 3 after max_s seconds, leaving a ceiling record in STORE.load.json."""
    meta = jload(f"{data}/meta.json")
    per_tx = meta["batch"] * meta["n_datoms_initial"] / meta["n_issues"]
    for suf in ("", "-wal", "-shm", ".entids", ".load.json"):
        if os.path.exists(store + suf):
            os.remove(store + suf)
    b = Ext(backend, store, meta)
    t0 = time.time()
    b.t(open(f"{data}/schema.edn").read())
    txs = 0
    for line in open(f"{data}/store/base.edn"):
        b.t(line)
        txs += 1
    import array
    entids = array.array("q", [0] * meta["n_issues"])
    t_mid = 0
    for f in sorted(glob.glob(f"{data}/store/issues-*.edn")):
        for line in open(f):
            el = time.time() - t0
            if el > max_s:
                info = {"ceiling": True, "elapsed_s": el, "txs": txs, "datoms_loaded_est": txs * per_tx,
                        "datoms_per_s": txs * per_tx / el, "n_datoms": meta["n_datoms"]}
                json.dump(info, open(store + ".load.json", "w"))
                print(json.dumps(info))
                sys.exit(3)
            r = json.loads(b.t(line))
            for k, v in r["tempids"].items():
                if k[0] == "i":
                    entids[int(k[1:])] = v
            t_mid = r["tx_id"]
            txs += 1
    t_initial = time.time() - t0
    hist = sorted(glob.glob(f"{data}/store/hist-*.edn"))
    t_since = last = t_mid
    import re
    sub = re.compile(r"@@(\d+)")
    for n, f in enumerate(hist):
        if n == len(hist) - 1:
            t_since = last
        for line in open(f):
            last = json.loads(b.t(sub.sub(lambda m: str(entids[int(m.group(1))]), line)))["tx_id"]
            txs += 1
    load_s = time.time() - t0
    with open(store + ".entids", "wb") as f:
        entids.tofile(f)
    info = {"t_mid": t_mid, "t_since": t_since, "load_s": load_s, "initial_load_s": t_initial,
            "txs": txs, "store_bytes": os.path.getsize(store), "n_datoms": meta["n_datoms"]}
    json.dump(info, open(store + ".load.json", "w"))
    print(json.dumps(info))


def _iter_arg(scen, meta, rng):
    if scen in ("point_lookup", "ref_traversal", "as_of"):
        return rng.randrange(meta["n_users"])
    if scen == "pull":
        return rng.randrange(meta["n_issues"])
    if scen == "predicate_scan":
        return 4
    if scen == "input_bindings":
        return [rng.randrange(meta["n_users"]) for _ in range(100)]
    return None


def _client(a):
    backend, data, store, scen, seed, min_s, max_s, min_n, probe_s, start_at = a
    meta = jload(f"{data}/meta.json")
    b = Ext(backend, store, meta)
    rng = random.Random(seed)
    errors, lat, i, first = 0, [], 0, 0.0
    mix = {"concurrency_sweep": READ_MIX, "read_mix2": READ_MIX2}.get(scen, [scen])
    w0 = time.time()
    for k in range(3):  # warm-up, discarded; bounded so a slow op cannot eat the budget
        s = mix[i % len(mix)]
        t = time.perf_counter()
        try:
            one(b, s, _iter_arg(s, meta, rng))
        except Exception as e:
            errors += 1
            print(f"error {s}: {e}", file=sys.stderr)
        if k == 0:
            first = (time.perf_counter() - t) * 1e3
        i += 1
        if first > probe_s * 1e3 or time.time() - w0 > max_s / 4:
            break
    while time.time() < start_at:
        time.sleep(0.001)
    t0 = time.time()
    if first > probe_s * 1e3:
        return lat, errors, 0.0, first
    while True:
        el = time.time() - t0
        if (len(lat) >= min_n and el >= min_s) or (el >= max_s and lat):
            break
        s = mix[i % len(mix)]
        i += 1
        arg = _iter_arg(s, meta, rng)
        t = time.perf_counter()
        try:
            one(b, s, arg)
            lat.append((time.perf_counter() - t) * 1e3)
        except Exception as e:
            errors += 1
            if errors < 3:
                print(f"error {s}: {e}", file=sys.stderr)
    return lat, errors, time.time() - t0, first


def ext_run(backend, data, store, scale, reps, clients_l, scens, min_s, max_s, min_n, probe_s):
    """Same contract as `mentat-scale bench`: rows per (scenario, clients, rep);
    a scenario whose first call exceeds probe_s gets one op=ceiling row and is
    dropped. One process per client, each with its own host connection."""
    meta = jload(f"{data}/meta.json")
    ceil = set()
    for rep in range(1, reps + 1):
        for scen in scens:
            for clients in clients_l:
                if scen in ceil:
                    continue
                start_at = time.time() + 2 + clients * 0.02
                per = -(-min_n // clients)
                args = [(backend, data, store, scen, 1 + c + 1000 * rep, min_s, max_s, per, probe_s, start_at)
                        for c in range(clients)]
                with mp.get_context("fork").Pool(clients) as pool:
                    res = pool.map(_client, args)
                first = max(r[3] for r in res)
                errs = sum(r[1] for r in res)
                if first > probe_s * 1e3:
                    print(row(scen, backend, scale, meta["n_datoms"], clients, "ceiling", [first],
                              first / 1e3, errs, rep), flush=True)
                    ceil.add(scen)
                    continue
                lat = [x for r in res for x in r[0]]
                print(row(scen, backend, scale, meta["n_datoms"], clients, "read", lat,
                          max(r[2] for r in res), errs, rep), flush=True)


def _writer(a):
    backend, data, store, secs = a
    meta = jload(f"{data}/meta.json")
    b = Ext(backend, store, meta)
    rng = random.Random(424242)
    states = ["open", "in-progress", "closed", "resolved", "reopened"]
    lat, err, t0 = [], 0, time.time()
    while time.time() - t0 < secs:
        e = b.entid(rng.randrange(meta["n_issues"]))
        if len(lat) % 2 == 0:
            tx = f"[[:db/add {e} :issue/state :state/{rng.choice(states)}]]"
        else:
            tx = f'[[:db/add {e} :issue/label (lookup-ref :label/name "label-{rng.randrange(meta["n_labels"])}")]]'
        t = time.perf_counter()
        try:
            b.t(tx)
            lat.append((time.perf_counter() - t) * 1e3)
        except Exception as ex:
            err += 1
            if err < 3:
                print(f"write error: {ex}", file=sys.stderr)
    return lat, err, time.time() - t0


def ext_mixed(backend, data, store, scale, reps, readers, secs):
    for rep in range(1, reps + 1):
        ext_mixed_once(backend, data, store, scale, rep, readers, secs)


def ext_mixed_once(backend, data, store, scale, rep, readers, secs):
    meta = jload(f"{data}/meta.json")
    ctx = mp.get_context("fork")
    with ctx.Pool(readers + 1) as pool:
        w = pool.apply_async(_writer, [(backend, data, store, secs)])
        rs = pool.map_async(_client, [(backend, data, store, "read_mix2", 7 + c, secs, secs, 1, 1e9, 0)
                                      for c in range(readers)])
        wl, we, ww = w.get()
        res = rs.get()
    lat = [x for r in res for x in r[0]]
    print(row("write_mixed", backend, scale, meta["n_datoms"], readers, "read", lat,
              max(r[2] for r in res), sum(r[1] for r in res), rep))
    print(row("write_mixed", backend, scale, meta["n_datoms"], 1, "write", wl, ww, we, rep), flush=True)



# --------------------------------------------------------------------------
# pgbench per-transaction logs -> one CSV row
# --------------------------------------------------------------------------
def pglog(scen, scale, n_datoms, clients, op, rep, wall, prefixes):
    lat, errors = [], 0
    for pre in prefixes:
        for f in glob.glob(pre + "*"):
            for line in open(f):
                p = line.split()
                # client_id transaction_no time script_no time_epoch time_us [schedule_lag]
                if len(p) >= 3:
                    if p[2] == "failed" or p[2] == "skipped":
                        errors += 1
                    else:
                        lat.append(int(p[2]) / 1e3)
    print(row(scen, "pg", scale, n_datoms, clients, op, lat, float(wall), errors, rep))


def pgwindows(out, read_prefix, write_prefix):
    """pgbench per-tx logs -> 10 s windows (same shape as the embedded runner's)."""
    rows = []
    for op, pre in (("read", read_prefix), ("write", write_prefix)):
        by = {}
        t0 = None
        recs = []
        for f in glob.glob(pre + "*"):
            for line in open(f):
                p = line.split()
                if len(p) >= 6 and p[2] not in ("failed", "skipped"):
                    recs.append((int(p[4]) + int(p[5]) / 1e6, int(p[2]) / 1e3))
        if not recs:
            continue
        t0 = min(r[0] for r in recs)
        for t, l in recs:
            by.setdefault(int((t - t0) // 10), []).append(l)
        for w in sorted(by):
            l = sorted(by[w])
            rows.append(f"{w * 10},{op},{len(l)},{pct(l, 50):.3f},{pct(l, 99):.3f},{len(l) / 10:.2f}")
    with open(out, "w") as f:
        f.write("t_s,op,count,p50_ms,p99_ms,throughput_ops_s\n" + "\n".join(rows) + "\n")


def medians(raw, out):
    groups = {}
    for line in open(raw):
        if line.startswith("scenario,"):
            continue
        p = line.strip().split(",")
        if len(p) < 13:
            continue
        groups.setdefault(tuple(p[:6]), []).append(p[6:13])
    with open(out, "w") as f:
        f.write(COLS + ",reps\n")
        for k, rs in groups.items():
            med = []
            for j in range(7):
                v = [float(r[j]) for r in rs]
                m = statistics.median(v)
                med.append(str(int(m)) if j in (0, 6) else f"{m:.3f}")
            f.write(",".join(list(k) + med + [str(len(rs))]) + "\n")


def main():
    a = sys.argv[1:]
    cmd = a[0] if a else ""
    if cmd == "check":
        meta = jload(f"{a[2]}/meta.json")
        fails = check(mk(a[1], a[2], a[3], meta), meta, "--post-write" in a)
        print(f"check {a[1]}: {'OK' if not fails else 'FAILED ' + ','.join(fails)}")
        sys.exit(1 if fails else 0)
    elif cmd == "load":
        ext_load(a[1], a[2], a[3], float(os.environ.get("LOAD_MAX_S", "inf")))
    elif cmd == "run":
        ext_run(a[1], a[2], a[3], a[4], int(a[5]), [int(x) for x in a[6].split(",")], a[7].split(","),
                float(a[8]), float(a[9]), int(a[10]), float(a[11]))
    elif cmd == "mixed":
        ext_mixed(a[1], a[2], a[3], a[4], int(a[5]), int(a[6]), float(a[7]))
    elif cmd == "pglog":
        pglog(a[1], a[2], a[3], a[4], a[5], a[6], a[7], a[8:])
    elif cmd == "pgwindows":
        pgwindows(a[1], a[2], a[3])
    elif cmd == "medians":
        medians(a[1], a[2])
    else:
        print(__doc__)
        sys.exit(2)


if __name__ == "__main__":
    main()
