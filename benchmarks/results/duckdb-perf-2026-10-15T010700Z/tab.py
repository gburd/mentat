"""Tables for findings.md from the raw results: tab.py RUN [RUN ...]"""
import csv, json, os, statistics, sys
H = os.path.dirname(os.path.abspath(__file__))
runs = sys.argv[1:]
SC = ["point_lookup", "pull", "input_bindings", "ref_traversal", "aggregate", "as_of", "since"]
def p50s(run, sc):
    d = {}
    for r in csv.reader(open(f"{H}/{run}/{sc}/bench.csv")):
        if len(r) > 8 and r[5] == "read": d.setdefault(r[0], []).append(float(r[7]))
    return {k: statistics.median(v) for k, v in d.items()}
for sc in ("xs", "s"):
    print(f"\n### {sc}: single client, p50 ms (median of 3 reps)\n")
    print("| scenario | " + " | ".join(runs) + " |"); print("|---|" + "---:|" * len(runs))
    P = [p50s(r, sc) for r in runs]
    for k in SC: print(f"| {k} | " + " | ".join(f"{p[k]:.2f}" for p in P) + " |")
    W = [json.load(open(f"{H}/{r}/{sc}/write.txt")) for r in runs]
    print("| single-datom transact (tx/s) | " + " | ".join(f"{w['tx_per_s']:.0f}" for w in W) + " |")
    print(f"\n### {sc}: throughput, ops/s at 1 / 8 / 32 client threads\n")
    print("| workload | " + " | ".join(runs) + " |"); print("|---|" + "---:|" * len(runs))
    C = [[json.loads(l) for l in open(f"{H}/{r}/{sc}/conc.jsonl")] for r in runs]
    for wl in ("point", "mix"):
        cells = []
        for c in C:
            by = {x["threads"]: x for x in c if x["workload"] == wl}
            cells.append(" / ".join(f"{by[n]['ops_s']:.0f}" for n in (1, 8, 32)))
        print(f"| {wl} | " + " | ".join(cells) + " |")
    print("| point p50 ms at 32 threads | " + " | ".join(f"{[x for x in c if x['workload']=='point' and x['threads']==32][0]['p50_ms']:.1f}" for c in C) + " |")
