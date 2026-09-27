#!/usr/bin/env python3
"""Diff two scale-suite result dirs (or timings.csv files) and flag regressions.

    compare.py OLD NEW [--threshold 10] [--metric p50_ms|p99_ms|throughput_ops_s]

Rows are matched on (scenario, backend, scale, clients, op). A regression is a
latency metric that rose by more than threshold%, or a throughput that fell by
more than threshold%. It also flags a scenario that became a ceiling, errors
that appeared, and rows that exist in OLD but are missing from NEW. Exits 1 if
anything regressed.
"""
import csv
import os
import sys


def load(p):
    if os.path.isdir(p):
        p = os.path.join(p, "timings.csv")
    with open(p) as f:
        return {(r["scenario"], r["backend"], r["scale"], r["clients"], r["op"]): r for r in csv.DictReader(f)}


def main():
    a = sys.argv[1:]
    thr, metrics = 10.0, ["p50_ms", "p99_ms", "throughput_ops_s"]
    if "--threshold" in a:
        i = a.index("--threshold"); thr = float(a[i + 1]); del a[i:i + 2]
    if "--metric" in a:
        i = a.index("--metric"); metrics = [a[i + 1]]; del a[i:i + 2]
    if len(a) != 2:
        print(__doc__); sys.exit(2)
    old, new = load(a[0]), load(a[1])
    bad = 0
    print(f"{'key':70} {'metric':17} {'old':>11} {'new':>11} {'delta':>8}")
    for k in sorted(old):
        name = "/".join(k)
        if k not in new:
            ceil = next((n for n in new if n[:4] == k[:4] and n[4] == "ceiling"), None)
            print(f"{name:70} {'MISSING' if not ceil else 'NOW CEILING':17}"); bad += 1
            continue
        o, n = old[k], new[k]
        if int(n.get("errors") or 0) > int(o.get("errors") or 0):
            print(f"{name:70} {'errors':17} {o['errors']:>11} {n['errors']:>11}  REGRESSION"); bad += 1
        for m in metrics:
            ov, nv = float(o[m]), float(n[m])
            if ov == 0:
                continue
            d = (nv - ov) / ov * 100
            worse = d < -thr if m.startswith("throughput") else d > thr
            better = d > thr if m.startswith("throughput") else d < -thr
            flag = "REGRESSION" if worse else ("improved" if better else "")
            if flag:
                print(f"{name:70} {m:17} {ov:11.3f} {nv:11.3f} {d:+7.1f}%  {flag}")
            bad += worse
    for k in sorted(set(new) - set(old)):
        print(f"{'/'.join(k):70} {'NEW':17}")
    print(f"\n{bad} regression(s) at threshold {thr}%")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
