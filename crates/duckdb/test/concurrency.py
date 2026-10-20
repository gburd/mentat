#!/usr/bin/env python3
"""Concurrency test for the mentat DuckDB extension: client threads, each
with its own cursor on ONE database, run edn_t and edn_q at once (as the
extension's connection pool runs them).

Checks that no transaction is lost or fails, that every entid and tx id is
allocated once, that readers never see part of a transaction, and that a
second store is written in parallel.

Usage: python3 test/concurrency.py build/debug/mentat.duckdb_extension
(needs the duckdb Python package, 1.5.6 or later; CI runs 1.5.6 and 2.0)."""
import json
import os
import sys
import tempfile
import threading

import duckdb

EXT = sys.argv[1]
WRITERS, READERS, TXS, BATCH = 8, 4, 15, 4

db = os.path.join(tempfile.mkdtemp(), "conc.duckdb")
con = duckdb.connect(db, config={"allow_unsigned_extensions": "true"})
con.load_extension(EXT)
SCHEMA = ("[{:db/ident :w/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}"
          " {:db/ident :w/writer :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]")
for store in ("one", "two"):
    con.execute("SELECT edn_t(?, ?)", [store, SCHEMA]).fetchall()

errors, ids, done = [], {"one": [], "two": []}, threading.Event()
lock = threading.Lock()


def count(cur, store):
    r = cur.execute("SELECT * FROM edn_q(?, '[:find (count ?e) . :where [?e :w/n _]]', '{}')",
                    [store]).fetchall()
    return int(r[0][0]) if r else 0


def writer(w):
    cur = con.cursor()
    store = "one" if w % 4 else "two"
    try:
        for t in range(TXS):
            tx = " ".join(f'{{:db/id "e{i}" :w/n {t * BATCH + i} :w/writer {w}}}' for i in range(BATCH))
            r = json.loads(cur.execute("SELECT edn_t(?, ?)", [store, f"[{tx}]"]).fetchone()[0])
            with lock:
                ids[store].append(r["tx_id"])
                ids[store].extend(r["tempids"].values())
    except Exception as e:  # noqa: BLE001
        errors.append(f"writer {w}: {e}")


def reader(k):
    cur, last, n_reads = con.cursor(), 0, 0
    try:
        while not done.is_set():
            n = count(cur, "one")
            if n % BATCH or n < last:
                errors.append(f"reader {k}: saw {n} after {last}")
                return
            last, n_reads = n, n_reads + 1
        if not n_reads:
            errors.append(f"reader {k}: no reads")
    except Exception as e:  # noqa: BLE001
        errors.append(f"reader {k}: {e}")


rs = [threading.Thread(target=reader, args=(k,)) for k in range(READERS)]
ws = [threading.Thread(target=writer, args=(w,)) for w in range(WRITERS)]
for t in rs + ws:
    t.start()
for t in ws:
    t.join()
done.set()
for t in rs:
    t.join()

writers_on = {"one": [w for w in range(WRITERS) if w % 4], "two": [w for w in range(WRITERS) if not w % 4]}
for store, ws_ in writers_on.items():
    got, want = count(con, store), len(ws_) * TXS * BATCH
    if got != want:
        errors.append(f"store {store}: {got} entities, expected {want}")
for store, xs in ids.items():
    if len(set(xs)) != len(xs):
        errors.append(f"store {store}: an entid or tx id was allocated twice")
    if len(xs) != len(writers_on[store]) * TXS * (BATCH + 1):
        errors.append(f"store {store}: {len(xs)} ids reported")
print("\n".join(errors) or f"concurrency OK: {WRITERS} writers on 2 stores, {READERS} readers")
sys.exit(1 if errors else 0)
