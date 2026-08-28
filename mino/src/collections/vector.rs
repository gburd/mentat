//! Persistent 32-way trie vector (Bagwell). Ports `src/collections/vec.c`
//! (persistent path only; the owned/transient, subvec, and materialize
//! machinery in vec.c is for Task 2.2+ transients and is skipped here —
//! `offset` is always 0 for a plain persistent vector).
//!
//! Layout:
//!   - `tail` holds the trailing 1..=32 elements; tail-appends are O(1)
//!     amortized (one 32-slot copy, no trie walk).
//!   - `root` is a trie whose leaves each hold 32 values; the rightmost
//!     spine may be partial.
//!   - `shift` encodes height: 0 => root is a leaf; else a branch whose
//!     children live at `shift - B`.
//!   - conj/assoc/pop are path-copies: they return fresh nodes along the
//!     walked path and share the rest with the source vector (persistent).

use crate::value::Value;
use gc::{Finalize, Gc, Trace};

const B: u32 = 5;
const WIDTH: usize = 1 << B; // 32
const MASK: usize = WIDTH - 1; // 0x1f

/// A trie node: either a branch (`slots` -> child `Node`s) or a leaf
/// (`items` -> `Value`s). vec.c uses an untyped union keyed by level; here
/// two enum arms make the distinction explicit and keep Trace derivable.
#[derive(Trace, Finalize, Clone)]
enum Node {
    Branch(Vec<Gc<Node>>),
    Leaf(Vec<Value>),
}

impl Node {
    fn as_branch(&self) -> &[Gc<Node>] {
        match self {
            Node::Branch(c) => c,
            Node::Leaf(_) => unreachable!("expected branch node"),
        }
    }
    fn as_leaf(&self) -> &[Value] {
        match self {
            Node::Leaf(v) => v,
            Node::Branch(_) => unreachable!("expected leaf node"),
        }
    }
}

/// A persistent vector.
#[derive(Trace, Finalize, Clone)]
pub struct PVec {
    root: Option<Gc<Node>>, // trie of full 32-wide leaves; None when empty
    tail: Vec<Value>,       // trailing 1..=32 elements
    shift: u32,             // height of the trie in bits (0 => root is a leaf)
    count: usize,           // total element count
}

impl PVec {
    /// The empty vector.
    pub fn empty() -> PVec {
        PVec {
            root: None,
            tail: Vec::new(),
            shift: 0,
            count: 0,
        }
    }

    /// Number of elements. Ports `vec.len`.
    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Flat index of the first element still living in the tail.
    fn tail_offset(&self) -> usize {
        self.count - self.tail.len()
    }

    /// Read one element by index; `None` if out of range. Ports `vec_nth`.
    pub fn nth(&self, i: usize) -> Option<&Value> {
        if i >= self.count {
            return None;
        }
        if i >= self.tail_offset() {
            return Some(&self.tail[i - self.tail_offset()]);
        }
        let mut node = self.root.as_ref().unwrap();
        let mut shift = self.shift;
        while shift > 0 {
            node = &node.as_branch()[(i >> shift) & MASK];
            shift -= B;
        }
        Some(&node.as_leaf()[i & MASK])
    }

    /// Append one element, returning a new vector. Ports `vec_conj1`.
    pub fn conj(&self, item: Value) -> PVec {
        // Tail has room: copy it and append. (Empty vec: tail_len 0 < WIDTH.)
        if self.tail.len() < WIDTH {
            let mut new_tail = self.tail.clone();
            new_tail.push(item);
            return PVec {
                root: self.root.clone(),
                tail: new_tail,
                shift: self.shift,
                count: self.count + 1,
            };
        }
        // Tail is full: push it into the trie as a leaf, start a fresh tail.
        let full_tail = Gc::new(Node::Leaf(self.tail.clone()));
        let trie_count = self.count - self.tail.len(); // before incorporation
        let (new_root, new_shift) = match &self.root {
            // Trie was empty: the old tail becomes the leaf root.
            None => (full_tail, 0),
            Some(root) => {
                // Root full at current height (trie holds 1<<(shift+B) elems):
                // add a level.
                if trie_count == (1usize << (self.shift + B)) {
                    let grown = Node::Branch(vec![
                        root.clone(),
                        new_path(self.shift, full_tail),
                    ]);
                    (Gc::new(grown), self.shift + B)
                } else {
                    (
                        push_tail(root, self.shift, trie_count, full_tail),
                        self.shift,
                    )
                }
            }
        };
        PVec {
            root: Some(new_root),
            tail: vec![item],
            shift: new_shift,
            count: self.count + 1,
        }
    }

    /// Update index `i` (or append when `i == len`), returning a new vector.
    /// `None` if `i > len`. Ports `vec_assoc1`.
    pub fn assoc(&self, i: usize, item: Value) -> Option<PVec> {
        if i == self.count {
            return Some(self.conj(item));
        }
        if i > self.count {
            return None;
        }
        if i >= self.tail_offset() {
            // In the tail: copy and overwrite one slot.
            let mut new_tail = self.tail.clone();
            new_tail[i - self.tail_offset()] = item;
            return Some(PVec {
                root: self.root.clone(),
                tail: new_tail,
                shift: self.shift,
                count: self.count,
            });
        }
        // In the trie: path-copy the spine.
        let new_root = trie_assoc(self.root.as_ref().unwrap(), self.shift, i, item);
        Some(PVec {
            root: Some(new_root),
            tail: self.tail.clone(),
            shift: self.shift,
            count: self.count,
        })
    }

    /// Remove the last element, returning a new vector. `None` if empty.
    /// Ports `vec_pop`.
    pub fn pop(&self) -> Option<PVec> {
        if self.count == 0 {
            return None;
        }
        let new_len = self.count - 1;
        if new_len == 0 {
            return Some(PVec::empty());
        }
        // Tail has more than one element: shrink the tail.
        if self.tail.len() > 1 {
            let mut new_tail = self.tail.clone();
            new_tail.pop();
            return Some(PVec {
                root: self.root.clone(),
                tail: new_tail,
                shift: self.shift,
                count: new_len,
            });
        }
        // tail_len == 1: pull the rightmost trie leaf up as the new tail.
        let root = self.root.as_ref().unwrap();
        let trie_count = self.count - self.tail.len();
        if self.shift == 0 {
            // Root is the only leaf; it becomes the new tail.
            return Some(PVec {
                root: None,
                tail: root.as_leaf().to_vec(),
                shift: 0,
                count: new_len,
            });
        }
        let (mut new_root, new_leaf) = pop_tail(root, self.shift, trie_count);
        let mut new_shift = self.shift;
        // Shrink height if the root now has only one child.
        if let Some(nr) = &new_root {
            if nr.as_branch().len() == 1 && new_shift > 0 {
                new_root = Some(nr.as_branch()[0].clone());
                new_shift -= B;
            }
        }
        Some(PVec {
            root: new_root,
            tail: new_leaf,
            shift: new_shift,
            count: new_len,
        })
    }

    /// Iterate elements in order. Simple index walk (O(n log32 n) total, fine
    /// for print/eq); ponytail: bulk chunk-walk if iteration ever gets hot.
    pub fn iter(&self) -> impl Iterator<Item = &Value> {
        (0..self.count).map(move |i| self.nth(i).unwrap())
    }

    /// Build from a flat Vec, consuming it. vec.c does an O(n) bulk build;
    /// ponytail: repeated conj is O(n log32 n) but simple and correct — swap
    /// in the bulk builder if construction ever shows up in a profile.
    pub fn from_vec(items: Vec<Value>) -> PVec {
        let mut v = PVec::empty();
        for item in items {
            v = v.conj(item);
        }
        v
    }
}

impl FromIterator<Value> for PVec {
    fn from_iter<I: IntoIterator<Item = Value>>(iter: I) -> PVec {
        let mut v = PVec::empty();
        for item in iter {
            v = v.conj(item);
        }
        v
    }
}

/// Build a spine from a branch at level `shift` down to `leaf`, placing the
/// leaf in slot 0 at every level. Ports `new_path`.
fn new_path(shift: u32, leaf: Gc<Node>) -> Gc<Node> {
    if shift == 0 {
        return leaf;
    }
    Gc::new(Node::Branch(vec![new_path(shift - B, leaf)]))
}

/// Insert `leaf` into the subtree at `node` (level `shift`) at `subindex`.
/// Path-copies the walked spine. Ports `push_tail`.
fn push_tail(node: &Gc<Node>, shift: u32, subindex: usize, leaf: Gc<Node>) -> Gc<Node> {
    let digit = (subindex >> shift) & MASK;
    let mut children = node.as_branch().to_vec();
    if shift == B {
        // Children are leaves: place the tail directly.
        set_or_push(&mut children, digit, leaf);
    } else {
        let new_child = match children.get(digit) {
            None => new_path(shift - B, leaf),
            Some(child) => push_tail(child, shift - B, subindex, leaf),
        };
        set_or_push(&mut children, digit, new_child);
    }
    Gc::new(Node::Branch(children))
}

/// Path-copy update of the trie element at flat index `i`. Ports `trie_assoc`.
fn trie_assoc(node: &Gc<Node>, shift: u32, i: usize, item: Value) -> Gc<Node> {
    if shift == 0 {
        let mut items = node.as_leaf().to_vec();
        items[i & MASK] = item;
        Gc::new(Node::Leaf(items))
    } else {
        let digit = (i >> shift) & MASK;
        let mut children = node.as_branch().to_vec();
        children[digit] = trie_assoc(&children[digit], shift - B, i, item);
        Gc::new(Node::Branch(children))
    }
}

/// Remove the rightmost leaf from the subtree at `node` (level `shift`).
/// Returns `(new_subtree_root_or_None, removed_leaf_items)`. Ports `pop_tail`.
fn pop_tail(node: &Gc<Node>, shift: u32, trie_count: usize) -> (Option<Gc<Node>>, Vec<Value>) {
    let digit = ((trie_count - 1) >> shift) & MASK;
    if shift == B {
        let leaf = node.as_branch()[digit].as_leaf().to_vec();
        if digit == 0 {
            return (None, leaf);
        }
        let mut children = node.as_branch().to_vec();
        children.truncate(digit);
        (Some(Gc::new(Node::Branch(children))), leaf)
    } else {
        let child = &node.as_branch()[digit];
        let (new_child, leaf) = pop_tail(child, shift - B, trie_count);
        if new_child.is_none() && digit == 0 {
            return (None, leaf);
        }
        let mut children = node.as_branch().to_vec();
        match new_child {
            None => children.truncate(digit),
            Some(nc) => children[digit] = nc,
        }
        (Some(Gc::new(Node::Branch(children))), leaf)
    }
}

/// Set `slots[idx]`, growing by one when `idx == len` (the append-a-child
/// case). vec.c relies on the union's fixed 32 slots + a `count`; here the
/// child Vec length plays that role.
fn set_or_push(children: &mut Vec<Gc<Node>>, idx: usize, val: Gc<Node>) {
    if idx == children.len() {
        children.push(val);
    } else {
        children[idx] = val;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::printer::print_str;
    use crate::reader::read_one;

    #[test]
    fn conj_grows_past_32_and_1024() {
        let mut v = PVec::empty();
        for i in 0..100 {
            v = v.conj(Value::Int(i));
        }
        assert_eq!(v.len(), 100);
        for i in 0..100 {
            assert!(matches!(v.nth(i as usize), Some(Value::Int(n)) if *n == i));
        }
        // Past the 1024 boundary (two trie levels).
        let mut big = PVec::empty();
        for i in 0..2000 {
            big = big.conj(Value::Int(i));
        }
        assert_eq!(big.len(), 2000);
        for i in [0usize, 31, 32, 1023, 1024, 1055, 1999] {
            assert!(matches!(big.nth(i), Some(Value::Int(n)) if *n == i as i64));
        }
        assert!(big.nth(2000).is_none());
    }

    #[test]
    fn assoc_is_persistent() {
        let v = PVec::from_vec(vec![Value::Int(1), Value::Int(2), Value::Int(3)]);
        let v2 = v.assoc(1, Value::Int(99)).unwrap();
        // New vec has the change.
        assert!(matches!(v2.nth(1), Some(Value::Int(99))));
        // Original is unchanged (immutability / structural sharing).
        assert!(matches!(v.nth(1), Some(Value::Int(2))));
        // assoc at len appends; beyond len fails.
        assert_eq!(v.assoc(3, Value::Int(4)).unwrap().len(), 4);
        assert!(v.assoc(4, Value::Int(4)).is_none());
    }

    #[test]
    fn assoc_persistent_across_trie() {
        // Exercise a path-copy update deep in the trie; original unchanged.
        let v = PVec::from_iter((0..100).map(Value::Int));
        let v2 = v.assoc(50, Value::Int(-1)).unwrap();
        assert!(matches!(v2.nth(50), Some(Value::Int(-1))));
        assert!(matches!(v.nth(50), Some(Value::Int(50))));
        assert_eq!(v.len(), 100);
        assert_eq!(v2.len(), 100);
    }

    #[test]
    fn pop_reduces_count_and_drops_last() {
        let v = PVec::from_iter((0..40).map(Value::Int));
        let v2 = v.pop().unwrap();
        assert_eq!(v2.len(), 39);
        assert!(matches!(v2.nth(38), Some(Value::Int(38))));
        assert!(v2.nth(39).is_none());
        // Original untouched.
        assert_eq!(v.len(), 40);
        assert!(matches!(v.nth(39), Some(Value::Int(39))));
        // Pop everything.
        let one = PVec::from_vec(vec![Value::Int(7)]);
        assert_eq!(one.pop().unwrap().len(), 0);
        assert!(PVec::empty().pop().is_none());
    }

    #[test]
    fn read_print_roundtrip_small() {
        let (v, _) = read_one("[1 2 3]").unwrap();
        assert_eq!(print_str(&v), "[1 2 3]");
    }

    #[test]
    fn read_print_roundtrip_large() {
        // 200 elements => two trie levels; must round-trip through read.
        let src = format!(
            "[{}]",
            (0..200)
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(" ")
        );
        let (v, _) = read_one(&src).unwrap();
        assert_eq!(print_str(&v), src);
    }
}
