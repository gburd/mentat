//! mino-rs: a pure-Rust port of the mino Clojure-dialect interpreter.
//! See docs/plan/mino-rs-port.md for the phased plan and scope.

// gc_derive 0.5's Trace/Finalize macros emit impls inside an anonymous const,
// tripping the non_local_definitions lint. Third-party codegen, not our code.
#![allow(unknown_lints, non_local_definitions)]

pub mod symbol;
pub mod value;

pub mod printer;
pub mod reader;

pub mod env;
pub mod error;
pub mod eval;
