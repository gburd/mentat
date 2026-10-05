| scenario (p50 ms, 1 client) | 1.10.3: datoms in a SQLite file | 1.11: datoms in DuckDB | new / old |
|---|---:|---:|---:|
| point_lookup | 0.58 | 2.79 | 4.78 |
| ref_traversal | 2.62 | 6.80 | 2.60 |
| aggregate | 3.02 | 3.68 | 1.22 |
| predicate_scan | 3.47 | 9.97 | 2.87 |
| pull | 0.55 | 2.98 | 5.43 |
| as_of | 8.77 | 19.98 | 2.28 |
| since | 1.26 | 2.34 | 1.85 |
| input_bindings | 0.92 | 4.90 | 5.33 |
| bulk load (s) | 2.3 | 5.7 | 2.51 |
| single-datom transact (tx/s) | 186 | 66 | 0.36 |
| store size (MB) | 21 | 16 | 0.74 |
