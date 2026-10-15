# duckdb/duckdb#26828 and mentat's DuckDB secondary indexes (2026-10-15)

EC2 c6i (kiwi, Debian 12), DuckDB CLI v1.5.6, mentat_duckdb debug build.

| Script | Before fix | After fix |
|---|---|---|
| `pure-duckdb-repro.sh` (no mentat): index on `(a)`, WAL-only rows, plain reopen | `a = X` -> 0 (scan: 13000); with an index on `(e,a)`, `a` is fine but every DELETE fails FATAL | n/a (upstream) |
| `mentat-repro.sh`: store created + loaded in one session, plain reopen, `[?e :p/name _]` | 0 | 13000 |
| `repair-1.14.0-store.sh`: a store made and corrupted by the released 1.14.0 extension, then opened by the fixed one | 0 | 13000, indexes dropped, retractEntity OK |
| `index-vs-scan-12M.sh`: 12M rows, `e =` / `e = AND a =` lookups | `SEQ_SCAN` in every plan, with or without indexes; same times (~1-2 ms warm) | — |

Insert cost (100k rows into 1.2M): 0.162 s with the 3 indexes, 0.019 s
without (8.5x).
