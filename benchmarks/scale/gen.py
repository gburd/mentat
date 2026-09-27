#!/usr/bin/env python3
"""Scale-suite dataset generator: phase2's issue-tracker workload, sharded.

Same schema (../phase2/schema.edn), same SEED/STATES/PRIORITIES/title words as
../phase2/gen_dataset.py, same entity shapes. What changes for scale:

  * Streams to disk shard by shard (never holds the dataset in memory) and
    generates shards in parallel. Each shard has its own RNG seeded from
    (SEED, shard), so output is identical for a given scale and does not
    depend on --jobs.
  * Two load formats from one pass:
      pg/     `SELECT 1 FROM edn_t('…');` lines. Explicit integer entids in
              high bands (U0/L0/I0 below), so shards load in parallel.
      store/  one EDN transaction per line, for the embedded engine (Rust
              runner, SQLite ext, DuckDB ext). Users/labels/issues use string
              tempids ("u7", "l3", "i42"), and refs use
              (lookup-ref :user/email …) / (lookup-ref :label/name …). Embedded
              mentat only accepts integer entids it allocated itself. History
              lines reference an issue as @@<idx>; the loader replaces that
              with the entid it recorded for tempid "i<idx>".
  * A history phase: about 5% of issues get a later :issue/state update, one
    hist-NNN file per issues shard. The loader records T_MID (the last tx of
    the initial load) and T_SINCE (the last tx before the final hist file),
    so as-of and since queries have known answers.
  * meta.json carries the truth that the runners check results against.

Usage: gen.py SCALE OUT_DIR [--jobs N]      (SCALE in xs,s,m,l,xl)
"""

import importlib.util
import json
import os
import random
import sys
from datetime import datetime, timedelta, timezone
from multiprocessing import Pool

HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "phase2_gen", os.path.join(HERE, "..", "phase2", "gen_dataset.py"))
phase2 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(phase2)
SEED, STATES, PRIORITIES = phase2.SEED, phase2.STATES, phase2.PRIORITIES
WORDS = ["crash", "ui glitch", "perf regression", "typo", "feature request"]

# name: (n_users, n_issues, n_labels). About 7.55 datoms per issue
# (6 attrs + ~1.5 labels + 0.05 history), and ~81 issues per user (phase2 ratio).
SCALES = {
    "xs": (200, 13_000, 50),               # ~0.1M datoms: local smoke
    "s":  (1_600, 130_000, 200),           # ~1M
    "m":  (16_000, 1_300_000, 500),        # ~10M
    "l":  (160_000, 13_000_000, 1_000),    # ~100M
    "xl": (500_000, 40_000_000, 2_000),    # ~300M
}
# PG entid bands: far above pg_mentat's partition_user_seq (starts at 1000001),
# far below the tx band (1e12).
U0, L0, I0 = 10_000_000_000, 11_000_000_000, 12_000_000_000
SHARD = 50_000    # issues per shard file (the PG loader runs shards in parallel)
# Issues (or history updates) per transaction. Keep it under ~700 issues:
# embedded mentat panics on any tx with >= 5461 datoms (off-by-one assert in
# crates/sqlite/db/src/db.rs insert_non_fts_searches: 6 * (32766/6) is not < 32766).
BATCH = 500
HIST_P = 0.05     # fraction of issues that get a later state update
BASE_TIME = datetime(2025, 1, 1, tzinfo=timezone.utc)


def probe_users(n_users):
    return sorted({0, 1, 2, n_users // 3, n_users // 2, (2 * n_users) // 3,
                   n_users - 2, n_users - 1})


def pg_line(edn):
    return "SELECT 1 FROM edn_t('" + edn.replace("'", "''") + "');\n"


def gen_shard(args):
    shard, start, end, n_users, n_labels, out = args
    rng = random.Random(SEED * 1_000_003 + shard)
    hrng = random.Random(SEED * 1_000_033 + shard)
    probes = set(probe_users(n_users))
    t = {"init": {s: 0 for s in STATES}, "final": {s: 0 for s in STATES},
         "open_p_ge": {str(p): 0 for p in PRIORITIES}, "label_refs": 0,
         "hist": 0, "probe": {}, "titles": {}}
    hist = []  # (idx, new_state)
    with open(f"{out}/pg/issues-{shard:04d}.sql", "w") as fp, \
         open(f"{out}/store/issues-{shard:04d}.edn", "w") as fs:
        pg, st = [], []
        for idx in range(start, end):
            k = rng.randint(0, min(3, n_labels))
            labels = rng.sample(range(n_labels), k) if n_labels else []
            title = f"Issue {idx}: " + rng.choice(WORDS)
            state = rng.choice(STATES)
            prio = rng.choice(PRIORITIES)
            asg = rng.randrange(n_users)
            rep = rng.randrange(n_users)
            created = (BASE_TIME + timedelta(minutes=rng.randint(0, 525_600))
                       ).strftime("%Y-%m-%dT%H:%M:%S+00:00")
            final = state
            if hrng.random() < HIST_P:
                final = hrng.choice([s for s in STATES if s != state])
                hist.append((idx, final))
            t["init"][state] += 1
            t["final"][final] += 1
            if final == ":state/open":
                for p in PRIORITIES:
                    if prio >= p:
                        t["open_p_ge"][str(p)] += 1
            t["label_refs"] += len(labels)
            if asg in probes:
                t["probe"].setdefault(str(asg), []).append([idx, state, final])
            if idx == start or idx == end - 1:
                t["titles"][str(idx)] = [title, state, final, prio]
            pg.append(f'{{:db/id {I0 + idx} :issue/title "{title}" :issue/state {state} '
                      f':issue/priority {prio} :issue/assignee {U0 + asg} '
                      f':issue/reporter {U0 + rep} :issue/created-at #inst "{created}"}}')
            pg.extend(f"[:db/add {I0 + idx} :issue/label {L0 + lb}]" for lb in labels)
            lab = ""
            if labels:
                lab = " :issue/label [" + " ".join(
                    f'(lookup-ref :label/name "label-{lb}")' for lb in labels) + "]"
            st.append(f'{{:db/id "i{idx}" :issue/title "{title}" :issue/state {state} '
                      f':issue/priority {prio} '
                      f':issue/assignee (lookup-ref :user/email "user{asg}@example.com") '
                      f':issue/reporter (lookup-ref :user/email "user{rep}@example.com") '
                      f':issue/created-at #inst "{created}"{lab}}}')
            if (idx - start + 1) % BATCH == 0 or idx == end - 1:
                fp.write(pg_line("[" + " ".join(pg) + "]"))
                fs.write("[" + " ".join(st) + "]\n")
                pg, st = [], []
    t["hist"] = len(hist)
    with open(f"{out}/pg/hist-{shard:04d}.sql", "w") as fp, \
         open(f"{out}/store/hist-{shard:04d}.edn", "w") as fs:
        for b in range(0, len(hist), BATCH):
            chunk = hist[b:b + BATCH]
            fp.write(pg_line("[" + " ".join(
                f"[:db/add {I0 + i} :issue/state {s}]" for i, s in chunk) + "]"))
            fs.write("[" + " ".join(f"[:db/add @@{i} :issue/state {s}]" for i, s in chunk) + "]\n")
    return shard, t


def main():
    argv = sys.argv[1:]
    jobs = os.cpu_count() or 1
    if "--jobs" in argv:
        i = argv.index("--jobs")
        jobs = int(argv[i + 1])
        del argv[i:i + 2]
    if len(argv) != 2 or argv[0] not in SCALES:
        print(__doc__, file=sys.stderr)
        sys.exit(2)
    scale, out = argv
    n_users, n_issues, n_labels = SCALES[scale]
    os.makedirs(f"{out}/pg", exist_ok=True)
    os.makedirs(f"{out}/store", exist_ok=True)

    with open(os.path.join(HERE, "..", "phase2", "schema.edn")) as f:
        schema = f.read()
    # Embedded mentat rejects :db.unique/identity without :db/index true
    # (pg_mentat does not care); phase2's :label/name lacks it.
    schema = schema.replace(
        ":db/unique       :db.unique/identity}",
        ":db/unique       :db.unique/identity\n   :db/index        true}")
    with open(f"{out}/schema.edn", "w") as f:
        f.write(schema)
    # Users + labels: one small base file per format.
    with open(f"{out}/pg/base.sql", "w") as fp, open(f"{out}/store/base.edn", "w") as fs:
        ents = [f'{{:db/id {U0 + u} :user/email "user{u}@example.com" :user/name "User {u}"}}'
                for u in range(n_users)] + \
               [f'{{:db/id {L0 + lb} :label/name "label-{lb}"}}' for lb in range(n_labels)]
        sts = [f'{{:db/id "u{u}" :user/email "user{u}@example.com" :user/name "User {u}"}}'
               for u in range(n_users)] + \
              [f'{{:db/id "l{lb}" :label/name "label-{lb}"}}' for lb in range(n_labels)]
        for b in range(0, len(ents), 4 * BATCH):
            fp.write(pg_line("[" + " ".join(ents[b:b + 4 * BATCH]) + "]"))
            fs.write("[" + " ".join(sts[b:b + 4 * BATCH]) + "]\n")

    shards = [(s, lo, min(lo + SHARD, n_issues), n_users, n_labels, out)
              for s, lo in enumerate(range(0, n_issues, SHARD))]
    with Pool(min(jobs, len(shards))) as pool:
        parts = dict(pool.imap_unordered(gen_shard, shards))

    truth = {"init": {s: 0 for s in STATES}, "final": {s: 0 for s in STATES},
             "open_p_ge": {str(p): 0 for p in PRIORITIES}, "probe": {}, "titles": {}}
    label_refs = hist = 0
    for s in sorted(parts):
        t = parts[s]
        for k in ("init", "final", "open_p_ge"):
            for kk, v in t[k].items():
                truth[k][kk] += v
        for u, rows in t["probe"].items():
            truth["probe"].setdefault(u, []).extend(rows)
        truth["titles"].update(t["titles"])
        label_refs += t["label_refs"]
        hist += t["hist"]
    last = max(parts)
    truth["since_hist"] = parts[last]["hist"]   # updates after T_SINCE
    n_initial = 2 * n_users + n_labels + 6 * n_issues + label_refs
    meta = {
        "scale": scale, "seed": SEED, "n_users": n_users, "n_issues": n_issues,
        "n_labels": n_labels, "n_label_refs": label_refs, "n_hist": hist,
        "n_datoms_initial": n_initial, "n_datoms": n_initial + hist,
        "shards": len(shards), "shard_size": SHARD, "batch": BATCH, "hist_p": HIST_P,
        "U0": U0, "L0": L0, "I0": I0, "probe_users": probe_users(n_users),
        "truth": truth,
    }
    with open(f"{out}/meta.json", "w") as f:
        json.dump(meta, f, indent=1)
    print(f"gen: scale={scale} users={n_users} issues={n_issues} labels={n_labels} "
          f"hist={hist} datoms={meta['n_datoms']} shards={len(shards)} -> {out}")


if __name__ == "__main__":
    main()
