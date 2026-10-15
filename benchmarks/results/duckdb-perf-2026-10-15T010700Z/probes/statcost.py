import duckdb, sys, time
c = duckdb.connect(sys.argv[1]); c.execute(f"SET search_path = '{sys.argv[2]}'")
E = "user7@example.com"
Q = {
 "point": ("SELECT DISTINCT d0.e, d1.v FROM datoms d0, datoms d1 WHERE d0.a = 65536 AND d0.v_s = ? AND d1.a = 65537 AND d0.e = d1.e", [E]),
 "ref_traversal": ("SELECT DISTINCT d1.e, d2.v, d3.v FROM datoms d0, datoms d1, datoms d2, datoms d3 WHERE d0.a = 65536 AND d0.v_s = ? AND d1.a = 65541 AND d1.v_i = d0.e AND d2.a = 65538 AND d2.e = d1.e AND d3.a = 65539 AND d3.e = d1.e", [E]),
 "aggregate": ("SELECT d.v, count(DISTINCT d.e) FROM datoms d WHERE d.a = 65539 GROUP BY d.v", []),
 "pull": ("SELECT a, e, v, value_type_tag FROM datoms WHERE e IN (78795) AND a IN (1, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 22, 37, 38, 39, 65536, 65537, 65538, 65539, 65540, 65541, 65542, 65543, 65544, 65545) ORDER BY a ASC, e ASC", []),
}
def t(sql, a, n=200):
    for _ in range(20): c.execute(sql, a).fetchall()
    t0 = time.perf_counter()
    for _ in range(n): c.execute(sql, a).fetchall()
    return (time.perf_counter() - t0) * 1e3 / n
for setting in ("", "statistics_propagation", ""):
    c.execute(f"SET disabled_optimizers = '{setting}'")
    print(f"{setting or 'default':24s}", "  ".join(f"{k} {t(*v):.3f}" for k, v in Q.items()))
