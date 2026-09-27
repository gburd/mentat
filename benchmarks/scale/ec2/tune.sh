#!/usr/bin/env bash
# Kernel + PostgreSQL tuning for the scale run (written for r6id.metal:
# 128 vCPU, 2 sockets, 1 TiB RAM). Run after ec2/bootstrap.sh.
#   SB_PCT=85 bash ec2/tune.sh      -> initdb $PGDATA, tune, start PG
#
# Reasoning, recorded here so env.txt can point at it:
#   shared_buffers = SB_PCT% of RAM (the user's rule). Everything the benchmark
#     touches must fit in it (checked after load: pg_database_size < s_b).
#   huge_pages = on, with vm.nr_hugepages = s_b/2MiB + 3% headroom (PG also
#     puts its other shared memory there). overcommit stays 0: with 85% pinned
#     in hugepages, strict mode 2 with the default ratio would refuse
#     ordinary allocations.
#   About 150 GiB of RAM is left for the OS, page cache and backends, split as
#     max_connections 300 x work_mem 64MB (worst case ~19 GiB for one sort
#     each), plus maintenance_work_mem 8GB x 8 autovacuum/maintenance workers.
#   effective_cache_size = s_b + 64GB (the planner is told the data is cached).
#   WAL: max_wal_size 100GB, checkpoint_timeout 30min, wal_compression lz4,
#     wal_buffers 1GB. synchronous_commit stays ON: durability is reported
#     honestly (single NVMe RAID-0 fsync per commit).
#   THP never, governor performance (where the metal box exposes cpufreq),
#     numa_balancing 0. The postmaster runs under numactl --interleave=all, so
#     s_b is spread over both sockets rather than filling node 0 first.
set -euxo pipefail
NV=/nvme
PG=$NV/pg16/bin
export PGDATA=${PGDATA:-$NV/pgdata}
SB_PCT=${SB_PCT:-85}
MEM_KB=$(awk '/MemTotal/{print $2}' /proc/meminfo)
SB_MB=$(( MEM_KB * SB_PCT / 100 / 1024 ))
HP=$(( SB_MB / 2 * 103 / 100 + 1024 ))

echo never | sudo tee /sys/kernel/mm/transparent_hugepage/enabled /sys/kernel/mm/transparent_hugepage/defrag >/dev/null
sudo sysctl -q -w kernel.numa_balancing=0 vm.overcommit_memory=0 vm.swappiness=1 \
  vm.dirty_background_bytes=1073741824 vm.dirty_bytes=4294967296
for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance | sudo tee "$g" >/dev/null 2>&1 || true; done
# nr_hugepages: allocate per node so both sockets get half.
sudo sysctl -q -w vm.nr_hugepages=$HP
grep -E 'HugePages_(Total|Free)' /proc/meminfo
[ "$(awk '/HugePages_Total/{print $2}' /proc/meminfo)" -ge "$HP" ] || { echo "could not reserve $HP hugepages"; exit 1; }

if [ ! -f "$PGDATA/PG_VERSION" ]; then
  $PG/initdb -D "$PGDATA" -E UTF8 --locale=C -U ec2-user >/dev/null
fi
cat > "$PGDATA/postgresql.auto.conf" <<EOF
# written by benchmarks/scale/ec2/tune.sh (see its header for the reasoning)
listen_addresses = ''
unix_socket_directories = '/tmp'
max_connections = 300
shared_buffers = ${SB_MB}MB
huge_pages = on
work_mem = 64MB
maintenance_work_mem = 8GB
effective_cache_size = $(( SB_MB + 65536 ))MB
max_wal_size = 100GB
min_wal_size = 8GB
checkpoint_timeout = 30min
checkpoint_completion_target = 0.9
wal_compression = lz4
wal_buffers = 1GB
synchronous_commit = ${SYNC_COMMIT:-on}
max_worker_processes = 128
max_parallel_workers = 64
max_parallel_workers_per_gather = 8
max_parallel_maintenance_workers = 16
random_page_cost = 1.1
effective_io_concurrency = 256
autovacuum_max_workers = 8
autovacuum_vacuum_cost_limit = 4000
shared_preload_libraries = 'pg_stat_statements'
pg_stat_statements.max = 10000
track_io_timing = on
log_min_duration_statement = -1
log_checkpoints = on
EOF
$PG/pg_ctl -D "$PGDATA" -m fast stop 2>/dev/null || true
numactl --interleave=all $PG/pg_ctl -D "$PGDATA" -l $NV/pg.log -w -t 900 start
$PG/psql -h /tmp -d postgres -XAtc "SHOW shared_buffers" -c "SHOW huge_pages" -c "SELECT version()"
grep -E 'HugePages_(Total|Free|Rsvd)' /proc/meminfo
