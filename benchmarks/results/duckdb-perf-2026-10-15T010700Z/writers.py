"""Write throughput: N threads (own cursor each, one database) doing
single-datom edn_t, all on ONE store or each on its own store.
Usage: writers.py EXT THREADS,.. SECS"""
import json, os, sys, tempfile, threading, time
import duckdb
ext, ns, secs = sys.argv[1], [int(x) for x in sys.argv[2].split(",")], float(sys.argv[3])
SCHEMA = "[{:db/ident :w/n :db/valueType :db.type/long :db/cardinality :db.cardinality/one}]"
for layout in ("one-store", "store-per-thread"):
    for n in ns:
        db = os.path.join(tempfile.mkdtemp(), "w.duckdb")
        con = duckdb.connect(db, config={"allow_unsigned_extensions": "true"}); con.load_extension(ext)
        stores = ["s0"] * n if layout == "one-store" else [f"s{k}" for k in range(n)]
        for s in set(stores): con.execute("SELECT edn_t(?, ?)", [s, SCHEMA]).fetchall()
        counts, errs, go = [0] * n, [0] * n, threading.Barrier(n + 1)
        def work(k):
            cur = con.cursor(); go.wait(); t0 = time.time(); i = 0
            while time.time() - t0 < secs:
                try:
                    cur.execute("SELECT edn_t(?, ?)", [stores[k], f'[{{:db/id "x" :w/n {i}}}]']).fetchall(); counts[k] += 1
                except Exception as e:
                    errs[k] += 1
                    if errs[k] < 2: print(f"error: {e}", file=sys.stderr)
                i += 1
        ts = [threading.Thread(target=work, args=(k,)) for k in range(n)]
        [t.start() for t in ts]; go.wait(); t0 = time.time(); [t.join() for t in ts]
        print(json.dumps({"layout": layout, "threads": n, "tx_s": round(sum(counts) / (time.time() - t0), 1), "errors": sum(errs)}), flush=True)
        con.close()
