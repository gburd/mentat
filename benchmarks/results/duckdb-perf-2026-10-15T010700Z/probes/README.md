One-off DuckDB probes behind findings.md "Not done", run against a copy of
a loaded store (`python3 X.py DB SCHEMA`; cine.py takes no args):

- `layout.py`: the point-lookup join on `datoms` vs a plain and an `(a, e)`-sorted copy.
- `search.py`: the transact search join vs a variant pre-filtered by entity.
- `statcost.py`: query times with `statistics_propagation` disabled.
- `cine.py`: per-transaction temp tables recreated vs reused with DELETE.
