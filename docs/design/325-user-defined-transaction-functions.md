# Design: user-defined transaction functions (Codeberg #325)

Status: **implemented in 1.14.0** (PostgreSQL, `script` builds) —
`crates/pg/pg_mentat/src/functions/txfn.rs`, tests in `src/txfn_tests.rs`.
The decisions below are as built; where the first draft differed, it says so.

## The gap

Datomic lets you install a database function and invoke it inside a
transaction:

```clojure
;; install
[{:db/ident :inc
  :db/fn #db/fn {:lang "clojure"
                 :params [db e a]
                 :code "(let [v (:v (d/entity db e))] [[:db/add e a (inc v)]])"}}]
;; use
[[:inc 42 :counter/n]]
```

The function receives the in-progress database value and returns more
transaction data, which is spliced into the same transaction and processed
recursively. This is how Datomic teams enforce atomic invariants (unique
counters, conditional writes, derived facts) without a read-modify-write race.

pg_mentat today ships only two **built-in** tx-fns, `:db.fn/cas` and
`:db.fn/retractEntity` (`transact.rs`, `recognize_tx_fn`). There is no way to
install your own.

## What we already have to build on

- A sandboxed mino (Clojure-dialect) interpreter: `edn_eval` /
  `functions/script.rs::build_interpreter()` uses
  `mino_rs::Interpreter::sandboxed()` — no filesystem, no net, with step / heap
  / depth GUC limits and an interrupt hook. This is the project's standing,
  security-reviewed way to run user code in the backend.
- A `ScriptBackend` the interpreter already exposes to scripts: `q`,
  `q_with_inputs`, `pull`, `entity`, `read`, `datoms`, `resolve_eid`,
  `basis_tx` (`crates/script/src/lib.rs`). A tx-fn needs exactly these to read
  the in-progress db.
- The transactor's expansion loop already splices generated datoms:
  `pending_datoms` plus the built-in `execute_*_fn` functions show the shape a
  user fn would feed into.

So a user tx-fn is: *run a sandboxed mino function, passing it a db value and
its args; it returns tx-data (EDN); splice that back into the same transaction
and keep processing.* The machinery exists; this is wiring, not new infra.

## Decisions

### D1. Language: mino, not Clojure/JVM
pg_mentat has no JVM. The sandboxed mino interpreter is the only in-backend
code runner we have and is already security-reviewed. **Decision: tx-fns are
mino functions**, stored as the function's source. (Datomic's `:lang "clojure"`
has no equivalent; we accept `:lang "mino"` or omit it.)

Trade-off: not source-compatible with Datomic `:db/fn`. A migrating team
rewrites fn bodies, but the data model, `d/entity`-style reads, and the
return-tx-data contract match, so the rewrite is mechanical.

### D2. Where the fn source lives
Store it as a new value type on the ident entity, mirroring Datomic's
`:db/fn`. Add a bootstrap attribute:

```
:db/fn   :db.type/string   :db.cardinality/one
```

The string holds the mino source of a `fn` form:
`(fn [db e a] ...)`. Resolved at invocation by entid/ident, same as any
attribute. No new storage machinery — it is a plain string datom.

Trade-off vs a dedicated bytecode/opaque type: a string is transparent (shows
in `pull`, `pg_dump`), re-parsed each call (cheap: mino parse is fast and we
cache the compiled fn per-transaction, see D5). Opaque storage would hide the
code and complicate backup. String wins.

### D3. Invocation syntax
A vector whose head is a namespaced keyword that resolves to an entity carrying
`:db/fn`:

```
[[:my/inc 42 :counter/n]]     ; :my/inc is an ident with a :db/fn value
```

The transactor's own list forms (`:db/add`, `:db/retract`, `:db/cas`,
`:db/retractEntity`, anything in `:db.fn/*`) are never looked up. A keyword head
with no `:db/fn` is left untouched, so the transactor reports its usual error.

### D4. The fn contract
`(fn [db & args] -> tx-data)`:
- `db` is the db-value map the script layer already uses (`DbRef`:
  `{:mentat.store/db true :conn N :basis-tx T ...}`), at the basis **before**
  this transaction -- as in Datomic. (The first draft proposed the in-progress
  basis; that would expose half-applied state, and pg_mentat applies a
  transaction only after expansion anyway.) For an atomic read-modify-write,
  return `:db.fn/cas` in the tx-data, as Datomic users do.
- returns EDN tx-data (a vector, or a list, of entities), parsed and spliced in
  place of the call; it may itself invoke other tx-fns (recursion, bounded by the
  step/depth limits + `mentat.txfn_max_depth`, D6).

### D5. Execution model
A **pre-pass** over the transaction vector, before the transactor's two passes:
each invocation is replaced (recursively) by the tx-data it returns, so the
transactor itself is unchanged. (The first draft proposed a work-list inside the
transactor's entity loop; that loop iterates a fixed vector, and a pre-pass
avoids restructuring it and can't disturb tempid / upsert ordering.) One
sandboxed interpreter per `edn_t` call, built lazily on the first invocation;
the fn source is re-read per call. The interpreter's `ScriptBackend` reads
through SPI as the caller.

Trade-off: building the interpreter has a fixed cost (~ms). A transaction with
no tx-fns pays nothing (lazy-build on first use). A transaction with many
tx-fn calls amortizes one interpreter.

### D6. Safety (hard requirements, non-negotiable)
- Runs under `Interpreter::sandboxed()` — no `slurp`/`spit`/`rm`/net.
- Bounded by the existing `mentat.script_max_steps` / `_heap_bytes` / `_depth`
  GUCs (`PGC_SUSET`), plus a new `mentat.txfn_max_depth` (default 16) for tx-fn
  recursion.
- Never `SECURITY DEFINER`; reads go through the caller's SPI with the caller's
  privileges, exactly like `edn_eval` (standing project rule).
- The whole transaction is already atomic; a tx-fn that errors aborts the tx.
- Installing a `:db/fn` is just transacting a string, so it needs only write
  access to the schema — same trust boundary as defining any attribute. (A
  future hardening GUC could gate `:db/fn` installation to a role; noted, not
  built — YAGNI until asked.)

## What shipped (1.14.0)

1. Bootstrap `:db/fn` string attribute, entid 20 (fresh installs and the
   `1.13.0--1.14.0` upgrade).
2. `txfn::expand_user_tx_fns`, called from `execute_transaction_inner` right
   after parsing, in `script` builds.
3. GUC `mentat.txfn_max_depth` (default 16), plus the existing step/heap/depth
   limits.
4. Tests (`txfn_tests.rs`): an `:inc` counter that reads with `q`; a fn that
   returns a lookup-ref `:db.fn/cas` and one that calls it (and the cas guard
   still aborts a stale call); clear errors for a throwing fn and a non-vector
   result; bounded self-recursion; a sandbox escape attempt (`slurp`) refused;
   a keyword head with no `:db/fn` left to the transactor; and the documented
   `:acct/deposit` example.

PostgreSQL only in 1.14.0. **1.15.0 (issue #333)** moved the expander into
the shared `mentat_script::txfn` crate and runs it on every backend:

- pg_mentat: lookup over SPI (`current_text`), eval in `build_interpreter()`.
- Embedded SQLite (`mino` feature): `Store::expand_tx_fns`, called by
  `Store::transact`, `Store::with_speculative`, the SQLite extension's
  `edn_t` and the CLI. The store is moved into a pinned scripting backend for
  the expansion, so `(mentat.store/open)` inside the fn is the store being
  transacted.
- DuckDB (`script` feature, on by default): `mentat_duckdb_store::script::
  transact`, used by `edn_t` and scripted transacts; the fn runs on the same
  connection before the store's `BEGIN`.
- The embedded/DuckDB core schema isn't changed: `:db/fn` is defined the first
  time a transaction mentions it, so existing stores need no migration.
- One shared model test (`transaction_functions` in
  `crates/script/src/model_tests.rs`) pins the behavior on all three.

## Why this is safe to ship incrementally
It reuses the exact sandbox and limits already reviewed for `edn_eval`, adds no
new host capability, and degrades to today's behavior when no `:db/fn` is
installed. The feature is additive and gated by the presence of a `:db/fn`
datom.
