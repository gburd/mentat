# mentat scale benchmark: ext-base

## Environment (abridged; full detail in env.txt)

```
instance_type:   c7i.8xlarge
kernel:          6.12.103-129.197.amzn2023.x86_64
cpu_model:       Intel(R) Xeon(R) Platinum 8488C
cpus:            24
sockets/numa:    1 1 
mem_total_gib:   61.8
hugepages:       HugePages_Total:=0 HugePages_Free:=0 Hugepagesize:=2048 
data_fs:         /dev/nvme0n1p1 xfs   150G  8.4G  142G   6% /
mentat_git:      n/a
duckdb:          1.5.5
postgres:        n/a
```

## Dataset and PG size vs shared_buffers

```
m: 9850258 datoms (16000 users, 1300000 issues, 64986 history updates)
```

## bulk_load

| backend | scale | datoms | wall s | datoms/s | ANALYZE s | VACUUM s | store size | note |
|---|---|---:|---:|---:|---:|---:|---:|---|
| embedded | m | 9,850,258 | 1,479 | 6,658 |  |  | 1.57 GiB |  |

## Correctness checks

15 PASS, 0 FAIL, 3 SKIP; 1 backend check runs OK, 0 FAILED (all FAIL/FAILED lines, with annotations, in checks.txt)


