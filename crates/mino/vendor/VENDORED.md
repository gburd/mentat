# Vendored crates

`gc/` and `gc_derive/` are the `rust-gc` tracing garbage collector,
<https://github.com/Manishearth/rust-gc>, tag `v0.5.1` (commit `292287d`),
identical to the `gc 0.5.1` / `gc_derive 0.5.0` crates on crates.io.

License: **Mozilla Public License 2.0** (`LICENSE`, copied from the upstream
repository; the crates.io package does not include it). The MPL is a file-level
copyleft: the files in `gc/` and `gc_derive/` stay under the MPL-2.0, including
our modifications to them, and their source must remain available. It places no
requirement on the rest of this repository, which keeps its own licenses.

## Why vendored

The upstream collector marks through `Trace::trace` recursively, so marking a
long chain of `Gc` objects uses Rust stack proportional to the chain's length.
`mino` builds every list as a chain of cons cells, so a live list of a few
thousand elements overflowed the stack during a collection in a debug build,
and nested data a few hundred thousand deep did so in release builds. We change
marking to use an explicit worklist. See `CHANGES.md` for every modification.
