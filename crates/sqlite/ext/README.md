# `mentat_sqlite_ext`: mentat as a SQLite loadable extension

This crate exposes the embedded mentat Datalog engine as SQL functions in any
SQLite host (the `sqlite3` CLI, Python's `sqlite3`, an app linking libsqlite3).
The function names match the DuckDB extension (`edn_t`, `edn_q`, `edn_pull`,
`edn_eval`). The JSON shapes follow pg_mentat's `mentat_query` and `mentat_pull`.

```sql
.load ./target/release/libmentat_sqlite     -- or: SELECT load_extension('…/libmentat_sqlite');
SELECT edn_t('/tmp/people.db', '[{:db/ident :person/name :db/valueType :db.type/string
                                  :db/cardinality :db.cardinality/one}]');
SELECT edn_t('/tmp/people.db', '[{:person/name "Alice"}]');
SELECT edn_q('/tmp/people.db', '[:find ?e ?n :where [?e :person/name ?n]]', NULL);
-- {"columns":["?e","?n"],"results":[[65537,"Alice"]],"result":[[65537,"Alice"]]}
```

## Build

```bash
cargo build -p mentat_sqlite_ext            # target/debug/libmentat_sqlite.so
cargo build -p mentat_sqlite_ext --release  # target/release/libmentat_sqlite.so
cargo test  -p mentat_sqlite_ext            # pure-Rust tests (inputs contract, shapes)
crates/sqlite/ext/test/smoke.sh             # real sqlite3 CLI; EXT=/SQLITE3= to override
```

The crate is a workspace **member**, but it is **not** a `default-member`, so a
plain root `cargo build` / `cargo test` never builds it. The root `Cargo.toml`
lists the `crates/sqlite/*` default members explicitly instead of using a glob
for this reason. `workspace.exclude` would also keep it out of the default
build, but then `-p mentat_sqlite_ext` would stop working from the root and
the crate would have its own `Cargo.lock`. The crate enables `mentat/mino`,
and Cargo unifies features. If it were a default member, every root build
would compile the default `mentat` with scripting on.

The entrypoint is `sqlite3_mentatsqlite_init`. SQLite derives that name from
the file name `libmentat_sqlite.so` (it drops `lib` and non-letters), so
`.load` needs no second argument. It is the only symbol the `.so` exports
(`nm -D --defined-only` prints just that one line).

The host SQLite must be 3.30.0 or newer, because the functions are registered
with `SQLITE_DIRECTONLY`. The extension checks the version at load time and
fails with an error message on older hosts.

## Functions

The first argument is always the path of mentat's store file, as in the
DuckDB extension. That store is independent of whatever database the host
has open. `''` opens an in-memory store that exists only for that one call.
If any argument is SQL NULL, the result is NULL.

| Function | Returns |
|---|---|
| `edn_t(db_path TEXT, edn TEXT)` | JSON tx-report `{"tx_id":N,"tx_instant":"RFC3339","tempids":{"a":N,…}}` (same as DuckDB `edn_t`) |
| `edn_q(db_path TEXT, query TEXT, opts TEXT)` | JSON result. The shape depends on the find spec (below). |
| `edn_pull(db_path TEXT, pattern TEXT, entity INTEGER)` | JSON map with `":ns/attr"` keys and `":db/id"` (pg_mentat shape) |
| `edn_eval(db_path TEXT, script TEXT)` | EDN text (`pr-str`) of the script's last form |

The shapes of `edn_q` results follow pg_mentat's `format_find_response`:

| Find spec | JSON |
|---|---|
| relation `[:find ?a ?b …]` | `{"columns":["?a","?b"],"results":[[…],…],"result":[[…],…]}` |
| relation, single aggregate `[:find (count ?e) …]` | `{"result": N}` |
| collection `[:find [?a ...] …]` | `{"result":[…]}` |
| tuple `[:find [?a ?b] …]` | `{"result":[a,b]}` or `{"result":null}` |
| scalar `[:find ?a . …]` | `{"result":v}` or `{"result":null}` |

The values in `edn_q` results use pg_mentat's `mentat_query` encoding:

- A ref is a plain integer.
- A keyword is a string such as `":ns/name"`.
- An instant is an ISO-8601 UTC string (`…Z`).
- A uuid is a hyphenated string.
- Bytes are a hex string.
- A `(pull ?e …)` column nests as a JSON object.

`edn_pull` values follow pg_mentat's `mentat_pull`:

- A ref is `{":db/id":N}`.
- An instant is epoch microseconds.
- A cardinality-many attribute is an array.

To use a result from SQL, pass it to `json_extract`, `json_each`, or `->>`.
For example, this joins a query result against a native table:

```sql
SELECT a.age, json_extract(m.value, '$[1]') AS name
  FROM json_each(edn_q('/tmp/people.db', '[:find ?e ?n :where [?e :person/name ?n]]', NULL), '$.results') m
  JOIN ages a ON a.name = json_extract(m.value, '$[1]');
```

`edn_q` is a scalar function, so it returns one value per call. A
table-valued version would need a virtual-table module, and this crate does
not have one yet.

## `edn_q` options: the inputs contract

`opts` is a JSON object. SQL NULL, `''`, `null`, and `{}` all mean "no
options". The DuckDB extension accepts the same object. The keys are a subset
of pg_mentat's.

| Key | Meaning |
|---|---|
| `"inputs": [...]` | Positional values, one per `:in` binding form. Source vars such as `$` are not binding forms. |
| `"asOf": T` | Query the database as of tx `T` (`Store::q_once_as_of`). |
| `"since": T` | Query only the datoms added after tx `T` (`Store::q_once_since`). Usually combined with a history pattern `[?e ?a ?v ?tx ?added]`. |

These cases are SQLite errors:

- `opts` is not valid JSON, or is not an object.
- `opts` has an unknown key.
- `asOf` and `since` are both set.
- The number of `inputs` differs from the number of `:in` bindings.

Each `inputs` element is bound according to its `:in` form:

- `?x` (scalar): the value itself.
- `[?x ...]` (collection): a JSON array of values.
- `[?a ?b]` (tuple): one JSON array.
- `[[?a ?b]]` (relation): an array of arrays.
- `_` placeholders are skipped.

JSON values are converted to mentat types the way pg_mentat's `bind_input_value` does:

| JSON | TypedValue |
|---|---|
| integer | `Long`. It becomes `Ref` when the variable is in an entity or tx position, or in the value position of a `:db.type/ref` attribute. |
| float | `Double` |
| `true` / `false` | `Boolean` |
| `":ns/name"` (string starting with `:`) | `Keyword` |
| other string | `String` |
| `null`, object | error |

A query can bind either scalar inputs or one collection/tuple/relation input,
but not both. `QueryInputs` cannot merge the two kinds yet. The DuckDB
extension has the same limit.

## `edn_eval` and the store path

`edn_eval` builds a `mentat::script::Interpreter::with_default_path(db_path)`.
With that interpreter, calling `(mentat.store/open)` with no arguments opens
`db_path`. The interpreter is **sandboxed** (`mino_rs::Interpreter::sandboxed()`),
so `slurp`, `spit`, and the other filesystem prims are unbound. Each eval is
also limited to 10M steps, a 64 MiB heap, and a call depth of 1000.

```sql
SELECT edn_eval('/tmp/people.db',
  '(def c (mentat.store/open)) (mentat.store/transact c [{:person/name "Carol"}])');
```

## Registration flags and safety

All four functions are registered with `SQLITE_UTF8 | SQLITE_DIRECTONLY`:

- They are **not** `SQLITE_DETERMINISTIC`, because the store can change
  between calls. `edn_t` and `edn_eval` write, and `edn_q`/`edn_pull` read
  mutable state.
- They are `SQLITE_DIRECTONLY` because they open files. They can be called from
  top-level SQL, but not from views, triggers, CHECK constraints, or generated
  columns in an untrusted schema. Such a call fails with "unsafe use of edn_q".
- Each function body runs under `std::panic::catch_unwind`. An error or a panic
  becomes `sqlite3_result_error`, so a Rust panic never unwinds into C.
- A `db_path` that names the host's own `main` database file is rejected. See
  the next section.

## Two SQLites: why this design

A loadable extension has to call SQLite through the host's
`sqlite3_api_routines` table. The mentat engine, however, depends on rusqlite
with `features = ["limits", "bundled"]` and opens its own store file.
Cargo unifies features across a build, so only one `libsqlite3-sys` exists in
this crate's graph.

**Option (b), rusqlite `loadable_extension` for the host plus a bundled SQLite
for the engine, does not work.** This was tested on EC2 with a scratch cdylib:
`rusqlite = { features = ["loadable_extension", "functions"] }` next to
`mentat` (which enables `bundled`), and one function that opens a mentat
store.

- **The build succeeds, but the bundled SQLite is gone.** Cargo unifies
  features, so the one `libsqlite3-sys` gets both `bundled` and
  `loadable_extension`, and `loadable_extension` wins. Its build script takes
  the `build_linked` path, so the bundled `sqlite3.c` is never compiled.
  `nm` finds no `sqlite3_open_v2` anywhere in the `.so`. Every `sqlite3_*`
  call, including the engine's, becomes a stub that calls through the host's
  API pointer.
- **At runtime, loading it crashed the host.** The `.load` step failed inside
  rusqlite's `extension_init2` with the panic `SQLite API not initialized or
  SQLite feature omitted`. That panic happens inside a no-unwind extern fn,
  so it **aborted the `sqlite3` process** (core dump).
- **Outside a host, the engine cannot run.** A plain `cargo test` that calls
  `Store::open` panics with the same message, because there is no API
  pointer.
- Even if loading worked, the engine would depend on the host's SQLite
  version and compile options (the store needs FTS4, for example).
- There is no way to add a separately named second copy. `libsqlite3-sys`
  declares `links = "sqlite3"`, and Cargo allows only one such package per
  dependency graph.

**Option (a) is what this crate does.** It never depends on rusqlite's
`loadable_extension`. `src/lib.rs` reads the twelve C API slots it needs
directly from the raw `sqlite3_api_routines` pointer, by fixed index. The
struct is append-only, and every slot used here exists since SQLite 3.7.16 (below the 3.30 floor checked at load).
The engine keeps its own statically bundled SQLite (rusqlite `bundled`,
currently 3.53) for the store. rustc links a cdylib with a version script
that exports only `#[no_mangle]` items, so the bundled `sqlite3_*` symbols
are local (`nm` shows them as `t`, not `T`). They never interpose on the
host's symbols, and the `.so` has no undefined `sqlite3_*` imports. The
DuckDB extension embeds the store in the same way.

The cost is two SQLite library copies in one process. POSIX advisory locks
belong to the process, and two copies of SQLite do not share their
lock bookkeeping, so they must not open the same file
([howtocorrupt §2.2.1](https://sqlite.org/howtocorrupt.html)). This is why
the functions reject a `db_path` that names the host's `main` database.
**Limitation:** databases added with `ATTACH` are not checked. Keep the
mentat store in its own file. Several `edn_*` calls on the same store are
safe, because they all go through the one bundled copy.
