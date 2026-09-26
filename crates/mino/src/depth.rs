//! The maximum nesting depth mino accepts in reading, printing, and comparing
//! data. Recursion over deeply nested `Value`s (a reader parsing `[[[…]]]`, the
//! printer walking a deep tree, structural `=`/`hash`/`compare`) uses Rust
//! stack proportional to the nesting, so an unbounded depth overflows the stack
//! and aborts the process — unacceptable when scripts come from untrusted
//! callers (`mentat_eval`).
//!
//! 512 is well below the depth at which any of these paths overflow a 1 MB
//! stack in a debug build (the shallowest, the reader, handles ~2000), and far
//! above any nesting real data uses.
pub const MAX_DATA_DEPTH: usize = 512;
