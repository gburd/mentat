# mentat scale benchmark: after

## Environment (abridged; full detail in env.txt)

```
instance_type:   c7i.8xlarge
kernel:          6.12.103-129.197.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8488C
cpus:            24
sockets/numa:    1 1 
mem_total_gib:   61.8
hugepages:       HugePages_Total:=0 HugePages_Free:=0 Hugepagesize:=2048 
data_fs:         /dev/nvme0n1p1 xfs   150G   22G  129G  15% /
mentat_git:      6fed515a
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
| embedded | s | 984,803 | 86.6 | 11,373 |  |  | 0.20 GiB |  |
| embedded | m | 9,850,258 | 2,041 | 4,825 |  |  | 2.00 GiB |  |

## point_lookup

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 512356 | 0.019 | 0.021 | 0.025 | 0.381 | 51,236 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 508308 | 0.019 | 0.022 | 0.025 | 0.110 | 50,831 | 0 | 3 |

## ref_traversal

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 450 | 22.575 | 23.163 | 23.340 | 23.643 | 44.9 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 35 | 290.191 | 307.288 | 317.436 | 317.436 | 3.4 | 0 | 3 |

## aggregate

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 321 | 31.214 | 31.567 | 31.763 | 32.356 | 32.0 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 371.578 | 373.067 | 373.500 | 373.500 | 2.7 | 0 | 3 |

## predicate_scan

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 417 | 24.003 | 24.215 | 24.381 | 25.094 | 41.6 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 37 | 275.251 | 277.258 | 278.134 | 278.134 | 3.6 | 0 | 3 |

## pull

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 271614 | 0.036 | 0.041 | 0.050 | 0.396 | 27,161 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 226901 | 0.044 | 0.052 | 0.061 | 0.236 | 22,690 | 0 | 3 |

## as_of

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 114 | 88.058 | 88.805 | 89.195 | 89.366 | 11.4 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 30 | 1,040 | 1,046 | 1,048 | 1,048 | 1.0 | 0 | 3 |

## since

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 2403 | 4.159 | 4.259 | 4.303 | 4.661 | 240.2 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 191 | 52.509 | 52.943 | 54.645 | 55.074 | 19.0 | 0 | 3 |

## input_bindings

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 25631 | 0.383 | 0.429 | 0.438 | 0.509 | 2,563 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 4675 | 2.125 | 2.277 | 2.348 | 2.531 | 467.5 | 0 | 3 |

## write_mixed

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | write | 17647 | 3.392 | 3.815 | 4.040 | 22.434 | 294.1 | 0 | 3 |
| embedded | s | 984,803 | 8 | read | 34668 | 10.211 | 30.983 | 41.230 | 110.381 | 577.5 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | write | 15646 | 3.410 | 6.973 | 8.611 | 25.330 | 260.8 | 0 | 3 |
| embedded | m | 9,850,258 | 8 | read | 2234 | 6.323 | 585.528 | 1,149 | 2,076 | 36.9 | 0 | 3 |

## concurrency_sweep

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | read | 1367 | 0.114 | 22.847 | 23.184 | 23.586 | 136.6 | 0 | 3 |
| embedded | s | 984,803 | 8 | read | 9986 | 0.081 | 24.433 | 24.735 | 26.542 | 996.3 | 0 | 3 |
| embedded | s | 984,803 | 32 | read | 21040 | 0.101 | 54.502 | 64.187 | 94.136 | 2,095 | 0 | 3 |
| embedded | s | 984,803 | 64 | read | 21084 | 0.103 | 115.099 | 144.955 | 175.393 | 2,093 | 0 | 3 |
| embedded | s | 984,803 | 128 | read | 21260 | 0.101 | 235.201 | 284.737 | 385.131 | 2,101 | 0 | 3 |
| embedded | m | 9,850,258 | 1 | read | 108 | 0.162 | 300.034 | 305.347 | 306.313 | 10.6 | 0 | 3 |
| embedded | m | 9,850,258 | 8 | read | 742 | 0.168 | 324.908 | 330.539 | 396.560 | 72.3 | 0 | 3 |
| embedded | m | 9,850,258 | 32 | read | 1639 | 0.176 | 661.280 | 701.176 | 765.435 | 157.1 | 0 | 3 |
| embedded | m | 9,850,258 | 64 | read | 1725 | 0.148 | 1,543 | 1,648 | 1,708 | 161.0 | 0 | 3 |
| embedded | m | 9,850,258 | 128 | read | 1792 | 0.158 | 2,814 | 3,005 | 3,279 | 160.4 | 0 | 3 |

## cold_vs_warm

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| embedded | s | 984,803 | 1 | cold_aggregate | 1 | 41.181 | 41.181 | 41.181 | 41.181 | 24.3 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_point_lookup | 1 | 8.894 | 8.894 | 8.894 | 8.894 | 112.4 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_pull | 1 | 4.608 | 4.608 | 4.608 | 4.608 | 217.0 | 0 | 1 |
| embedded | s | 984,803 | 1 | cold_ref_traversal | 1 | 523.707 | 523.707 | 523.707 | 523.707 | 1.9 | 0 | 1 |
| embedded | s | 984,803 | 1 | open | 1 | 44.886 | 44.886 | 44.886 | 44.886 | 22.3 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_aggregate | 30 | 31.196 | 31.599 | 31.667 | 31.667 | 32.0 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_point_lookup | 30 | 0.020 | 1.126 | 1.385 | 1.385 | 7,225 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_pull | 30 | 1.416 | 4.676 | 5.348 | 5.348 | 507.9 | 0 | 1 |
| embedded | s | 984,803 | 1 | warm_ref_traversal | 30 | 24.840 | 98.729 | 132.994 | 132.994 | 27.9 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_aggregate | 1 | 4,984 | 4,984 | 4,984 | 4,984 | 0.2 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_point_lookup | 1 | 12.646 | 12.646 | 12.646 | 12.646 | 79.1 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_pull | 1 | 1.421 | 1.421 | 1.421 | 1.421 | 703.8 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | cold_ref_traversal | 1 | 4,941 | 4,941 | 4,941 | 4,941 | 0.2 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | open | 1 | 50.201 | 50.201 | 50.201 | 50.201 | 19.9 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_aggregate | 30 | 374.378 | 375.532 | 375.533 | 375.533 | 2.7 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_point_lookup | 30 | 1.080 | 2.573 | 2.592 | 2.592 | 928.5 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_pull | 30 | 5.078 | 8.393 | 8.643 | 8.643 | 186.0 | 0 | 1 |
| embedded | m | 9,850,258 | 1 | warm_ref_traversal | 30 | 392.530 | 477.170 | 520.620 | 520.620 | 2.5 | 0 | 1 |

## Correctness checks

52 PASS, 0 FAIL, 0 SKIP; 4 backend check runs OK, 0 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)


