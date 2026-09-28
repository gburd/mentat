# Per scenario: one call over Quack; TCP connections opened and listen overflows it caused.
import os, sys, subprocess, time, json, random
sys.path.insert(0, os.path.expanduser("~/mx/benchmarks/scale"))
import bench
def ns():
    o = subprocess.run("nstat -az TcpActiveOpens TcpExtListenOverflows", shell=True, capture_output=True, text=True).stdout
    return {l.split()[0]: int(l.split()[1]) for l in o.splitlines()[1:]}
data, store = sys.argv[1], sys.argv[2]
meta = bench.jload(f"{data}/meta.json")
b = bench.Ext("duckdb-quack", store, meta)
b.load = bench.jload(store + ".load.json")
for s in ["point_lookup", "pull", "input_bindings", "since", "ref_traversal", "predicate_scan", "aggregate", "as_of"]:
    arg = bench._iter_arg(s, meta, random.Random(5))
    bench.one(b, s, arg)  # warm
    a = ns(); t = time.perf_counter(); r = bench.one(b, s, arg); ms = (time.perf_counter() - t) * 1e3; z = ns()
    print(f"{s:15s} rows={len(r) if hasattr(r,'__len__') else '-':>6} {ms:8.1f} ms  conns={z['TcpActiveOpens']-a['TcpActiveOpens']:3d}  overflows={z['TcpExtListenOverflows']-a['TcpExtListenOverflows']}")
