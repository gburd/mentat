//! The tagged `Value` enum: mino's runtime value representation.
//! Ports `src/values/layout.h` + `val.c`. Immediates + heap `Gc<T>` cells.
//! Later phases extend this enum (Vector/Map/Set/Fn/Prim/Handle).

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
    // later phases extend this enum
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
