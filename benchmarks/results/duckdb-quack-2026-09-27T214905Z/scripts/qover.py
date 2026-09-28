# Quack protocol cost with mentat out of the picture: the same SQL run locally
# and through quack_query on a local server. Rows are (BIGINT, VARCHAR ~12 B).
import duckdb, subprocess, sys, time
U, T = sys.argv[1], sys.argv[2]
c = duckdb.connect(); c.execute("LOAD quack")
def opens():
    o = subprocess.run("nstat -az TcpActiveOpens", shell=True, capture_output=True, text=True).stdout
    return int(o.splitlines()[1].split()[1])
def bench(n, remote, k):
    sql = f"SELECT i, 'User ' || i AS s FROM range({n}) t(i)"
    f = (lambda: c.execute("SELECT * FROM quack_query(?, ?, token => ?, disable_ssl => true)", [U, sql, T]).fetchall()) if remote \
        else (lambda: c.execute(sql).fetchall())
    f(); a = opens(); ts = []
    for _ in range(k):
        t = time.perf_counter(); f(); ts.append((time.perf_counter() - t) * 1e3)
    ts.sort()
    return ts[len(ts) // 2], (opens() - a) / k
print("| rows | local p50 ms | Quack p50 ms | Quack − local ms | TCP connects / call |")
print("|---:|---:|---:|---:|---:|")
for n, k in ((1, 300), (100, 300), (1000, 200), (10000, 100), (100000, 30), (1000000, 10)):
    l, _ = bench(n, False, k); r, o = bench(n, True, k)
    print(f"| {n:,} | {l:.3f} | {r:.3f} | {r - l:+.3f} | {o:.1f} |")
