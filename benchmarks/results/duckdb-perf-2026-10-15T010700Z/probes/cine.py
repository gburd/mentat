import duckdb, time
c = duckdb.connect()
cols = "e0 BIGINT NOT NULL, a0 BIGINT NOT NULL, v0 UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB) NOT NULL"
c.execute(f"CREATE TEMP TABLE IF NOT EXISTS x ({cols})")
def run(setup, n=500):
    st = c.cursor()  # same connection? no: cursor = new connection; use c
    t0 = time.perf_counter()
    for i in range(n):
        c.execute("BEGIN"); setup(); c.execute("INSERT INTO temp.x VALUES (?, ?, ?::UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB))", [i, 1, 5]); c.execute("COMMIT")
    return (time.perf_counter() - t0) * 1e3 / n
print("DELETE only           %.3f" % run(lambda: c.execute("DELETE FROM temp.x")))
print("CINE + DELETE         %.3f" % run(lambda: (c.execute(f"CREATE TEMP TABLE IF NOT EXISTS x ({cols})"), c.execute("DELETE FROM temp.x"))))
print("DROP + CREATE         %.3f" % run(lambda: (c.execute("DROP TABLE IF EXISTS temp.x"), c.execute(f"CREATE TEMP TABLE x ({cols})"))))
