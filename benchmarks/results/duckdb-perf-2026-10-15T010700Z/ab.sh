#!/bin/bash
# Interleaved A/B of two extension builds, same store copy, per-call loop.
sc=$1; shift; N=${N:-2000}
for round in 1 2 3; do
  for v in "$@"; do
    for wl in point pull tx; do
      cp ~/stores/$sc.pristine /tmp/ab-$sc.duckdb; rm -f /tmp/ab-$sc.duckdb.wal; cp ~/stores/$sc.entids /tmp/ab-$sc.entids
      n=$N; [ $wl = tx ] && n=300
      printf "%-8s r%d " $v $round; ~/v/bin/python $(dirname $0)/loop.py /tmp/ab-$sc.duckdb ~/$v/mentat.duckdb_extension /home/admin/stores/$sc $n $wl 2>&1
    done
  done
done
