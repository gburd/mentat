# Publishing the embedded crates to crates.io

The 22 crates of the SQLite engine are published to crates.io. `pg_mentat`,
`mentatd`, the SQLite/DuckDB loadable extensions, `mentat_ffi` and the
benchmark runner are **not** published (`publish = false`); they ship via PGXN
and the DuckDB Community registry instead.

## Package names vs library names

Package names (what goes in `[dependencies]`) were chosen to be free on
crates.io. **Library names are unchanged**, so `use mentat::`, `use edn::`,
`use gc::`, `use core_traits::` keep compiling. The mapping:

| Package (crates.io) | Library (`[lib] name`) |
|---|---|
| `mentatdb` | `mentat` |
| `mentat_edn` | `edn` |
| `mino_gc`, `mino_gc_derive` (vendored) | `gc`, `gc_derive` |
| `mentat_core_traits`, `mentat_db_traits`, `mentat_sql_traits`, `mentat_public_traits` | `core_traits`, … |
| `mentat_query_{algebrizer,pull,projector}_traits` | `query_*_traits` |
| the rest (`mentat_core`, `mentat_db`, `mentat_query_*`, `mentat_transaction`, `mentat_script`, `mentat_cli`, `mino-rs`) | same |

A crate that keeps a non-default library name sets it explicitly in `[lib]`
(e.g. `mentatdb` has `[lib] name = "mentat"`). Dependents reference it by the
library name with a `package =` override, e.g.
`edn = { package = "mentat_edn", path = "../edn", version = "=1.12.0" }`.

## Consuming from crates.io

```toml
[dependencies]
mentatdb = "1.12"   # the crate is still imported as `mentat`
```

## Per-release publish

1. Confirm the tag's tree is checked out and the version is bumped everywhere
   (the release.yml gate checks Cargo.toml / pg_mentat.control / META.json).
2. Credentials live in `~/.cargo/credentials.toml`. On this host `CARGO_HOME`
   is set to the repo's `.cargo/` (which has **no** credentials), so publish
   with it unset and the token passed explicitly:
   ```sh
   unset CARGO_HOME
   export CARGO_REGISTRY_TOKEN="$(python3 -c "import tomllib;print(tomllib.load(open('$HOME/.cargo/credentials.toml','rb'))['registry']['token'])")"
   ```
3. Publish in dependency order (`~/.cache/mentat/pub_order.txt`, regenerated
   from `cargo metadata`), one crate at a time, waiting for each to appear on
   the index. The script `~/.cache/mentat/publish.sh` is idempotent (skips a
   crate+version already on the index) and honors crates.io's rate limit.

## Gotchas (each one cost a failed publish)

- **New-crate rate limit.** After an initial burst of ~5 new crates, crates.io
  allows roughly **one new crate per 10 minutes** and returns
  `429 ... try again after <RFC2822 timestamp>`. The publish script parses that
  timestamp and sleeps until it. First publish of ~22 names takes ~3 hours.
- **Every dependency needs a version.** `cargo publish` rejects a path dep with
  no `version`. All intra-workspace deps carry `version = "=1.12.0"`. The
  vendored `gc` dep from `mino` needed `version = "0.5.1"` added by hand (the
  bump script skipped the vendored path).
- **Versioned dev-deps are validated against the index even though the path is
  stripped.** A *circular* dev-dep then can't be satisfied: `mentat_query_projector_traits`
  dev-depends on `mentat_query_projector`, which build-depends back on it. Make
  such circular dev-deps **path-only (no version)** so cargo strips them without
  an index lookup.
- **`description` is required.** `mentatdb` (formerly `mentat`) never had one;
  crates.io rejects a publish with no description.
- **cargo-deny + a self-referential renamed dev-dep panics.** The vendored `gc`
  crate's self dev-dep (`gc = { package = "mino_gc", path = "." }`) made
  `krates` (cargo-deny) hit `unreachable!`. Fixed by giving `gc` an explicit
  `[lib] name = "gc"` so the self-reference resolves to the library name.

## Verify

A clean consumer must build against crates.io alone:
```sh
cargo new /tmp/c && cd /tmp/c
echo 'mentatdb = "1.12"' >> Cargo.toml
# use mentat::{Store, Queryable, QueryResults}; open "", transact, q_once
cargo run
```
