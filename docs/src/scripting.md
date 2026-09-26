# Scripting: mino

Mentat embeds [mino](https://codeberg.org/gregburd/mentat) (`crates/mino`), a
pure-Rust Clojure-dialect interpreter, and exposes a `mentat.store/*` primitive
surface for scripting transactions and queries in a single evaluated program.
Upstream mino is archived; this project maintains the interpreter from here on.

The same surface is available on both backends:

- **Embedded** — `mentat::ScriptInterpreter` (in the `mentat` crate) runs mino
  scripts against SQLite-backed `Store`s.
- **PostgreSQL** — the SQL function `mentat_eval(TEXT)` runs a script against the
  current database. It is behind the optional `script` cargo feature (off by
  default).

## The Datomic-style model

A script talks to the store through a small set of primitives that follow
Datomic's split between a mutable connection and immutable database values:

- **conn handle** — an opaque integer from `mentat.store/open`. Mutating and
  lifecycle operations (`transact`, `close`) take a conn.
- **db value** — an immutable snapshot, represented as a mino map tagged
  `:mentat.store/db` carrying its basis tx and any `as-of`/`since` bounds.
  `mentat.store/db` turns a conn into a db value at the current basis;
  `as-of`/`since`/`with` return *new* db values. Reads take a db value (a bare
  conn integer is accepted and treated as the current db). Entity ids may be
  integers, ident keywords, or `[:attr val]` lookup refs.

### Primitives

| Primitive | Kind | Purpose |
|---|---|---|
| `mentat.store/open` | conn | open/get a connection handle |
| `mentat.store/close` | conn | close a connection (embedded) |
| `mentat.store/db` | read | db value at the current basis |
| `mentat.store/as-of` | read | db value fixed at a past tx |
| `mentat.store/since` | read | db value floored at a tx |
| `mentat.store/transact` | write | commit EDN tx data, returns the tx report |
| `mentat.store/with` | write | speculative tx (savepoint, rolled back) |
| `mentat.store/q` | read | run a Datalog query against a db value |
| `mentat.store/pull` | read | pull a pattern for entity ids |
| `mentat.store/entity` | read | attribute map for an entity |
| `mentat.store/read` | read | read one attribute value |
| `mentat.store/datoms` | read | raw datoms for a db value |
| `mentat.store/entities` | read | entities matching a pattern |

The embedded layer also registers `mentat.store/q-once`; the PostgreSQL layer
registers `mentat.store/q-once` as an alias of `q`.

### Example

```clojure
(let [conn (mentat.store/open)]
  (mentat.store/transact conn
    [{:db/ident       :person/name
      :db/valueType   :db.type/string
      :db/cardinality :db.cardinality/one}])
  (mentat.store/transact conn [{:person/name "Bob"}])
  (mentat.store/q (mentat.store/db conn)
                  '[:find ?name :where [?e :person/name ?name]]))
```

From SQL:

```sql
SELECT mentat_eval($$
  (let [conn (mentat.store/open)]
    (mentat.store/transact conn [{:person/name "Bob"}])
    (mentat.store/q (mentat.store/db conn)
                    '[:find ?name :where [?e :person/name ?name]]))
$$);
```

`mentat_eval` returns the result as EDN text (`pr-str`). A mino exception
surfaces as a clean PostgreSQL `ERROR` carrying the mino message.

### Temporal reads across backends

The PostgreSQL query engine accepts `as-of`/`since` temporal inputs, so
arbitrary Datalog `mentat.store/q` runs faithfully against a historical basis:
an as-of/since db value forwards its bound into the query inputs.

The embedded algebrizer has no as-of query rewriting yet (planned for 1.7.1), so
on the embedded backend a historical `mentat.store/q` is an error. The
`datoms`/`entity`/`read`/`pull` paths still honor `as-of`/`since` by replaying
the transaction log; only arbitrary Datalog `q` against a past basis is the gap.

## Security — `mentat_eval`

`mentat_eval` is **callable by every role by design**. The extension issues no
`REVOKE`, so PostgreSQL's default grants `EXECUTE` to `PUBLIC`. That makes the
sandbox the entire defense, so it is built to hold against a hostile caller, not
just a careless one.

- **Sandboxed — no host access.** The interpreter is built with
  `mino_rs::Interpreter::sandboxed()`: the language, regex, bignum, atoms, and
  the in-memory `mentat.store/*` surface are present, but every host-filesystem
  primitive (`slurp`, `spit`, `rm-rf`, `mkdir-p`, `file-exists?`) and the
  file-backed store are absent — unbound, not merely refused. A script cannot
  reach the server's filesystem.
- **Resource-limited.** Three GUCs bound every call. All are `PGC_SUSET`: a
  superuser (or `ALTER SYSTEM`) sets them, and an ordinary role cannot raise
  them for its own session.

  | GUC | Default |
  |---|---|
  | `mentat.script_max_steps` | 10,000,000 |
  | `mentat.script_max_heap_bytes` | 64 MiB |
  | `mentat.script_max_depth` | 2,000 |

  These guard, respectively, against a runaway loop (`(loop [] (recur))`), a
  single step that allocates unboundedly (`(range 1e11)`), and deep non-tail
  recursion that would otherwise overflow the stack and crash the backend.
  Exceeding any of them throws an `:eval/limit` error. `statement_timeout` still
  applies on top, via an interrupt check hook that also wires in
  `pg_cancel_backend()` and `stack_is_too_deep()`.
- **No privilege gain.** A script runs through SPI as the calling role, so it
  reaches only the stores that role can already query with
  `mentat_query`/`mentat_transact`. It must **not** be made `SECURITY DEFINER` —
  that would turn it into a privilege escalation.

The embedded `ScriptInterpreter` uses the same sandboxed interpreter with
step/heap/depth limits set by the host.
