import duckdb, sys, time
c = duckdb.connect(sys.argv[1]); c.execute(f"SET search_path = '{sys.argv[2]}'")
def t(name, sql, args=()):
    for _ in range(20): c.execute(sql, args).fetchall()
    n = 300; t0 = time.perf_counter()
    for _ in range(n): c.execute(sql, args).fetchall()
    print(f"{(time.perf_counter()-t0)*1e3/n:7.3f} ms  {name}")
E = "user7@example.com"
J = "SELECT DISTINCT d0.e, d1.v FROM {t} d0, {t} d1 WHERE d0.a = 65536 AND d0.v_s = ? AND d1.a = 65537 AND d0.e = d1.e"
c.execute("CREATE OR REPLACE TABLE copy_plain AS SELECT * FROM datoms")
c.execute("CREATE OR REPLACE TABLE copy_sorted AS SELECT * FROM datoms ORDER BY a, e")
c.execute("CREATE OR REPLACE TABLE copy_sorted_idx AS SELECT * FROM datoms ORDER BY a, e"); c.execute("CREATE INDEX csi_ea ON copy_sorted_idx (e, a)"); c.execute("CREATE INDEX csi_a ON copy_sorted_idx (a)")
c.execute("CHECKPOINT")
print("deleted rows in datoms:", c.execute("SELECT max(rowid)+1 - count(*) FROM datoms").fetchone()[0])
for tb in ("datoms", "copy_plain", "copy_sorted", "copy_sorted_idx"):
    t(f"{tb:16s} count a=",  f"SELECT count(*) FROM {tb} WHERE a = 65536")
    t(f"{tb:16s} a+v_s",  f"SELECT e FROM {tb} WHERE a = 65536 AND v_s = ?", [E])
    t(f"{tb:16s} join", J.format(t=tb), [E])
