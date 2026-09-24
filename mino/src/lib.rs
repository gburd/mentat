//! mino-rs: a pure-Rust port of the mino Clojure-dialect interpreter.
//! See docs/plan/mino-rs-port.md for the phased plan and scope.

// gc_derive 0.5's Trace/Finalize macros emit impls inside an anonymous const,
// tripping the non_local_definitions lint. Third-party codegen, not our code.
#![allow(unknown_lints, non_local_definitions)]

pub mod collections;
pub mod symbol;
pub mod value;

pub mod printer;
pub mod reader;

pub mod env;
pub mod error;
pub mod eval;
pub mod prim;
pub mod store;

pub mod corpus;
pub mod embed;

// Host-facing re-exports: a downstream crate embeds via `mino_rs::Interpreter`
// and constructs values/prims with these types without reaching into modules.
pub use embed::Interpreter;
pub use eval::Interp;
pub use error::Throw;
pub use value::{PrimFn, Value};
pub use symbol::Symbol;
pub use value::PrimClosure;

// A host embedding constructs collection `Value`s (`Value::Str(Gc::new(..))`,
// `Value::Map(Gc::new(..))`, ...) when bridging its own data back into the
// language. Re-exported so the host uses `mino_rs::Gc` without depending on
// the exact `gc` crate version this port pins.
pub use gc::Gc;
