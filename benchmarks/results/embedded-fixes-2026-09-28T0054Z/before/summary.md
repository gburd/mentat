# mentat scale benchmark: before

## Environment (abridged; full detail in env.txt)

```
instance_type:   c7i.8xlarge
kernel:          6.12.103-129.197.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8488C
cpus:            24
sockets/numa:    1 1 
mem_total_gib:   61.8
hugepages:       HugePages_Total:=0 HugePages_Free:=0 Hugepagesize:=2048 
data_fs:         /dev/nvme0n1p1 xfs   150G  7.6G  143G   6% /
mentat_git:      72ee3765
duckdb:          n/a
postgres:        n/a
```

## Dataset and PG size vs shared_buffers

```
s: 984803 datoms (1600 users, 130000 issues, 6492 history updates)
m: 9850258 datoms (16000 users, 1300000 issues, 64986 history updates)
```

## bulk_load

| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |
|---|---|---:|---:|---:|---:|---:|---:|---|
| embedded | s | 984,803 | 66.1 | 14,903 |  |  | 0.15 GiB |  |
| embedded | m | 9,850,258 | 1,482 | 6,646 |  |  | 1.57 GiB |  |

## point_lookup

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 139908 | 0.070 | 0.080 | 0.084 | 0.141 | 13,991 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 19058 | 0.515 | 0.572 | 0.586 | 0.684 | 1,906 | 0 | 3 |

## ref_traversal

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 234 | 42.842 | 43.410 | 43.561 | 43.768 | 23.3 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 556.256 | 562.578 | 562.993 | 562.993 | 1.8 | 0 | 3 |

## aggregate

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 99 | 101.769 | 102.549 | 104.314 | 104.314 | 9.8 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 1,352 | 1,370 | 1,373 | 1,373 | 0.8 | 0 | 3 |

## predicate_scan

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 391 | 25.595 | 25.829 | 26.009 | 26.451 | 39.0 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 36 | 279.930 | 281.160 | 281.895 | 281.895 | 3.6 | 0 | 3 |

## pull

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 236041 | 0.042 | 0.047 | 0.057 | 0.161 | 23,604 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 204778 | 0.048 | 0.055 | 0.065 | 0.173 | 20,478 | 0 | 3 |

## as_of

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 3 | 20,769 | 23,047 | 23,047 | 23,047 | 0.1 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | **CEILING** (first call) | 1 | 20,000 | 20,000 | 20,000 | 20,000 | 0.1 | 1 | 1 |

## since

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 207 | 48.442 | 48.657 | 49.328 | 49.654 | 20.6 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 484.891 | 486.573 | 487.344 | 487.344 | 2.1 | 0 | 3 |

## input_bindings

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 25965 | 0.378 | 0.423 | 0.432 | 0.507 | 2,596 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 4918 | 2.026 | 2.111 | 2.153 | 2.252 | 491.7 | 0 | 3 |

## write_mixed

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | write | 15498 | 3.867 | 4.397 | 4.676 | 6.586 | 258.3 | 0 | 3 |
| embedded | s | 984,803 | 8 | read | 590 | 2.139 | 1,697 | 1,727 | 1,763 | 9.7 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | write | 14219 | 4.170 | 5.007 | 5.586 | 22.136 | 237.0 | 0 | 3 |
| embedded | m | 9,850,258 | 8 | read | 48 | 4.225 | 23,940 | 24,083 | 24,083 | 0.7 | 0 | 3 |

## concurrency_sweep

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 295 | 0.214 | 108.372 | 120.420 | 127.766 | 29.2 | 0 | 1 |
| embedded | s | 984,803 | 8 | read | 83 | 0.851 | 3,062 | 3,101 | 3,101 | 6.9 | 0 | 1 |
| embedded | s | 984,803 | 32 | read | 101 | 5.952 | 14,947 | 14,957 | 14,966 | 6.8 | 0 | 1 |
| embedded | s | 984,803 | 64 | **CEILING** (first call) | 1 | 20,002 | 20,002 | 20,002 | 20,002 | 0.1 | 20 | 1 |
| embedded | m | 9,850,258 | 1 | read | 30 | 0.631 | 1,938 | 1,946 | 1,946 | 1.6 | 0 | 1 |
| embedded | m | 9,850,258 | 8 | **CEILING** (first call) | 1 | 20,001 | 20,001 | 20,001 | 20,001 | 0.1 | 3 | 1 |

## cold_vs_warm

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | cold_aggregate | 1 | 123.480 | 123.480 | 123.480 | 123.480 | 8.1 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_point_lookup | 1 | 24.041 | 24.041 | 24.041 | 24.041 | 41.6 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_pull | 1 | 4.410 | 4.410 | 4.410 | 4.410 | 226.8 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_ref_traversal | 1 | 619.741 | 619.741 | 619.741 | 619.741 | 1.6 | 0 | 1 |
| embedded | s | 984,803 | 1 | open | 1 | 1,621 | 1,621 | 1,621 | 1,621 | 0.6 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_aggregate | 30 | 97.855 | 99.214 | 99.951 | 99.951 | 10.2 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_point_lookup | 30 | 0.140 | 1.014 | 1.065 | 1.065 | 2,406 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_pull | 30 | 2.168 | 3.212 | 4.113 | 4.113 | 483.6 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_ref_traversal | 30 | 47.580 | 60.582 | 61.416 | 61.416 | 20.3 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_aggregate | 1 | 3,831 | 3,831 | 3,831 | 3,831 | 0.3 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_point_lookup | 1 | 59.671 | 59.671 | 59.671 | 59.671 | 16.8 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_pull | 1 | 5.785 | 5.785 | 5.785 | 5.785 | 172.9 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_ref_traversal | 1 | 6,757 | 6,757 | 6,757 | 6,757 | 0.1 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | open | 1 | 16,499 | 16,499 | 16,499 | 16,499 | 0.1 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_aggregate | 30 | 1,333 | 1,357 | 1,370 | 1,370 | 0.8 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_point_lookup | 30 | 1.431 | 1.556 | 1.647 | 1.647 | 790.3 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_pull | 30 | 4.189 | 6.211 | 6.615 | 6.615 | 230.7 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_ref_traversal | 30 | 576.792 | 592.832 | 608.399 | 608.399 | 1.7 | 0 | 1 |

## Correctness checks

46 PASS, 0 FAIL, 6 SKIP; 4 backend check runs OK, 0 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)


