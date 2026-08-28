//! Interned symbols/keywords with namespaced names.
//!
//! Split rules mirror Mentat's `edn/namespaceable_name.rs`: a name is always
//! non-empty, and a present namespace is non-empty. Keywords reuse `Symbol`
//! (the `Value` variant distinguishes them).

use std::fmt;
use std::rc::Rc;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Symbol {
    pub ns: Option<Rc<str>>,
    pub name: Rc<str>,
}

impl Symbol {
    pub fn plain(name: &str) -> Self {
        assert!(!name.is_empty(), "Symbols and keywords cannot be unnamed.");
        Self { ns: None, name: name.into() }
    }

    pub fn namespaced(ns: &str, name: &str) -> Self {
        assert!(!name.is_empty(), "Symbols and keywords cannot be unnamed.");
        assert!(!ns.is_empty(), "Symbols and keywords cannot have an empty namespace.");
        Self { ns: Some(ns.into()), name: name.into() }
    }
}

impl fmt::Display for Symbol {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match &self.ns {
            Some(ns) => write!(f, "{ns}/{}", self.name),
            None => write!(f, "{}", self.name),
        }
    }
}

// Symbol holds only Rc<str>, no Gc pointers -> empty trace.
// (gc 0.5's `unsafe_empty_trace!` takes no type arg, so impl by hand.)
impl gc::Finalize for Symbol {}
unsafe impl gc::Trace for Symbol {
    gc::unsafe_empty_trace!();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symbol_display_and_split() {
        assert_eq!(Symbol::plain("foo").to_string(), "foo");
        let s = Symbol::namespaced("mino.store", "open");
        assert_eq!(s.to_string(), "mino.store/open");
        assert_eq!(s.ns.as_deref(), Some("mino.store"));
        assert_eq!(&*s.name, "open");
    }
}
