# Test fixtures

Files the embedded engine's tests read. Paths in tests are relative to a crate
directory under `crates/sqlite/` (`../fixtures/...`).

| File | What it is | Used by |
|---|---|---|
| `cities.schema` | The Seattle communities schema: 30 idents (`:community/*`, `:neighborhood/*`, `:district/*`, `:region/*`), two fulltext attributes. EDN transaction. | `mentat/tests/pull.rs`, `mentat/src/store.rs` tests, `seattle.script` |
| `all_seattle.edn` | Data for `cities.schema`: about 2000 entity maps, with Datomic-style string tempids (`"a17592186045438"`). EDN transaction. | the same |
| `seattle.script` | A `mentat_cli` script: imports the two files above, runs fulltext, join, cache and ordered queries. Run it from `crates/sqlite/`: `mentat_cli -d /tmp/s.db -f fixtures/seattle.script`. | `cli/tests/batch.rs` (`test_seattle_script_fixture`) |
| `v1encrypted.db` | A store encrypted with SQLCipher, key `key`. | `db/src/db.rs` `test_sqlcipher_*` (only with `--features sqlcipher`) |
| `v1empty.db` | An empty store from 2018 (Mozilla Mentat, `PRAGMA user_version = 1`): bootstrap schema only. | `mentat/tests/scale_regressions.rs` |
| `v1toodle_empty.db` | The same, with the Toodle sample app's schema (`:label/*`, `:item/*`) and no data. | `mentat/tests/scale_regressions.rs` |
| `old/` | Older pre-release stores, kept for reference only (see `old/README.md`). | nothing |

## Why the `.db` files are "version 1"

They are kept as they were written, on purpose: they are the inputs to the
encryption tests and to the test that an old file is handled safely. Do not
regenerate them with a current build, which would write a version-3 store and
make those tests check nothing.

`v1empty.db` and `v1toodle_empty.db` predate the version 1 that the current
code upgrades (`upgrade` in `db/src/db.rs` handles the 1.x layout, where
`transactions` is a view over `timelined_transactions` and partitions live in
`known_parts`). In these files `transactions` is a table and partitions are in
`parts`. Opening one fails with `no such table: main.timelined_transactions`
and leaves the file unchanged; `test_prerelease_v1_fixtures_are_refused_untouched`
checks both. The upgrade from the real version 1 and 2 layouts is tested on
stores the tests create and then downgrade
(`test_v1_store_is_upgraded_on_open`, `test_upgraded_store_gets_schema_indexes`).

Their bootstrap idents are those of today except `:db.schema/core`, which
came later. That is expected: they are not meant to be current.
