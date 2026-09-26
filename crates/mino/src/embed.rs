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
use crate::value::{Prim, PrimClosure, Value};
use gc::Gc;

use crate::error::Throw;
pub use crate::value::PrimFn;

/// Resource limits for one top-level eval. `None` = unlimited. The budget
/// (steps, heap) resets at each top-level [`Interpreter::eval`]; exceeding
/// any limit aborts that eval with an UNCATCHABLE
/// `{:mino/kind :eval/limit :mino/code "MLM001" :mino/data {:limit :steps|:heap|:depth ..}}`
/// (a `try`/`catch` inside the script cannot swallow it).
///
/// * `steps` — eval steps, plus one per element produced by a bulk prim.
/// * `heap_bytes` — bytes charged BEFORE bulk allocations (`range`, `vec`,
///   `into`, `concat`, `str`, print capture, ...), cumulative per eval.
/// * `depth` — nesting of eval + fn-application frames. This is what turns
///   runaway non-tail recursion into an error instead of a Rust stack
///   overflow (which aborts the whole process).
///
/// # Choosing `depth`
///
/// Measured on x86_64 (rustc 1.9x), bisecting the largest `depth` that
/// still returns a limit error instead of overflowing, per thread stack size:
///
/// | build   | simple fn `(inc (f (dec n)))` | worst seen (destructuring + let + try + macro) |
/// |---------|-------------------------------|-----------------------------------------------|
/// | debug   | ~1.95 KB per depth unit       | ~3.2 KB per depth unit                        |
/// | release | ~0.32 KB per depth unit       | ~0.74 KB per depth unit                       |
///
/// One user-level recursive call costs ~4 depth units (eval of the call,
/// apply, the body's `if`, the enclosing call it is an argument of), i.e.
/// ~7.8 KB (debug) / ~1.3 KB (release) of stack per mino call level.
///
/// Recommended safe `depth` (worst case with ~2x headroom for the host's
/// own frames): **2 MB stack: 300 debug / 1400 release; 8 MB stack: 1000
/// debug / 5000 release.** [`Interpreter::sandboxed`] sets no limits.
///
/// Out of scope here: deeply nested *data* (reader, printer, `=`, GC drop)
/// still recurses without a depth check.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub steps: Option<u64>,
    pub heap_bytes: Option<u64>,
    pub depth: Option<u32>,
}

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

    /// A fresh interpreter with NO host access: the filesystem prims
    /// (`slurp`, `spit`, `rm-rf`, `mkdir-p`, `file-exists?`) are unbound,
    /// durable (path) `mino.store/open` throws, and `print`/`println`/`prn`
    /// output is captured (read it with [`take_output`](Interpreter::take_output)).
    /// No limits are set; the host decides via [`set_limits`](Interpreter::set_limits).
    pub fn sandboxed() -> Self {
        Interpreter {
            it: Interp::new_sandboxed(),
        }
    }

    /// Set the resource limits applied to each subsequent top-level eval.
    pub fn set_limits(&mut self, limits: Limits) {
        self.it.limits = limits;
    }

    /// Called every 4096 eval steps and each time eval depth crosses a
    /// multiple of 64. Returning Err aborts the current top-level eval with
    /// that Throw's message (uncatchable, like a limit). Use it for host
    /// cancellation / wall-clock timeouts.
    pub fn set_check_hook(&mut self, hook: Box<dyn FnMut() -> Result<(), Throw>>) {
        self.it.check_hook = Some(hook);
    }

    /// Sandboxed interpreters capture print output instead of writing stdout.
    /// Returns and clears it (always `""` for a non-sandboxed interpreter).
    pub fn take_output(&mut self) -> String {
        self.it.out.as_mut().map(std::mem::take).unwrap_or_default()
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

    /// Register a native primitive that CAN capture host state (a closure),
    /// unlike [`register_prim`](Interpreter::register_prim) which takes a bare
    /// fn pointer. This is how a host exposes a *stateful* engine to the
    /// language: the closure can hold e.g. an `Rc<RefCell<mentat::Store>>` and
    /// each call reads/mutates it. `name` may be namespaced (`ns/foo`); a
    /// namespaced name binds a namespaced key, a bare name a bare key.
    pub fn register_prim_fn<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&mut Interp, &[Value]) -> Result<Value, crate::error::Throw> + 'static,
    {
        let sym = match name.rsplit_once('/') {
            Some((ns, n)) if !ns.is_empty() && !n.is_empty() => Symbol::namespaced(ns, n),
            _ => Symbol::plain(name),
        };
        let pc = PrimClosure {
            f: Box::new(f),
            name: name.to_string(),
        };
        self.root().set(sym, Value::PrimClosure(Gc::new(pc)));
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
        fn host_echo(_it: &mut Interp, args: &[Value]) -> Result<Value, crate::error::Throw> {
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

    #[test]
    fn register_prim_fn_captures_host_state() {
        // The capability the Mentat SQLite bridge needs: a prim closing over
        // mutable host state. Here a shared counter stands in for a Store.
        use std::cell::RefCell;
        use std::rc::Rc;
        let counter = Rc::new(RefCell::new(0i64));
        let mut it = Interpreter::new();
        let c1 = counter.clone();
        it.register_prim_fn("host/bump", move |_it, args| {
            let by = match args.first() {
                Some(Value::Int(n)) => *n,
                _ => 1,
            };
            *c1.borrow_mut() += by;
            Ok(Value::Int(*c1.borrow()))
        });
        assert_eq!(it.eval_to_string("(host/bump 5)").unwrap(), "5");
        assert_eq!(it.eval_to_string("(host/bump 3)").unwrap(), "8");
        // The captured state is visible outside the interpreter too.
        assert_eq!(*counter.borrow(), 8);
        // A closure-prim is a fn and callable via higher-order fns.
        assert_eq!(it.eval_to_string("(fn? host/bump)").unwrap(), "true");
        assert_eq!(
            it.eval_to_string("(map host/bump [1 1 1])").unwrap(),
            "(9 10 11)"
        );
    }
}
