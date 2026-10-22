# Design notes: the Datomic-review issues (#325–#332)

Each entry: what it is, the decision taken, the trade-offs, and what's left.
Issues #329, #331, #332 (core) shipped in 1.13.0; the rest land in 1.14.0.

---

## #325 — user-defined transaction functions (PostgreSQL, `script` builds)

**What.** An ident carrying `:db/fn` (bootstrap attribute entid 20, a string
holding mino source `(fn [db & args] ...)`) is a transaction function. A
transaction invokes it as `[:my/fn arg ...]`; it returns tx-data that is spliced
into the same transaction.

**Decision.** Expand invocations *before* the transactor's two passes
(`functions/txfn.rs::expand_user_tx_fns`), recursively, in one sandboxed
interpreter per transaction. The expansion is plain tx-data, so tempids,
upserts, built-ins (`:db.fn/cas`, `:db/retractEntity`) and cardinality are
handled by the existing code unchanged. Full design in
`325-user-defined-transaction-functions.md`.

| Trade-off | Chosen | Alternative | Why |
|---|---|---|---|
| Language | mino (sandboxed Clojure dialect) | JVM Clojure | No JVM in the backend; mino is the reviewed in-backend runner. Bodies need a mechanical rewrite from Datomic `:db/fn`. |
| Where expansion happens | Pre-pass over the tx vector | Inside the transactor's entity loop (work-queue) | The loop iterates a fixed vector; a pre-pass needed no transactor restructuring and can't disturb tempid/upsert ordering. |
| `db` the fn sees | Basis at the start of the tx | In-progress db including earlier datoms in the same tx | Matches Datomic, and avoids exposing half-applied state. A fn that needs an atomic read-modify-write uses `:db.fn/cas` in its output. |
| Storage of the fn | `:db/fn` string datom | Opaque/compiled blob | Transparent (pull, pg_dump), versioned in history like any datom. Re-parsed per tx; cheap. |
| Recursion bound | `mentat.txfn_max_depth` (16) + the `mentat.script_max_*` limits | Unbounded | A self-calling fn must fail cleanly, not loop. |
| Availability | `script` feature only | Always on | The interpreter is behind `script`; default builds pay nothing. |

**Security.** Same sandbox and limits as `edn_eval` (no filesystem/network
prims; steps/heap/depth GUCs; interrupt hook); runs as the calling role; never
`SECURITY DEFINER`. Installing a fn is transacting a string, i.e. the same trust
as defining any attribute.

**Not done (yet).** The embedded SQLite and DuckDB backends: they have the
same `ScriptBackend` seam, so the same pre-pass could run there; tracked as a
follow-up. A role gate on *installing* `:db/fn` (only if someone asks).

---

## #326 — `:db/retractEntity` retracts inbound references

**What.** Datomic's retractEntity retracts every datom whose *value* is a ref to
the entity, and cascades through `:db/isComponent`. All three backends did the
component cascade but left inbound refs dangling.

**Decision.** Retract `[?referrer ?a e]` for every live ref datom pointing at
`e` (pg: `current_datoms` where `v_ref = e`; SQLite: `datoms` via the VAET
index; DuckDB: the `v_i` mirror column). Only the ref datom goes; the referring
entity survives. New `MentatStoring::referrers` on SQLite/DuckDB.

| Trade-off | Chosen | Alternative |
|---|---|---|
| Behavior change | Always (Datomic semantics) | Opt-in flag | It's a correctness fix to match the documented model; a flag would keep the surprising default. |
| Cascade referrers? | No — retract only the ref datom | Delete referring entities | Datomic doesn't; it would destroy unrelated data. |
| Read source (pg) | Current-state projection | Append-only log `WHERE added` | The log keeps superseded assertions; reading it re-retracts history (found while testing; the outbound read moved to the projection too). |

**Cost.** One indexed lookup per retracted entity (VAET / `v_ref` index).
History is unchanged in shape: the extra retractions are ordinary datoms.

---

## #327 — lookup refs in `:db.fn/cas` / `:db/retractEntity`

**What.** Datomic accepts `[:unique/attr value]` wherever an entity id goes.

**State.** PostgreSQL already did (1.13.0 added a regression test). The embedded
SQLite (and DuckDB, which shares the transactor) rejected it.

**Decision.** `resolve_tx_fn_entid` resolves a lookup ref through the same
`resolve_avs` the transactor uses for ordinary lookup refs; the attribute must be
`:db/unique` and the entity must exist (a tx-fn can't upsert).

| Trade-off | Chosen | Alternative |
|---|---|---|
| Missing entity | Error | Allocate (upsert) | cas/retractEntity of a non-existent entity is meaningless; erroring is what pg_mentat does. |

---

## #328 — scripting `q` against an as-of / since db

**What.** `(mentat.store/q (mentat.store/as-of db t) query)` errored on the
embedded backend ("Mentat has no as-of query rewriting").

**Finding.** The refusal was stale: the embedded engine has had temporal
Datalog (`Store::q_once_as_of` / `q_once_since`) since 1.8.0, DuckDB's backend
already used it, and pg passes `asOf`/`since` to `edn_q`.

**Decision.** Route an as-of/since db to the temporal query entry points, with
or without `:in` inputs. A shared model test now pins it on all three backends.

| Trade-off | Chosen | Alternative |
|---|---|---|
| `pull` on a historical db (SQLite) | Still refused | Reconstruct via datoms | Separate change; `entity`/`read`/`datoms` already work on a historical basis. Done in 1.18.0: pull reads the basis on all three backends. |

---

## #330 — `mentat_explain` / `mentat_query_sql` / `query_view` ignore collection `:in`

**What.** These compiled the query through a wrapper that passed *no* enriched
(collection / tuple / relation) bindings, so a `[?x ...]` input was silently
dropped: the SQL had no filter and EXPLAIN showed a plan edn_q never runs.

**Decision.** Build the enriched bindings exactly as the executing path does
(`parse_input_bindings_v2`) in all three entry points, and delete the
binding-dropping wrapper so it can't recur.

| Trade-off | Chosen | Alternative |
|---|---|---|
| Missing declared input | Error (same as edn_q) | Silently unfiltered SQL | The old behavior hid the bug. |

---

## #332 follow-up — `(bm25-match ...)` and `(turbovec-near ...)` where-fns

**What.** 1.13.0 shipped the pg_fts / pg_turbovec helper functions; queries
couldn't use the indexes from Datalog.

**Decision.**
- `[(bm25-match $ :attr "query" k) [[?e ?score]]]` → a derived table over
  `datoms_text_new` with `to_ftsdoc(lang, v) @@@ to_ftsquery(lang, q)` and
  `ORDER BY ... <=> ...` (BM25, served by the pg_fts index), `LIMIT k`.
- `[(turbovec-near $ :attr "[...]" k [:cosine|:l2|:inner|:l1]) [[?e ?dist]]]`
  → top-K from the per-attribute `turbovec.vector` aux table, using
  schema-qualified `OPERATOR(turbovec.<op>)`.

| Trade-off | Chosen | Alternative |
|---|---|---|
| bm25 attr/store predicate | Inlined integer literals | Bind parameters | `create_fts_index` builds a **partial** index (`WHERE a = <entid> AND store_id = 0 AND added`); the planner only uses it if the query predicate provably implies that, which a parameter doesn't. Both values are integers we resolve, never user text. |
| Score direction | Raw `<=>` (smaller = better), as pg_fts orders | Negate to "higher is better" | Matches pg_fts and pgvector-style distance semantics already used by `vector-near`. |
| Vector dimension | `CHECK (vector_dims(v) = N)` | `turbovec.vector(N)` typmod | pg_turbovec's type takes no typmod. Note pg_turbovec also requires `dim % 8 == 0` to build an index. |
