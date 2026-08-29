//! Datomic-in-Clojure-style scripting layer for Mentat.
//!
//! This module wraps the embedded [`mino_rs`] interpreter and exposes it to
//! Mentat under the `mentat.*` namespaces — the user writes `mentat.store/...`
//! (and `mentat/...`), which is the naming this project standardizes on, even
//! though the bundled Clojure library inside mino-rs stays `mino.store` so the
//! vendored corpus remains bit-identical to upstream mino. The `mentat.*` names
//! are thin host-side namespace aliases, registered on construction.
//!
//! The layer currently runs on mino-rs's own in-process EAVT store, so
//! `mentat.store/open`/`transact`/`read`/`q` operate on that store, not on
//! Mentat's SQLite-backed engine.
//!
//! Gated behind the `mino` feature; nothing here is compiled for a default
//! (pure-Rust, mino-off) Mentat build.

/// A Mentat scripting interpreter: an embedded mino-rs interpreter with the
/// `mentat.store` / `mentat` namespaces aliased onto the bundled `mino.store` /
/// `mino` namespaces.
pub struct Interpreter {
    inner: mino_rs::Interpreter,
}

impl Interpreter {
    /// A fresh scripting interpreter with `mentat.store` / `mentat` aliased so
    /// users write `mentat.store/...`.
    pub fn new() -> Self {
        let mut inner = mino_rs::Interpreter::new();
        inner.alias_namespace("mentat.store", "mino.store");
        inner.alias_namespace("mentat", "mino");
        // TODO(next): bind mentat.store prims to mentat::Store — replace the
        // in-process EAVT store with Mentat's SQLite engine via
        // `inner.register_prim(...)`. Not implemented in this task.
        Interpreter { inner }
    }

    /// Eval a source string; the error is the printed exception.
    pub fn eval(&mut self, src: &str) -> Result<mino_rs::Value, String> {
        self.inner.eval(src)
    }

    /// Eval a source string and return `pr-str` of the result (EDN text).
    pub fn eval_to_string(&mut self, src: &str) -> Result<String, String> {
        self.inner.eval_to_string(src)
    }

    /// The underlying mino-rs interpreter, for host extensions.
    pub fn inner(&mut self) -> &mut mino_rs::Interpreter {
        &mut self.inner
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}
