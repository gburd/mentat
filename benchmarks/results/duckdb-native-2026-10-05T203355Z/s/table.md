| scenario (p50 ms, 1 client) | 1.10.3: datoms in a SQLite file | 1.11: datoms in DuckDB | new / old |
|---|---:|---:|---:|
| point_lookup | 0.60 | 3.23 | 5.40 |
| ref_traversal | 22.39 | 10.37 | 0.46 |
| aggregate | 30.15 | 5.53 | 0.18 |
| predicate_scan | 28.89 | 40.43 | 1.40 |
| pull | 0.56 | 3.83 | 6.85 |
| as_of | 88.57 | 25.46 | 0.29 |
| since | 5.25 | 2.73 | 0.52 |
| input_bindings | 1.12 | 8.78 | 7.82 |
| bulk load (s) | 177.4 | 62.7 | 0.35 |
| single-datom transact (tx/s) | 182 | 54 | 0.29 |
| store size (MB) | 213 | 157 | 0.74 |
