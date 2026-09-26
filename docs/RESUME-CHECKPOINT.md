# Resume checkpoint — 1.9.0: edn_* surface across SQLite / Postgres / DuckDB

## Released
- v1.8.0 on Codeberg + GitHub (master bc58439b, tag v1.8.0). GitHub aligned.
- Codeberg->GitHub "sync on push" now ON (user set it).
- DuckDB Community Extensions PR https://github.com/duckdb/community-extensions/pull/2812
  (fork gburd/community-extensions, branch mentat-1.8.0) — CONVERTED TO DRAFT pending
  the rename; must update its description.yml ref + hello_world and mark ready.
- PGXN: creds NOT in sops (~/ws/nix-config has no pgxn entry). User must supply.

## User request (in flight)
Remove mentat_hello. Rename mentat_transact->edn_t, mentat_query->edn_q on ALL three
backends; DuckDB also gets edn_pull + edn_eval + working as-of/since.

## Decisions
- SQLite had NO SQL surface -> new loadable extension crate crates/sqlite/ext
  (mentat_sqlite_ext, cdylib, non-default member): edn_t/edn_q(JSON)/edn_pull/edn_eval.
  Two-SQLite design: host API via raw sqlite3_api_routines, engine keeps bundled sqlite.
- PG: rename the 4 SQL functions to edn_*, keep old mentat_* as DEPRECATED SQL wrappers
  (non-breaking), mentat.q/t/pull aliases -> new names, upgrade SQL 1.8.0->1.9.0.
  Other mentat_* functions unchanged. Version -> 1.9.0.
- DuckDB: drop mentat_hello, edn_t/edn_q/edn_pull/edn_eval, inputs JSON (pg contract:
  {"inputs":[...]}, {"asOf":T}, {"since":T}), fix quoted-string rendering in edn_q.
- inputs JSON contract identical on all three (pg's parse_temporal_options/parse_input_bindings).

## Agents (EC2 i-022a42db31ad0453b, isolated copies ~/m-duck ~/m-pg ~/m-sqlite)
- DuckDB  f0b02c7b  owns crates/duckdb/ + script.rs default-path hook
- PG      89201361  owns crates/pg/, docs/src PG pages, root Cargo.toml version, META.json
- SQLite  9648ff61  owns crates/sqlite/ext/, root Cargo.toml members

## Coordinator TODO after agents land
- Reconcile root Cargo.toml (PG version bump + SQLite members), regen Cargo.lock.
- Root README (3 backends x edn_* table), CHANGELOG 1.9.0, architecture.md, .github CI
  (add test-sqlite-ext job to ci.yml + .forgejo), release.yml (attach sqlite ext .so).
- Full gate on EC2, update registry PR #2812 (ref + hello_world), mark ready.
- Tag v1.9.0, push Codeberg (mirror syncs). Terminate EC2 + delete SG/key.
