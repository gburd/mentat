# pg_mentat → embedded Mentat: advances port inventory

Comparison of `~/ws/pg_mentat` (PostgreSQL extension, pgrx, v1.5.7) against
`~/ws/mentat` (embedded SQLite fork). Goal: identify which advances in the
Postgres codebase can be brought into the embedded SQLite Mentat, **excluding
anything that only exists because it is a Postgres extension**.

Verified against source on the dates in git. All classifications cite files.

## TL;DR

- The Datalog engine, pull, transact, rules, time-travel, excision in
  pg_mentat live in `pg_mentat/src/functions/*.rs` and **generate SQL strings
  run through pgrx `Spi`**. The *algorithms* are mostly storage-agnostic Rust;
  the storage touch-points are localized (e.g. `query.rs` is 7,556 LOC with
  only ~42 `Spi`/pgrx references).
- The **gating dependency** for every query-side port is the diverged `edn`
  crate: pg_mentat's `ParsedQuery` and `PullAttributeSpec` are supersets of the
  embedded ones. Port `edn` grammar first, then features.
- `core` / `core-traits` are essentially identical between the two → low
  friction.

## Classification legend

- **PORTABLE** — A lacks it; logic is storage-agnostic Rust or SQL-string
  generation retargetable to SQLite. No Postgres-only feature required.
- **ADAPT** — Portable in principle, but the impl leans on a Postgres feature
  (triggers, sequences, `WITH RECURSIVE` specifics, multi-table cascade,
  schema-per-store) needing a SQLite equivalent or rework.
- **PG-ONLY** — Exists only because it is a PG extension. Excluded from the port.
- **ALREADY-IN-A** — Embedded Mentat already has it (may need validation).

## Verified findings (spot-checked against source)

| Claim | Verified |
|---|---|
| A has SAVEPOINT primitives but no `:with`/speculative op | ✅ `~/ws/mentat/transaction/src/lib.rs:323` has `savepoint`/`rollback_savepoint`/`release_savepoint`; no speculative txn built on them |
| B implements `:with` via savepoint | ✅ `pg_mentat/src/functions/transact.rs:288` `mentat_with` → `execute_speculative_transaction` (SAVEPOINT) |
| A has no `:as-of`/`:since` query integration | ✅ grep of query-algebrizer + timelines.rs empty |
| A `ParsedQuery` lacks `in_bindings`, `offset`, `distinct`, `rules` | ✅ `~/ws/mentat/edn/src/query.rs:946` vs `~/ws/pg_mentat/edn/src/query.rs:989` |

## Advances ranked by port effort

| Advance | Class | LOC in B | Effort | Source file (B) |
|---|---|---|---|---|
| `:as-of` / `:since` / history queries | PORTABLE | 655 | Low | `functions/time_travel.rs` |
| Speculative transactions (`:with`) | PORTABLE | ~500 | Low | `functions/transact.rs:263+` |
| Pull: reverse refs, nested/recursive, defaults, rename, limit | PORTABLE | ~600 net | Low–Med | `functions/pull.rs` (2,964) |
| EDN: `offset`, `in_bindings`, `distinct`, `rules` | PORTABLE | ~100 | Low (high cascade) | `edn/src/query.rs` |
| Recursive rules + cycle detection | PORTABLE | 512 | Med | `functions/recursive_queries.rs` |
| Virtual tables (auto UNION-ALL views) | PORTABLE | 1,341 | Med | `functions/virtual_tables.rs` |
| Prepared-statement cache | PORTABLE | ~200 | Low | in `functions/query.rs` |
| GDPR excision | ADAPT | 180 | Med | `functions/excision.rs` |
| Materialized views | ADAPT | 613 | Med | `functions/materialized_views.rs` |
| Multi-store namespacing | ADAPT | 484 | Med | `functions/store_management.rs` |
| Partition banding + collision repair | ADAPT | 512 | Med | (schema/bootstrap) |
| Append-only log + current-state projections | ADAPT | schema-level | High | `lib.rs` + bootstrap DDL |
| Reactive subscriptions (LISTEN/NOTIFY) | PG-ONLY | 481 | — | `functions/subscriptions.rs` |
| Hot-standby read path | PG-ONLY | — | — | — |
| Full-text BM25; pgvector / pg_trgm / PostGIS / pg_infer / rum / fuzzystrmatch soft integrations | PG-ONLY | 2,000+ | — | `*_tests.rs`, where-fns |

### Full Datalog (`or`/`or-join`/`not`/`not-join`), named rules

Both codebases parse these (`edn/src/query.rs` `WhereClause` has `OrJoin`,
`NotJoin`, `RuleExpr`; `RuleInvocation`/`Rule`/`RuleClause` present in both).
A has `query-algebrizer/src/clauses/or.rs` and `not.rs`. **Classification:
LIKELY-ALREADY-IN-A**, integration/coverage needs validation before assuming
parity. B's `rules: Vec<Rule>` on `ParsedQuery` is the recursive-rule hook A
lacks.

## Coupling detail (PORTABLE / ADAPT items)

- **`:as-of` / `:since`** — ~95% pure Rust. Temporal predicate is
  `WHERE tx <= N`; basis-t tracking is pure Rust. Boundary: query translation.
- **`:with`** — ~98% storage-agnostic. rusqlite already supports SAVEPOINT
  (A already exposes it). Boundary: transaction wrapper.
- **Pull richer features** — reverse refs = standard SQL join + a new
  `PullAttributeSpec` variant; nested/recursive = a recursive Rust descent with
  a depth guard (same execution boundary as A's existing pull). Gate: `edn`
  `PullAttributeSpec` extension.
- **Recursive rules** — cycle detection is pure Rust; CTE generation emits
  `WITH RECURSIVE` (SQLite ≥ 3.8.3 supports it). Replace `Spi` exec with
  rusqlite.
- **Excision** — referential checks + multi-table `DELETE ... WHERE e IN (...)`.
  SQL is portable; swap `Spi` for rusqlite. Adaptation: A's single denormalized
  datom table vs B's nine typed tables changes the DELETE fan-out.
- **Materialized views** — `CREATE VIEW` is portable; **auto-refresh triggers
  are Postgres-native** — rusqlite has no trigger API, so refresh becomes
  manual/explicit.
- **Append-only log** — B separates immutable log from `current_<type>`
  projections; A conflates them. High-effort schema restructure with a
  migration path.

## edn / core / core-traits divergence (porting friction)

- **`edn`** — friction **HIGH**. `ParsedQuery` in B adds `in_bindings:
  Vec<Binding>`, `offset: Offset`, `distinct: bool`, `rules: Vec<Rule>`
  (`~/ws/pg_mentat/edn/src/query.rs:989`) that A's lacks
  (`~/ws/mentat/edn/src/query.rs:946`). `PullAttributeSpec` in B is a superset
  (reverse refs, defaults, rename, recursion limit). Adding these cascades into
  the algebrizer's validation. **Port edn grammar first.**
- **`core`** — friction **LOW**. `TypedValue`, `ValueType`, `Entid`,
  `KnownEntid` are the same; ~733 LOC each.
- **`core-traits`** — friction **MEDIUM**. Trait boundaries stable, but B may
  carry extra `MentatError` variants for new features (excision, subscriptions,
  matviews). Diff error enums before porting.

## Suggested staging (not a commitment — for scoping only)

1. **edn grammar superset** (`offset`, `in_bindings`, `distinct`, `rules`,
   richer `PullAttributeSpec`) — unblocks everything else.
2. **Low-risk query features**: `:as-of`/`:since`, `:with` (A already has the
   savepoint primitive), richer pull.
3. **Recursive rules + cycle detection** (needs `rules` from step 1).
4. **ADAPT items** (excision, matviews, multi-store) as separately-scoped work.
5. **Never**: subscriptions, hot-standby, PG soft integrations, BM25.

## Progress log

### Step 1 — edn grammar superset — DONE

Ported the query AST + PEG grammar superset into A's `edn` and threaded the new
fields through the algebrizer. Additive, minimal-diff; all workspace tests green.

- `edn/src/query.rs`: added `Offset` enum; `RuleInvocation`/`RuleClause`/`Rule`
  structs; changed `WhereClause::RuleExpr` unit variant →
  `RuleExpr(RuleInvocation)`; added `ParsedQuery` fields `in_bindings`,
  `offset`, `distinct`, `rules`; added `QueryPart` variants + `from_parts`
  parsing with scalar-binding→`in_vars` back-compat.
- `edn/src/lib.rs` (PEG grammar): added `offset()`, `rule_invocation()`,
  `rule_head()`, `rule_clause()`, `rule_definitions()`; wired `rule_invocation()`
  into `where_clause()`; added `:offset`, `:distinct`, `:rules` query parts.
- `query-algebrizer/src/{types,lib}.rs`: added `offset`/`distinct`/`rules` to
  `FindQuery` and threaded them through `from_parsed_query` + `simple` (data no
  longer silently dropped; SQL generation for them is later per-feature work).
- `edn/tests/query_tests.rs`: 7 new parser tests (offset fixed/var/zero,
  distinct, rule invocation, recursive rule definitions, reserved-clause guard).

**Deliberate scoping decisions (deviations from pg_mentat, on purpose):**
- Kept `Limit::None` (did NOT rename to B's `Limit::Unlimited`): the rename
  would ripple across algebrizer/projector for zero functional gain.
- Kept `:in` parsing as `InVars` (B changed it to binding-form `InBindings`):
  avoids regressing A's existing `:in` tuple/coll/rel handling. `InBindings` +
  `in_bindings` are ported as dormant infra (marked with a `ponytail:` note),
  wired when a feature needs binding-form `:in`.
- Used Datomic-standard `:rules` for rule definitions, not B's overloaded
  `:with` (A's `:with` already means with-variables — overloading is ambiguous).
- Deferred the `Pattern.added` 5th-element field (history queries
  `[?e ?a ?v ?tx ?added]`): would touch ~33 `Pattern` call sites + the
  algebrizer's evolved-pattern machinery for a feature not yet wired. Add when
  porting history queries.

**Next:** low-risk query features — `:as-of`/`:since` and `:with` (A already
has the SAVEPOINT primitive at `transaction/src/lib.rs:323`). Then recursive
rules (now that `rules` is parsed and carried through to `FindQuery`).

### Step 2 — speculative transactions (`:with` / `d/with`) — DONE

Ported pg_mentat's `mentat.with` semantics to embedded Mentat. B implements it
by running the normal transact code path inside a PG subtransaction (PL/pgSQL
`RAISE`-to-rollback) and capturing the report; A does the direct SQLite
equivalent with a SAVEPOINT (which A already exposed).

- `transaction/src/lib.rs`: added `InProgress::transact_speculative` and
  `transact_entities_speculative`. Snapshots `partition_map` + `schema`, opens
  a `mentat_speculative` SAVEPOINT, runs the real `transact_entities`, then
  `ROLLBACK TO` + `RELEASE` the savepoint and restores the in-memory snapshots
  — so the `InProgress` is left as before, and no datoms, entids or schema
  changes persist. Same code path as a real transact → identical tempid
  resolution / constraint checking.
- `src/conn.rs`: added `Conn::transact_speculative` convenience wrapper (opens
  an IMMEDIATE transaction, runs the speculative transact, drops without
  committing).
- Test `conn::tests::test_transact_speculative`: proves the report reflects the
  change, the datom does NOT persist, the entid is NOT consumed, and a
  subsequent real transact reuses the reported entid.

No Postgres-only machinery needed; fully storage-agnostic on SQLite.

**Next:** `:as-of` / `:since` temporal queries (WHERE tx <= N; basis-t is pure
Rust). Then recursive rules (uses `FindQuery.rules` from step 1).

### Sequencing correction (after reading A's architecture more closely)

The first-pass effort estimates for `:as-of/:since` and recursive rules were
too optimistic — they assumed B's storage/query shape. A differs:

- **`:as-of` / `:since` — NOT low-effort in A.** A's `datoms` table holds
  **current state only**; full history lives in `timelined_transactions` /
  the `transactions` view (`db/src/db.rs:171` vs `:197`). So `:as-of N` is not
  a `WHERE tx <= N` filter on the current query path (that path can't see
  retracted/superseded values) — it needs point-in-time state reconstruction by
  replaying the log up to `N`. That touches the storage/query core → reclassify
  **ADVANCE-NEEDS-ADAPTATION (High)**, tied to the append-only-log restructure.
  A already has a specialized tx-log query API
  (`query-algebrizer/src/clauses/tx_log_api.rs`) but not general point-in-time
  Datalog.
- **Recursive rules — NOT low-effort in A.** B's real rule expansion is woven
  into its 7,556-LOC SQL-string generator (`functions/query.rs` ~2149–2586),
  emitting `WITH RECURSIVE` CTEs inline. A instead algebrizes to a `SelectQuery`
  IR; expanding `RuleExpr` means new logic in the CC (ConjoiningClauses)
  machinery. Reclassify **Medium–High**. `functions/recursive_queries.rs` is a
  standalone `mentat.recursive(raw_sql)` convenience — PG-only, not the Datalog
  path.

**Revised next step: richer Pull** — genuinely low-risk. A's pull is a
self-contained 272-LOC crate (`query-pull/src/lib.rs`) with a clean
`Puller`/`PullAttributeSpec` abstraction whose enum already has commented-out
placeholders for the exact features B adds (`LimitedAttribute`,
`DefaultedAttribute`). It operates on attribute lists + entity ids via
`lookup_values_for_attribute`, independent of the algebrizer/storage-history
complexity.

### Step 3 — reverse-reference pull (`:ns/_attr`) — DONE

Ported reverse pull from pg_mentat, staying on SQLite. B does this in its
SQL-layer `pull.rs` with its own spec parser; A does it through the `edn` AST +
the `Puller`, querying the VAET index directly.

- `edn/src/query.rs`: added `reverse: bool` to `NamedPullAttribute` (forward
  ident is stored; `reverse` marks the walk direction); updated `From`/`Display`.
- `edn/src/lib.rs` (grammar): `pull_attribute()` now accepts a backward
  namespaced keyword (`:person/_friend`), storing `k.to_reversed()` as the
  forward ident with `reverse: true`.
- `query-pull/src/lib.rs`: `Puller` gained `reverse_attributes`; `prepare`
  routes reverse specs (default output key = reversed keyword, or `:as` alias);
  `pull` runs `SELECT DISTINCT e FROM datoms WHERE a=? AND v=? AND
  value_type_tag=0 AND index_vaet IS NOT 0` per pulled entity and emits the
  referrers as a `Binding::Vec` of `Ref`s. (Refs store as plain ints, tag 0.)
- `query-pull/Cargo.toml`: added `db_traits` path dep (already in tree) so
  rusqlite errors map to `PullError` via `DbError`.
- Tests: `tests/pull.rs::test_reverse_pull` (end-to-end via `(pull ?e
  [:person/_friend])`: two referrers found, empty map when none) and
  `edn/tests/query_tests.rs::can_parse_reverse_pull_attribute`.

`:as` rename was already supported. Nested/recursive pulls, defaults and limits
remain deferred (the crate's own TODOs; larger cache-walk changes).

**Next:** `:as-of`/`:since` (High, needs point-in-time reconstruction from the
`transactions` log) and recursive rules (Medium–High, algebrizer CC changes).

### Step 4 — named (non-recursive) rules — DONE

Rule invocations are now expanded, not `unimplemented!()`. Implemented as an
AST→AST pre-pass (`expand_rules` in `query-algebrizer/src/lib.rs`) rather than
inside the CC machinery, so the query IR is untouched.

- `query-algebrizer/src/lib.rs`: `expand_rules` inlines single-clause,
  non-recursive rule invocations — substitutes invocation args for head params,
  gensyms body-local vars to avoid capture, recurses into or/not bodies and into
  rules that call other (non-recursive) rules (depth-capped at 32).
- `query-algebrizer/src/clauses/mod.rs`: the `RuleExpr` arm is now a clean
  `UnknownRule` error instead of a panic (rules are expanded away before this).
- `query-algebrizer-traits/errors.rs`: added `UnknownRule`,
  `RuleArgumentMismatch`, `RecursiveRuleUnsupported`, `MultiClauseRuleUnsupported`.
- Tests (`tests/query.rs`): `test_named_rule_expansion` (a `grandparent` rule of
  two `parent` patterns resolves correctly) and `test_recursive_rule_errors_cleanly`
  (recursive/multi-clause rule errors, no panic).

**Deliberate scope (bounded, ponytail):** single-clause + variable-args only.
Multi-clause rules (OR alternatives) and recursive rules need `WITH RECURSIVE`
in the `SelectQuery` IR — rejected with a specific error, deferred as future
work. Constant rule args also deferred.

### Step 5 — `:as-of` / `:since` — DEFERRED (High, storage-model work)

Confirmed not tractable as a small port: A's `datoms` table is current-state
only; point-in-time queries need reconstruction from the `transactions` log
(`db/src/db.rs`). This is the append-only-log / current-projection restructure
flagged as High. Left for a dedicated effort; the tx-log query API
(`clauses/tx_log_api.rs`) already covers basic log reads.

## Mentat port status summary

| Advance | Status |
|---|---|
| edn grammar superset (`:offset`/`:distinct`/`:rules`, rule invocations) | DONE |
| Speculative `:with` transactions | DONE |
| Reverse-ref pull (`:ns/_attr`) | DONE |
| `:as` pull rename | already present |
| Named non-recursive rules | DONE |
| Recursive rules (`WITH RECURSIVE`) | deferred — needs SQL IR work |
| `:as-of` / `:since`, history | deferred — needs log-replay/storage restructure |
| Nested/recursive pull, defaults, limits | deferred — cache-walk changes |
| Excision, materialized views, multi-store | deferred — ADAPT |
| Subscriptions, hot-standby, PG soft integrations, BM25 | out of scope (PG-only) |
