//! Collection, sequence, and predicate primitives.
//! Ports `src/prim/collections.c` (car cdr cons count nth first rest assoc get
//! conj keys vals contains? disj dissoc) and `src/prim/sequences.c` +
//! `sequences_seq.c` (reduce into apply reverse sort mapv filterv seq map
//! filter concat range ...), plus the type predicates from `reflection.c` /
//! `numeric.c`.
//!
//! `map`/`filter` are EAGER here (fully realized into a list). mino's are lazy
//! (lazy.c); the printed shape of an eager realization matches a lazy seq
//! (`(2 3 4)`), so the corpus can't tell. // Phase 5: lazy seqs.
//!
//! `list?` returns true for a proper list: any `Cons` chain or the empty
//! list (`Value::EmptyList`), matching the empty-list-distinct-from-nil model.
//! mino further distinguishes a concrete PersistentList from a lazy-seq cell;
//! the port has a single `Cons` arm, which are_test doesn't exercise.
//! // Phase 5: distinct seq types for exact list?/seq?.

use crate::collections::hashing::eq_val;
use crate::collections::map::{PMap, PSet};
use crate::collections::vector::PVec;
use crate::error::{throw_str, Throw};
use crate::eval::func::apply;
use crate::eval::Interp;
use crate::value::Value;
use gc::Gc;

// ---- shared helpers ----------------------------------------------------

/// Build a proper list from a slice. Empty -> the empty-list value `()`
/// (distinct from nil, matching mino's MINO_EMPTY_LIST).
fn list_of(items: &[Value]) -> Value {
    let mut acc = Value::EmptyList;
    for v in items.iter().rev() {
        acc = Value::Cons(Gc::new((v.clone(), acc)));
    }
    acc
}

/// Realize any seqable value into a flat Vec, in order. nil -> empty.
/// Ports the `seq_iter_*` fan-out: list/vector walk elements, string yields
/// chars, map yields `[k v]` entry vectors, set yields elements.
fn to_vec(v: &Value) -> Result<Vec<Value>, Throw> {
    match v {
        Value::Nil | Value::EmptyList => Ok(Vec::new()),
        Value::Cons(_) => {
            let mut out = Vec::new();
            let mut cur = v;
            while let Value::Cons(cell) = cur {
                out.push(cell.0.clone());
                cur = &cell.1;
            }
            Ok(out)
        }
        Value::Vector(vec) => Ok(vec.iter().cloned().collect()),
        Value::Str(s) => Ok(s.chars().map(Value::Char).collect()),
        Value::Map(m) => Ok(m
            .entries()
            .map(|(k, val)| Value::Vector(Gc::new(PVec::from_vec(vec![k.clone(), val.clone()]))))
            .collect()),
        Value::Set(set) => Ok(set.iter().cloned().collect()),
        other => Err(throw_str(&format!(
            "don't know how to create seq from: {}",
            crate::printer::print_str(other)
        ))),
    }
}

fn as_int(v: &Value, ctx: &str) -> Result<i64, Throw> {
    match v {
        Value::Int(n) => Ok(*n),
        _ => Err(throw_str(&format!(
            "{ctx}: expected integer, got {}",
            crate::printer::print_str(v)
        ))),
    }
}

fn bool_v(b: bool) -> Value {
    Value::Bool(b)
}

// ---- sequence / list basics -------------------------------------------

pub fn list(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    Ok(list_of(args))
}

/// `(cons x seq)`: prepend x. seq is realized then re-linked as a list.
pub fn cons(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 {
        return Err(throw_str("cons requires exactly 2 arguments"));
    }
    let tail = to_vec(&args[1])?;
    let mut acc = list_of(&tail);
    acc = Value::Cons(Gc::new((args[0].clone(), acc)));
    Ok(acc)
}

pub fn first(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let items = to_vec(&args[0])?;
    Ok(items.first().cloned().unwrap_or(Value::Nil))
}

/// `(rest coll)`: everything after the first, as a list. Empty/nil -> `()`.
pub fn rest(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let items = to_vec(&args[0])?;
    Ok(list_of(items.get(1..).unwrap_or(&[])))
}

/// `(next coll)`: like rest but nil when the tail is empty.
pub fn next_(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let items = to_vec(&args[0])?;
    match items.get(1..) {
        Some(tail) if !tail.is_empty() => Ok(list_of(tail)),
        _ => Ok(Value::Nil),
    }
}

/// `(count coll)`: elements in a coll, chars in a string, 0 for nil.
pub fn count(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let n = match &args[0] {
        Value::Nil | Value::EmptyList => 0,
        Value::Vector(v) => v.len(),
        Value::Map(m) => m.count(),
        Value::Set(s) => s.count(),
        Value::Str(s) => s.chars().count(),
        Value::Cons(_) => to_vec(&args[0])?.len(),
        other => {
            return Err(throw_str(&format!(
                "count not supported on: {}",
                crate::printer::print_str(other)
            )))
        }
    };
    Ok(Value::Int(n as i64))
}

/// `(nth coll i)` / `(nth coll i not-found)`. Without not-found, out-of-range
/// throws (matches mino MBD001); with it, returns not-found.
pub fn nth(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() < 2 {
        return Err(throw_str("nth requires at least 2 arguments"));
    }
    let i = as_int(&args[1], "nth")?;
    let not_found = args.get(2);
    // Fast path for vectors/strings; else realize.
    let get_idx = |items: &[Value]| -> Option<Value> {
        if i < 0 {
            None
        } else {
            items.get(i as usize).cloned()
        }
    };
    let found = match &args[0] {
        Value::Str(s) => {
            let chars: Vec<char> = s.chars().collect();
            if i >= 0 {
                chars.get(i as usize).map(|c| Value::Char(*c))
            } else {
                None
            }
        }
        other => get_idx(&to_vec(other)?),
    };
    match found {
        Some(v) => Ok(v),
        None => match not_found {
            Some(nf) => Ok(nf.clone()),
            None => Err(throw_str("nth index out of range")),
        },
    }
}

/// `(conj coll x ...)` polymorphic: list prepends, vector appends, set adds,
/// map takes `[k v]` pairs, nil -> list. Confirmed against the binary.
pub fn conj(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.is_empty() {
        return Ok(Value::Vector(Gc::new(PVec::empty())));
    }
    let mut coll = args[0].clone();
    for x in &args[1..] {
        coll = conj1(&coll, x)?;
    }
    Ok(coll)
}

fn conj1(coll: &Value, x: &Value) -> Result<Value, Throw> {
    match coll {
        // nil conj -> a one-element list; empty-list conj -> prepend.
        Value::Nil | Value::EmptyList => Ok(Value::Cons(Gc::new((x.clone(), Value::EmptyList)))),
        Value::Cons(_) => Ok(Value::Cons(Gc::new((x.clone(), coll.clone())))),
        Value::Vector(v) => Ok(Value::Vector(Gc::new(v.conj(x.clone())))),
        Value::Set(s) => Ok(Value::Set(Gc::new(s.conj(x.clone())))),
        Value::Map(m) => {
            // Clojure conj on a map accepts: another map (merge), a [k v]
            // vector/2-list (add), or nil (no-op).
            match x {
                Value::Nil => Ok(Value::Map(m.clone())),
                Value::Map(other) => {
                    let mut out = crate::collections::map::PMap::empty();
                    for (k, v) in m.entries() {
                        out = out.assoc(k.clone(), v.clone());
                    }
                    for (k, v) in other.entries() {
                        out = out.assoc(k.clone(), v.clone());
                    }
                    Ok(Value::Map(Gc::new(out)))
                }
                _ => {
                    let pair = to_vec(x)?;
                    if pair.len() != 2 {
                        return Err(throw_str("conj on a map requires a [k v] pair"));
                    }
                    Ok(Value::Map(Gc::new(m.assoc(pair[0].clone(), pair[1].clone()))))
                }
            }
        }
        other => Err(throw_str(&format!(
            "cannot conj onto: {}",
            crate::printer::print_str(other)
        ))),
    }
}

/// `(get coll k)` / `(get coll k not-found)`: map by key, vector/string by
/// index, set by membership. nil-safe. Missing -> not-found (or nil).
pub fn get(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() < 2 {
        return Err(throw_str("get requires at least 2 arguments"));
    }
    let not_found = args.get(2).cloned().unwrap_or(Value::Nil);
    let found = match &args[0] {
        Value::Nil => None,
        Value::Map(m) => m.get(&args[1]).cloned(),
        Value::Set(s) => {
            if s.contains(&args[1]) {
                Some(args[1].clone())
            } else {
                None
            }
        }
        Value::Vector(v) => match &args[1] {
            Value::Int(i) if *i >= 0 => v.nth(*i as usize).cloned(),
            _ => None,
        },
        Value::Str(s) => match &args[1] {
            Value::Int(i) if *i >= 0 => s.chars().nth(*i as usize).map(Value::Char),
            _ => None,
        },
        // get on any other type returns not-found (nil), like Clojure.
        _ => None,
    };
    Ok(found.unwrap_or(not_found))
}

/// `(contains? coll k)`: map/set key present, vector index in range.
pub fn contains(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 {
        return Err(throw_str("contains? requires exactly 2 arguments"));
    }
    let yes = match &args[0] {
        Value::Nil => false,
        Value::Map(m) => m.contains(&args[1]),
        Value::Set(s) => s.contains(&args[1]),
        Value::Vector(v) => matches!(&args[1], Value::Int(i) if *i >= 0 && (*i as usize) < v.len()),
        Value::Str(s) => {
            matches!(&args[1], Value::Int(i) if *i >= 0 && (*i as usize) < s.chars().count())
        }
        _ => false,
    };
    Ok(bool_v(yes))
}

/// `(empty? coll)`: true for nil and zero-length colls/strings.
pub fn empty_p(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let empty = match &args[0] {
        // Clojure/mino: (empty? nil) and (empty? ()) are both true.
        Value::Nil | Value::EmptyList => true,
        Value::Vector(v) => v.is_empty(),
        Value::Map(m) => m.is_empty(),
        Value::Set(s) => s.is_empty(),
        Value::Str(s) => s.is_empty(),
        Value::Cons(_) => false, // a cons cell always has a head
        other => {
            return Err(throw_str(&format!(
                "empty? not supported on: {}",
                crate::printer::print_str(other)
            )))
        }
    };
    Ok(bool_v(empty))
}

/// `(seq coll)`: a seq of the coll, or nil when empty (Clojure/mino contract).
pub fn seq(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let items = to_vec(&args[0])?;
    if items.is_empty() {
        Ok(Value::Nil)
    } else {
        Ok(list_of(&items))
    }
}

pub fn reverse(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut items = to_vec(&args[0])?;
    items.reverse();
    Ok(list_of(&items))
}

// ---- higher-order ------------------------------------------------------

/// `(map f coll & colls)`: EAGER. Applies f across colls in lockstep, stopping
/// at the shortest, and returns a realized list. // Phase 5: lazy seqs.
pub fn map(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() < 2 {
        return Err(throw_str("map requires a function and at least one coll"));
    }
    let f = args[0].clone();
    let colls: Vec<Vec<Value>> = args[1..].iter().map(to_vec).collect::<Result<_, _>>()?;
    let n = colls.iter().map(|c| c.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let call: Vec<Value> = colls.iter().map(|c| c[i].clone()).collect();
        out.push(apply(it, &f, &call)?);
    }
    Ok(list_of(&out))
}

/// `(filter pred coll)`: EAGER list of items where `(pred x)` is truthy.
pub fn filter(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 {
        return Err(throw_str("filter requires a predicate and a coll"));
    }
    let f = args[0].clone();
    let mut out = Vec::new();
    for x in to_vec(&args[1])? {
        if apply(it, &f, std::slice::from_ref(&x))?.is_truthy() {
            out.push(x);
        }
    }
    Ok(list_of(&out))
}

/// `(reduce f coll)` / `(reduce f init coll)`. No-init on empty calls `(f)`;
/// no-init on non-empty seeds with the first element.
pub fn reduce(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args.len() {
        2 => {
            let f = args[0].clone();
            let items = to_vec(&args[1])?;
            if items.is_empty() {
                return apply(it, &f, &[]);
            }
            let mut acc = items[0].clone();
            for x in &items[1..] {
                acc = apply(it, &f, &[acc, x.clone()])?;
            }
            Ok(acc)
        }
        3 => {
            let f = args[0].clone();
            let mut acc = args[1].clone();
            for x in to_vec(&args[2])? {
                acc = apply(it, &f, &[acc, x])?;
            }
            Ok(acc)
        }
        _ => Err(throw_str("reduce requires 2 or 3 arguments")),
    }
}

/// `(apply f a b ... args)`: flatten the trailing coll into the call.
pub fn apply_prim(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() < 2 {
        return Err(throw_str("apply requires a function and an argument list"));
    }
    let f = args[0].clone();
    let mut call: Vec<Value> = args[1..args.len() - 1].to_vec();
    call.extend(to_vec(&args[args.len() - 1])?);
    apply(it, &f, &call)
}

/// `(into to from)`: conj every element of `from` into `to`. For a list target
/// this reverses order (each element prepends), matching the binary.
pub fn into(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() != 2 {
        return Err(throw_str("into requires exactly 2 arguments"));
    }
    let mut to = args[0].clone();
    for x in to_vec(&args[1])? {
        to = conj1(&to, &x)?;
    }
    Ok(to)
}

pub fn mapv(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let l = map(it, args)?;
    Ok(Value::Vector(Gc::new(to_vec(&l)?.into_iter().collect())))
}

pub fn filterv(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let l = filter(it, args)?;
    Ok(Value::Vector(Gc::new(to_vec(&l)?.into_iter().collect())))
}

/// `(range)` (infinite: unsupported here), `(range end)`, `(range start end)`,
/// `(range start end step)`. Returns a realized list. // Phase 5: lazy range.
pub fn range(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (start, end, step) = match args.len() {
        0 => return Err(throw_str("infinite range needs lazy seqs (Phase 5)")),
        1 => (0, as_int(&args[0], "range")?, 1),
        2 => (as_int(&args[0], "range")?, as_int(&args[1], "range")?, 1),
        3 => (
            as_int(&args[0], "range")?,
            as_int(&args[1], "range")?,
            as_int(&args[2], "range")?,
        ),
        _ => return Err(throw_str("range takes 0 to 3 arguments")),
    };
    if step == 0 {
        return Err(throw_str("range step cannot be 0"));
    }
    let mut out = Vec::new();
    let mut i = start;
    if step > 0 {
        while i < end {
            out.push(Value::Int(i));
            i += step;
        }
    } else {
        while i > end {
            out.push(Value::Int(i));
            i += step;
        }
    }
    Ok(list_of(&out))
}

pub fn vec_prim(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let items = if args.is_empty() {
        Vec::new()
    } else {
        to_vec(&args[0])?
    };
    Ok(Value::Vector(Gc::new(items.into_iter().collect())))
}

pub fn set_prim(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let items = if args.is_empty() {
        Vec::new()
    } else {
        to_vec(&args[0])?
    };
    let mut s = PSet::empty();
    for x in items {
        s = s.conj(x);
    }
    Ok(Value::Set(Gc::new(s)))
}

/// `(hash-map k v k v ...)`.
pub fn hash_map(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() % 2 != 0 {
        return Err(throw_str("hash-map requires an even number of arguments"));
    }
    let mut m = PMap::empty();
    for kv in args.chunks(2) {
        m = m.assoc(kv[0].clone(), kv[1].clone());
    }
    Ok(Value::Map(Gc::new(m)))
}

/// `(hash-set x ...)`.
pub fn hash_set(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut s = PSet::empty();
    for x in args {
        s = s.conj(x.clone());
    }
    Ok(Value::Set(Gc::new(s)))
}

// A total ordering over the numeric/string/char/kw tiers used by `sort`.
// Ports the default comparator: numeric by magnitude, strings/chars/keywords
// lexicographically. Mixed uncomparable types -> error.
fn default_cmp(a: &Value, b: &Value) -> Result<std::cmp::Ordering, Throw> {
    use std::cmp::Ordering;
    // nil sorts before everything (Clojure: (compare nil x) < 0, (compare x nil) > 0).
    match (a, b) {
        (Value::Nil, Value::Nil) => return Ok(Ordering::Equal),
        (Value::Nil, _) => return Ok(Ordering::Less),
        (_, Value::Nil) => return Ok(Ordering::Greater),
        _ => {}
    }
    let as_f = |v: &Value| match v {
        Value::Int(n) => Some(*n as f64),
        Value::Float(x) => Some(*x),
        _ => None,
    };
    if let (Some(x), Some(y)) = (as_f(a), as_f(b)) {
        return x.partial_cmp(&y).ok_or_else(|| throw_str("cannot compare NaN"));
    }
    match (a, b) {
        (Value::Str(x), Value::Str(y)) => Ok(x.cmp(y)),
        (Value::Char(x), Value::Char(y)) => Ok(x.cmp(y)),
        (Value::Keyword(x), Value::Keyword(y)) => Ok(x.to_string().cmp(&y.to_string())),
        (Value::Sym(x), Value::Sym(y)) => Ok(x.to_string().cmp(&y.to_string())),
        (Value::Bool(x), Value::Bool(y)) => Ok(x.cmp(y)),
        // Vectors compare by length first, then element-wise (Clojure semantics).
        (Value::Vector(x), Value::Vector(y)) => {
            if x.len() != y.len() {
                return Ok(x.len().cmp(&y.len()));
            }
            for i in 0..x.len() {
                let ord = default_cmp(x.nth(i).unwrap(), y.nth(i).unwrap())?;
                if ord != Ordering::Equal {
                    return Ok(ord);
                }
            }
            Ok(Ordering::Equal)
        }
        _ if eq_val(a, b) => Ok(Ordering::Equal),
        _ => Err(throw_str(&format!(
            "cannot compare {} and {}",
            crate::printer::print_str(a),
            crate::printer::print_str(b)
        ))),
    }
}

/// `(compare a b)` -> -1/0/1. The default total order used by `sort`. nil
/// sorts first; numbers by magnitude; strings/chars/keywords/symbols
/// lexicographically; vectors by length then element-wise. Ports `prim_compare`.
pub fn compare(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (a, b) = match args {
        [a, b] => (a, b),
        _ => return Err(throw_str("compare requires two arguments")),
    };
    Ok(Value::Int(match default_cmp(a, b)? {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

/// `(sort coll)` / `(sort cmp coll)`: cmp is either a 2-arg predicate (truthy =
/// "a before b") or defaults to natural order. Returns a realized list.
pub fn sort(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (cmp, coll) = match args.len() {
        1 => (None, &args[0]),
        2 => (Some(args[0].clone()), &args[1]),
        _ => return Err(throw_str("sort requires 1 or 2 arguments")),
    };
    let mut items = to_vec(coll)?;
    sort_with(it, &mut items, cmp.as_ref(), |v| v.clone())?;
    Ok(list_of(&items))
}

/// `(sort-by keyfn coll)` / `(sort-by keyfn cmp coll)`.
pub fn sort_by(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let (keyfn, cmp, coll) = match args.len() {
        2 => (args[0].clone(), None, &args[1]),
        3 => (args[0].clone(), Some(args[1].clone()), &args[2]),
        _ => return Err(throw_str("sort-by requires 2 or 3 arguments")),
    };
    let mut items = to_vec(coll)?;
    // Precompute keys once.
    let keys: Vec<Value> = items
        .iter()
        .map(|x| apply(it, &keyfn, std::slice::from_ref(x)))
        .collect::<Result<_, _>>()?;
    // Pair items with keys, sort by key, unpair.
    let mut idx: Vec<usize> = (0..items.len()).collect();
    let err = sort_indices(it, &mut idx, &keys, cmp.as_ref());
    err?;
    let sorted: Vec<Value> = idx.into_iter().map(|i| items[i].clone()).collect();
    items = sorted;
    Ok(list_of(&items))
}

// Sort `items` in place. `key` projects the comparison key. `cmp` (if given)
// is a user predicate: truthy means the first arg sorts before the second.
fn sort_with(
    it: &mut Interp,
    items: &mut [Value],
    cmp: Option<&Value>,
    key: impl Fn(&Value) -> Value,
) -> Result<(), Throw> {
    let keys: Vec<Value> = items.iter().map(&key).collect();
    let mut idx: Vec<usize> = (0..items.len()).collect();
    sort_indices(it, &mut idx, &keys, cmp)?;
    let reordered: Vec<Value> = idx.iter().map(|&i| items[i].clone()).collect();
    items.clone_from_slice(&reordered);
    Ok(())
}

// Insertion sort over an index permutation, comparing `keys`. Stable and lets
// the comparator be a fallible user fn. // ponytail: O(n^2), fine for corpus;
// swap in a merge sort if sort ever gets hot.
fn sort_indices(
    it: &mut Interp,
    idx: &mut [usize],
    keys: &[Value],
    cmp: Option<&Value>,
) -> Result<(), Throw> {
    for i in 1..idx.len() {
        let mut j = i;
        while j > 0 {
            let a = &keys[idx[j - 1]];
            let b = &keys[idx[j]];
            let before = match cmp {
                // user cmp: truthy (b, a) means b<a, so swap when (cmp b a).
                Some(f) => apply(it, f, &[b.clone(), a.clone()])?.is_truthy(),
                None => default_cmp(a, b)? == std::cmp::Ordering::Greater,
            };
            if before {
                idx.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
    Ok(())
}

pub fn concat(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut out = Vec::new();
    for a in args {
        out.extend(to_vec(a)?);
    }
    Ok(list_of(&out))
}

/// `(assoc coll k v k v ...)`: map or vector.
pub fn assoc(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() < 3 || (args.len() - 1) % 2 != 0 {
        return Err(throw_str("assoc requires a coll and key/value pairs"));
    }
    let mut coll = args[0].clone();
    for kv in args[1..].chunks(2) {
        coll = match &coll {
            Value::Nil => Value::Map(Gc::new(PMap::empty().assoc(kv[0].clone(), kv[1].clone()))),
            Value::Map(m) => Value::Map(Gc::new(m.assoc(kv[0].clone(), kv[1].clone()))),
            Value::Vector(v) => {
                let i = as_int(&kv[0], "assoc")?;
                if i < 0 {
                    return Err(throw_str("assoc index out of range"));
                }
                match v.assoc(i as usize, kv[1].clone()) {
                    Some(nv) => Value::Vector(Gc::new(nv)),
                    None => return Err(throw_str("assoc index out of range")),
                }
            }
            other => {
                return Err(throw_str(&format!(
                    "cannot assoc onto: {}",
                    crate::printer::print_str(other)
                )))
            }
        };
    }
    Ok(coll)
}

/// `(dissoc map k ...)`.
pub fn dissoc(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.is_empty() {
        return Err(throw_str("dissoc requires a map"));
    }
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Map(m) => {
            if args.len() == 1 {
                return Ok(args[0].clone());
            }
            let mut cur = m.dissoc(&args[1]);
            for k in &args[2..] {
                cur = cur.dissoc(k);
            }
            Ok(Value::Map(Gc::new(cur)))
        }
        other => Err(throw_str(&format!(
            "cannot dissoc from: {}",
            crate::printer::print_str(other)
        ))),
    }
}

/// `(disj set x ...)`.
pub fn disj(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    if args.len() < 2 {
        return Err(throw_str("disj requires a set and at least one key"));
    }
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Set(s) => {
            let mut cur = s.disj(&args[1]);
            for x in &args[2..] {
                cur = cur.disj(x);
            }
            Ok(Value::Set(Gc::new(cur)))
        }
        other => Err(throw_str(&format!(
            "cannot disj from: {}",
            crate::printer::print_str(other)
        ))),
    }
}

/// `(pop coll)`: remove the last element of a vector (or first of a list),
/// preserving metadata. Throws on empty. Ports `prim_pop` (collections.c).
pub fn pop(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match args {
        [Value::Vector(v)] => match v.pop() {
            Some(nv) => Ok(Value::Vector(Gc::new(nv))),
            None => Err(throw_str("Can't pop empty vector")),
        },
        // Lists pop from the front (mino: pop on a list = rest).
        [Value::Cons(cell)] => Ok(cell.1.clone()),
        [Value::EmptyList] => Err(throw_str("Can't pop empty list")),
        [other] => Err(throw_str(&format!(
            "cannot pop: {}",
            crate::printer::print_str(other)
        ))),
        _ => Err(throw_str("pop requires one argument")),
    }
}

/// `(keys map)`: keys in insertion order, or nil when empty.
pub fn keys(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Map(m) => {
            if m.is_empty() {
                Ok(Value::Nil)
            } else {
                Ok(list_of(&m.keys().cloned().collect::<Vec<_>>()))
            }
        }
        other => Err(throw_str(&format!(
            "keys not supported on: {}",
            crate::printer::print_str(other)
        ))),
    }
}

/// `(vals map)`: values in insertion order, or nil when empty.
pub fn vals(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    match &args[0] {
        Value::Nil => Ok(Value::Nil),
        Value::Map(m) => {
            if m.is_empty() {
                Ok(Value::Nil)
            } else {
                Ok(list_of(&m.vals().cloned().collect::<Vec<_>>()))
            }
        }
        other => Err(throw_str(&format!(
            "vals not supported on: {}",
            crate::printer::print_str(other)
        ))),
    }
}

/// `(merge m1 m2 ...)`: later maps win; nil args skipped; all-nil -> nil.
pub fn merge(_it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let mut acc: Option<PMap> = None;
    for a in args {
        match a {
            Value::Nil => {}
            Value::Map(m) => match acc.take() {
                // Seed from the FIRST map (cloning its payload) so its metadata
                // survives. Matches mino: (merge x y) keeps x's meta.
                None => acc = Some((**m).clone_shallow_pub()),
                Some(mut cur) => {
                    for (k, v) in m.entries() {
                        cur = cur.assoc(k.clone(), v.clone());
                    }
                    acc = Some(cur);
                }
            },
            other => {
                return Err(throw_str(&format!(
                    "merge expects maps, got: {}",
                    crate::printer::print_str(other)
                )))
            }
        }
    }
    match acc {
        Some(m) => Ok(Value::Map(Gc::new(m))),
        None => Ok(Value::Nil),
    }
}

/// `(merge-with f & maps)` — merge maps; on key collision, combine with
/// `(f existing new)`. Keeps the first map's metadata (mino: C prim in
/// sequences.c). Ports `prim_merge_with`.
pub fn merge_with(it: &mut Interp, args: &[Value]) -> Result<Value, Throw> {
    let Some((f, maps)) = args.split_first() else {
        return Err(throw_str("merge-with requires a function and maps"));
    };
    let mut acc: Option<PMap> = None;
    for a in maps {
        match a {
            Value::Nil => {}
            Value::Map(m) => match acc.take() {
                // Seed from the first map's payload so its metadata survives.
                None => acc = Some((**m).clone_shallow_pub()),
                Some(mut cur) => {
                    for (k, v) in m.entries() {
                        let next = match cur.get(k) {
                            Some(existing) => apply(it, f, &[existing.clone(), v.clone()])?,
                            None => v.clone(),
                        };
                        cur = cur.assoc(k.clone(), next);
                    }
                    acc = Some(cur);
                }
            },
            other => {
                return Err(throw_str(&format!(
                    "merge-with expects maps, got: {}",
                    crate::printer::print_str(other)
                )))
            }
        }
    }
    match acc {
        Some(m) => Ok(Value::Map(Gc::new(m))),
        None => Ok(Value::Nil),
    }
}

// ---- type predicates ---------------------------------------------------

fn pred(args: &[Value], f: impl Fn(&Value) -> bool) -> Result<Value, Throw> {
    Ok(bool_v(args.first().map(f).unwrap_or(false)))
}

pub fn number_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| {
        matches!(
            v,
            Value::Int(_)
                | Value::Float(_)
                | Value::Float32(_)
                | Value::BigInt(_)
                | Value::Ratio(_)
        )
    })
}
pub fn nil_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Nil))
}
pub fn string_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Str(_)))
}
pub fn keyword_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Keyword(_)))
}
pub fn symbol_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Sym(_)))
}
pub fn vector_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Vector(_)))
}
pub fn map_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Map(_)))
}
pub fn set_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Set(_)))
}
pub fn list_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    // A list is a cons chain or the empty list; NOT nil.
    // (list? ()) => true, (list? nil) => false.
    pred(a, |v| matches!(v, Value::Cons(_) | Value::EmptyList))
}
pub fn seq_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    // seq? is true for any seq incl. the empty list; false for nil.
    // (seq? ()) => true, (seq? nil) => false.
    pred(a, |v| matches!(v, Value::Cons(_) | Value::EmptyList))
}
pub fn cons_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    // cons? is true only for a non-empty cons cell; false for (), nil,
    // vectors, etc. Ports `prim_cons_p` (reflection.c). Used by ->/->>/case.
    pred(a, |v| matches!(v, Value::Cons(_)))
}
pub fn fn_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Fn(_) | Value::Prim(_)))
}
pub fn int_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    // int? is the long tier ONLY (not bigint), matching mino's C prim.
    pred(a, |v| matches!(v, Value::Int(_)))
}
pub fn float_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    // float? is true for BOTH the 64-bit float and the 32-bit float32 tier.
    pred(a, |v| matches!(v, Value::Float(_) | Value::Float32(_)))
}
/// `(NaN? x)`: true for a NaN in either float tier.
pub fn nan_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| match v {
        Value::Float(f) => f.is_nan(),
        Value::Float32(f) => f.is_nan(),
        _ => false,
    })
}
pub fn boolean_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Bool(_)))
}
pub fn char_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| matches!(v, Value::Char(_)))
}
pub fn coll_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    pred(a, |v| {
        matches!(
            v,
            Value::Vector(_) | Value::Map(_) | Value::Set(_) | Value::Cons(_) | Value::EmptyList
        )
    })
}

fn int_pred(args: &[Value], f: impl Fn(i64) -> bool, ctx: &str) -> Result<Value, Throw> {
    match args.first() {
        Some(Value::Int(n)) => Ok(bool_v(f(*n))),
        // even?/odd? also accept bigints: test the low bit of the magnitude.
        Some(Value::BigInt(b)) => {
            use num_integer::Integer;
            Ok(bool_v(f(if b.0.is_even() { 0 } else { 1 })))
        }
        Some(other) => Err(throw_str(&format!(
            "{ctx}: not an integer: {}",
            crate::printer::print_str(other)
        ))),
        None => Err(throw_str(&format!("{ctx} requires 1 argument"))),
    }
}

pub fn even_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    int_pred(a, |n| n % 2 == 0, "even?")
}
pub fn odd_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    int_pred(a, |n| n % 2 != 0, "odd?")
}

// zero?/pos?/neg? accept the whole numeric tower (numeric.c). The int and
// float closures cover Int/Float; BigInt and Ratio dispatch on their sign,
// Float32 on its double value.
fn num_pred(args: &[Value], fi: fn(i64) -> bool, ff: fn(f64) -> bool, ctx: &str) -> Result<Value, Throw> {
    use num_traits::Signed;
    // Map a sign (-1/0/1) through the int predicate (which only tests the
    // sign for pos?/neg?/zero?).
    let by_sign = |s: i64| fi(s);
    match args.first() {
        Some(Value::Int(n)) => Ok(bool_v(fi(*n))),
        Some(Value::Float(x)) => Ok(bool_v(ff(*x))),
        Some(Value::Float32(x)) => Ok(bool_v(ff(*x as f64))),
        Some(Value::BigInt(b)) => {
            let s = if b.0.is_positive() { 1 } else if b.0.is_negative() { -1 } else { 0 };
            Ok(bool_v(by_sign(s)))
        }
        Some(Value::Ratio(r)) => {
            let s = if r.0.is_positive() { 1 } else if r.0.is_negative() { -1 } else { 0 };
            Ok(bool_v(by_sign(s)))
        }
        Some(other) => Err(throw_str(&format!(
            "{ctx}: not a number: {}",
            crate::printer::print_str(other)
        ))),
        None => Err(throw_str(&format!("{ctx} requires 1 argument"))),
    }
}

pub fn zero_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    num_pred(a, |n| n == 0, |x| x == 0.0, "zero?")
}
pub fn pos_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    num_pred(a, |n| n > 0, |x| x > 0.0, "pos?")
}
pub fn neg_p(_it: &mut Interp, a: &[Value]) -> Result<Value, Throw> {
    num_pred(a, |n| n < 0, |x| x < 0.0, "neg?")
}

#[cfg(test)]
mod tests {
    use crate::eval::Interp;
    use crate::printer::print_str;

    fn ev(src: &str) -> String {
        let mut it = Interp::new();
        print_str(&it.eval_str(src).unwrap())
    }
    fn err(src: &str) -> bool {
        let mut it = Interp::new();
        it.eval_str(src).is_err()
    }

    #[test]
    fn conj_polymorphism() {
        assert_eq!(ev("(conj (list 1 2) 3)"), "(3 1 2)");
        assert_eq!(ev("(conj [1 2] 3)"), "[1 2 3]");
        assert_eq!(ev("(conj #{1 2} 3)"), "#{1 2 3}");
        assert_eq!(ev("(conj {:a 1} [:b 2])"), "{:a 1, :b 2}");
        assert_eq!(ev("(conj nil 1)"), "(1)");
    }

    #[test]
    fn reduce_variants() {
        assert_eq!(ev("(reduce + [1 2 3 4])"), "10");
        assert_eq!(ev("(reduce + 100 [1 2 3])"), "106");
        assert_eq!(ev("(reduce + [])"), "0"); // no-init empty -> (f)
        assert_eq!(ev("(reduce + 5 [])"), "5");
    }

    #[test]
    fn into_and_apply() {
        assert_eq!(ev("(into [] (list 1 2 3))"), "[1 2 3]");
        assert_eq!(ev("(into [1] [2 3])"), "[1 2 3]");
        assert_eq!(ev("(into #{} [1 1 2])"), "#{1 2}");
        assert_eq!(ev("(into {} [[:a 1] [:b 2]])"), "{:a 1, :b 2}");
        assert_eq!(ev("(into (list) [1 2 3])"), "(3 2 1)");
        assert_eq!(ev("(apply + [1 2 3])"), "6");
        assert_eq!(ev("(apply + 1 2 [3 4])"), "10");
    }

    #[test]
    fn seq_on_empty_is_nil() {
        assert_eq!(ev("(seq [])"), "nil");
        assert_eq!(ev("(seq nil)"), "nil");
        assert_eq!(ev("(seq [1 2])"), "(1 2)");
        assert_eq!(ev("(seq \"ab\")"), "(\\a \\b)");
    }

    #[test]
    fn range_variants() {
        assert_eq!(ev("(range 5)"), "(0 1 2 3 4)");
        assert_eq!(ev("(range 2 5)"), "(2 3 4)");
        assert_eq!(ev("(range 0 10 2)"), "(0 2 4 6 8)");
        assert_eq!(ev("(range 5 0 -1)"), "(5 4 3 2 1)");
        assert_eq!(ev("(range 0 0)"), "()"); // empty range -> () (empty list)
    }

    #[test]
    fn map_filter_eager_shape() {
        assert_eq!(ev("(map inc [1 2 3])"), "(2 3 4)");
        assert_eq!(ev("(map + [1 2] [10 20])"), "(11 22)");
        assert_eq!(ev("(filter even? [1 2 3 4])"), "(2 4)");
        assert_eq!(ev("(mapv inc [1 2 3])"), "[2 3 4]");
        assert_eq!(ev("(filterv even? [1 2 3 4])"), "[2 4]");
    }

    #[test]
    fn seq_basics_match_oracle() {
        assert_eq!(ev("(first [1 2 3])"), "1");
        assert_eq!(ev("(first nil)"), "nil");
        assert_eq!(ev("(rest (list 1))"), "()"); // rest always returns a seq
        assert_eq!(ev("(rest nil)"), "()"); //     (the empty list), never nil.
        assert_eq!(ev("(next (list 1))"), "nil");
        assert_eq!(ev("(next [1 2 3])"), "(2 3)");
        assert_eq!(ev("(count {:a 1 :b 2})"), "2");
        assert_eq!(ev("(count nil)"), "0");
        assert_eq!(ev("(reverse [1 2 3])"), "(3 2 1)");
        assert_eq!(ev("(cons 1 [2 3])"), "(1 2 3)");
    }

    #[test]
    fn nth_get_contains() {
        assert_eq!(ev("(nth [10 20 30] 1)"), "20");
        assert_eq!(ev("(nth [10 20 30] 9 :none)"), ":none");
        assert!(err("(nth [] 0)")); // out of range without not-found throws
        assert_eq!(ev("(get {:a 1} :b :none)"), ":none");
        assert_eq!(ev("(get [10 20] 1)"), "20");
        assert_eq!(ev("(get nil :a)"), "nil");
        assert_eq!(ev("(contains? [10 20] 1)"), "true");
        assert_eq!(ev("(contains? [10 20] 5)"), "false");
        assert_eq!(ev("(contains? #{1 2} 2)"), "true");
    }

    #[test]
    fn map_ops() {
        assert_eq!(ev("(assoc {:a 1} :b 2)"), "{:a 1, :b 2}");
        assert_eq!(ev("(assoc [1 2 3] 1 99)"), "[1 99 3]");
        assert_eq!(ev("(dissoc {:a 1 :b 2} :a)"), "{:b 2}");
        assert_eq!(ev("(disj #{1 2 3} 2)"), "#{1 3}");
        assert_eq!(ev("(keys {:a 1 :b 2})"), "(:a :b)");
        assert_eq!(ev("(vals {:a 1 :b 2})"), "(1 2)");
        assert_eq!(ev("(keys {})"), "nil");
        assert_eq!(ev("(merge {:a 1} {:a 9 :b 2})"), "{:a 9, :b 2}");
        assert_eq!(ev("(merge)"), "nil");
    }

    #[test]
    fn sort_and_concat() {
        assert_eq!(ev("(sort [3 1 2])"), "(1 2 3)");
        assert_eq!(ev("(sort > [1 3 2])"), "(3 2 1)");
        assert_eq!(ev("(sort-by count [\"aaa\" \"a\" \"aa\"])"), "(\"a\" \"aa\" \"aaa\")");
        assert_eq!(ev("(concat [1 2] [3 4])"), "(1 2 3 4)");
        assert_eq!(ev("(concat (list 1) [2] nil)"), "(1 2)");
    }

    #[test]
    fn predicates() {
        assert_eq!(ev("(number? 1)"), "true");
        assert_eq!(ev("(number? :a)"), "false");
        assert_eq!(ev("(vector? [1])"), "true");
        assert_eq!(ev("(vector? (list 1))"), "false");
        assert_eq!(ev("(map? {:a 1})"), "true");
        assert_eq!(ev("(set? #{1})"), "true");
        assert_eq!(ev("(list? (list 1))"), "true");
        assert_eq!(ev("(fn? inc)"), "true");
        assert_eq!(ev("(int? 1)"), "true");
        assert_eq!(ev("(int? 1.0)"), "false");
        assert_eq!(ev("(float? 1.0)"), "true");
        assert_eq!(ev("(char? \\a)"), "true");
        assert_eq!(ev("(coll? [1])"), "true");
        assert_eq!(ev("(coll? 1)"), "false");
        assert_eq!(ev("(even? 4)"), "true");
        assert_eq!(ev("(odd? 3)"), "true");
        assert_eq!(ev("(zero? 0)"), "true");
        assert_eq!(ev("(pos? 1)"), "true");
        assert_eq!(ev("(neg? -1)"), "true");
        assert_eq!(ev("(inc 5)"), "6");
        assert_eq!(ev("(dec 5)"), "4");
        assert_eq!(ev("(inc 2.5)"), "3.5");
    }
}
