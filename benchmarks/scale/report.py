#!/usr/bin/env python3
"""Render summary tables for a scale result dir: report.py OUT_DIR > summary.md

The tables are generated. Findings are hand-written: if OUT_DIR/findings.md
exists, it is included at the top.
"""
import csv
import os
import sys


def rd(p):
    if not os.path.exists(p):
        return []
    with open(p) as f:
        return list(csv.DictReader(f))


def fmt(v, nd=2):
    try:
        x = float(v)
    except (TypeError, ValueError):
        return v or ""
    if x >= 1000:
        return f"{x:,.0f}"
    return f"{x:.{nd}f}"


def main():
    out = sys.argv[1]
    t = rd(f"{out}/timings.csv")
    loads = rd(f"{out}/loads.csv")
    print(f"# mentat scale benchmark: {os.path.basename(os.path.abspath(out))}\n")
    if os.path.exists(f"{out}/findings.md"):
        print(open(f"{out}/findings.md").read().rstrip() + "\n")
    env = open(f"{out}/env.txt").read().splitlines() if os.path.exists(f"{out}/env.txt") else []
    print("## Environment (abridged; full detail in env.txt)\n\n```")
    for line in env:
        if line.split(":")[0].strip() in ("instance_type", "cpu_model", "cpus", "sockets/numa", "mem_total_gib",
                                          "kernel", "mentat_git", "postgres", "duckdb", "hugepages", "data_fs"):
            print(line)
    print("```\n")
    if os.path.exists(f"{out}/sizes.txt"):
        print("## Dataset and PG size vs shared_buffers\n\n```\n" + open(f"{out}/sizes.txt").read().rstrip() + "\n```\n")
    print("## bulk_load\n")
    print("| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |")
    print("|---|---|---:|---:|---:|---:|---:|---:|---|")
    for r in loads:
        size = f"{int(r['store_bytes']) / 2**30:.2f} GiB" if r.get("store_bytes") else ""
        note = r["note"] or ("CEILING" if r["ceiling"] == "1" else "")
        print(f"| {r['backend']} | {r['scale']} | {int(r['n_datoms']):,} | {fmt(r['load_s'], 1)} | {fmt(r['datoms_per_s'], 0)} | "
              f"{fmt(r['analyze_s'], 1)} | {fmt(r['vacuum_s'], 1)} | {size} | {note} |")
    print()
    scen_order = ["point_lookup", "ref_traversal", "aggregate", "predicate_scan", "pull", "as_of", "since",
                  "input_bindings", "write_mixed", "concurrency_sweep", "cold_vs_warm", "sustained"]
    scens = sorted({r["scenario"] for r in t}, key=lambda s: scen_order.index(s) if s in scen_order else 99)
    for s in scens:
        rows = [r for r in t if r["scenario"] == s]
        print(f"## {s}\n")
        print("| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |")
        print("|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|")
        order = {"xs": 0, "s": 1, "m": 2, "l": 3, "xl": 4}
        rows.sort(key=lambda r: (r["backend"], order.get(r["scale"], 9), int(r["clients"]), r["op"]))
        for r in rows:
            op = "**CEILING** (first call)" if r["op"] == "ceiling" else r["op"]
            print(f"| {r['backend']} | {r['scale']} | {int(r['n_datoms']):,} | {r['clients']} | {op} | {r['count']} | "
                  f"{fmt(r['p50_ms'], 3)} | {fmt(r['p95_ms'], 3)} | {fmt(r['p99_ms'], 3)} | {fmt(r['max_ms'], 3)} | "
                  f"{fmt(r['throughput_ops_s'], 1)} | {r['errors']} | {r.get('reps', '')} |")
        print()
    if os.path.exists(f"{out}/checks.txt"):
        c = open(f"{out}/checks.txt").read()
        print(f"## Correctness checks\n\n{c.count(': PASS')} PASS, {c.count(': FAIL')} FAIL, "
              f"{c.count(': SKIP')} SKIP; {c.count(' OK')} backend check runs OK, "
              f"{c.count(': FAILED')} FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)\n")
        for line in c.splitlines():
            if line.startswith("== ") or ": FAIL" in line or "HARNESS" in line or ": SKIP (backend" in line:
                print(f"- `{line[:220]}`")
        print()


if __name__ == "__main__":
    main()
