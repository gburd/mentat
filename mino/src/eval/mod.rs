//! Eval front door. Ports `eval/eval.c` (`eval_value`, `eval_implicit_do`) and
//! the `if`/`do`/`quote` inline handlers from `special_registry.c`.
//! Task 1.1 scope: self-eval, symbol lookup, and those three special forms.
//! Fn application (Task 1.2) and the rest of the special forms land later.

use crate::embed::Limits;
use crate::env::Env;
use crate::error::{throw_str, Throw};
use crate::reader::read_all;
use crate::symbol::Symbol;
use crate::value::Value;

pub mod bindings;
pub mod control;
pub mod func;
pub mod special;

pub struct Interp {
    pub root: Env,
    /// Forms from core.clj that failed to read or eval during bootstrap, as
    /// (form-summary, error-message). Inspected by tests to keep the failure
    /// set bounded and confined to expected-deferred categories.
    core_failures: Vec<(String, String)>,
    /// Monotonic per-interpreter store id, incremented by `store-open*` for the
    /// `#store[0xN VAL]` print. Mirrors `mino_state.next_store_id`.
    pub next_store_id: u64,
    /// Resource limits; all `None` (the default) means unlimited. Set by the
    /// host via [`crate::embed::Interpreter::set_limits`].
    pub limits: Limits,
    /// Eval steps (plus elements charged by bulk prims) used by the current
    /// top-level eval. Reset at each top-level `eval_str`.
    pub steps: u64,
    /// Current nesting of `eval` + `func::apply` frames.
    pub depth: u32,
    /// Bytes charged against `limits.heap_bytes` by the current top-level eval
    /// (cumulative allocation, not live size, like upstream mino's GC count).
    pub heap: u64,
    /// Host hook run every 4096 steps and at each 64th depth level.
    pub(crate) check_hook: Option<Box<dyn FnMut() -> Result<(), Throw>>>,
    /// No host access: fs prims unbound, durable store refused.
    pub sandboxed: bool,
    /// Captured print output (`Some` for sandboxed interpreters).
    pub out: Option<String>,
    /// The payload of a tripped limit. While set, every eval step and every
    /// `try` re-raises it (uncatchable) until the top-level eval returns.
    pub(crate) tripped: Option<Value>,
}

impl Interp {
    /// A fresh interpreter with primitives installed AND core.clj loaded.
    pub fn new() -> Self {
        Self::with_host(true)
    }

    /// Like [`Interp::new`] but with no host access: the fs prims
    /// (`file-exists?`, `mkdir-p`, `rm-rf`, `spit`, `slurp`) are not bound,
    /// durable (path) stores are refused, and print output is captured in
    /// `out` instead of going to stdout.
    pub fn new_sandboxed() -> Self {
        Self::with_host(false)
    }

    fn with_host(host: bool) -> Self {
        let mut it = Self::bare(host);
        it.load_core();
        // core.clj redefines map/filter/concat/etc. as lazy seqs the port
        // can't run yet; re-assert the eager prims so the working versions win.
        crate::prim::install_eager_seq_prims(&it.root);
        it.load_supplement();
        it.load_clojure_string();
        it.load_mino_store();
        it
    }

    /// A bare interpreter: primitives only, no core.clj. Used by low-level
    /// unit tests that must not depend on the stdlib bootstrap.
    pub fn new_bare() -> Self {
        Self::bare(true)
    }

    fn bare(host: bool) -> Self {
        let root = Env::root();
        crate::prim::install_core(&root);
        crate::store::install(&root);
        if host {
            crate::store::install_host_fs(&root);
        }
        Interp {
            root,
            core_failures: Vec::new(),
            next_store_id: 0,
            limits: Limits::default(),
            steps: 0,
            depth: 0,
            heap: 0,
            check_hook: None,
            sandboxed: !host,
            out: if host { None } else { Some(String::new()) },
            tripped: None,
        }
    }

    /// Load the bundled `lib/clojure/string.clj` verbatim (copied to
    /// `resources/clojure/string.clj`). It defines blank?/capitalize/escape/
    /// triml/trimr/reverse/index-of/last-index-of/re-quote-replacement on top
    /// of the C string prims. Runs after core.clj + supplement so its
    /// `defn`/`cond`/`loop`/`when-not`/`ex-info` deps resolve. Forms using
    /// regex literals (`split-lines` = `#"\r?\n"`) fail to read and are
    /// skipped by the resilient loader (regex is Task 5.2).
    /// ponytail: bundled lib loaded as data, not rewritten; regex-dependent
    /// forms (split-lines) land with Task 5.2.
    fn load_clojure_string(&mut self) {
        // Capture clojure.core's collection `reverse`/`replace` (a prim and a
        // core.clj fn) before string.clj's bare `(defn reverse/replace ...)`
        // shadow them.
        let core_reverse = self.root.get(&Symbol::plain("reverse"));
        let core_replace = self.root.get(&Symbol::plain("replace"));
        let src = include_str!("../../resources/clojure/string.clj");
        let env = self.root.clone();
        for slot in crate::reader::read_all_resilient(src) {
            if let Ok(form) = slot {
                if let Err(t) = self.eval(&form, &env) {
                    let summary = form_summary(&form);
                    self.core_failures
                        .push((summary, crate::printer::print_str(&t.0)));
                }
            }
        }
        // `reverse`/`replace` are the only clojure.string names that collide
        // with clojure.core. Move the string versions to their qualified keys
        // (reached by the `str` alias) and restore the bare names to the
        // clojure.core versions so `(reverse [..])` / `(replace smap coll)`
        // keep working. Every other clojure.string name is collision-free and
        // resolves via the bare fallback. `replace-first` also gets a qualified
        // copy so `str/replace-first` reaches it.
        if let Some(v) = self.root.get(&Symbol::plain("reverse")) {
            self.root
                .set(Symbol::namespaced("clojure.string", "reverse"), v);
        }
        if let Some(v) = self.root.get(&Symbol::plain("replace")) {
            self.root
                .set(Symbol::namespaced("clojure.string", "replace"), v);
        }
        if let Some(v) = self.root.get(&Symbol::plain("replace-first")) {
            self.root
                .set(Symbol::namespaced("clojure.string", "replace-first"), v);
        }
        if let Some(v) = core_reverse {
            self.root.set(Symbol::plain("reverse"), v);
        }
        if let Some(v) = core_replace {
            self.root.set(Symbol::plain("replace"), v);
        }
        // `(require '[clojure.string :as str])` is a no-op in the flat env, so
        // seed the alias the gate corpus uses. Ports the `:as` alias registered
        // by mino's require.
        self.root.alias("str", "clojure.string");
    }

    /// Load `clojure/instant.clj` (copied verbatim): the ISO-8601 parser
    /// behind the `#inst "..."` reader literal. `#inst "S"` expands to
    /// `(clojure.instant/read-instant-date S)`, which resolves here via the
    /// ns-fallback. Pure Clojure (atop `parse-long`/`subs`/`mod`/`quot`), so
    /// load-as-data is lazier than a native inst prim. store_test's temporal
    /// reads (`as-of`/`since` by `#inst`) need it.
    fn load_clojure_instant(&mut self) {
        let src = include_str!("../../resources/clojure/instant.clj");
        let env = self.root.clone();
        for form in crate::reader::read_all_resilient(src).into_iter().flatten() {
            if let Err(t) = self.eval(&form, &env) {
                let summary = form_summary(&form);
                self.core_failures
                    .push((summary, crate::printer::print_str(&t.0)));
            }
        }
        self.root.alias("instant", "clojure.instant");
    }

    /// Load `clojure/set.clj` (copied verbatim) and register the `set` alias so
    /// `set/union` etc. resolve. `mino.store` does `(require '[clojure.set :as
    /// set])`. It is pure Clojure (union/intersection/difference/select/...
    /// atop `into`/`disj`/`reduce`), so loading it as data is lazier than
    /// porting each fn as a prim. Runs after core.clj + supplement.
    fn load_clojure_set(&mut self) {
        // clojure.set's `select`/`index`/`join` shadow bare names already
        // bound (clojure.string/join, etc.) in the flat env. Capture them,
        // let set.clj overwrite, then move set's public fns to `clojure.set/X`
        // (reached by the `set/X` alias) and restore the shadowed bare names.
        const SET_FNS: &[&str] = &[
            "union",
            "intersection",
            "difference",
            "select",
            "project",
            "rename-keys",
            "rename",
            "index",
            "map-invert",
            "join",
            "subset?",
            "superset?",
        ];
        let shadowed: Vec<(&str, Option<Value>)> = SET_FNS
            .iter()
            .map(|n| (*n, self.root.get(&Symbol::plain(n))))
            .collect();
        let src = include_str!("../../resources/clojure/set.clj");
        let env = self.root.clone();
        for form in crate::reader::read_all_resilient(src).into_iter().flatten() {
            if let Err(t) = self.eval(&form, &env) {
                let summary = form_summary(&form);
                self.core_failures
                    .push((summary, crate::printer::print_str(&t.0)));
            }
        }
        // Publish each set fn under its qualified key, then restore the bare
        // names that existed before (so clojure.string/join etc. survive).
        for (name, prev) in shadowed {
            if let Some(v) = self.root.get(&Symbol::plain(name)) {
                self.root.set(Symbol::namespaced("clojure.set", name), v);
            }
            if let Some(prev) = prev {
                self.root.set(Symbol::plain(name), prev);
            }
        }
        self.root.alias("set", "clojure.set");
    }

    /// Load mino's bundled `lib/mino/store.clj` (copied verbatim) as data on
    /// top of the store C prims (`crate::store::install`) + clojure.set. The
    /// port has a flat env, so every `store/X` used by the corpus resolves via
    /// the `mino.store/X` -> bare-`X` ns-fallback (see `Env::get`). Runs last,
    /// after clojure.set (store.clj `(require '[clojure.set :as set])`). Ports
    /// `mino_install_mino_store` (store.clj loaded as a bundled lib).
    fn load_mino_store(&mut self) {
        self.load_clojure_instant();
        self.load_clojure_set();
        // Capture the clojure.core fns that store.clj's PUBLIC defs would
        // shadow in the flat env (`merge`, `read`). store.clj is namespaced
        // in mino, so bare `merge`/`read` stay clojure.core's; the flat port
        // must restore them after load (same pattern as clojure.string's
        // reverse/replace). store.clj itself captures core's merge as the
        // private `map-merge`, so its internals are unaffected.
        let core_merge = self.root.get(&Symbol::plain("merge"));
        let core_read = self.root.get(&Symbol::plain("read"));
        let src = include_str!("../../resources/mino/store.clj");
        let env = self.root.clone();
        for form in crate::reader::read_all_resilient(src).into_iter().flatten() {
            if let Err(t) = self.eval(&form, &env) {
                let summary = form_summary(&form);
                self.core_failures
                    .push((summary, crate::printer::print_str(&t.0)));
            }
        }
        // Move store's colliding public fns to their qualified keys (reached by
        // `store/merge` / `mino.store/read` via the ns-fallback) and restore
        // the bare names to clojure.core.
        for name in ["merge", "read"] {
            if let Some(v) = self.root.get(&Symbol::plain(name)) {
                self.root.set(Symbol::namespaced("mino.store", name), v);
            }
        }
        if let Some(v) = core_merge {
            self.root.set(Symbol::plain("merge"), v);
        }
        if let Some(v) = core_read {
            self.root.set(Symbol::plain("read"), v);
        }
        // `(require '[mino.store :as store])` in store_test is a flat no-op, so
        // seed the alias the corpus uses.
        self.root.alias("store", "mino.store");
    }

    /// Read and eval mino's bundled `core.clj` (embedded via include_str!) form
    /// by form into the root env. Ports `install_core_mino` (prim/install.c),
    /// but resilient: a form that fails to read or eval is recorded in
    /// `core_failures` and load CONTINUES, so unsupported forms (lazy seqs,
    /// atoms, multimethods, protocols, records, bignum — later phases) do not
    /// block the hundreds of control macros / fns that load fine.
    /// ponytail: resilient load; tighten to abort-on-error once core.clj loads clean.
    pub fn load_core(&mut self) {
        let src = include_str!("../../resources/core.clj");
        let env = self.root.clone();
        for slot in crate::reader::read_all_resilient(src) {
            match slot {
                Err(e) => self
                    .core_failures
                    .push(("<unreadable form>".to_string(), format!("read error: {e}"))),
                Ok(form) => {
                    if let Err(t) = self.eval(&form, &env) {
                        let summary = form_summary(&form);
                        let msg = crate::printer::print_str(&t.0);
                        self.core_failures.push((summary, msg));
                    }
                }
            }
        }
    }

    /// The core.clj bootstrap failures: (form-summary, error). Empty once
    /// core.clj loads clean; until then bounded to expected-deferred features.
    pub fn core_load_report(&self) -> &[(String, String)] {
        &self.core_failures
    }

    /// Eval a small pure-Clojure supplement defining higher-order and
    /// collection fns that mino ships as C prims (comp/partial/complement/
    /// juxt/zipmap/empty/find/some/every?/not-any?) but the port has not
    /// ported as prims. They compose from prims the port already has, so
    /// defining them in Clojure is lazier than re-porting the C closure
    /// builders. Runs after core.clj so it can use its macros/fns.
    /// ponytail: Clojure-defined stand-ins for mino's C higher-order prims;
    /// port to native prims only if a corpus test needs exact prim identity.
    fn load_supplement(&mut self) {
        const SUPPLEMENT: &str = r#"
(defn empty [coll]
  (cond (nil? coll) nil
        (vector? coll) []
        (map? coll) {}
        (set? coll) #{}
        (or (list? coll) (seq? coll)) ()
        :else nil))
(defn comp [& fs]
  (if (= 0 (count fs))
    identity
    (if (= 1 (count fs))
      (first fs)
      (fn [& args]
        (let [rfs (reverse fs)]
          (reduce (fn [acc g] (g acc))
                  (apply (first rfs) args)
                  (rest rfs)))))))
(defn partial [f & bound]
  (fn [& args] (apply f (concat bound args))))
(defn complement [f]
  (fn [& args] (not (apply f args))))
(defn juxt [& fs]
  (fn [& args] (mapv (fn [f] (apply f args)) fs)))
(defn zipmap [ks vs]
  (loop [m {} ks (seq ks) vs (seq vs)]
    (if (and ks vs)
      (recur (assoc m (first ks) (first vs)) (next ks) (next vs))
      m)))
(defn find [m k]
  (if (and (map? m) (contains? m k)) [k (get m k)] nil))
(defn some [pred coll]
  (loop [s (seq coll)]
    (if s
      (or (pred (first s)) (recur (next s)))
      nil)))
(defn every? [pred coll]
  (loop [s (seq coll)]
    (if s
      (if (pred (first s)) (recur (next s)) false)
      true)))
(defn not-any? [pred coll] (not (some pred coll)))
(defn not-every? [pred coll] (not (every? pred coll)))
(defn key [entry] (nth entry 0))
(defn val [entry] (nth entry 1))
(defn take [n coll]
  (loop [acc [] s (seq coll) k n]
    (if (and s (> k 0))
      (recur (conj acc (first s)) (next s) (dec k))
      (seq acc))))
(defn drop [n coll]
  (loop [s (seq coll) k n]
    (if (and s (> k 0)) (recur (next s) (dec k)) s)))
(defn take-while [pred coll]
  (loop [acc [] s (seq coll)]
    (if (and s (pred (first s)))
      (recur (conj acc (first s)) (next s))
      (seq acc))))
(defn drop-while [pred coll]
  (loop [s (seq coll)]
    (if (and s (pred (first s))) (recur (next s)) s)))
(defn second [coll] (first (next coll)))
(defn ffirst [coll] (first (first coll)))
(defn fnext [coll] (first (next coll)))
(defn nnext [coll] (next (next coll)))
(defn nfirst [coll] (next (first coll)))
(defn last [coll]
  (loop [s (seq coll)]
    (if (next s) (recur (next s)) (first s))))
(defn mapcat [f & colls] (apply concat (apply map f colls)))
(defn remove [pred coll] (filter (fn [x] (not (pred x))) coll))
;; The port has no chunked seqs, so chunked-seq? is always false; keep/map/
;; etc. in core.clj then take their per-element branch. (mino ships this as a
;; C prim; a false stub is lazier and correct for the non-chunked port.)
(defn chunked-seq? [x] false)
;; declare: forward-declare names (each becomes an unbound def until its real
;; definition lands). mino ships this as a special form; a def-per-name macro
;; is the lazy stand-in. store.clj uses `(declare pull-entity)`.
(defmacro declare [& names]
  (cons 'do (map (fn [n] (list 'def n)) names)))
"#;
        let env = self.root.clone();
        for slot in crate::reader::read_all_resilient(SUPPLEMENT) {
            if let Ok(form) = slot {
                if let Err(t) = self.eval(&form, &env) {
                    let summary = form_summary(&form);
                    self.core_failures
                        .push((summary, crate::printer::print_str(&t.0)));
                }
            }
        }
    }

    /// Resolve a Var's current value: look up its symbol in the root env,
    /// falling back to the bare name (the flat-env var model). Used by
    /// `deref` on a Var and by calling a Var. Ports var-root-deref.
    pub fn var_value(&self, sym: &Symbol) -> Option<Value> {
        self.root.get(sym)
    }

    /// Read ALL forms from `src`, eval each in the root env, return the last.
    /// Empty input yields `Nil` (mirrors mino's load/eval-string semantics).
    /// At top level (not re-entered from a prim) this starts a fresh
    /// step/heap budget and clears a previous limit trip.
    pub fn eval_str(&mut self, src: &str) -> Result<Value, Throw> {
        if self.depth == 0 {
            self.steps = 0;
            self.heap = 0;
            self.tripped = None;
        }
        let forms = read_all(src).map_err(|e| throw_str(&format!("read error: {e:?}")))?;
        let mut last = Value::Nil;
        let env = self.root.clone();
        for form in &forms {
            last = self.eval(form, &env)?;
        }
        // Safety net: a trip swallowed by some prim still fails the eval.
        match &self.tripped {
            Some(p) => Err(Throw(p.clone())),
            None => Ok(last),
        }
    }

    /// Record a limit trip and return its (uncatchable) throw.
    fn trip(&mut self, payload: Value) -> Throw {
        self.tripped = Some(payload.clone());
        Throw(payload)
    }

    fn trip_limit(&mut self, which: &str, value: u64) -> Throw {
        let msg = match which {
            "steps" => "step limit exceeded".to_string(),
            _ => format!("{which} limit exceeded"),
        };
        self.trip(crate::error::limit_diag(&msg, which, value))
    }

    /// Run the host check hook; an `Err` becomes an uncatchable limit trip
    /// carrying the hook's message.
    fn run_hook(&mut self) -> Result<(), Throw> {
        let Some(hook) = self.check_hook.as_mut() else {
            return Ok(());
        };
        match hook() {
            Ok(()) => Ok(()),
            Err(t) => {
                let msg = crate::error::message_of(&t.0);
                let steps = self.steps;
                Err(self.trip(crate::error::limit_diag(&msg, "hook", steps)))
            }
        }
    }

    /// Charge `elements` against the step budget and `bytes` against the heap
    /// budget BEFORE a bulk allocation, so an argument-driven size (`(range
    /// 1e11)`) fails up front instead of allocating.
    pub(crate) fn charge(&mut self, elements: u64, bytes: u64) -> Result<(), Throw> {
        if let Some(p) = &self.tripped {
            return Err(Throw(p.clone()));
        }
        self.steps = self.steps.saturating_add(elements);
        self.heap = self.heap.saturating_add(bytes);
        if let Some(max) = self.limits.heap_bytes {
            if self.heap > max {
                return Err(self.trip_limit("heap", max));
            }
        }
        if let Some(max) = self.limits.steps {
            if self.steps > max {
                return Err(self.trip_limit("steps", max));
            }
        }
        Ok(())
    }

    /// Charge `n` elements of `Value` (steps + `n * size_of::<Value>()` bytes).
    pub(crate) fn charge_values(&mut self, n: usize) -> Result<(), Throw> {
        let n = n as u64;
        self.charge(n, n.saturating_mul(std::mem::size_of::<Value>() as u64))
    }

    /// Enter one eval/apply frame: re-raise a prior trip, enforce the depth
    /// limit, and run the hook at each 64th level. Pair with `self.depth -= 1`.
    pub(crate) fn enter_frame(&mut self) -> Result<(), Throw> {
        if let Some(p) = &self.tripped {
            return Err(Throw(p.clone()));
        }
        if let Some(max) = self.limits.depth {
            if self.depth >= max {
                return Err(self.trip_limit("depth", max as u64));
            }
        }
        self.depth += 1;
        if self.depth.is_multiple_of(64) {
            if let Err(t) = self.run_hook() {
                self.depth -= 1;
                return Err(t);
            }
        }
        Ok(())
    }

    /// One eval step: count it, enforce the step limit, run the hook every
    /// 4096 steps.
    fn step(&mut self) -> Result<(), Throw> {
        self.steps += 1;
        if let Some(max) = self.limits.steps {
            if self.steps > max {
                return Err(self.trip_limit("steps", max));
            }
        }
        if self.steps.is_multiple_of(4096) {
            self.run_hook()?;
        }
        Ok(())
    }

    /// Evaluate `form` in `env`. Every call is one step and one depth level
    /// (the guard below restores `depth` on every return path).
    pub fn eval(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        self.enter_frame()?;
        let r = match self.step() {
            Ok(()) => self.eval_form(form, env),
            Err(t) => Err(t),
        };
        self.depth -= 1;
        r
    }

    fn eval_form(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        match form {
            // Self-evaluating: scalars plus already-built fns/prims/vars.
            Value::Nil
            | Value::Bool(_)
            | Value::Int(_)
            | Value::Float(_)
            | Value::Float32(_)
            | Value::BigInt(_)
            | Value::Ratio(_)
            | Value::Char(_)
            | Value::Str(_)
            | Value::Keyword(_)
            | Value::Fn(_)
            | Value::Prim(_)
            | Value::PrimClosure(_)
            | Value::Regex(_)
            | Value::Var(_)
            | Value::Atom(_)
            | Value::Store(_) => Ok(form.clone()),

            // A `recur` signal only appears here when re-evaluated as data
            // (it never occurs in source); pass it through so the loop/fn
            // trampoline sees it. Non-tail eval sites use `eval_value`, which
            // rejects it.
            Value::Recur(_) => Ok(form.clone()),

            // The empty list self-evaluates to itself (Clojure: `()` => `()`),
            // it is NOT an empty call.
            Value::EmptyList => Ok(form.clone()),

            // Collection literals evaluate their elements (Clojure semantics:
            // `[a b]` => a vector of the *values* of a and b). Out of line so
            // their temporaries stay off the recursive eval path's stack.
            Value::Vector(_) | Value::Map(_) | Value::Set(_) => self.eval_coll_literal(form, env),

            Value::Sym(sym) => env.get(sym).ok_or_else(|| unbound(sym)),

            Value::Cons(_) => self.eval_list(form, env),
        }
    }

    #[inline(never)]
    fn eval_coll_literal(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        match form {
            // Collection literals evaluate their elements (Clojure semantics:
            // `[a b]` => a vector of the *values* of a and b).
            Value::Vector(v) => {
                let mut out = crate::collections::vector::PVec::empty();
                for e in v.iter() {
                    out = out.conj(self.eval_value(e, env)?);
                }
                Ok(Value::Vector(gc::Gc::new(out)))
            }
            Value::Map(m) => {
                let mut out = crate::collections::map::PMap::empty();
                for (k, val) in m.entries() {
                    let ek = self.eval_value(k, env)?;
                    let ev = self.eval_value(val, env)?;
                    out = out.assoc(ek, ev);
                }
                Ok(Value::Map(gc::Gc::new(out)))
            }
            Value::Set(s) => {
                let mut out = crate::collections::map::PSet::empty();
                for e in s.iter() {
                    out = out.conj(self.eval_value(e, env)?);
                }
                Ok(Value::Set(gc::Gc::new(out)))
            }

            _ => unreachable!("eval_coll_literal on a non-collection"),
        }
    }

    /// Evaluate `form` for its value at a NON-tail position: a stray `recur`
    /// (`Value::Recur`) here is an error ("recur must be in tail position").
    /// Ports mino's `eval_value`.
    pub fn eval_value(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        match self.eval(form, env) {
            Ok(Value::Recur(_)) => Err(recur_not_tail()),
            r => r,
        }
    }

    fn eval_list(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        let Value::Cons(cell) = form else {
            unreachable!("eval_list called on non-cons")
        };
        let (head, rest) = (&cell.0, &cell.1);

        // Special forms dispatch by BARE name. Syntax-quote inside core.clj
        // macros (when/and/or/->/defn...) qualifies core forms to
        // `clojure.core/let` etc.; the port has no ns tables, so accept a
        // `clojure.core` prefix as equivalent to bare so those expansions
        // still route to the special-form handlers. `if` is checked inline
        // (it is on nearly every recursive path); the rest dispatch in
        // `eval_special`, kept out of line to keep this frame small.
        if let Value::Sym(sym) = head {
            if matches!(sym.ns.as_deref(), None | Some("clojure.core")) {
                if &*sym.name == "if" {
                    return self.eval_if(rest, env);
                }
                if let Some(r) = self.eval_special(&sym.name, rest, env) {
                    return r;
                }
            }
        }

        // Macro call: if the head resolves to a macro, expand it once with
        // the UNEVALUATED argument forms and evaluate the result. macroexpand
        // (the repeat-until-not-a-macro loop) falls out naturally: the
        // expansion is re-eval'd, and if its head is another macro this same
        // check fires again. Ports macroexpand1 + the eval-time dispatch.
        if let Some(r) = self.eval_macro_call(form, env) {
            return r;
        }

        // Application: eval the head, then args left-to-right, then apply.
        // Callability (fn/prim/keyword/symbol/map/set/vector as lookup fns)
        // is decided in `func::apply`, the single choke point all calls route
        // through — so the check lives in one place, not here.
        let callee = self.eval_value(head, env)?;
        let mut args = Vec::new();
        let mut cur = rest;
        while let Value::Cons(cell) = cur {
            args.push(self.eval_value(&cell.0, env)?);
            cur = &cell.1;
        }
        func::apply(self, &callee, &args)
    }

    /// If `form`'s head is a macro, expand once and eval the expansion.
    /// Out of line so a plain call's frame does not carry the expansion
    /// temporaries.
    #[inline(never)]
    fn eval_macro_call(&mut self, form: &Value, env: &Env) -> Option<Result<Value, Throw>> {
        match self.macroexpand1_flagged(form) {
            Err(t) => Some(Err(t)),
            Ok((expanded, true)) => Some(self.eval(&expanded, env)),
            Ok((_, false)) => None,
        }
    }

    /// The special forms other than `if`. `None` = not a special form. Out of
    /// line so its many temporaries stay off the hot call path's stack.
    #[inline(never)]
    fn eval_special(
        &mut self,
        name: &str,
        rest: &Value,
        env: &Env,
    ) -> Option<Result<Value, Throw>> {
        let mut special = true;
        let r = (|| -> Result<Value, Throw> {
            match name {
                "do" => self.eval_do(rest, env),
                // `(lazy-seq body...)`: the port has no deferred seqs
                // (Phase 5), so evaluate the body eagerly like an implicit
                // `do`. Correct for finite seqs (re-seq over a string);
                // an infinite lazy-seq would not terminate here.
                // ponytail: eager lazy-seq; real deferral lands in Phase 5.
                "lazy-seq" => self.eval_do(rest, env),
                "quote" => self.eval_quote(rest),
                // `(var sym)` / `#'sym`: return the Var identity for `sym`.
                // The port has a flat env, so a Var just carries the
                // symbol; `deref`/call resolve it in root. Ports the `var`
                // special form.
                "var" => {
                    let (arg, _) = pop(rest);
                    let arg = arg.ok_or_else(|| throw_str("var requires one argument"))?;
                    match &arg {
                        Value::Sym(s) => Ok(Value::Var(s.clone())),
                        _ => Err(throw_str("var requires a symbol")),
                    }
                }
                "def" => {
                    let args = collect(rest);
                    special::eval_def(self, &args, env)
                }
                "defmacro" => {
                    let args = collect(rest);
                    special::eval_defmacro(self, &args, env)
                }
                "quasiquote" => {
                    let (arg, _) = pop(rest);
                    let arg = arg.ok_or_else(|| throw_str("quasiquote requires one argument"))?;
                    self.quasiquote_expand(&arg, env)
                }
                "fn" | "fn*" => {
                    let args = collect(rest);
                    special::eval_fn(self, &args, env)
                }
                "let" | "let*" => {
                    let args = collect(rest);
                    bindings::eval_let(self, &args, env)
                }
                "loop" | "loop*" => {
                    let args = collect(rest);
                    bindings::eval_loop(self, &args, env)
                }
                "try" => {
                    let args = collect(rest);
                    control::eval_try(self, &args, env)
                }
                "letfn*" => {
                    let args = collect(rest);
                    bindings::eval_letfn_star(self, &args, env)
                }
                "recur" => {
                    // Eval args at non-tail (they must be values), then
                    // return the recur signal for the loop/fn trampoline.
                    let mut vals = Vec::new();
                    let mut cur = rest;
                    while let Value::Cons(cell) = cur {
                        vals.push(self.eval_value(&cell.0, env)?);
                        cur = &cell.1;
                    }
                    Ok(Value::Recur(gc::Gc::new(vals)))
                }
                // Namespace / load machinery. The port has a single flat
                // env (no ns tables), so these are no-ops that return nil
                // — enough that core.clj's `(in-ns 'clojure.core)` etc. and
                // any `(ns ..)`/`(require ..)` load without erroring.
                // ponytail: flat-env no-op ns machinery; real ns tables in Phase 4.
                "in-ns" | "ns" | "require" | "use" | "refer" | "refer-clojure" | "load"
                | "load-file" | "import" => Ok(Value::Nil),
                _ => {
                    special = false;
                    Ok(Value::Nil)
                }
            }
        })();
        special.then_some(r)
    }

    /// `(if cond then else?)`: eval cond, then-branch when truthy else
    /// else-branch; a missing else yields nil.
    fn eval_if(&mut self, args: &Value, env: &Env) -> Result<Value, Throw> {
        // Walk the arg list by reference: this frame is on every recursive
        // path, so no clones/temporaries beyond the test value.
        let Value::Cons(c1) = args else {
            return Err(if_too_few());
        };
        let Value::Cons(c2) = &c1.1 else {
            return Err(if_too_few());
        };
        // Condition is a value (non-tail); branches are tail positions and
        // may legitimately produce a `recur` signal, so use plain `eval`.
        if self.eval_value(&c1.0, env)?.is_truthy() {
            self.eval(&c2.0, env)
        } else if let Value::Cons(c3) = &c2.1 {
            self.eval(&c3.0, env)
        } else {
            Ok(Value::Nil)
        }
    }

    /// `(do e1 e2 ... en)`: eval each, return the last; empty yields nil.
    /// Ports `eval_implicit_do`. Non-last forms are values (reject `recur`);
    /// the last form is tail position (propagates a `recur` signal).
    pub fn eval_implicit_do(&mut self, body: &[Value], env: &Env) -> Result<Value, Throw> {
        let Some((last, init)) = body.split_last() else {
            return Ok(Value::Nil);
        };
        for form in init {
            self.eval_value(form, env)?;
        }
        self.eval(last, env)
    }

    /// The `(do ...)` special form: body is the cons tail.
    fn eval_do(&mut self, body: &Value, env: &Env) -> Result<Value, Throw> {
        let forms = collect(body);
        self.eval_implicit_do(&forms, env)
    }

    /// `(quote x)`: return x unevaluated.
    fn eval_quote(&mut self, args: &Value) -> Result<Value, Throw> {
        match args {
            Value::Cons(cell) => Ok(cell.0.clone()),
            _ => Ok(Value::Nil),
        }
    }

    /// Expand `form` once if its head resolves to a macro, else return it
    /// unchanged. Ports `macroexpand1` (eval.c). Uses the root env for macro
    /// lookup (the port binds all macros there). Returns the (possibly
    /// unchanged) form; use [`Interp::macroexpand1_flagged`] for the
    /// expanded/not-expanded distinction.
    pub fn macroexpand1(&mut self, form: &Value) -> Result<Value, Throw> {
        Ok(self.macroexpand1_flagged(form)?.0)
    }

    /// Like [`Interp::macroexpand1`] but also reports whether an expansion
    /// happened, so `macroexpand`'s fixpoint loop terminates without printing.
    pub fn macroexpand1_flagged(&mut self, form: &Value) -> Result<(Value, bool), Throw> {
        let Value::Cons(cell) = form else {
            return Ok((form.clone(), false));
        };
        let Value::Sym(sym) = &cell.0 else {
            return Ok((form.clone(), false));
        };
        let env = self.root.clone();
        if let Some(Value::Fn(ref closure)) = env.get(sym) {
            if closure.is_macro {
                let arg_forms = collect(&cell.1);
                let expanded = func::apply(self, &Value::Fn(closure.clone()), &arg_forms)?;
                return Ok((expanded, true));
            }
        }
        Ok((form.clone(), false))
    }

    /// Expand a syntax-quote template. Ports `quasiquote_expand` (eval.c):
    /// symbols auto-qualify, `(unquote x)` -> eval x, `(unquote-splicing x)`
    /// splices a seq, vectors/maps recurse, everything else is structural.
    /// `foo#` gensym is done in the reader, so those symbols arrive plain.
    pub fn quasiquote_expand(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        match form {
            Value::Sym(sym) => Ok(self.qq_qualify_symbol(sym, env)),
            Value::Vector(v) => {
                let items: Vec<Value> = v.iter().cloned().collect();
                let out = self.qq_expand_seq(&items, env)?;
                let mut pv = crate::collections::vector::PVec::empty();
                for e in out {
                    pv = pv.conj(e);
                }
                Ok(Value::Vector(gc::Gc::new(pv)))
            }
            Value::Map(m) => {
                let mut out = crate::collections::map::PMap::empty();
                for (k, val) in m.entries() {
                    let kk = self.quasiquote_expand(k, env)?;
                    let vv = self.quasiquote_expand(val, env)?;
                    out = out.assoc(kk, vv);
                }
                Ok(Value::Map(gc::Gc::new(out)))
            }
            Value::Cons(_) | Value::EmptyList => self.qq_expand_cons(form, env),
            // Scalars are structural.
            _ => Ok(form.clone()),
        }
    }

    /// Expand a cons-list template. Top-level `(unquote x)` evals x;
    /// `(unquote-splicing x)` at top level is an error. Otherwise walk each
    /// element, splicing `~@` elements. Ports `qq_expand_cons`.
    fn qq_expand_cons(&mut self, form: &Value, env: &Env) -> Result<Value, Throw> {
        if let Value::Cons(cell) = form {
            if let Value::Sym(s) = &cell.0 {
                if s.ns.is_none() && &*s.name == "unquote" {
                    let (arg, _) = pop(&cell.1);
                    let arg = arg.ok_or_else(|| throw_str("unquote requires one argument"))?;
                    return self.eval_value(&arg, env);
                }
                if s.ns.is_none() && &*s.name == "unquote-splicing" {
                    return Err(throw_str("unquote-splicing must appear inside a list"));
                }
            }
        }
        let items = collect(form);
        let out = self.qq_expand_seq(&items, env)?;
        Ok(list_from_slice(&out))
    }

    /// Expand each element of a list/vector template, splicing `~@` elements
    /// (each expected to yield a seqable). Shared by list and vector paths.
    fn qq_expand_seq(&mut self, items: &[Value], env: &Env) -> Result<Vec<Value>, Throw> {
        let mut out = Vec::new();
        for elem in items {
            if let Value::Cons(cell) = elem {
                if let Value::Sym(s) = &cell.0 {
                    if s.ns.is_none() && &*s.name == "unquote-splicing" {
                        let (arg, _) = pop(&cell.1);
                        let arg =
                            arg.ok_or_else(|| throw_str("unquote-splicing requires one argument"))?;
                        let spliced = self.eval_value(&arg, env)?;
                        out.extend(seqable_to_vec(&spliced));
                        continue;
                    }
                }
            }
            out.push(self.quasiquote_expand(elem, env)?);
        }
        Ok(out)
    }

    /// Auto-qualify a bare symbol inside syntax-quote, matching the mino
    /// binary (eval.c `qq_qualify_symbol`), adapted to the port's flat env:
    ///   * namespaced symbols pass through unchanged;
    ///   * `foo__N__auto__` gensyms (reader output) stay bare;
    ///   * true special forms (if/do/def/quote/recur/try/let*/fn*/loop*/...)
    ///     stay bare;
    ///   * public macro-family forms (fn/let/loop/when/and/or/binding/declare/
    ///     defmacro/ns/lazy-seq/cond/->/...) qualify to clojure.core/NAME;
    ///   * symbols bound in the root env (defs/prims) qualify to user/NAME
    ///     UNLESS locally bound in a child frame (macro-local args stay bare);
    ///   * unbound symbols stay bare.
    fn qq_qualify_symbol(&self, sym: &Symbol, env: &Env) -> Value {
        if sym.ns.is_some() {
            return Value::Sym(sym.clone());
        }
        let name = &*sym.name;
        // Reader gensyms and the special `&` marker stay bare.
        if name.ends_with("__auto__") || name == "&" || name == "/" {
            return Value::Sym(sym.clone());
        }
        // Locally bound (a macro's own let/fn args) -> bare.
        if env.is_local(sym) {
            return Value::Sym(sym.clone());
        }
        if is_true_special_form(name) {
            return Value::Sym(sym.clone());
        }
        if is_public_macro_form(name) {
            return Value::Sym(Symbol::namespaced("clojure.core", name));
        }
        // Bound at the root: a built-in prim lives in clojure.core, a user
        // `def`/`defn`/`defmacro` lives in user. mino qualifies each to its
        // owning ns; the port keys everything in one flat env, so we tell
        // them apart by value type (prims are the built-ins). Both spellings
        // resolve by bare-name fallback in Env::get.
        match self.root.get(sym) {
            Some(Value::Prim(_)) => Value::Sym(Symbol::namespaced("clojure.core", name)),
            Some(_) => Value::Sym(Symbol::namespaced("user", name)),
            None => Value::Sym(sym.clone()),
        }
    }
}

impl Default for Interp {
    fn default() -> Self {
        Self::new()
    }
}

#[cold]
#[inline(never)]
fn if_too_few() -> Throw {
    throw_str("if: too few forms")
}

#[cold]
#[inline(never)]
fn recur_not_tail() -> Throw {
    throw_str("recur must be in tail position")
}

/// The "unbound symbol" error, out of line (cold path).
#[cold]
#[inline(never)]
fn unbound(sym: &Symbol) -> Throw {
    crate::error::throw_classified("name", "MNS001", &format!("unbound symbol: {sym}"))
}

/// Split a cons list into (first?, rest). Returns `(None, Nil)` at the end.
fn pop(list: &Value) -> (Option<Value>, Value) {
    match list {
        Value::Cons(cell) => (Some(cell.0.clone()), cell.1.clone()),
        _ => (None, Value::Nil),
    }
}

/// Collect a proper cons list's elements into a Vec (special-form args).
fn collect(list: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    let mut cur = list;
    while let Value::Cons(cell) = cur {
        out.push(cell.0.clone());
        cur = &cell.1;
    }
    out
}

/// Build a proper list from a slice; empty -> `()` (the empty-list value).
fn list_from_slice(items: &[Value]) -> Value {
    let mut acc = Value::EmptyList;
    for e in items.iter().rev() {
        acc = Value::Cons(gc::Gc::new((e.clone(), acc)));
    }
    acc
}

/// A short, one-line summary of a core.clj form for the failure report:
/// `(def-family NAME ...)` or the head symbol, so the failure list is
/// scannable without printing 40-line macro bodies.
fn form_summary(form: &Value) -> String {
    if let Value::Cons(cell) = form {
        if let Value::Sym(head) = &cell.0 {
            // For def-family forms, include the defined name.
            if let Value::Cons(rest) = &cell.1 {
                if let Value::Sym(name) = &rest.0 {
                    return format!("({} {} ...)", head.name, name.name);
                }
            }
            return format!("({} ...)", head.name);
        }
    }
    crate::printer::print_str(form)
}

/// Flatten a seqable value (list/vector/set/nil/empty) into its elements for
/// `~@` splicing. Ports the vector-and-cons walk in `qq_expand_cons`.
fn seqable_to_vec(v: &Value) -> Vec<Value> {
    match v {
        Value::Nil | Value::EmptyList => Vec::new(),
        Value::Cons(_) => collect(v),
        Value::Vector(pv) => pv.iter().cloned().collect(),
        Value::Set(s) => s.iter().cloned().collect(),
        // A non-seq splice arg: treat as a single element (mino would error,
        // but the port has no such path in Task 4.1 usage).
        other => vec![other.clone()],
    }
}

/// True special forms that syntax-quote leaves BARE (matching the mino
/// binary): implementation-surface forms with no clojure.core macro face.
fn is_true_special_form(name: &str) -> bool {
    matches!(
        name,
        "if" | "do"
            | "def"
            | "quote"
            | "quasiquote"
            | "unquote"
            | "unquote-splicing"
            | "var"
            | "recur"
            | "try"
            | "catch"
            | "finally"
            | "throw"
            | "let*"
            | "fn*"
            | "loop*"
            | "letfn*"
            | "monitor-enter"
            | "monitor-exit"
            | "new"
            | "."
            | "set!"
    )
}

/// The public macro-family forms that syntax-quote qualifies to
/// clojure.core/NAME (mino's eval_is_public_form_name plus the core.clj
/// macros that resolve there once core is loaded). Kept in sync with the
/// clojure.core names so syntax-quoted output resolves in Phase 4.2.
fn is_public_macro_form(name: &str) -> bool {
    matches!(
        name,
        "fn" | "let"
            | "loop"
            | "lazy-seq"
            | "binding"
            | "declare"
            | "defmacro"
            | "ns"
            | "when"
            | "and"
            | "or"
    )
}

/// Short type label for a value, matching mino's `type_tag_str` (error.c) for
/// the values the port has so far. Used in "not a function (got TYPE)".
pub fn type_tag_of(v: &Value) -> &'static str {
    type_tag(v)
}

fn type_tag(v: &Value) -> &'static str {
    match v {
        Value::Nil => "nil",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Float32(_) => "float32",
        Value::BigInt(_) => "bigint",
        Value::Ratio(_) => "ratio",
        Value::Char(_) => "char",
        Value::Str(_) => "string",
        Value::Sym(_) => "symbol",
        Value::Keyword(_) => "keyword",
        Value::EmptyList | Value::Cons(_) => "list",
        Value::Vector(_) => "vector",
        Value::Map(_) => "map",
        Value::Set(_) => "set",
        Value::Fn(_) | Value::Prim(_) => "fn",
        Value::PrimClosure(_) => "fn",
        Value::Regex(_) => "regex",
        Value::Var(_) => "var",
        Value::Atom(_) => "atom",
        Value::Store(_) => "store",
        Value::Recur(_) => "recur",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::print_str;

    /// core.clj + the Clojure supplement provide the control macros and
    /// higher-order fns. Each expected value is copied from the mino binary
    /// oracle (`mino -e '...'`). This is the Task 4.2 end-to-end check.
    #[test]
    fn core_macros_work_end_to_end() {
        let mut it = Interp::new();
        let ev = |it: &mut Interp, s: &str| print_str(&it.eval_str(s).unwrap());
        assert_eq!(ev(&mut it, "(when true 1 2)"), "2");
        assert_eq!(ev(&mut it, "(when-not false :yes)"), ":yes");
        assert_eq!(ev(&mut it, "(cond false 1 :else 2)"), "2");
        assert_eq!(ev(&mut it, "(case 2 1 :a 2 :b :c)"), ":b");
        assert_eq!(ev(&mut it, "(-> 5 inc inc)"), "7");
        assert_eq!(ev(&mut it, "(->> [1 2 3] (map inc) (reduce +))"), "9");
        assert_eq!(ev(&mut it, "(if-let [x 5] x :no)"), "5");
        assert_eq!(ev(&mut it, "(when-let [x 5] x)"), "5");
        assert_eq!(ev(&mut it, "(and 1 2 3)"), "3");
        assert_eq!(ev(&mut it, "(or nil false 7)"), "7");
        assert_eq!(ev(&mut it, "(not nil)"), "true");
        // for: eager here, but shape matches the binary's lazy seq.
        assert_eq!(ev(&mut it, "(for [x [1 2 3]] (* x x))"), "(1 4 9)");
        // defn round-trips.
        assert_eq!(ev(&mut it, "(defn sq [x] (* x x)) (sq 6)"), "36");
        // Higher-order fns from the supplement.
        assert_eq!(ev(&mut it, "((partial + 20) 20)"), "40");
        assert_eq!(ev(&mut it, "((complement even?) 3)"), "true");
        assert_eq!(ev(&mut it, "((comp inc inc) 5)"), "7");
        assert_eq!(ev(&mut it, "((juxt :a :b) {:a 1 :b 2})"), "[1 2]");
        assert_eq!(ev(&mut it, "(zipmap [:a :b] [1 2])"), "{:a 1, :b 2}");
        assert_eq!(ev(&mut it, "(empty [1 2])"), "[]");
        // keyword/symbol/map-as-fn callability.
        assert_eq!(ev(&mut it, "(:a {:a 1})"), "1");
        assert_eq!(ev(&mut it, "('inc {'inc 1})"), "1");
        assert_eq!(ev(&mut it, "({:a 1} :a)"), "1");
    }

    /// The resilient core.clj load leaves only a BOUNDED set of failures, all
    /// in genuinely-later-phase feature categories. This keeps the load from
    /// silently regressing: if a control macro or predicate stops loading, the
    /// count jumps and this test fails.
    #[test]
    fn core_load_failures_bounded_and_deferred() {
        let it = Interp::new();
        let fails = it.core_load_report();
        // Every failure must be one of the deferred categories (Phase 4/5/5.5
        // vars, lazy seqs, atoms/delays, regex, hex literals, queues, host).
        let deferred = |msg: &str| {
            msg.contains("lazy-map-1")            // lazy seqs (Phase 5)
                || msg.contains("realized?")      // delays (Phase 5.3)
                || msg.contains("atom")           // atoms (Phase 5.3)
                || msg.contains("alter-var-root") // real vars (Phase 4); the
                                                  // #inst data-reader setup now
                                                  // READS (#' support) then
                                                  // fails on alter-var-root
                || msg.contains("-empty-queue")   // persistent queue (out of scope)
                || msg.contains("read error: unsupported reader dispatch macro #\"") // regex (Phase 5.2)
                || msg.contains("read error: unsupported reader dispatch macro #'")  // var-quote (Phase 4)
                || msg.contains("read error: invalid number: 0x") // hex literal (Phase 5.5)
        };
        for (summary, msg) in fails {
            assert!(
                deferred(msg),
                "unexpected core.clj load failure: {summary} => {msg}"
            );
        }
        // Bound the count so a regression that drops many defs is caught.
        assert!(
            fails.len() <= 12,
            "core.clj load failures grew to {} (expected <=12 deferred)",
            fails.len()
        );
    }

    #[test]
    fn eval_core_forms() {
        let mut it = Interp::new();
        assert_eq!(print_str(&it.eval_str("42").unwrap()), "42");
        assert_eq!(print_str(&it.eval_str("(quote (a b))").unwrap()), "(a b)");
        assert_eq!(print_str(&it.eval_str("(if true 1 2)").unwrap()), "1");
        assert_eq!(print_str(&it.eval_str("(if nil 1 2)").unwrap()), "2");
        assert_eq!(print_str(&it.eval_str("(do 1 2 3)").unwrap()), "3");
    }

    #[test]
    fn eval_edge_cases_match_oracle() {
        let mut it = Interp::new();
        // (if nil 1) -> nil, (do) -> nil, empty input -> nil
        assert_eq!(print_str(&it.eval_str("(if nil 1)").unwrap()), "nil");
        assert_eq!(print_str(&it.eval_str("(do)").unwrap()), "nil");
        assert_eq!(print_str(&it.eval_str("").unwrap()), "nil");
        assert_eq!(print_str(&it.eval_str("(if true 1)").unwrap()), "1");
        // unbound symbol throws
        assert!(it.eval_str("nope").is_err());
    }

    /// The empty list `()` is a distinct value from `nil` (mino's
    /// MINO_EMPTY_LIST). Every expected value below is copied verbatim from
    /// the `mino -e '(pr-str ...)'` oracle.
    #[test]
    fn empty_list_distinct_from_nil() {
        let mut it = Interp::new();
        let ev = |it: &mut Interp, s: &str| print_str(&it.eval_str(s).unwrap());

        // Reading/printing: () self-evaluates and prints "()", not "nil".
        assert_eq!(ev(&mut it, "()"), "()");
        assert_eq!(ev(&mut it, "'()"), "()");
        assert_eq!(ev(&mut it, "(list)"), "()");

        // Equality: () != nil, () == (), () == [], (list) == ().
        assert_eq!(ev(&mut it, "(= () nil)"), "false");
        assert_eq!(ev(&mut it, "(= () ())"), "true");
        assert_eq!(ev(&mut it, "(= () [])"), "true");
        assert_eq!(ev(&mut it, "(= (list) ())"), "true");

        // Predicates: nil? true only for nil; seq?/list? true for the empty
        // list but false for nil; empty? true for both () and nil.
        assert_eq!(ev(&mut it, "(nil? ())"), "false");
        assert_eq!(ev(&mut it, "(nil? nil)"), "true");
        assert_eq!(ev(&mut it, "(seq? ())"), "true");
        assert_eq!(ev(&mut it, "(list? ())"), "true");
        assert_eq!(ev(&mut it, "(seq? nil)"), "false");
        assert_eq!(ev(&mut it, "(list? nil)"), "false");
        assert_eq!(ev(&mut it, "(empty? ())"), "true");
        assert_eq!(ev(&mut it, "(empty? nil)"), "true");
        assert_eq!(ev(&mut it, "(empty? [1])"), "false");

        // seq on empty -> nil; on non-empty -> a seq.
        assert_eq!(ev(&mut it, "(seq ())"), "nil");
        assert_eq!(ev(&mut it, "(seq nil)"), "nil");
        assert_eq!(ev(&mut it, "(seq [1])"), "(1)");

        // first on empty/nil -> nil.
        assert_eq!(ev(&mut it, "(first ())"), "nil");
        assert_eq!(ev(&mut it, "(first nil)"), "nil");

        // rest ALWAYS returns a seq (the empty list), never nil.
        assert_eq!(ev(&mut it, "(rest ())"), "()");
        assert_eq!(ev(&mut it, "(rest nil)"), "()");
        assert_eq!(ev(&mut it, "(rest (list 1))"), "()");
        assert_eq!(ev(&mut it, "(rest [1])"), "()");

        // next returns nil when there is no more.
        assert_eq!(ev(&mut it, "(next (list 1))"), "nil");
        assert_eq!(ev(&mut it, "(next ())"), "nil");
        assert_eq!(ev(&mut it, "(next nil)"), "nil");

        // cons onto nil / () / a list all make proper lists.
        assert_eq!(ev(&mut it, "(cons 1 nil)"), "(1)");
        assert_eq!(ev(&mut it, "(cons 1 ())"), "(1)");
        assert_eq!(ev(&mut it, "(cons 1 (list 2))"), "(1 2)");

        // count of both empties is 0.
        assert_eq!(ev(&mut it, "(count ())"), "0");
        assert_eq!(ev(&mut it, "(count nil)"), "0");

        // conj on nil or () makes a one-element list.
        assert_eq!(ev(&mut it, "(conj nil 1)"), "(1)");
        assert_eq!(ev(&mut it, "(conj () 1)"), "(1)");

        // () is truthy (only nil and false are falsy).
        assert_eq!(ev(&mut it, "(if () :t :f)"), ":t");
    }

    /// Macros, macroexpand, quasiquote, and named-fn self-recursion. Every
    /// expected value is copied verbatim from the `mino -e '...'` binary.
    #[test]
    fn macros_and_quasiquote_match_oracle() {
        let ev = |src: &str| -> String {
            let mut it = Interp::new();
            match it.eval_str(src) {
                Ok(v) => print_str(&v),
                Err(e) => format!("ERR {e:?}"),
            }
        };

        // defmacro + call: (my-if true 1 2) => 1 (args UNEVALUATED at expand).
        assert_eq!(
            ev("(defmacro my-if [c a b] (list 'if c a b)) (my-if true 1 2)"),
            "1"
        );
        // macroexpand-1 on the same macro yields the (if ...) form.
        assert_eq!(
            ev("(defmacro my-if [c a b] (list 'if c a b)) (macroexpand-1 '(my-if true 1 2))"),
            "(if true 1 2)"
        );
        // macroexpand-1 on a non-macro call is a no-op.
        assert_eq!(ev("(macroexpand-1 '(+ 1 2))"), "(+ 1 2)");

        // Recursive macro expansion: m1 expands to an m2 call which expands
        // to (+ x 1). (m1 10) => 11; macroexpand fully expands to (+ 10 1).
        let two = "(defmacro m2 [x] (list '+ x 1)) (defmacro m1 [x] (list 'm2 x)) ";
        assert_eq!(ev(&format!("{two}(m1 10)")), "11");
        assert_eq!(ev(&format!("{two}(macroexpand '(m1 10))")), "(+ 10 1)");

        // Quasiquote with ~ and ~@ in list context.
        assert_eq!(ev("`(a ~(+ 1 2) ~@[3 4] b)"), "(a 3 3 4 b)");
        // ~@ over an empty seq / nil splices nothing.
        assert_eq!(ev("`(a ~@[] b)"), "(a b)");
        assert_eq!(ev("`(a ~@nil b)"), "(a b)");

        // Quasiquote in vector context, nested data `[1 ~x ~@ys 4].
        assert_eq!(ev("(def x 2) (def ys [3 4]) `[1 ~x ~@ys 5]"), "[1 2 3 4 5]");
        // Map quasiquote expands values.
        assert_eq!(ev("(def v 7) `{:a ~v}"), "{:a 7}");

        // Symbol qualification, matching the binary EXACTLY:
        //   prim -> clojure.core/, user def -> user/, special form -> bare,
        //   public macro form -> clojure.core/, unbound -> bare.
        assert_eq!(ev("`inc"), "clojure.core/inc");
        assert_eq!(ev("(def y 1) `(inc y)"), "(clojure.core/inc user/y)");
        assert_eq!(ev("`if"), "if");
        assert_eq!(ev("`do"), "do");
        assert_eq!(ev("`when"), "clojure.core/when");
        assert_eq!(ev("`let"), "clojure.core/let");
        assert_eq!(ev("`foo"), "foo");
        // Nested syntax-quote stays as literal (quasiquote b) data.
        assert_eq!(ev("`(a `b)"), "(a (quasiquote b))");
        // quasiquote with no unquotes equals the quoted structure.
        assert_eq!(ev("`(a b c)"), ev("'(a b c)"));

        // Auto-gensym: `foo#` inside one syntax-quote maps to a single
        // per-read gensym; both refs match. (Number is process-monotonic,
        // so assert structural equality of the two positions.)
        let g = ev("`(x# x#)");
        assert!(
            g.starts_with("(x__") && g.ends_with("__auto__)"),
            "gensym shape: {g}"
        );
        let (a, b) = g[1..g.len() - 1].split_once(' ').unwrap();
        assert_eq!(a, b, "the two x# must be the same gensym: {g}");
        // The core.clj `and`/`or` pattern: (let [g# ...] ... g#) qualifies
        // let to clojure.core and gensyms g# consistently.
        let lg = ev("`(let [g# 1] g#)");
        assert!(lg.starts_with("(clojure.core/let [g__"), "{lg}");
    }
}
