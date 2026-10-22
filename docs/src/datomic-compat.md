# Datomic Compatibility

pg_mentat implements the core Datomic data model and query language as a PostgreSQL extension. This document reflects the actual implementation status of the current codebase.

## Overview

pg_mentat provides Datomic-compatible schema, transactions, and Datalog queries via SQL functions. The companion `mentatd` server exposes an HTTP API compatible with Datomic client libraries.

---

## Transaction Operations

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| `:db/add` | Yes | Yes | **Complete** |
| `:db/retract` | Yes | Yes | **Complete** |
| `:db/retractEntity` | Yes | Yes | **Complete** -- cascades `:db/isComponent` children and retracts inbound refs (1.14.0); accepts a lookup ref |
| `:db.fn/cas` | Yes | Yes | **Complete** -- compare-and-swap; accepts a lookup ref |
| `with` (speculative) | Yes | Yes | **Complete** -- `mentat_with()` |
| Map notation | Yes | Yes | **Complete** |
| Tempids | Yes | Yes | **Complete** |
| Lookup refs | Yes | Yes | **Complete** -- `[:unique/attr v]` (or `(lookup-ref a v)`) in entity position, `:db/id`, and as the value of a ref attribute; in map form, for a cardinality-many ref, `[[:a/id 1] [:a/id 2]]` is a list of lookup refs (see [Lookup refs in a list of values](#lookup-refs-in-a-list-of-values)). The embedded engine and DuckDB accept the vector form since 1.18.0 |
| Upsert (unique/identity) | Yes | Yes | **Complete** |
| Tx metadata (:db/txInstant) | Yes | Yes | **Complete** |
| Negative tempids | Yes | No | N/A -- use string tempids |
| `:db/fn` (database functions) | Yes (Clojure) | Yes (mino) | pg_mentat 1.14.0 (`script` builds); embedded SQLite (`mino` feature) and the DuckDB extension in 1.15.0 -- sandboxed mino `(fn [db & args] ...)` returning tx-data; see [Transaction functions](#transaction-functions) |

### Lookup refs in a list of values

A lookup ref `[:attr v]` names the entity whose `:attr` is `v`, for any
`:db/unique` attribute (`:db.unique/identity` or `:db.unique/value`), in
`:db/id`, entity position, and as the value of a cardinality-one ref. The
embedded engine and DuckDB also take one in reverse notation
(`{:a/_ref [:attr v]}`); pg_mentat has no reverse notation in transactions.

In list form, `[:db/add e a v]` takes one value, as in Datomic, so on a ref
attribute (cardinality-one or -many) a vector `v` is always a lookup ref, and
an error if it doesn't resolve. To add several values, use several `:db/add`s
or map form.

The whole value of a cardinality-many ref attribute in map form is the one
ambiguous place: a two-element vector there can be one lookup ref or a list
of two values. Every backend reads it the way DataScript does: when the first
element is the ident of a `:db.unique/identity` attribute, the vector is one
lookup ref. Otherwise it is a list of values, and each vector in that list is
a lookup ref (any `:db/unique` attribute; one that doesn't resolve is an
error).

```clojure
;; :user/id is :db.unique/identity, :user/email is :db.unique/value
{:form/owners [:user/id "u1"]}                   ; one ref: the entity with :user/id "u1"
{:form/owners [[:user/id "u1"] [:user/id "u2"]]} ; two refs, one per lookup ref
{:form/owners [[:user/email "a@x"]]}             ; one ref, by :user/email
{:db/id [:user/email "a@x"] :user/name "A"}      ; :db/id may use either kind
;; :user/name is not unique
{:form/fields [:user/email :user/name]}          ; two refs, to the two attribute entities
;; list form: one value
[:db/add "f" :form/owners [:user/email "a@x"]]   ; one ref, by :user/email
[:db/add "f" :form/fields [:user/email :user/name]] ; error: a lookup ref that does not resolve
```

`:db/ident` is itself `:db.unique/identity`, so `[:db/ident :user/name]` is a
lookup ref to the entity named `:user/name`, not refs to `:db/ident` and
`:user/name`. The same goes for a list whose first element names any identity
attribute. To write two refs in that case, make the first one a lookup ref
(or use entids):

```clojure
{:form/fields [[:db/ident :user/id] :user/name]} ; two refs: :user/id and :user/name
```

### Transaction functions

On every backend with the scripting interpreter -- pg_mentat built with the
`script` feature, the embedded store built with `mino`, the SQLite extension,
and the DuckDB extension -- an ident carrying `:db/fn` is a transaction
function. Its value is mino (a Clojure dialect) source for
`(fn [db & args] ...)` returning tx-data, which is spliced into the calling
transaction (and may itself invoke tx-functions, built-ins, or lookup refs):

```sql
SELECT edn_t($$[{:db/ident :acct/deposit
                 :db/fn "(fn [db id amt]
                           (let [bal (ffirst (mentat.store/q db
                                       '[:find ?b :in $ ?id :where [?e :acct/id ?id] [?e :acct/balance ?b]]
                                       id))]
                             [[:db/cas [:acct/id id] :acct/balance bal (+ bal amt)]]))"}]$$);

SELECT edn_t('[[:acct/deposit "A-1" 50]]');
```

The function sees the database as of the start of the transaction (as in
Datomic) and runs in the same sandbox as `edn_eval`: no filesystem or network,
bounded by `mentat.script_max_steps` / `_heap_bytes` / `_depth`, and
`mentat.txfn_max_depth` (default 16; `MENTAT_TXFN_MAX_DEPTH` on the embedded
and DuckDB backends) bounds tx-functions calling tx-functions. On PostgreSQL it
runs as the calling role. The embedded and DuckDB stores define the `:db/fn`
attribute the first time a transaction installs a function; pg_mentat
bootstraps it. Use `:db.fn/cas` in the returned tx-data for an
atomic read-modify-write, as above. On pg_mentat 1.18.0 a tx-function runs on
the default store only; invoking one on a named store (`t('acme', ...)`) is
`:db.error/unsupported` (issue #389).

---

## Query Operations

### Find Specifications

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| Relation `[:find ?x ?y]` | Yes | Yes | **Complete** |
| Collection `[:find [?x ...]]` | Yes | Yes | **Complete** |
| Tuple `[:find [?x ?y]]` | Yes | Yes | **Complete** |
| Scalar `[:find ?x .]` | Yes | Yes | **Complete** |

### Where Clauses

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| Basic patterns | Yes | Yes | **Complete** |
| NOT clauses | Yes | Yes | **Complete** -- NOT EXISTS subquery |
| NOT-JOIN | Yes | Yes | **Complete** |
| OR clauses | Yes | Yes | **Complete** -- UNION |
| OR-JOIN | Yes | Yes | **Complete** |

### Input Bindings

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| Scalar `:in ?x` | Yes | Yes | **Complete** |
| Collection `:in [?x ...]` | Yes | Yes | **Complete** |
| Tuple `:in [?x ?y]` | Yes | Yes | **Complete** |
| Relation `:in [[?x ?y]]` | Yes | Yes | **Complete** |

### Query Functions

| Function | Datomic | pg_mentat | Status |
|----------|---------|-----------|--------|
| `get-else` | Yes | Yes | **Complete** -- pg_mentat: a correlated subquery + COALESCE, usable in predicates; embedded SQLite/DuckDB: desugared to or-join (1.17.0). Rejects cardinality-many (as Datomic) |
| `missing?` | Yes | Yes | **Complete** -- NOT EXISTS subquery (embedded: `not`, 1.17.0) |
| `ground` | Yes | Yes | **Complete** -- constant binding |
| `get-some` | Yes | No | Not implemented |
| `tuple` | Yes | No | Not implemented |
| `untuple` | Yes | No | Not implemented |

### Predicates

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| `=`, `!=` | Yes | Yes | **Complete** |
| `<`, `>`, `<=`, `>=` | Yes | Yes | **Complete** |
| Arithmetic (`+`, `-`, `*`, `/`) | Yes | Yes | **Complete** |
| `like` / `ilike` | N/A | Yes | **Extension** -- PostgreSQL LIKE/ILIKE |
| Type-safety guard | Yes | Yes | **Complete** -- mismatched types produce empty set |

### Aggregates

| Aggregate | Datomic | pg_mentat | Status |
|-----------|---------|-----------|--------|
| `count` | Yes | Yes | **Complete** |
| `count-distinct` | Yes | Yes | **Complete** |
| `sum` | Yes | Yes | **Complete** |
| `min` | Yes | Yes | **Complete** |
| `max` | Yes | Yes | **Complete** |
| `avg` | Yes | Yes | **Complete** |
| `sample` | Yes | Yes | **Complete** |
| `median` | Yes | No | Not implemented |
| `variance` | Yes | No | Not implemented |
| `stddev` | Yes | No | Not implemented |

### Rules

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| Named rules | Yes | Yes | **Complete** |
| Recursive rules | Yes | Yes | **Complete** -- WITH RECURSIVE CTE |
| Rule sets (multiple heads) | Yes | Yes | **Complete** |
| Rules with predicates | Yes | Yes | **Complete** |

---

## Pull API

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| Attribute selection | Yes | Yes | **Complete** |
| Wildcard `[*]` | Yes | Yes | **Complete** |
| Reverse lookup `[:_attr]` | Yes | Yes | **Complete** |
| Nested/map specs | Yes | Yes | **Complete** |
| Recursion `{:attr ...}` | Yes | Yes | **Complete** -- cycle detection |
| Bounded recursion `{:attr N}` | Yes | Yes | **Complete** |
| Default values | Yes | Yes | **Complete** |
| Rename (`:as`) | Yes | Yes | **Complete** |
| Limit (`:limit N`) | Yes | Yes | **Complete** |
| Component auto-expand | Yes | Yes | **Complete** |
| `pull-many` | Yes | Yes | **Complete** |
| Namespace wildcard | Yes | No | Not implemented |

---

## Schema

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| `:db/ident` | Yes | Yes | **Complete** |
| `:db/valueType` | Yes | Yes | **Complete** (9 types) |
| `:db/cardinality` | Yes | Yes | **Complete** (one/many) |
| `:db/unique` (value) | Yes | Yes | **Complete** |
| `:db/unique` (identity) | Yes | Yes | **Complete** + upsert |
| `:db/index` | Yes | Yes | **Complete** |
| `:db/fulltext` | Yes | Yes | **Complete** -- BM25 scoring |
| `:db/isComponent` | Yes | Yes | **Complete** -- cascade retraction |
| `:db/noHistory` | Yes | Yes | **Complete** |
| `:db/doc` | Yes | Yes | **Complete** |
| Schema alteration | Yes | Yes | **Complete** |

### Value Types

| Type | Datomic | pg_mentat | Storage |
|------|---------|-----------|---------|
| `:db.type/boolean` | Yes | Yes | `BOOLEAN` in `datoms_boolean_new` |
| `:db.type/long` | Yes | Yes | `BIGINT` in `datoms_long_new` |
| `:db.type/double` | Yes | Yes | `DOUBLE PRECISION` in `datoms_double_new` |
| `:db.type/string` | Yes | Yes | `TEXT` in `datoms_text_new` |
| `:db.type/keyword` | Yes | Yes | `TEXT` in `datoms_keyword_new` |
| `:db.type/ref` | Yes | Yes | `BIGINT` in `datoms_ref_new` |
| `:db.type/instant` | Yes | Yes | `TIMESTAMPTZ` in `datoms_instant_new` |
| `:db.type/uuid` | Yes | Yes | `UUID` in `datoms_uuid_new` |
| `:db.type/bytes` | Yes | Yes | `BYTEA` in `datoms_bytes_new` |
| `:db.type/bigint` | Yes | No | Not implemented |
| `:db.type/bigdec` | Yes | No | Not implemented |
| `:db.type/uri` | Yes | No | **Not Planned** -- use string |
| `:db.type/tuple` | Yes | No | **Not Planned** -- Datomic Cloud only |

---

## Time-Travel

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| `as-of` | Yes | Yes | **Complete** -- `mentat_as_of()` |
| `since` | Yes | Yes | **Complete** -- `mentat_since()` |
| `history` | Yes | Yes | **Complete** -- `mentat_history()` |
| `tx-range` | Yes | Yes | **Complete** -- `mentat_tx_range()` |
| Basis-t | Yes | Yes | **Complete** -- MAX(tx) query |

---

## Excision

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| Entity excision | Yes | Yes | **Complete** -- `mentat_excise()` |
| Partition gating | Yes | Yes | **Complete** -- `allow_excision` flag |
| Schema protection | Yes | Yes | **Complete** -- entid < 10000 blocked |
| Dangling ref check | Yes | Yes | **Complete** |
| Excision log | N/A | Yes | **Extension** -- audit trail |

---

## Subscriptions / tx-report-queue

| Feature | Datomic | pg_mentat | Status |
|---------|---------|-----------|--------|
| tx-report-queue | Yes | Yes | **Complete** -- LISTEN/NOTIFY triggers |
| Subscribe by query | No | Yes | **Extension** -- `mentat_subscribe()` |
| Unsubscribe | Yes | Yes | **Complete** -- `mentat_unsubscribe()` |

---

## mentatd HTTP Protocol

| Feature | Datomic Client | mentatd | Status |
|---------|---------------|---------|--------|
| list-databases | Yes | Yes | **Complete** |
| create-database | Yes | Yes | **Complete** |
| delete-database | Yes | Yes | **Complete** |
| connect | Yes | Yes | **Complete** |
| transact | Yes | Yes | **Complete** |
| query | Yes | Yes | **Complete** |
| pull | Yes | Yes | **Complete** |
| entity | Yes | Yes | **Complete** |
| EDN serialization | Yes | Yes | **Complete** |
| Transit+JSON | Yes | Yes | **Complete** |
| CORS | N/A | Yes | **Complete** |
| HTTP/2 | Yes | No | Not implemented |
| Streaming responses | Yes | No | Not implemented |

---

## Known Incompatibilities

| Feature | Reason | Workaround |
|---------|--------|------------|
| `:db/fn` body language | Datomic fns are JVM Clojure; pg_mentat runs sandboxed mino | Port the body (same contract: `db` + args -> tx-data) |
| Peer mode | Architecture incompatible (embedded JVM) | Use mentatd HTTP API |
| Datomic Analytics | Proprietary | PostgreSQL analytics tools |
| Negative tempids | Not supported | Use string tempids |
| Peer cache | No equivalent | Application-layer caching |

---

## Migration from Datomic

Schema definitions transfer directly. Remove `#db/id` partition references:

```clojure
;; Datomic
[{:db/id #db/id[:db.part/db]
  :db/ident :person/name
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one
  :db.install/_attribute :db.part/db}]

;; pg_mentat (same minus partition boilerplate)
[{:db/ident :person/name
  :db/valueType :db.type/string
  :db/cardinality :db.cardinality/one}]
```

Queries work unchanged. Transactions work unchanged. Pull patterns work unchanged.
