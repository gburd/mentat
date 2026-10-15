"""Concurrency: N Python threads, each its own cursor (a DuckDB connection) on
ONE database, calling the extension. Usage: conc.py DATA STORE THREADS,.. SECS WORKLOAD
(WORKLOAD: point = point_lookup only; mix = point_lookup/ref_traversal/pull)."""
import json, os, random, sys, threading, time
sys.path.insert(0, os.path.expanduser("~/mentat/benchmarks/scale"))
import bench

data, store, threads_l, secs, wl = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), sys.argv[5]
meta = bench.jload(f"{data}/meta.json")
mix = ["point_lookup"] if wl == "point" else bench.READ_MIX
bench.Ext("duckdb", store, meta)  # opens the database + loads the extension once
for n in map(int, threads_l.split(",")):
    lats, errs, go = [[] for _ in range(n)], [0] * n, threading.Barrier(n + 1)

    def work(k):
        b = bench.Ext("duckdb", store, meta)  # own cursor
        rng = random.Random(k + 1)
        for s in mix:
            bench.one(b, s, bench._iter_arg(s, meta, rng))
        go.wait()
        t0, i = time.time(), 0
        while time.time() - t0 < secs:
            s = mix[i % len(mix)]; i += 1
            arg = bench._iter_arg(s, meta, rng)
            t = time.perf_counter()
            try:
                bench.one(b, s, arg)
                lats[k].append((time.perf_counter() - t) * 1e3)
            except Exception as e:
                errs[k] += 1
                if errs[k] < 3:
                    print(f"error {s}: {e}", file=sys.stderr)

    ts = [threading.Thread(target=work, args=(k,)) for k in range(n)]
    [t.start() for t in ts]
    go.wait(); t0 = time.time()
    [t.join() for t in ts]
    wall = time.time() - t0
    lat = sorted(x for l in lats for x in l)
    print(json.dumps({"workload": wl, "threads": n, "ops": len(lat), "ops_s": round(len(lat) / wall, 1),
                      "p50_ms": round(bench.pct(lat, 50), 3), "p95_ms": round(bench.pct(lat, 95), 3),
                      "errors": sum(errs)}), flush=True)
