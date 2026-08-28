//! The tagged `Value` enum: mino's runtime value representation.
//! Ports `src/values/layout.h` + `val.c`. Immediates + heap `Gc<T>` cells.
//! Later phases extend this enum (Vector/Map/Set/Fn/Prim/Handle).

use crate::eval::func::Closure;
use gc::{Finalize, Gc, Trace};

#[derive(Trace, Finalize, Clone)]
pub enum Value {
    Nil,
    Bool(bool),
    Int(i64),
    Float(f64),
    Char(char),
    Str(Gc<String>),
    Sym(crate::symbol::Symbol),
    Keyword(crate::symbol::Symbol),
    Cons(Gc<(Value, Value)>),
    // Persistent vector: 32-way trie (see collections::vector::PVec).
    Vector(Gc<crate::collections::vector::PVec>),
    // Phase 2: replace Vec-of-pairs backing with a HAMT. Insertion order is
    // preserved for printing until then. (Set dedup on read: not yet — Phase 2.)
    Map(Gc<Vec<(Value, Value)>>),
    Set(Gc<Vec<Value>>),
    // Closures (`fn`) and built-in primitives. Ports MINO_FN / native prim.
    Fn(Gc<Closure>),
    Prim(Prim),
    // A namespace var, as returned by `def`. Prints `#'ns/name`. The full
    // var cell (root binding, metadata, dynamic) lands with namespaces in
    // Phase 4; Task 1.2 only needs its identity for def's return value.
    Var(crate::symbol::Symbol),
    // later phases extend this enum
}

/// A native primitive: a Rust fn pointer plus its name (for printing).
pub type PrimFn = fn(&mut crate::eval::Interp, &[Value]) -> Result<Value, crate::error::Throw>;

/// Newtype wrapping a `PrimFn` so `Value` can derive `Trace`: a bare fn
/// pointer holds no Gc roots, so its trace is empty.
#[derive(Clone, Copy)]
pub struct Prim(pub PrimFn, pub &'static str);

impl Finalize for Prim {}
unsafe impl Trace for Prim {
    gc::unsafe_empty_trace!();
}

impl Value {
    /// Clojure truthiness: only `nil` and `false` are falsy. 0, "", empty
    /// collections are all truthy. Ports `MINO_IS_NIL`/bool semantics.
    pub fn is_truthy(&self) -> bool {
        !matches!(self, Value::Nil | Value::Bool(false))
    }
}

#[cfg(test)]
fn gc_str(s: &str) -> Gc<String> {
    Gc::new(s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truthiness_matches_clojure() {
        assert!(!Value::Nil.is_truthy());
        assert!(!Value::Bool(false).is_truthy());
        assert!(Value::Bool(true).is_truthy());
        assert!(Value::Int(0).is_truthy()); // 0 is truthy in Clojure
        assert!(Value::Str(gc_str("")).is_truthy()); // "" is truthy
    }
}
