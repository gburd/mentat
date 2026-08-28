//! Lexical environment: a `Gc<EnvInner>` frame with an optional parent link
//! and a mutable `HashMap<Symbol, Value>` of bindings. Ports the frame model
//! in `src/runtime/env.c` (namespace vars / interning come in later phases).

use crate::symbol::Symbol;
use crate::value::Value;
use gc::{Finalize, Gc, GcCell, Trace};
use std::collections::HashMap;

#[derive(Trace, Finalize)]
pub struct EnvInner {
    parent: Option<Gc<EnvInner>>,
    bindings: GcCell<HashMap<Symbol, Value>>,
}

// HashMap<Symbol, Value>: Symbol is empty-trace, Value derives Trace, so gc's
// blanket HashMap Trace impl applies. GcCell gives interior mutability.

#[derive(Trace, Finalize, Clone)]
pub struct Env(Gc<EnvInner>);

impl Env {
    /// A fresh top-level frame with no parent.
    pub fn root() -> Self {
        Env(Gc::new(EnvInner {
            parent: None,
            bindings: GcCell::new(HashMap::new()),
        }))
    }

    /// A child frame whose parent is this one.
    pub fn child(&self) -> Self {
        Env(Gc::new(EnvInner {
            parent: Some(self.0.clone()),
            bindings: GcCell::new(HashMap::new()),
        }))
    }

    /// Look up `sym`, walking the parent chain. `None` if unbound anywhere.
    pub fn get(&self, sym: &Symbol) -> Option<Value> {
        let mut cur = &self.0;
        loop {
            if let Some(v) = cur.bindings.borrow().get(sym) {
                return Some(v.clone());
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => return None,
            }
        }
    }

    /// Bind `sym` in THIS frame (does not touch parents).
    pub fn set(&self, sym: Symbol, val: Value) {
        self.0.bindings.borrow_mut().insert(sym, val);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_walks_parent_chain_set_is_local() {
        let root = Env::root();
        root.set(Symbol::plain("x"), Value::Int(1));
        let child = root.child();
        // inherited from parent
        assert!(matches!(child.get(&Symbol::plain("x")), Some(Value::Int(1))));
        // local shadow does not leak to parent
        child.set(Symbol::plain("x"), Value::Int(2));
        assert!(matches!(child.get(&Symbol::plain("x")), Some(Value::Int(2))));
        assert!(matches!(root.get(&Symbol::plain("x")), Some(Value::Int(1))));
        // unbound
        assert!(root.get(&Symbol::plain("nope")).is_none());
    }
}
