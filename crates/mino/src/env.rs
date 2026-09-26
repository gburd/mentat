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
    /// Namespace aliases (`alias -> target-ns`), e.g. `str -> clojure.string`
    /// from `(require '[clojure.string :as str])`. Only the root frame carries
    /// them; namespaced-symbol lookup retargets an aliased ns before the
    /// bare-name fallback. Ports the per-ns alias table (`ns_env.c`) as a
    /// single flat map, enough for the flat-env resolution the port uses.
    aliases: GcCell<HashMap<String, String>>,
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
            aliases: GcCell::new(HashMap::new()),
        }))
    }

    /// A child frame whose parent is this one.
    pub fn child(&self) -> Self {
        Env(Gc::new(EnvInner {
            parent: Some(self.0.clone()),
            bindings: GcCell::new(HashMap::new()),
            aliases: GcCell::new(HashMap::new()),
        }))
    }

    /// Look up `sym`, walking the parent chain. `None` if unbound anywhere.
    /// A namespaced symbol resolves in order: exact `ns/name`; then, if `ns`
    /// is a registered alias, `target-ns/name`; then the bare name. The port
    /// has no per-ns var tables, so most namespaced spellings collapse to the
    /// bare name, but aliased qualified names (`str/replace` ->
    /// `clojure.string/replace`) must reach their qualified binding *before*
    /// the bare fallback, since bare `replace` is clojure.core's collection fn.
    pub fn get(&self, sym: &Symbol) -> Option<Value> {
        if let Some(v) = self.get_exact(sym) {
            return Some(v);
        }
        if let Some(ns) = &sym.ns {
            if let Some(target) = self.alias_target(ns) {
                if let Some(v) = self.get_exact(&Symbol::namespaced(&target, &sym.name)) {
                    return Some(v);
                }
            }
            // Bare-name fallback for a QUALIFIED symbol resolves against the
            // ROOT frame only (namespace-level defs/prims), never local `let`/
            // `fn` frames: `mino.store/schema` must reach the store fn even
            // when a local `let [schema ...]` shadows the bare name. A local
            // binding is unqualified by construction, so a qualified read can
            // never mean it.
            return self.root_get_exact(&Symbol::plain(&sym.name));
        }
        None
    }

    /// Record `alias -> target` on the root frame.
    pub fn alias(&self, alias: &str, target: &str) {
        let mut cur = &self.0;
        while let Some(p) = &cur.parent {
            cur = p;
        }
        cur.aliases
            .borrow_mut()
            .insert(alias.to_string(), target.to_string());
    }

    /// Resolve an alias to its target ns (root frame), if any.
    fn alias_target(&self, alias: &str) -> Option<String> {
        let mut cur = &self.0;
        loop {
            if let Some(t) = cur.aliases.borrow().get(alias) {
                return Some(t.clone());
            }
            match &cur.parent {
                Some(p) => cur = p,
                None => return None,
            }
        }
    }

    fn get_exact(&self, sym: &Symbol) -> Option<Value> {
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

    /// Look up `sym` in the ROOT frame only (skip all local frames). The
    /// bare-name fallback for a qualified symbol uses this so a local
    /// shadow can't capture a namespace-qualified read.
    fn root_get_exact(&self, sym: &Symbol) -> Option<Value> {
        let mut cur = &self.0;
        while let Some(p) = &cur.parent {
            cur = p;
        }
        cur.bindings.borrow().get(sym).cloned()
    }

    /// True if `sym` is bound in a NON-root frame (a lexical local). Used by
    /// syntax-quote to leave macro-local args (let/fn bindings) unqualified.
    pub fn is_local(&self, sym: &Symbol) -> bool {
        let mut cur = &self.0;
        loop {
            match &cur.parent {
                // Root frame reached: bindings here are defs/prims, not locals.
                None => return false,
                Some(p) => {
                    if cur.bindings.borrow().contains_key(sym) {
                        return true;
                    }
                    cur = p;
                }
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
        assert!(matches!(
            child.get(&Symbol::plain("x")),
            Some(Value::Int(1))
        ));
        // local shadow does not leak to parent
        child.set(Symbol::plain("x"), Value::Int(2));
        assert!(matches!(
            child.get(&Symbol::plain("x")),
            Some(Value::Int(2))
        ));
        assert!(matches!(root.get(&Symbol::plain("x")), Some(Value::Int(1))));
        // unbound
        assert!(root.get(&Symbol::plain("nope")).is_none());
    }
}
