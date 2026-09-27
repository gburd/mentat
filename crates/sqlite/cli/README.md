# mentat_cli

A REPL and batch runner for an embedded (SQLite) mentat store.

```sh
cargo run -p mentat_cli -- -d my.db                    # interactive
cargo run -p mentat_cli --features mino -- -d my.db    # with .eval
```

## Commands

| command | what it does |
|---|---|
| `.open PATH` / `.close` | open a store file / go back to an in-memory one |
| `.transact EDN` (`.t`) | transact, print the tx report |
| `.import FILE` (`.i`) | transact a file's contents |
| `.query EDN [OPTIONS]` (`.q`) | run a query; OPTIONS as for SQL `edn_q`, below |
| `.pull PATTERN ENTITY` | pull `[*]`, `[:a/b {:c/d [*]}]`, ... for an entid or `:an/ident` |
| `.eval SCRIPT` | run a mino script; `(mentat.store/open)` opens the current store (needs `--features mino`) |
| `.tune` / `.tune!` | show / apply index changes (`Store::tune_indexes`) |
| `.tune off\|schema\|adaptive` | set the auto-index mode (`Store::set_auto_index`) |
| `.explain_query EDN` (`.eq`) | the SQL and SQLite plan for a query |
| `.query_prepared EDN` | prepare, then run, timed |
| `.schema`, `.cache :a/b forward\|reverse\|both`, `.timer on\|off`, `.help`, `.exit` | |

Query options are the JSON object SQL `edn_q` takes (pg_mentat, the SQLite and
DuckDB extensions), parsed by the same `mentat::options_from_json`:

```
.q [:find ?age . :in ?name :where [?e :person/name ?name] [?e :person/age ?age]] {"inputs": ["Alice"]}
.q [:find ?e :in [?name ...] :where [?e :person/name ?name]] {"inputs": [["Alice", "Bob"]]}
.q [:find ?age . :where [?e :person/name "Alice"] [?e :person/age ?age]] {"asOf": 268435460}
.q [:find ?e :where [?e :person/age _]] {"since": 268435460}
```

`inputs` has one element per `:in` binding form: a value for `?x`, an array for
`[?x ...]` or `[?a ?b]`, an array of arrays for `[[?a ?b]]`. An integer bound to
an entity position is a ref, `":kw"` a keyword. Queries, transactions and
scripts may span lines; the REPL keeps reading until the form closes.

## Batch mode

`-e COMMAND` (repeatable), `--file FILE` (`-` for stdin), or a piped stdin run
REPL commands with no prompts and exit 1 if any command failed:

```sh
mentat_cli -d my.db \
  -e '.t [{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}]' \
  -e '.t [{:person/name "Alice"}]' \
  -e '.q [:find ?e :in ?n :where [?e :person/name ?n]] {"inputs": ["Alice"]}'
printf '.q [:find ?n\n :where [_ :person/name ?n]]\n' | mentat_cli -d my.db
```

`-t`, `-i` and `-q` still run a transact / import / query at startup before
the REPL (or batch input).
