import duckdb, sys, time
c = duckdb.connect(sys.argv[1]); c.execute(f"SET search_path = '{sys.argv[2]}'")
V = "UNION(i BIGINT, d DOUBLE, s VARCHAR, b BLOB)"
cols = f"e0 BIGINT NOT NULL, a0 BIGINT NOT NULL, v0 {V} NOT NULL, value_type_tag0 SMALLINT NOT NULL, added0 BOOLEAN NOT NULL, flags0 SMALLINT NOT NULL"
for t in ("exact_searches", "inexact_searches"): c.execute(f"CREATE TEMP TABLE {t} ({cols})")
c.execute(f"CREATE TEMP TABLE search_results ({cols}, search_type VARCHAR NOT NULL, rid BIGINT, v {V})")
e = c.execute("SELECT max(e) FROM datoms WHERE a = 65540").fetchone()[0]
c.execute(f"INSERT INTO temp.inexact_searches VALUES ({e}, 65540, 's'::{V}, 10, true, 0)")
cur = """INSERT INTO temp.search_results
 SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':db.cardinality/many', d.rowid, d.v
 FROM temp.exact_searches AS t LEFT JOIN datoms AS d ON t.e0 = d.e AND t.a0 = d.a AND t.value_type_tag0 = d.value_type_tag AND t.v0 = d.v
 UNION ALL
 SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':db.cardinality/one', d.rowid, d.v
 FROM temp.inexact_searches AS t LEFT JOIN datoms AS d ON t.e0 = d.e AND t.a0 = d.a"""
new = """INSERT INTO temp.search_results
 SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':db.cardinality/many', d.rowid, d.v
 FROM temp.exact_searches AS t LEFT JOIN (SELECT rowid, * FROM datoms WHERE e IN (SELECT e0 FROM temp.exact_searches)) AS d
 ON t.e0 = d.e AND t.a0 = d.a AND t.value_type_tag0 = d.value_type_tag AND t.v0 = d.v
 UNION ALL
 SELECT t.e0, t.a0, t.v0, t.value_type_tag0, t.added0, t.flags0, ':db.cardinality/one', d.rowid, d.v
 FROM temp.inexact_searches AS t LEFT JOIN (SELECT rowid, * FROM datoms WHERE e IN (SELECT e0 FROM temp.inexact_searches)) AS d
 ON t.e0 = d.e AND t.a0 = d.a"""
def run(sql, n=200):
    tot = 0; rows = None
    for i in range(n + 10):
        c.execute("BEGIN"); t = time.perf_counter(); c.execute(sql); dt = time.perf_counter() - t
        rows = c.execute("SELECT e0, a0, rid, v FROM temp.search_results ORDER BY ALL").fetchall()
        c.execute("ROLLBACK")
        if i >= 10: tot += dt
    return tot * 1e3 / n, rows
a, ra = run(cur); b, rb = run(new)
print(f"current {a:.3f} ms, rewritten {b:.3f} ms, same rows: {ra == rb} {ra}")
