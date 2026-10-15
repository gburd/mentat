# Evaluation: sparsemap for mentat's entity-id sets

Status: **evaluated 2026-10-15; not adopted.** No dependency added.

The TODO (docs/RESUME-CHECKPOINT.md, 2026-09-27) asked whether sparsemap — the
maintainer's run-length-compressed bitmap, now **5.8.2** on crates.io as a 100%
safe, `no_std`, dependency-free Rust port that is wire-compatible with the C
library — would help mentat's entid sets. It also asked for a benchmark on a
real hot path before adding any dependency.

## Where mentat holds sets of entity ids

`rg 'BTreeSet<Entid>|HashSet<i64>'` over the engine (tests excluded) finds 46.
By role:

| Site | Typical size | Hot? | What it costs today |
|---|---|---|---|
| Query `:in [?x ...]` collections | 1–10k | per query | Not a Rust set: SQLite gets a `VALUES` table, pg an `= ANY($n)` array. A bitmap can't be passed to either. |
| `as-of` / `since` retraction filters | log-sized | per query | Not a Rust set: a correlated `NOT EXISTS` on `transactions` (SQL). |
| Attribute cache `v_es` (non-unique reverse cache: value → entities) | 1–100s per value | read on cache hits | `BTreeSet<Entid>` per value. Most values have a handful of entities. |
| `pull` / `retractEntity` visited sets (cycle guard) | 1–100s | per pull | `HashSet<i64>`, a few inserts. |
| Schema/attribute sets (`AttributeSet`, auto-index `indexed`/`rejected`, value-filtered attrs) | 10s | per tx / query | `BTreeSet<Entid>` of attribute ids. |
| pg_mentat history / as-of | — | per query | SQL over B-tree indexes; no in-memory bitmap. |

The two places the TODO hoped bitmaps would pay — the `:in` collections and
the as-of retraction sets — are expressed in SQL and executed by
SQLite/DuckDB/Postgres. They never exist as Rust sets, so a Rust bitmap has
nothing to replace. What is left are small sets: cycle guards, per-value
cache entries and attribute lists.

## Benchmark

`benchmarks/results/sparsemap-eval-2026-10-15/smbench.rs` (release build;
`BTreeSet<u64>` vs `sparsemap::SparseMap` 5.8.2). The four shapes mentat
produces:

- **tiny**: a visited set or cache entry;
- **dense**: entids from one bulk transaction, contiguous in `:db.part/user`;
- **mixed**: several transactions over time, long runs with gaps;
- **sparse**: what a selective query touches across a large partition.

Measurements: build from a sorted id list; 1000 membership probes (half hits);
intersection with every 3rd element. Memory: `BTreeSet` estimated at 24 B per
element; `SparseMap` is its serialized size.

| shape | n | bytes BTreeSet | bytes SparseMap | build bt / sm | probe bt / sm | and bt / sm |
|---|---:|---:|---:|---|---|---|
| tiny | 8 | 192 | 80 | 143ns / 234ns | 9ns / 14ns | 256ns / 97ns |
| tiny | 64 | 1,536 | 352 | 534ns / 1.5us | 16ns / 21ns | 872ns / 275ns |
| dense | 100k | 2.4 MB | 88 B | 1.9ms / 3.4ms | 90ns / 22ns | 1.9ms / 4.9us |
| dense | 1M | 24 MB | 88 B | 27ms / 31ms | 112ns / 11ns | 12ms / 177us |
| mixed | 100k | 2.4 MB | 10 KB | 1.0ms / 5.1ms | 51ns / 34ns | 0.93ms / 47us |
| mixed | 1M | 24 MB | 102 KB | 15ms / 64ms | 116ns / 46ns | 11ms / 2.7ms |
| sparse | 10k | 240 KB | 210 KB | 154us / 2.8ms | 50ns / 106ns | 99us / 726us |
| sparse | 100k | 2.4 MB | 1.1 MB | 1.0ms / 19ms | 201ns / 109ns | 1.4ms / 12.5ms |

What the numbers say:

- **Where sparsemap shines** is dense and mixed sets of 100k+ ids. They are
  20–270,000x smaller, probes are 2–10x faster, and intersections are 4–2400x
  faster. That's exactly the shape of a bulk-loaded partition.
- **For the sets mentat actually keeps in Rust** (tiny cycle guards and cache
  entries), it is a wash. Probes are within a few ns of `BTreeSet`; building is
  1.5–3x slower; memory is smaller, but a few hundred bytes either way doesn't
  matter.
- **On sparse sets it is worse**: building is 10–20x slower, and intersection
  5–9x slower. Random ids across a big partition are its worst case, and
  they're the shape a selective query produces.

## Decision

Don't adopt it now (YAGNI):

1. mentat has no large in-memory entid set on a hot path. The large sets are
   SQL, where the database's own indexes and hash joins do the work.
2. The small sets it does have see no gain on probes (the cache-hit path) and
   lose on builds (the per-query path).
3. A dependency in `mentat_core` / `mentat_db` would reach every backend and
   every crates.io consumer for no measured win.

## When to revisit

Adopt sparsemap if mentat grows an **in-memory set of 100k+ entids that is
dense or clustered**. Candidates, none of which exist today:

- an in-process **entity-membership cache** for a large attribute (e.g. "all
  entities with `:x/kind :order`"), used to prune joins before SQL;
- a client-side **as-of visibility bitmap** materialized once per basis and
  reused across queries (the embedded engine re-derives visibility with
  `NOT EXISTS` on every query);
- shipping a **set of entids over the wire** (mentatd / Quack), where the
  88-byte encoding of a million-id run beats any list.

At that point, take the crates.io crate (`sparsemap = "5.8"`, pure Rust, no
`-sys` needed) behind a feature, and benchmark the specific path.
