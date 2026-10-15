import duckdb, sys, time, random
db, ext, store, n, wl = sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]), sys.argv[5]
c = duckdb.connect(db, config={"allow_unsigned_extensions": "true"}); c.load_extension(ext)
q1 = "[:find ?e ?name :in $ ?email :where [?e :user/email ?email] [?e :user/name ?name]]"
rng = random.Random(1)
def one():
    if wl == "point":
        c.execute("SELECT * FROM edn_q(?, ?, ?)", [store, q1, '{"inputs": ["user%d@example.com"]}' % rng.randrange(200)]).fetchall()
    elif wl == "tx":
        c.execute("SELECT edn_t(?, ?)", [store, "[[:db/add %d :issue/state :state/open]]" % ents[rng.randrange(len(ents))]]).fetchall()
    else:
        c.execute("SELECT edn_pull(?, '[*]', ?)", [store, ents[rng.randrange(len(ents))]]).fetchall()
import array
ents = array.array("q"); ents.frombytes(open(db[:-len(".duckdb")] + ".entids", "rb").read())
for _ in range(30): one()
t0 = time.perf_counter()
for _ in range(n): one()
print(f"{wl}: {(time.perf_counter() - t0) * 1e3 / n:.3f} ms/call", file=sys.stderr)
