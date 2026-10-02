#!/usr/bin/env bash
# One-time host bootstrap for the scale suite on a fresh AL2023 box (tested on
# r6id.metal). Idempotent-ish: re-running skips finished steps.
#   - RAID-0 every "Instance Storage" NVMe disk -> XFS at /nvme
#   - toolchain: gcc/clang/rustup 1.90/cargo-pgrx 0.17.0/python3.11 venv (duckdb 1.5.6)
#   - PostgreSQL 16 built from source WITHOUT --enable-cassert (pgrx's
#     `init --pg16 download` builds an assert-enabled server, which is not a
#     fair benchmark target), registered with pgrx via its pg_config.
#   - DuckDB v1.5.6 CLI
set -euxo pipefail
PGV="${PGV:-16.15}"
NV=/nvme

sudo dnf install -y -q git gcc gcc-c++ make clang libicu-devel readline-devel zlib-devel \
  flex bison perl-FindBin perl-IPC-Cmd perl-core openssl-devel pkgconfig mdadm xfsprogs \
  sysstat numactl python3.11 python3.11-pip unzip perf tmux jq bzip2 >/dev/null

if ! mountpoint -q $NV; then
  mapfile -t DISKS < <(lsblk -d -n -o NAME,MODEL | awk '/Instance Storage/{print "/dev/"$1}')
  sudo mdadm --create /dev/md0 --run --level=0 --chunk=256 --raid-devices=${#DISKS[@]} "${DISKS[@]}"
  sudo mkfs.xfs -f -q /dev/md0
  sudo mkdir -p $NV && sudo mount -o noatime,nodiratime /dev/md0 $NV
  sudo chown ec2-user:ec2-user $NV
fi

command -v rustup >/dev/null || curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.90
source ~/.cargo/env
rustup toolchain install 1.90 --profile minimal -c rustfmt -c clippy

if [ ! -x $NV/pg16/bin/postgres ]; then
  cd $NV && curl -sfLO https://ftp.postgresql.org/pub/source/v$PGV/postgresql-$PGV.tar.bz2
  tar xjf postgresql-$PGV.tar.bz2 && cd postgresql-$PGV
  CFLAGS="-O2 -g" ./configure --prefix=$NV/pg16 --with-icu >/dev/null
  make -s -j"$(nproc)" >/dev/null && make -s install >/dev/null
  make -s -C contrib/pg_stat_statements install >/dev/null
fi

cargo pgrx --version 2>/dev/null | grep -q 0.17.0 || cargo +1.90 install cargo-pgrx --version 0.17.0 --locked
cargo pgrx init --pg16 $NV/pg16/bin/pg_config

[ -x $NV/venv/bin/python ] || python3.11 -m venv $NV/venv
$NV/venv/bin/pip install -q duckdb==1.5.6

if [ ! -x $NV/bin/duckdb ]; then
  mkdir -p $NV/bin && cd $NV/bin
  curl -sfLo duckdb.zip https://github.com/duckdb/duckdb/releases/download/v1.5.6/duckdb_cli-linux-amd64.zip
  unzip -o duckdb.zip && rm duckdb.zip
fi
$NV/bin/duckdb --version
$NV/venv/bin/python -c 'import duckdb; print("pyduckdb", duckdb.__version__)'
$NV/pg16/bin/postgres --version
echo BOOTSTRAP_OK
