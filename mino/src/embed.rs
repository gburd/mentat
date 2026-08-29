//! Host embedding API.
//!
//! A thin, documented facade over [`crate::eval::Interp`] for a host program
//! (Mentat, pg_mentat) that wants to embed the interpreter. The natural ABI at
//! this boundary is EDN text: the host sends source strings and reads back
//! `pr-str` output, so both sides speak the same printed representation the
//! language uses internally.
//!
//! The host can extend the running interpreter three ways:
//!   * [`Interpreter::register_prim`] — add a native Rust primitive (this is
//!     how a host later exposes its own storage engine to the language);
//!   * [`Interpreter::alias_namespace`] — make one namespace resolve to
//!     another (e.g. expose the bundled `mino.store` as `mentat.store`);
//!   * [`Interpreter::def_global`] — inject a value into the root env.

use crate::env::Env;
use crate::eval::Interp;
use crate::printer::print_str;
use crate::symbol::Symbol;
use crate::value::{Prim, Value};

pub use crate::value::PrimFn;

/// An embedded mino interpreter.
///
/// [`Interpreter::new`] builds a fresh interpreter with `core.clj` and every
/// bundled library (`clojure.string`/`set`/`instant`, regex, atoms, and the
/// `mino.store` EAVT store) loaded, ready to [`eval`](Interpreter::eval).
pub struct Interpreter {
    it: Interp,
}

impl Interpreter {
    /// A fresh interpreter: primitives installed and all bundled Clojure libs
    /// (incl. `mino.store`) loaded.
    pub fn new() -> Self {
        Interpreter { it: Interp::new() }
    }

    /// Eval a source string (one or more forms) and return the value of the
    /// last form. On a thrown exception the error is the printed exception
    /// (mino's `pr-str` of the diagnostic), via [`crate::error::Throw`]'s
    /// `Display`.
    pub fn eval(&mut self, src: &str) -> Result<Value, String> {
        self.it.eval_str(src).map_err(|t| t.to_string())
    }

    /// Eval a source string and return `pr-str` of the result — the EDN text
    /// that is this API's natural boundary type.
    pub fn eval_to_string(&mut self, src: &str) -> Result<String, String> {
        self.eval(src).map(|v| print_str(&v))
    }

    /// Register a native primitive under `name`. A `ns/name` spelling binds a
    /// namespaced key; a bare name binds a bare key (matching the internal
    /// prim registration). This is how a host injects its own native
    /// functions (e.g. a SQLite-backed store) into the language.
    pub fn register_prim(&mut self, name: &str, f: PrimFn) {
        // `Prim` prints its name and wants `&'static str`; prims are registered
        // once at setup and few in number, so leaking the name string is the
        // lazy, correct choice. ponytail: leaked name string; intern if a host
        // ever registers prims in an unbounded loop.
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        let sym = match name.rsplit_once('/') {
            Some((ns, n)) if !ns.is_empty() && !n.is_empty() => Symbol::namespaced(ns, n),
            _ => Symbol::plain(name),
        };
        self.root().set(sym, Value::Prim(Prim(f, leaked)));
    }

    /// Register a namespace alias so `alias/foo` resolves to `target/foo`.
    /// Backed by the flat-env ns-fallback already used for `clojure.string` →
    /// `str`. E.g. `alias_namespace("mentat.store", "mino.store")` makes
    /// `(mentat.store/open)` reach the bundled store.
    pub fn alias_namespace(&mut self, alias: &str, target: &str) {
        self.root().alias(alias, target);
    }

    /// Bind `v` under `name` in the root env, for a host to inject data.
    pub fn def_global(&mut self, name: &str, v: Value) {
        self.root().set(Symbol::plain(name), v);
    }

    /// The underlying [`Interp`], for hosts that need the lower-level API.
    pub fn interp(&mut self) -> &mut Interp {
        &mut self.it
    }

    fn root(&self) -> Env {
        self.it.root.clone()
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_to_string_basic() {
        let mut it = Interpreter::new();
        assert_eq!(it.eval_to_string("(+ 1 2)").unwrap(), "3");
        assert_eq!(it.eval_to_string("(map inc [1 2 3])").unwrap(), "(2 3 4)");
    }

    #[test]
    fn eval_error_is_printed_exception() {
        let mut it = Interpreter::new();
        let err = match it.eval("(this-is-unbound)") {
            Err(e) => e,
            Ok(_) => panic!("expected an error"),
        };
        assert!(err.contains("unbound symbol"), "got: {err}");
    }

    /// After aliasing, `mentat.store/...` reaches the bundled `mino.store` and
    /// a transact/read round-trips. Mirrors the store smoke test.
    #[test]
    fn alias_namespace_makes_mentat_store_work() {
        let mut it = Interpreter::new();
        it.alias_namespace("mentat.store", "mino.store");
        assert_eq!(
            it.eval_to_string("(mentat.store/store? (mentat.store/open))")
                .unwrap(),
            "true"
        );
        let got = it
            .eval_to_string(
                "(def c (mentat.store/open)) \
                 (mentat.store/transact c {:alice {:name \"Alice\" :age 30}}) \
                 (mentat.store/read (mentat.store/db c) :alice :age)",
            )
            .unwrap();
        assert_eq!(got, "30");
    }

    #[test]
    fn register_prim_host_echo() {
        fn host_echo(
            _it: &mut Interp,
            args: &[Value],
        ) -> Result<Value, crate::error::Throw> {
            Ok(args.first().cloned().unwrap_or(Value::Nil))
        }
        let mut it = Interpreter::new();
        it.register_prim("host-echo", host_echo);
        assert_eq!(it.eval_to_string("(host-echo 42)").unwrap(), "42");
        assert_eq!(it.eval_to_string("(host-echo :hi)").unwrap(), ":hi");
    }

    #[test]
    fn def_global_injects_value() {
        let mut it = Interpreter::new();
        it.def_global("injected", Value::Int(99));
        assert_eq!(it.eval_to_string("injected").unwrap(), "99");
    }
}
