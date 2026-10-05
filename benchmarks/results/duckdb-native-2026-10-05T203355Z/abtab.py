import csv, statistics, os, sys
sc = sys.argv[1]
def load(v):
    d = {}
    for r in csv.reader(open(os.path.expanduser(f"~/ab-{sc}/bench-{v}.csv"))):
        if len(r) > 8 and r[5] == "read": d.setdefault(r[0], []).append(float(r[7]))
    return {k: statistics.median(x) for k, x in d.items()}
a, b = load("v1103"), load("mentat")
print(f"{'scenario':16s} {'1.10.3 SQLite ms':>17s} {'new DuckDB ms':>14s} {'ratio':>7s}")
for k in a:
    nb = b.get(k, float("nan"))
    print(f"{k:16s} {a[k]:17.3f} {nb:14.3f} {nb/a[k]:7.2f}")
