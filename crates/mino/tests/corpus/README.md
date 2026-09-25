# Conformance corpus

The `.clj` files here are the test suite of the original mino interpreter
(https://github.com/leifericf/mino), copied verbatim at commit `ead6e160`,
the version `mino-rs` was ported from. `tests/conformance.rs` runs each file
against this crate. They are MIT-licensed (see `LICENSE`).

Upstream mino is archived. These files are ours to maintain now: fix a test
here when it encodes a bug, and add tests for bugs we find.
