# mentat documentation

This directory is an [mdBook](https://rust-lang.github.io/mdBook/). It documents
both storage backends of the merged repository:

- the **`pg_mentat` PostgreSQL extension** (getting started, Datalog, pull API,
  time travel, the cookbook, SQL/GUC reference, deployment), and
- the **embedded (SQLite) `mentat`** store (`src/embedded/`).

Build it:

```sh
mdbook build docs      # output in docs/book/
mdbook serve docs      # live preview
```

Preserved alongside the book (not part of it): `superpowers/` (merge plans),
`RESUME-CHECKPOINT.md`, `mino-integration-plan.md`, `pg_mentat-port-inventory.md`,
and `CONTRIBUTING.md`.
