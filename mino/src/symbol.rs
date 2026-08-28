//! Interned symbols/keywords with namespaced names.
//!
//! Minimal placeholder for Task 0.1 so `value.rs` compiles; Task 0.2 fleshes
//! this out (namespaced ctor, Display, split rules).

use std::rc::Rc;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub ns: Option<Rc<str>>,
    pub name: Rc<str>,
}

impl Symbol {
    pub fn plain(name: &str) -> Self {
        Self { ns: None, name: name.into() }
    }
}

// Symbol holds only Rc<str>, no Gc pointers -> empty trace.
// (gc 0.5's `unsafe_empty_trace!` takes no type arg, so impl by hand.)
impl gc::Finalize for Symbol {}
unsafe impl gc::Trace for Symbol {
    gc::unsafe_empty_trace!();
}
