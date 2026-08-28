//! Value hashing + structural equality, ported from mino's
//! `src/collections/map_hash.c` (`hash_val`, FNV-1a helpers) and
//! `src/values/val.c` (`mino_eq`). These back the HAMT key discipline in
//! `map.rs`: keys index by `hash_val`, collisions resolve by `eq_val`.
//!
//! Contract (matches mino): equal values hash equal. Integral floats hash
//! the same as the equivalent int (tag 0x03) so a hash-map keyed by `1` and
//! probed by `1.0` lands in the same bucket -- but `eq_val(1, 1.0)` is still
//! FALSE (int and float are distinct value classes; `=` never bridges them),
//! so they never actually collide as the *same* key. `hash` may collide;
//! `eq` is the source of truth.

use crate::value::Value;

const FNV_OFFSET: u32 = 2166136261;
const FNV_PRIME: u32 = 16777619;

fn fnv_mix(h: u32, b: u8) -> u32 {
    (h ^ b as u32).wrapping_mul(FNV_PRIME)
}

fn fnv_bytes(mut h: u32, p: &[u8]) -> u32 {
    for &b in p {
        h = fnv_mix(h, b);
    }
    h
}

// Eight little-endian bytes of an i64. The numeric tier (int, integral
// float) shares tag 0x03 so (= 1 1.0) hashes the same.
fn hash_i64_bytes(mut h: u32, mut n: i64) -> u32 {
    for _ in 0..8 {
        h = fnv_mix(h, (n & 0xFF) as u8);
        n = ((n as u64) >> 8) as i64;
    }
    h
}

// Four little-endian bytes of a u32; folds a subhash into its parent.
fn hash_u32_bytes(mut h: u32, mut x: u32) -> u32 {
    for _ in 0..4 {
        h = fnv_mix(h, (x & 0xFF) as u8);
        x >>= 8;
    }
    h
}

// Pointer-identity hash for non-hashable kinds (Fn/Prim/Var). Uses the
// Gc/heap address so distinct allocations hash distinctly.
fn hash_identity(h: u32, p: usize) -> u32 {
    let mut h = h;
    let mut p = p;
    for _ in 0..std::mem::size_of::<usize>() {
        h = fnv_mix(h, (p & 0xFF) as u8);
        p >>= 8;
    }
    h
}

/// Hash a value, compatible with [`eq_val`]. Ports `hash_val`. Returns u64
/// (the HAMT only uses the low 32 bits, matching mino's `uint32_t`).
pub fn hash_val(v: &Value) -> u64 {
    hash32(v) as u64
}

fn hash32(v: &Value) -> u32 {
    let h = FNV_OFFSET;
    match v {
        Value::Nil => fnv_mix(h, 0x01),
        Value::Bool(b) => fnv_mix(fnv_mix(h, 0x02), if *b { 1 } else { 0 }),
        Value::Int(n) => hash_i64_bytes(fnv_mix(h, 0x03), *n),
        Value::Float(d) => {
            // Integral, finite floats collapse to the int tag so (= 1 1.0)
            // hashes alike; everything else hashes its raw IEEE bytes.
            if d.is_finite() {
                let ll = *d as i64;
                if ll as f64 == *d {
                    return hash_i64_bytes(fnv_mix(h, 0x03), ll);
                }
            }
            fnv_bytes(fnv_mix(h, 0x04), &d.to_le_bytes())
        }
        Value::Char(c) => hash_u32_bytes(fnv_mix(h, 0x0f), *c as u32),
        Value::Str(s) => fnv_bytes(fnv_mix(h, 0x05), s.as_bytes()),
        // Symbols/keywords hash their full text (ns/name), matching mino's
        // `v->as.s.data` which stores the namespaced string verbatim.
        Value::Sym(sym) => fnv_bytes(fnv_mix(h, 0x06), sym.to_string().as_bytes()),
        Value::Keyword(sym) => fnv_bytes(fnv_mix(h, 0x07), sym.to_string().as_bytes()),
        // Sequentials (cons list, vector, empty-list) share tag 0x09 so
        // (= '(1 2) [1 2]) and (= () []). The empty list hashes as the empty
        // sequential.
        Value::EmptyList | Value::Cons(_) | Value::Vector(_) => hash_sequential(v),
        // Maps: order-insensitive XOR-fold of per-entry hashes.
        Value::Map(m) => {
            let mut acc: u32 = 0;
            for (k, val) in m.entries() {
                let hk = hash32(k);
                let hv = hash32(val);
                acc ^= hk ^ hv.wrapping_mul(2654435761);
            }
            hash_u32_bytes(fnv_mix(h, 0x0a), acc)
        }
        // Sets: order-insensitive XOR-fold of element hashes.
        Value::Set(set) => {
            let mut acc: u32 = 0;
            for e in set.iter() {
                acc ^= hash32(e);
            }
            hash_u32_bytes(fnv_mix(h, 0x0d), acc)
        }
        // Non-hashable: identity by heap address (default tag 0x0b).
        Value::Fn(gc) => hash_identity(fnv_mix(h, 0x0b), &**gc as *const _ as usize),
        Value::Prim(p) => hash_identity(fnv_mix(h, 0x0b), p.0 as usize),
        Value::Var(sym) => fnv_bytes(fnv_mix(h, 0x06), sym.to_string().as_bytes()),
        // Internal recur signal: identity hash; it never enters a real
        // collection, but the match must stay exhaustive.
        Value::Recur(gc) => hash_identity(fnv_mix(h, 0x0b), &**gc as *const _ as usize),
        // Regex: identity hash (Clojure Patterns are never value-equal).
        Value::Regex(gc) => hash_identity(fnv_mix(h, 0x0b), &**gc as *const _ as usize),
    }
}

// hash_sequential: unified scheme for cons chains + vectors (tag 0x09), so
// equal content across representations hashes equal.
fn hash_sequential(v: &Value) -> u32 {
    let mut h = fnv_mix(FNV_OFFSET, 0x09);
    let mut cur = v;
    loop {
        match cur {
            Value::Vector(vec) => {
                for e in vec.iter() {
                    h = hash_u32_bytes(h, hash32(e));
                }
                break;
            }
            Value::Cons(cell) => {
                h = hash_u32_bytes(h, hash32(&cell.0));
                cur = &cell.1;
            }
            _ => break, // EmptyList / nil terminator (or improper tail: stop)
        }
    }
    h
}

/// Structural equality, compatible with [`hash_val`]. Ports `mino_eq`.
/// `(= 1 1.0)` is FALSE (distinct value classes); strings/symbols/keywords
/// compare by content; collections compare structurally (maps/sets ignore
/// insertion order).
pub fn eq_val(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Nil, Value::Nil) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => **x == **y,
        (Value::Sym(x), Value::Sym(y)) => x == y,
        (Value::Keyword(x), Value::Keyword(y)) => x == y,
        (Value::Var(x), Value::Var(y)) => x == y,
        // Sequentials compare element-wise across cons/vector/empty-list.
        // `(= () [])` and `(= () '())` are true; `(= () nil)` is false.
        (
            Value::EmptyList | Value::Cons(_) | Value::Vector(_),
            Value::EmptyList | Value::Cons(_) | Value::Vector(_),
        ) => eq_sequential(a, b),
        (Value::Map(x), Value::Map(y)) => eq_map(x, y),
        (Value::Set(x), Value::Set(y)) => eq_set(x, y),
        // Callables: identity. Distinct allocations are never `=`.
        (Value::Fn(x), Value::Fn(y)) => Gc_ptr_eq_fn(x, y),
        (Value::Prim(x), Value::Prim(y)) => {
            // Prim identity: compare the fn pointers. (Casting to a data
            // pointer avoids the unstable `ptr::fn_addr_eq`.)
            x.0 as usize == y.0 as usize
        }
        _ => false,
    }
}

// gc::Gc has no ptr_eq; compare the referent addresses.
#[allow(non_snake_case)]
fn Gc_ptr_eq_fn(x: &gc::Gc<crate::eval::func::Closure>, y: &gc::Gc<crate::eval::func::Closure>) -> bool {
    std::ptr::eq(&**x, &**y)
}

// Iterate two sequentials (cons list or vector) in lockstep.
fn eq_sequential(a: &Value, b: &Value) -> bool {
    let mut ia = SeqCursor::new(a);
    let mut ib = SeqCursor::new(b);
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) => {
                if !eq_val(x, y) {
                    return false;
                }
            }
            _ => return false,
        }
    }
}

// A cursor over a sequential value that yields &Value in order, walking a
// cons spine or a vector index. ponytail: vectors clone-free via nth ref.
enum SeqCursor<'a> {
    Cons(&'a Value),
    Vec(&'a crate::collections::vector::PVec, usize),
}

impl<'a> SeqCursor<'a> {
    fn new(v: &'a Value) -> Self {
        match v {
            Value::Vector(vec) => SeqCursor::Vec(vec, 0),
            // EmptyList is an empty cons chain: the cursor yields nothing.
            other => SeqCursor::Cons(other),
        }
    }
    fn next(&mut self) -> Option<&'a Value> {
        match self {
            SeqCursor::Cons(cur) => match cur {
                Value::Cons(cell) => {
                    let car = &cell.0;
                    *cur = &cell.1;
                    Some(car)
                }
                _ => None,
            },
            SeqCursor::Vec(vec, i) => {
                let e = vec.nth(*i);
                if e.is_some() {
                    *i += 1;
                }
                e
            }
        }
    }
}

// Same-key-set, same-values; order-independent (Clojure map equality).
fn eq_map(a: &crate::collections::map::PMap, b: &crate::collections::map::PMap) -> bool {
    if a.count() != b.count() {
        return false;
    }
    for (k, av) in a.entries() {
        match b.get(k) {
            Some(bv) if eq_val(av, bv) => {}
            _ => return false,
        }
    }
    true
}

// Same elements; order-independent.
fn eq_set(a: &crate::collections::map::PSet, b: &crate::collections::map::PSet) -> bool {
    if a.count() != b.count() {
        return false;
    }
    a.iter().all(|e| b.contains(e))
}
