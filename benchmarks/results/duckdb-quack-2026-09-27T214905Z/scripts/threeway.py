# Three-way table: (a) ext-base per-call open (1.9.0), (b) ext-cache in-process+cache, (c) duckdb-quack.
#   threeway.py EXT_BASE EXT_CACHE DUCKDB_QUACK [NOCACHE_TIMINGS_CSV]
# (a') = the optional 4th arg: per-call open on the O(1)-open build (MENTAT_STORE_CACHE=0).
import csv, os, sys
def path(d):
    return d if d.endswith(".csv") else os.path.join(d, "timings.csv")
def load(p):
    return {(r["scenario"], r["backend"], r["scale"], r["clients"], r["op"]): r for r in csv.DictReader(open(path(p)))}
a, b, c = (load(x) for x in sys.argv[1:4])
a2 = load(sys.argv[4]) if len(sys.argv) > 4 else {}
def g(t, k, m):
    r = t.get(k)
    if r is None:
        return "ceiling" if any(kk[:4] == k[:4] and kk[4] == "ceiling" for kk in t) else "—"
    v = float(r[m]); return f"{v:,.3f}" if v < 10 else f"{v:,.1f}"
print("## Single client, p50 ms\n")
print("| scale | scenario | (a) per-call open 1.9.0 | (a') per-call open, O(1) open | (b) in-process + cache | (c) Quack + cache | (c) − (b) ms |")
print("|---|---|---:|---:|---:|---:|---:|")
for sc in ("s", "m"):
    for s in ("point_lookup", "ref_traversal", "aggregate", "predicate_scan", "pull", "as_of", "since", "input_bindings"):
        ka, kc = (s, "duckdb", sc, "1", "read"), (s, "duckdb-quack", sc, "1", "read")
        d = ""
        if ka in b and kc in c:
            d = f"{float(c[kc]['p50_ms']) - float(b[ka]['p50_ms']):+.2f}"
        print(f"| {sc} | {s} | {g(a, ka, 'p50_ms')} | {g(a2, ka, 'p50_ms')} | {g(b, ka, 'p50_ms')} | {g(c, kc, 'p50_ms')} | {d} |")
print("\n## concurrency_sweep (client processes; mix of point_lookup, ref_traversal, pull)\n")
print("| scale | clients | (a) ops/s | (b) ops/s | (c) ops/s | (b) p50 / p99 ms | (c) p50 / p99 ms |")
print("|---|---:|---:|---:|---:|---:|---:|")
for sc in ("s", "m"):
    for n in ("1", "8", "32", "64", "128"):
        ka, kc = ("concurrency_sweep", "duckdb", sc, n, "read"), ("concurrency_sweep", "duckdb-quack", sc, n, "read")
        print(f"| {sc} | {n} | {g(a, ka, 'throughput_ops_s')} | {g(b, ka, 'throughput_ops_s')} | {g(c, kc, 'throughput_ops_s')} | "
              f"{g(b, ka, 'p50_ms')} / {g(b, ka, 'p99_ms')} | {g(c, kc, 'p50_ms')} / {g(c, kc, 'p99_ms')} |")
print("\n## write_mixed (8 readers + 1 writer, 60 s) and sustained (32 readers + 1 writer, 300 s, m)\n")
print("| scenario | scale | op | (a) ops/s | (b) ops/s | (c) ops/s | (b) p50 / p99 ms | (c) p50 / p99 ms |")
print("|---|---|---|---:|---:|---:|---:|---:|")
for scen, sc, n, op in (("write_mixed", "s", "8", "read"), ("write_mixed", "s", "1", "write"), ("write_mixed", "m", "8", "read"),
                        ("write_mixed", "m", "1", "write"), ("sustained", "m", "32", "read"), ("sustained", "m", "1", "write")):
    ka, kc = (scen, "duckdb", sc, n, op), (scen, "duckdb-quack", sc, n, op)
    bb = c if scen == "sustained" else b   # in-process sustained ran in the quack dir
    print(f"| {scen} | {sc} | {op} x{n} | {g(a, ka, 'throughput_ops_s')} | {g(bb, ka, 'throughput_ops_s')} | "
          f"{g(c, kc, 'throughput_ops_s')} | {g(bb, ka, 'p50_ms')} / {g(bb, ka, 'p99_ms')} | {g(c, kc, 'p50_ms')} / {g(c, kc, 'p99_ms')} |")
