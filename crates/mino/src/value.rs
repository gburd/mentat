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
    // A 32-bit float, produced by `(float x)` / `(unchecked-float x)`.
    // A DISTINCT tier from Float (f64): mino tags it MINO_FLOAT32 so
    // `(double? (float 5))` is false while `(float? (float 5))` is true,
    // and `type` returns `:float32`. Prints with f32's shortest decimal.
    Float32(f32),
    Char(char),
    Str(Gc<String>),
    Sym(crate::symbol::Symbol),
    Keyword(crate::symbol::Symbol),
    // The empty list `()`. mino models this as a distinct singleton
    // (MINO_EMPTY_LIST), NOT nil: `(= () nil)` is false, `(nil? ())` is
    // false, but `(seq? ())`/`(list? ())`/`(empty? ())` are true. A proper
    // list is a chain of `Cons` cells terminating in `EmptyList`.
    EmptyList,
    Cons(Gc<(Value, Value)>),
    // Persistent vector: 32-way trie (see collections::vector::PVec).
    Vector(Gc<crate::collections::vector::PVec>),
    // Persistent HAMT map/set (see collections::map). Both track key
    // insertion order for printing (mino's key_order companion vector).
    Map(Gc<crate::collections::map::PMap>),
    Set(Gc<crate::collections::map::PSet>),
    // Closures (`fn`) and built-in primitives. Ports MINO_FN / native prim.
    Fn(Gc<Closure>),
    Prim(Prim),
    // A native primitive that CAN capture host state (a `Box<dyn Fn>`), unlike
    // `Prim` (a bare fn pointer). This is how a host embeds a stateful engine:
    // Mentat registers `mentat.store/*` prims closing over an
    // `Rc<RefCell<mentat::Store>>`. Equality/hash by identity. Holds no *Gc*
    // pointers (captured state is behind the host's own Rc/RefCell), empty trace.
    PrimClosure(Gc<PrimClosure>),
    // Arbitrary-precision integer (`42N`, or the auto-promotion result of
    // `+'`/`*'`/etc. on i64 overflow). A distinct tier from Int: prints with
    // an `N` suffix and `type` returns `:bigint`. A BigInt is only produced
    // when a value cannot (or must not, per contagion) stay in the i64 tier;
    // `(+ 1N 1)` stays `2N` (bigint contagion), it never narrows back to Int.
    // Holds no Gc pointers, so its trace is empty.
    BigInt(Gc<BigIntVal>),
    // Exact fraction in lowest terms with denominator != 1 (`22/7`, `(/ 7 2)`).
    // ALWAYS reduced; a ratio that reduces to an integer is constructed as
    // Int/BigInt instead, never Ratio. `type` returns `:ratio`, prints
    // `num/den`. Holds no Gc pointers, so its trace is empty.
    Ratio(Gc<RatioVal>),
    // A compiled regex literal `#"..."` (or the result of `re-pattern`).
    // Holds the pattern SOURCE verbatim plus a lazily-compiled matcher. mino
    // compiles at match time (re-find/re-matches), not at construction, so an
    // invalid pattern surfaces as a throw from re-find, not from the reader or
    // re-pattern. Equality is by identity (Clojure: `(= #"a" #"a")` is false).
    Regex(Gc<RegexVal>),
    // A namespace var, as returned by `def`. Prints `#'ns/name`. The full
    // var cell (root binding, metadata, dynamic) lands with namespaces in
    // Phase 4; Task 1.2 only needs its identity for def's return value.
    Var(crate::symbol::Symbol),
    // Internal `recur` signal (mirrors mino's MINO_RECUR value type). Produced
    // ONLY by the `recur` special form and consumed by the `loop`/`fn`
    // trampolines; it must never escape to user code. `eval_value` (non-tail
    // eval sites) rejects it as "recur must be in tail position".
    Recur(Gc<Vec<Value>>),
    // Internal tail-call signal (mirrors mino's MINO_TAIL_CALL): a call to a
    // user fn in tail position returns (callee, evaluated args) instead of
    // nesting a Rust frame; `apply_closure` loops on it, and non-tail eval
    // sites (`eval_value`/`eval_force`) run it to completion. Never escapes.
    TailCall(Gc<(Value, Vec<Value>)>),
    // A mutable reference cell (`atom`). Holds the current value plus an
    // optional validator fn and a watches map. Equality/hash are by identity
    // (Clojure: two distinct atoms are never `=`, even with equal contents).
    // Ports MINO_ATOM (prim/stateful.c). The port is single-threaded, so
    // `swap!` is a plain read-compute-store (no CAS loop needed).
    Atom(Gc<gc::GcCell<crate::prim::stateful::AtomState>>),
    // An EAVT store connection (`mino.store/open`). An identity cell holding
    // the current immutable db value plus a durable path (None = in-memory),
    // a monotonic print id, and a watches map. Equality/hash are by identity
    // (Clojure: two distinct stores are never `=`). Ports MINO_STORE
    // (prim/store.c). Durability (WAL/snapshot) is Phase 7.
    Store(Gc<gc::GcCell<crate::store::StoreState>>),
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

/// A native primitive that can capture host state. The boxed closure lets a
/// host register prims that hold a handle to its own engine (e.g. a
/// `Rc<RefCell<mentat::Store>>`). The captured state lives behind the host's
/// own interior-mutability wrapper, not in the GC heap, so the trace is empty.
pub type PrimClosureFn =
    dyn Fn(&mut crate::eval::Interp, &[Value]) -> Result<Value, crate::error::Throw>;

pub struct PrimClosure {
    pub f: Box<PrimClosureFn>,
    pub name: String,
}

impl Finalize for PrimClosure {}
unsafe impl Trace for PrimClosure {
    gc::unsafe_empty_trace!();
}

/// A regex value: the pattern SOURCE (verbatim, as read) plus a lazily
/// compiled `fancy_regex::Regex` cached on first use. mino compiles the
/// pattern only when a match is attempted, so `re-pattern`/the reader never
/// fail on a bad pattern; `re-find`/`re-matches` do. Holds no Gc pointers, so
/// its trace is empty.
pub struct RegexVal {
    pub source: String,
    pub compiled: std::cell::OnceCell<Result<fancy_regex::Regex, String>>,
}

impl RegexVal {
    pub fn new(source: String) -> Self {
        Self {
            source,
            compiled: std::cell::OnceCell::new(),
        }
    }
}

impl Finalize for RegexVal {}
unsafe impl Trace for RegexVal {
    gc::unsafe_empty_trace!();
}

/// Arbitrary-precision integer wrapper. Holds a `num_bigint::BigInt`, no Gc
/// pointers, so its trace is empty.
pub struct BigIntVal(pub num_bigint::BigInt);
impl Finalize for BigIntVal {}
unsafe impl Trace for BigIntVal {
    gc::unsafe_empty_trace!();
}

/// Exact rational wrapper. Holds a `num_rational::BigRational`, ALWAYS in
/// lowest terms with denominator != 1 (constructors that would yield an
/// integer return Int/BigInt instead). No Gc pointers, so its trace is empty.
pub struct RatioVal(pub num_rational::BigRational);
impl Finalize for RatioVal {}
unsafe impl Trace for RatioVal {
    gc::unsafe_empty_trace!();
}

impl Value {
    /// Clojure truthiness: only `nil` and `false` are falsy. 0, "", empty
    /// collections are all truthy. Ports `MINO_IS_NIL`/bool semantics.
    pub fn is_truthy(&self) -> bool {
        // Only nil and false are falsy; the empty list is truthy.
        !matches!(self, Value::Nil | Value::Bool(false))
    }

    /// The empty list `()`. Cons chains end in `EmptyList`; seq walkers stop
    /// on it and list predicates treat it as a (empty) list, distinct from nil.
    pub fn is_empty_list(&self) -> bool {
        matches!(self, Value::EmptyList)
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
