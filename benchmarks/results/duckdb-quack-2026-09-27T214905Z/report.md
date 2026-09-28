# mentat scale benchmark: duckdb-quack

## Environment (abridged; full detail in env.txt)

```
instance_type:   c7i.8xlarge
kernel:          6.12.103-129.197.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8488C
cpus:            32
sockets/numa:    1 1 
mem_total_gib:   61.8
hugepages:       HugePages_Total:=0 HugePages_Free:=0 Hugepagesize:=2048 
data_fs:         /dev/nvme0n1p1 xfs   150G   14G  137G  10% /
mentat_git:      d34e0177 + duckdb-quack backend
duckdb:          1.5.5
postgres:        n/a
```

## Dataset and PG size vs shared_buffers

```
s: 984803 datoms (1600 users, 130000 issues, 6492 history updates)
m: duckdb-quack reads a copy of the embedded-built store (as ext-cache did)
```

## bulk_load

| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |
|---|---|---:|---:|---:|---:|---:|---:|---|
| duckdb-quack | s | 984,803 | 67.6 | 14,565 |  |  | 0.19 GiB |  |

## point_lookup

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 3367 | 2.889 | 3.649 | 4.939 | 6.875 | 336.7 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 2850 | 3.454 | 4.078 | 4.303 | 123.795 | 284.9 | 0 | 1 |

## ref_traversal

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 237 | 42.203 | 43.332 | 43.621 | 45.425 | 23.7 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 30 | 500.107 | 506.938 | 507.900 | 507.900 | 2.0 | 0 | 1 |

## aggregate

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 106 | 94.838 | 96.119 | 97.778 | 99.150 | 10.5 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 30 | 1,100 | 1,121 | 1,123 | 1,123 | 0.9 | 0 | 1 |

## predicate_scan

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 280 | 35.317 | 39.007 | 40.108 | 41.413 | 28.0 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 30 | 380.790 | 1,837 | 2,408 | 2,408 | 1.0 | 0 | 1 |

## pull

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 3585 | 2.753 | 3.486 | 3.659 | 4.856 | 358.5 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 1978 | 5.001 | 7.322 | 8.885 | 12.701 | 197.8 | 0 | 1 |

## as_of

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 113 | 89.270 | 90.072 | 90.795 | 91.744 | 11.2 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 30 | 1,103 | 1,120 | 1,135 | 1,135 | 0.9 | 0 | 1 |

## since

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 1247 | 7.962 | 8.525 | 11.379 | 12.633 | 124.6 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 191 | 52.415 | 53.935 | 57.554 | 58.838 | 19.0 | 0 | 1 |

## input_bindings

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 2829 | 3.516 | 4.070 | 4.217 | 5.246 | 282.9 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 1915 | 5.197 | 5.611 | 5.811 | 10.284 | 191.5 | 0 | 1 |

## write_mixed

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | write | 8389 | 6.780 | 8.811 | 17.284 | 30.712 | 139.8 | 0 | 1 |
| duckdb-quack | s | 984,803 | 8 | read | 16115 | 44.664 | 71.155 | 83.037 | 125.830 | 268.4 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | write | 7205 | 7.806 | 11.795 | 15.859 | 43.360 | 120.1 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 8 | read | 1092 | 555.242 | 1,490 | 2,042 | 2,258 | 17.7 | 0 | 1 |

## concurrency_sweep

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb-quack | s | 984,803 | 1 | read | 624 | 3.838 | 42.056 | 42.397 | 43.665 | 62.4 | 0 | 1 |
| duckdb-quack | s | 984,803 | 8 | read | 4601 | 3.124 | 52.983 | 64.564 | 66.521 | 458.8 | 0 | 1 |
| duckdb-quack | s | 984,803 | 32 | read | 11092 | 3.675 | 73.167 | 82.188 | 1,147 | 1,030 | 0 | 1 |
| duckdb-quack | s | 984,803 | 64 | read | 11142 | 6.407 | 90.591 | 1,108 | 2,537 | 1,010 | 0 | 1 |
| duckdb-quack | s | 984,803 | 128 | read | 11333 | 9.435 | 1,056 | 1,547 | 5,647 | 1,014 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | read | 59 | 4.290 | 505.275 | 507.038 | 507.038 | 5.8 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 8 | read | 470 | 4.733 | 526.820 | 547.811 | 562.513 | 44.8 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 32 | read | 1117 | 9.364 | 857.612 | 903.347 | 1,070 | 104.8 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 64 | read | 1169 | 26.533 | 1,922 | 2,084 | 2,945 | 104.8 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 128 | read | 1195 | 49.104 | 3,505 | 3,684 | 4,849 | 97.2 | 0 | 1 |

## sustained

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | m | 9,850,258 | 1 | write | 40154 | 4.787 | 17.282 | 29.491 | 272.002 | 133.8 | 0 | 1 |
| duckdb | m | 9,850,258 | 32 | read | 11134 | 997.484 | 2,944 | 4,264 | 7,481 | 37.0 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 1 | write | 15978 | 15.946 | 40.401 | 56.994 | 458.566 | 53.3 | 0 | 1 |
| duckdb-quack | m | 9,850,258 | 32 | read | 16166 | 920.525 | 1,407 | 1,694 | 3,891 | 53.7 | 0 | 1 |

## Correctness checks

52 PASS, 0 FAIL, 0 SKIP; 4 backend check runs OK, 0 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)


