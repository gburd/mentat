# mentat scale benchmark: final

## Environment (abridged; full detail in env.txt)

```
instance_type:   c7i.8xlarge
kernel:          6.12.103-129.197.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8488C
cpus:            32
sockets/numa:    1 1 
mem_total_gib:   61.8
hugepages:       HugePages_Total:=0 HugePages_Free:=0 Hugepagesize:=2048 
data_fs:         /dev/nvme0n1p1 xfs   150G   47G  104G  31% /
mentat_git:      final
duckdb:          1.5.5
postgres:        postgres (PostgreSQL) 16.15
```

## Dataset and PG size vs shared_buffers

```
s: 984803 datoms (1600 users, 130000 issues, 6492 history updates)
m: 9850258 datoms (16000 users, 1300000 issues, 64986 history updates)
```

## bulk_load

| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |
|---|---|---:|---:|---:|---:|---:|---:|---|

## point_lookup

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 33687 | 0.191 | 0.649 | 0.743 | 3.265 | 3,369 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 33550 | 0.191 | 0.650 | 0.743 | 3.236 | 3,355 | 0 | 3 |

## ref_traversal

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 10289 | 0.915 | 1.254 | 1.365 | 5.449 | 1,029 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 8515 | 1.131 | 1.459 | 1.554 | 6.157 | 851.5 | 0 | 3 |

## aggregate

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 1088 | 9.172 | 9.708 | 10.090 | 12.763 | 108.8 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 173 | 57.659 | 59.074 | 59.918 | 64.397 | 17.3 | 0 | 3 |

## predicate_scan

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 205 | 48.908 | 50.596 | 51.199 | 57.870 | 20.5 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 99 | 612.583 | 627.577 | 682.471 | 682.471 | 1.6 | 0 | 3 |

## pull

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 11592 | 0.845 | 0.963 | 1.027 | 4.137 | 1,159 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 11177 | 0.876 | 0.990 | 1.052 | 4.237 | 1,118 | 0 | 3 |

## as_of

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 5977 | 1.643 | 1.991 | 2.142 | 10.047 | 597.7 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 5021 | 1.962 | 2.339 | 2.541 | 10.781 | 502.1 | 0 | 3 |

## since

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 5243 | 1.890 | 2.004 | 2.097 | 4.442 | 524.3 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 3271 | 3.047 | 3.149 | 3.278 | 5.949 | 327.1 | 0 | 3 |

## input_bindings

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 11411 | 0.814 | 1.160 | 1.255 | 4.063 | 1,141 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 10115 | 0.922 | 1.254 | 1.346 | 4.770 | 1,012 | 0 | 3 |

## concurrency_sweep

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| pg | s | 984,803 | 1 | read | 12776 | 0.793 | 1.242 | 1.368 | 3.401 | 1,278 | 0 | 3 |
| pg | s | 984,803 | 8 | read | 102347 | 0.823 | 1.239 | 1.381 | 6.487 | 10,235 | 0 | 3 |
| pg | s | 984,803 | 32 | read | 325690 | 1.113 | 1.495 | 1.582 | 14.812 | 32,569 | 0 | 3 |
| pg | m | 9,850,258 | 1 | read | 12625 | 0.867 | 1.215 | 1.317 | 3.276 | 1,262 | 0 | 3 |
| pg | m | 9,850,258 | 8 | read | 90670 | 0.906 | 1.453 | 1.592 | 7.307 | 9,067 | 0 | 3 |
| pg | m | 9,850,258 | 32 | read | 307182 | 1.113 | 1.700 | 1.810 | 13.666 | 30,718 | 0 | 3 |

## Correctness checks

36 PASS, 0 FAIL, 0 SKIP; 2 backend check runs OK, 0 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)


