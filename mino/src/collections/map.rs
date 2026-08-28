//! Persistent HAMT map (`PMap`) and set (`PSet`), ported from
//! `src/collections/map.c`. A 32-wide hash-array-mapped trie keyed by
//! [`hash_val`], with `eq_val` collision handling, plus a companion key
//! `PVec` recording insertion order (mino's `key_order`) so printing and
//! `entries()` iterate in the order keys first appeared -- a bare HAMT has
//! no such order.
//!
//! Ported: the persistent `hamt_assoc` / `hamt_get` walks and `mino_map` /
//! `mino_set` construction. Skipped (YAGNI): the flatmap small-map fast path
//! and the owned/transient `*_owned` mutators from map.c/map_owned.c -- pure
//! persistent path only, add if a profile ever demands it.
//!
//! ponytail: `dissoc` rebuilds the HAMT from the surviving keys rather than
//! porting a structural `hamt_dissoc` -- mino's persistent `mino_map_dissoc1`
//! does exactly this (rebuild from key_order minus the key). O(n log n) per
//! dissoc; add a structural node-collapse if dissoc-heavy workloads appear.

use crate::collections::hashing::{eq_val, hash_val};
use crate::collections::vector::PVec;
use crate::value::Value;
use gc::{Finalize, Gc, Trace};

const B: u32 = 5;
const MASK: u32 = 31; // 5-bit digit

// A stored key/value pair (mino's hamt_entry_t).
#[derive(Trace, Finalize, Clone)]
struct Entry {
    key: Value,
    val: Value,
}

// A HAMT node: either a bitmap-indexed branch or a hash-collision bucket.
// Bitmap slots hold either a child Node or a leaf Entry.
#[derive(Trace, Finalize)]
enum Node {
    // bitmap: which of 32 digit-slots are populated.
    // subnode_mask: of those, which hold a child Node (rest hold an Entry).
    // slots: packed by digit index, length popcount(bitmap).
    Bitmap {
        bitmap: u32,
        subnode_mask: u32,
        slots: Vec<Slot>,
    },
    // All entries share collision_hash but differ by key (hash exhausted).
    Collision {
        hash: u32,
        entries: Vec<Entry>,
    },
}

#[derive(Trace, Finalize, Clone)]
enum Slot {
    Child(Gc<Node>),
    Leaf(Entry),
}

fn popcount(x: u32) -> u32 {
    x.count_ones()
}

fn digit(h: u32, shift: u32) -> u32 {
    (h >> shift) & MASK
}

// Build the smallest subtree separating two leaves at level `shift`.
fn merge_entries(e1: Entry, h1: u32, e2: Entry, h2: u32, shift: u32) -> Gc<Node> {
    if h1 == h2 || shift >= 32 {
        return Gc::new(Node::Collision {
            hash: h1,
            entries: vec![e1, e2],
        });
    }
    let i1 = digit(h1, shift);
    let i2 = digit(h2, shift);
    if i1 == i2 {
        let child = merge_entries(e1, h1, e2, h2, shift + B);
        Gc::new(Node::Bitmap {
            bitmap: 1 << i1,
            subnode_mask: 1 << i1,
            slots: vec![Slot::Child(child)],
        })
    } else {
        let (a, b) = if i1 < i2 {
            (Slot::Leaf(e1), Slot::Leaf(e2))
        } else {
            (Slot::Leaf(e2), Slot::Leaf(e1))
        };
        Gc::new(Node::Bitmap {
            bitmap: (1 << i1) | (1 << i2),
            subnode_mask: 0,
            slots: vec![a, b],
        })
    }
}

// Insert/rebind `entry` (hash `h`) into the subtree at `node`. Returns the
// new subtree and sets `*replaced` when the key was already present.
fn hamt_assoc(node: Option<&Gc<Node>>, entry: Entry, h: u32, shift: u32, replaced: &mut bool) -> Gc<Node> {
    let node = match node {
        None => {
            let i = digit(h, shift);
            return Gc::new(Node::Bitmap {
                bitmap: 1 << i,
                subnode_mask: 0,
                slots: vec![Slot::Leaf(entry)],
            });
        }
        Some(n) => n,
    };
    match &**node {
        Node::Collision { hash, entries } => {
            if h == *hash {
                // Update in place, or append a new colliding key.
                if let Some(j) = entries.iter().position(|e| eq_val(&e.key, &entry.key)) {
                    let mut new = entries.clone();
                    new[j] = entry;
                    *replaced = true;
                    return Gc::new(Node::Collision { hash: *hash, entries: new });
                }
                let mut new = entries.clone();
                new.push(entry);
                return Gc::new(Node::Collision { hash: *hash, entries: new });
            }
            // Different hash: promote the bucket into a bitmap node at this
            // level, then route the new entry.
            let ib = digit(*hash, shift);
            let in_ = digit(h, shift);
            if ib == in_ {
                let sub = hamt_assoc(Some(node), entry, h, shift + B, replaced);
                Gc::new(Node::Bitmap {
                    bitmap: 1 << ib,
                    subnode_mask: 1 << ib,
                    slots: vec![Slot::Child(sub)],
                })
            } else {
                let bucket = Slot::Child(node.clone());
                let leaf = Slot::Leaf(entry);
                let slots = if ib < in_ { vec![bucket, leaf] } else { vec![leaf, bucket] };
                Gc::new(Node::Bitmap {
                    bitmap: (1 << ib) | (1 << in_),
                    subnode_mask: 1 << ib,
                    slots,
                })
            }
        }
        Node::Bitmap { bitmap, subnode_mask, slots } => {
            let i = digit(h, shift);
            let bit = 1u32 << i;
            let phys = popcount(bitmap & (bit - 1)) as usize;
            if bitmap & bit == 0 {
                // Empty slot: insert a leaf.
                let mut new = slots.clone();
                new.insert(phys, Slot::Leaf(entry));
                return Gc::new(Node::Bitmap {
                    bitmap: bitmap | bit,
                    subnode_mask: *subnode_mask,
                    slots: new,
                });
            }
            match &slots[phys] {
                Slot::Child(child) => {
                    let new_child = hamt_assoc(Some(child), entry, h, shift + B, replaced);
                    let mut new = slots.clone();
                    new[phys] = Slot::Child(new_child);
                    Gc::new(Node::Bitmap {
                        bitmap: *bitmap,
                        subnode_mask: *subnode_mask,
                        slots: new,
                    })
                }
                Slot::Leaf(existing) => {
                    if eq_val(&existing.key, &entry.key) {
                        let mut new = slots.clone();
                        new[phys] = Slot::Leaf(entry);
                        *replaced = true;
                        return Gc::new(Node::Bitmap {
                            bitmap: *bitmap,
                            subnode_mask: *subnode_mask,
                            slots: new,
                        });
                    }
                    // Split: two distinct keys share this slot.
                    let eh = hash_val(&existing.key) as u32;
                    let sub = merge_entries(existing.clone(), eh, entry, h, shift + B);
                    let mut new = slots.clone();
                    new[phys] = Slot::Child(sub);
                    Gc::new(Node::Bitmap {
                        bitmap: *bitmap,
                        subnode_mask: subnode_mask | bit,
                        slots: new,
                    })
                }
            }
        }
    }
}

// Look up a key; None if absent.
fn hamt_get<'a>(mut node: Option<&'a Gc<Node>>, key: &Value, h: u32, mut shift: u32) -> Option<&'a Value> {
    while let Some(n) = node {
        match &**n {
            Node::Collision { hash, entries } => {
                if h != *hash {
                    return None;
                }
                return entries.iter().find(|e| eq_val(&e.key, key)).map(|e| &e.val);
            }
            Node::Bitmap { bitmap, subnode_mask, slots } => {
                let bit = 1u32 << digit(h, shift);
                if bitmap & bit == 0 {
                    return None;
                }
                let phys = popcount(bitmap & (bit - 1)) as usize;
                if subnode_mask & bit != 0 {
                    match &slots[phys] {
                        Slot::Child(child) => {
                            node = Some(child);
                            shift += B;
                        }
                        Slot::Leaf(_) => unreachable!("subnode_mask/slot disagree"),
                    }
                } else {
                    return match &slots[phys] {
                        Slot::Leaf(e) if eq_val(&e.key, key) => Some(&e.val),
                        _ => None,
                    };
                }
            }
        }
    }
    None
}

/// A persistent HAMT map with insertion-order tracking. Immutable: every
/// `assoc`/`dissoc` returns a fresh map sharing unmodified subtrees.
#[derive(Trace, Finalize)]
pub struct PMap {
    root: Option<Gc<Node>>,
    key_order: PVec, // keys in first-insertion order (for iteration/printing)
    len: usize,
}

impl PMap {
    /// The empty map.
    pub fn empty() -> PMap {
        PMap { root: None, key_order: PVec::empty(), len: 0 }
    }

    /// Number of entries.
    pub fn count(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Look up a value by key.
    pub fn get(&self, key: &Value) -> Option<&Value> {
        if self.len == 0 {
            return None;
        }
        hamt_get(self.root.as_ref(), key, hash_val(key) as u32, 0)
    }

    /// Whether `key` is present.
    pub fn contains(&self, key: &Value) -> bool {
        self.get(key).is_some()
    }

    /// Associate `key` -> `val`, returning a new map. A new key appends to
    /// insertion order; a rebind leaves order unchanged.
    pub fn assoc(&self, key: Value, val: Value) -> PMap {
        let h = hash_val(&key) as u32;
        let mut replaced = false;
        let entry = Entry { key: key.clone(), val };
        let root = hamt_assoc(self.root.as_ref(), entry, h, 0, &mut replaced);
        let (key_order, len) = if replaced {
            (self.key_order.clone(), self.len)
        } else {
            (self.key_order.conj(key), self.len + 1)
        };
        PMap { root: Some(root), key_order, len }
    }

    /// Remove `key`, returning a new map. Absent key returns a clone.
    /// Rebuilds the HAMT from surviving keys (mino's persistent dissoc).
    pub fn dissoc(&self, key: &Value) -> PMap {
        if self.len == 0 || !self.contains(key) {
            return self.clone_shallow();
        }
        let mut out = PMap::empty();
        for (k, v) in self.entries() {
            if !eq_val(k, key) {
                out = out.assoc(k.clone(), v.clone());
            }
        }
        out
    }

    // Cheap structural clone (shares the trie + key_order via Gc/PVec clone).
    fn clone_shallow(&self) -> PMap {
        PMap { root: self.root.clone(), key_order: self.key_order.clone(), len: self.len }
    }

    /// Iterate `(key, value)` in insertion order.
    pub fn entries(&self) -> impl Iterator<Item = (&Value, &Value)> {
        self.key_order.iter().map(move |k| (k, self.get(k).unwrap()))
    }

    /// Iterate keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &Value> {
        self.key_order.iter()
    }

    /// Iterate values in insertion order.
    pub fn vals(&self) -> impl Iterator<Item = &Value> {
        self.entries().map(|(_, v)| v)
    }
}

/// A persistent HAMT set with insertion-order tracking. Backed by the same
/// HAMT with a sentinel value per element (mino stores `true`); a companion
/// key vector records insertion order for `#{...}` printing.
#[derive(Trace, Finalize)]
pub struct PSet {
    root: Option<Gc<Node>>,
    order: PVec,
    len: usize,
}

impl PSet {
    /// The empty set.
    pub fn empty() -> PSet {
        PSet { root: None, order: PVec::empty(), len: 0 }
    }

    /// Number of elements.
    pub fn count(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Whether `elem` is a member.
    pub fn contains(&self, elem: &Value) -> bool {
        if self.len == 0 {
            return false;
        }
        hamt_get(self.root.as_ref(), elem, hash_val(elem) as u32, 0).is_some()
    }

    /// Add `elem`, returning a new set. A duplicate returns an equivalent set
    /// unchanged (dedup, as `set`/`conj` do in mino).
    pub fn conj(&self, elem: Value) -> PSet {
        let h = hash_val(&elem) as u32;
        let mut replaced = false;
        let entry = Entry { key: elem.clone(), val: Value::Bool(true) };
        let root = hamt_assoc(self.root.as_ref(), entry, h, 0, &mut replaced);
        if replaced {
            // Already present: order and len unchanged.
            return PSet { root: Some(root), order: self.order.clone(), len: self.len };
        }
        PSet { root: Some(root), order: self.order.conj(elem), len: self.len + 1 }
    }

    /// Remove `elem`, returning a new set. Rebuilds from survivors.
    pub fn disj(&self, elem: &Value) -> PSet {
        if self.len == 0 || !self.contains(elem) {
            return PSet { root: self.root.clone(), order: self.order.clone(), len: self.len };
        }
        let mut out = PSet::empty();
        for e in self.iter() {
            if !eq_val(e, elem) {
                out = out.conj(e.clone());
            }
        }
        out
    }

    /// Iterate elements in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        self.order.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::print_str;
    use crate::reader::read_one;

    fn kw(name: &str) -> Value {
        Value::Keyword(crate::symbol::Symbol::plain(name))
    }

    #[test]
    fn assoc_get_dissoc_count_100() {
        // Exercise HAMT growth well past one bitmap node.
        let mut m = PMap::empty();
        for i in 0..100 {
            m = m.assoc(Value::Int(i), Value::Int(i * 10));
        }
        assert_eq!(m.count(), 100);
        for i in 0..100 {
            assert!(matches!(m.get(&Value::Int(i)), Some(Value::Int(n)) if *n == i * 10));
        }
        assert!(m.get(&Value::Int(100)).is_none());
        // Dissoc half, verify count + membership.
        let mut m2 = m.clone_shallow();
        for i in (0..100).step_by(2) {
            m2 = m2.dissoc(&Value::Int(i));
        }
        assert_eq!(m2.count(), 50);
        assert!(m2.get(&Value::Int(0)).is_none());
        assert!(matches!(m2.get(&Value::Int(1)), Some(Value::Int(10))));
        // Original unchanged (persistence).
        assert_eq!(m.count(), 100);
        assert!(matches!(m.get(&Value::Int(0)), Some(Value::Int(0))));
    }

    #[test]
    fn assoc_is_persistent_and_rebind_keeps_order() {
        let m = PMap::empty().assoc(kw("a"), Value::Int(1)).assoc(kw("b"), Value::Int(2));
        let m2 = m.assoc(kw("a"), Value::Int(9)); // rebind
        assert!(matches!(m2.get(&kw("a")), Some(Value::Int(9))));
        // Original unchanged.
        assert!(matches!(m.get(&kw("a")), Some(Value::Int(1))));
        // Rebind preserves insertion order: a before b.
        let keys: Vec<_> = m2.keys().map(print_str).collect();
        assert_eq!(keys, [":a", ":b"]);
        assert_eq!(m2.count(), 2);
    }

    #[test]
    fn insertion_order_preserved_in_entries_and_print() {
        let m = PMap::empty()
            .assoc(kw("c"), Value::Int(3))
            .assoc(kw("a"), Value::Int(1))
            .assoc(kw("b"), Value::Int(2));
        let order: Vec<_> = m.entries().map(|(k, _)| print_str(k)).collect();
        assert_eq!(order, [":c", ":a", ":b"]);
        assert_eq!(print_str(&Value::Map(Gc::new(m))), "{:c 3, :a 1, :b 2}");
    }

    #[test]
    fn read_print_roundtrip_maps() {
        // Comma-separated with a space between k and v, insertion order.
        let (v, _) = read_one("{:a 1, :b 2}").unwrap();
        assert_eq!(print_str(&v), "{:a 1, :b 2}");
        // Nested map with vector value and set value.
        let (v, _) = read_one("{:a [1 2], :b #{:x}}").unwrap();
        assert_eq!(print_str(&v), "{:a [1 2], :b #{:x}}");
        // Empty map.
        let (v, _) = read_one("{}").unwrap();
        assert_eq!(print_str(&v), "{}");
    }

    #[test]
    fn set_dedup_and_roundtrip() {
        // `set`/`conj` dedup (mino: (set [1 1 2]) => #{1 2}).
        let s = PSet::empty().conj(Value::Int(1)).conj(Value::Int(1)).conj(Value::Int(2));
        assert_eq!(s.count(), 2);
        assert!(s.contains(&Value::Int(1)));
        assert!(s.contains(&Value::Int(2)));
        assert!(!s.contains(&Value::Int(3)));
        // #{1 2} round-trips (insertion order).
        let (v, _) = read_one("#{1 2}").unwrap();
        assert_eq!(print_str(&v), "#{1 2}");
        let (v, _) = read_one("#{}").unwrap();
        assert_eq!(print_str(&v), "#{}");
        // disj.
        let s2 = s.disj(&Value::Int(1));
        assert_eq!(s2.count(), 1);
        assert!(!s2.contains(&Value::Int(1)));
        assert_eq!(s.count(), 2); // persistence
    }

    #[test]
    fn map_equality_ignores_order() {
        // (= {:a 1 :b 2} {:b 2 :a 1}) => true, but each prints its own order.
        let m1 = PMap::empty().assoc(kw("a"), Value::Int(1)).assoc(kw("b"), Value::Int(2));
        let m2 = PMap::empty().assoc(kw("b"), Value::Int(2)).assoc(kw("a"), Value::Int(1));
        assert!(eq_val(&Value::Map(Gc::new(PMap {
            root: m1.root.clone(),
            key_order: m1.key_order.clone(),
            len: m1.len,
        })), &Value::Map(Gc::new(PMap {
            root: m2.root.clone(),
            key_order: m2.key_order.clone(),
            len: m2.len,
        }))));
        assert_eq!(print_str(&Value::Map(Gc::new(m1))), "{:a 1, :b 2}");
        assert_eq!(print_str(&Value::Map(Gc::new(m2))), "{:b 2, :a 1}");
    }

    #[test]
    fn hash_eq_contract() {
        use crate::collections::hashing::{eq_val, hash_val};
        // Integral floats hash like ints (mino tag collapse) but are NOT eq.
        assert_eq!(hash_val(&Value::Int(1)), hash_val(&Value::Float(1.0)));
        assert!(!eq_val(&Value::Int(1), &Value::Float(1.0)));
        // Equal collections hash equal.
        let (a, _) = read_one("[1 2]").unwrap();
        let (b, _) = read_one("[1 2]").unwrap();
        assert!(eq_val(&a, &b));
        assert_eq!(hash_val(&a), hash_val(&b));
        // Order-independent map hash.
        let (m1, _) = read_one("{:a 1 :b 2}").unwrap();
        let (m2, _) = read_one("{:b 2 :a 1}").unwrap();
        assert!(eq_val(&m1, &m2));
        assert_eq!(hash_val(&m1), hash_val(&m2));
        // Pinned to the mino binary: (hash :a) => 1080988421, (hash 1) =>
        // 1200595475, (hash "hello") => 886912120 (FNV-1a, low 32 bits).
        assert_eq!(hash_val(&kw("a")), 1080988421);
        assert_eq!(hash_val(&Value::Int(1)), 1200595475);
        assert_eq!(
            hash_val(&read_one("\"hello\"").unwrap().0),
            886912120
        );
    }
}
