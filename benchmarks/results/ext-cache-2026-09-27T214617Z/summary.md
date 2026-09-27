# mentat scale benchmark: ext-cache

## Environment (abridged; full detail in env.txt)

```
instance_type:   c7i.8xlarge
kernel:          6.12.103-129.197.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8488C
cpus:            32
sockets/numa:    1 1 
mem_total_gib:   61.8
hugepages:       HugePages_Total:=0 HugePages_Free:=0 Hugepagesize:=2048 
data_fs:         /dev/nvme0n1p1 xfs   150G   13G  138G   9% /
mentat_git:      d34e0177
duckdb:          1.5.5
postgres:        n/a
```

## Dataset and PG size vs shared_buffers

```
s: 984803 datoms (1600 users, 130000 issues, 6492 history updates)
m: sqlite-ext/duckdb read the embedded-built store (loaded at 1.9.0, upgraded to v2 on first open: 16 s once)
```

## bulk_load

| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |
|---|---|---:|---:|---:|---:|---:|---:|---|
| sqlite-ext | s | 984,803 | 67.6 | 14,566 |  |  | 0.19 GiB |  |
| duckdb | s | 984,803 | 67.9 | 14,513 |  |  | 0.19 GiB |  |

## point_lookup

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 15574 | 0.625 | 0.713 | 0.745 | 1.502 | 1,557 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 8655 | 1.126 | 1.287 | 1.393 | 2.342 | 865.5 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 115297 | 0.083 | 0.098 | 0.107 | 0.487 | 11,530 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 18942 | 0.514 | 0.591 | 0.602 | 1.823 | 1,894 | 0 | 1 |

## ref_traversal

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 259 | 38.725 | 39.282 | 39.392 | 40.833 | 25.9 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 30 | 493.431 | 502.992 | 503.067 | 503.067 | 2.0 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 268 | 37.439 | 38.040 | 38.365 | 38.933 | 26.7 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 30 | 479.298 | 487.431 | 487.907 | 487.907 | 2.1 | 0 | 1 |

## aggregate

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 112 | 89.561 | 90.464 | 90.884 | 92.339 | 11.2 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 30 | 1,067 | 1,085 | 1,090 | 1,090 | 0.9 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 112 | 89.406 | 92.040 | 92.597 | 94.205 | 11.1 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 30 | 1,083 | 1,099 | 1,125 | 1,125 | 0.9 | 0 | 1 |

## predicate_scan

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 350 | 27.942 | 31.257 | 31.978 | 32.931 | 34.9 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 30 | 333.272 | 344.593 | 346.458 | 346.458 | 3.0 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 317 | 31.225 | 33.444 | 34.376 | 34.851 | 31.6 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 30 | 390.711 | 396.112 | 400.309 | 400.309 | 2.6 | 0 | 1 |

## pull

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 17318 | 0.559 | 0.640 | 0.671 | 42.579 | 1,732 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 4794 | 1.798 | 4.394 | 5.553 | 8.868 | 479.4 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 205169 | 0.047 | 0.055 | 0.068 | 0.473 | 20,517 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 6750 | 1.149 | 3.765 | 4.792 | 7.680 | 674.9 | 0 | 1 |

## as_of

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 118 | 85.256 | 85.996 | 87.037 | 87.267 | 11.7 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 30 | 1,096 | 1,110 | 1,119 | 1,119 | 0.9 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 118 | 84.768 | 85.500 | 86.394 | 86.401 | 11.8 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 30 | 1,097 | 1,112 | 1,122 | 1,122 | 0.9 | 0 | 1 |

## since

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 1933 | 5.123 | 5.258 | 7.924 | 9.373 | 193.3 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 200 | 48.264 | 53.064 | 90.671 | 156.318 | 19.9 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 2122 | 4.666 | 4.806 | 6.116 | 7.760 | 212.1 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 211 | 46.990 | 49.386 | 57.181 | 58.417 | 21.1 | 0 | 1 |

## input_bindings

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 9112 | 1.051 | 1.162 | 1.193 | 1.902 | 911.1 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 3731 | 2.660 | 2.758 | 2.826 | 3.529 | 373.1 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 19368 | 0.485 | 0.559 | 0.574 | 0.901 | 1,937 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 4570 | 2.149 | 2.258 | 2.346 | 13.933 | 456.9 | 0 | 1 |

## write_mixed

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | write | 14801 | 4.011 | 4.579 | 5.019 | 92.345 | 246.7 | 0 | 1 |
| duckdb | s | 984,803 | 8 | read | 19776 | 41.120 | 52.478 | 68.578 | 88.657 | 329.2 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | write | 11471 | 4.494 | 7.951 | 10.383 | 88.017 | 191.2 | 0 | 1 |
| duckdb | m | 9,850,258 | 8 | read | 1040 | 560.158 | 1,445 | 2,204 | 3,128 | 17.2 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | write | 17744 | 3.376 | 3.748 | 4.055 | 17.326 | 295.7 | 0 | 1 |
| sqlite-ext | s | 984,803 | 8 | read | 20934 | 40.001 | 49.107 | 63.524 | 81.062 | 348.7 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | write | 13011 | 3.828 | 7.389 | 9.725 | 25.896 | 216.8 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 8 | read | 950 | 559.156 | 1,759 | 2,297 | 2,382 | 15.7 | 0 | 1 |

## concurrency_sweep

| backend | scale | datoms | clients | op | n | p50 ms | p95 ms | p99 ms | max ms | ops/s | errors | reps |
|---|---|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| duckdb | s | 984,803 | 1 | read | 749 | 0.694 | 39.149 | 39.366 | 40.230 | 74.8 | 0 | 1 |
| duckdb | s | 984,803 | 8 | read | 5806 | 0.717 | 40.356 | 40.652 | 63.589 | 578.8 | 0 | 1 |
| duckdb | s | 984,803 | 32 | read | 12901 | 1.305 | 70.622 | 79.311 | 177.967 | 1,282 | 0 | 1 |
| duckdb | s | 984,803 | 64 | read | 12713 | 2.914 | 166.626 | 197.772 | 269.112 | 1,261 | 0 | 1 |
| duckdb | s | 984,803 | 128 | read | 12255 | 9.997 | 338.355 | 385.633 | 565.661 | 1,206 | 0 | 1 |
| duckdb | m | 9,850,258 | 1 | read | 62 | 1.219 | 496.008 | 497.443 | 497.443 | 6.0 | 0 | 1 |
| duckdb | m | 9,850,258 | 8 | read | 472 | 1.588 | 506.754 | 509.463 | 517.167 | 46.5 | 0 | 1 |
| duckdb | m | 9,850,258 | 32 | read | 1120 | 2.859 | 838.959 | 846.750 | 901.919 | 109.9 | 0 | 1 |
| duckdb | m | 9,850,258 | 64 | read | 1150 | 9.844 | 2,063 | 2,501 | 2,603 | 104.0 | 0 | 1 |
| duckdb | m | 9,850,258 | 128 | read | 1180 | 33.738 | 3,672 | 4,156 | 4,218 | 101.6 | 0 | 1 |
| sqlite-ext | s | 984,803 | 1 | read | 797 | 0.147 | 37.908 | 38.116 | 39.247 | 79.4 | 0 | 1 |
| sqlite-ext | s | 984,803 | 8 | read | 6220 | 0.149 | 38.820 | 39.055 | 40.287 | 619.6 | 0 | 1 |
| sqlite-ext | s | 984,803 | 32 | read | 14137 | 0.236 | 67.877 | 68.661 | 139.083 | 1,405 | 0 | 1 |
| sqlite-ext | s | 984,803 | 64 | read | 14024 | 0.233 | 168.802 | 208.719 | 278.308 | 1,390 | 0 | 1 |
| sqlite-ext | s | 984,803 | 128 | read | 14048 | 0.241 | 348.606 | 417.555 | 488.853 | 1,382 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 1 | read | 62 | 0.606 | 484.710 | 488.314 | 488.314 | 6.1 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 8 | read | 496 | 0.713 | 496.333 | 498.205 | 507.335 | 47.9 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 32 | read | 1161 | 1.178 | 840.448 | 848.237 | 865.542 | 109.7 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 64 | read | 1157 | 1.187 | 2,083 | 2,559 | 2,826 | 105.7 | 0 | 1 |
| sqlite-ext | m | 9,850,258 | 128 | read | 1210 | 1.330 | 3,984 | 4,392 | 4,799 | 102.2 | 0 | 1 |

## Correctness checks

104 PASS, 0 FAIL, 0 SKIP; 8 backend check runs OK, 0 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)


