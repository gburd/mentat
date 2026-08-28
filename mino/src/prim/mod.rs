//! Primitive installation table. Ports the registration in `src/prim/install.c`.

pub mod collections;
pub mod numeric;

use crate::env::Env;
use crate::symbol::Symbol;
use crate::value::{Prim, PrimFn, Value};

fn register(root: &Env, name: &'static str, f: PrimFn) {
    root.set(Symbol::plain(name), Value::Prim(Prim(f, name)));
}

/// Install the core numeric primitives into `root`. Called from `Interp::new`.
pub fn install_core(root: &Env) {
    register(root, "+", numeric::add);
    register(root, "-", numeric::sub);
    register(root, "*", numeric::mul);
    register(root, "/", numeric::div);
    register(root, "=", numeric::eq);
    register(root, "<", numeric::lt);
    register(root, ">", numeric::gt);
    register(root, "<=", numeric::le);
    register(root, ">=", numeric::ge);

    // Collection / sequence prims (Task 2.3, prim/collections.c + sequences.c).
    use collections as c;
    register(root, "list", c::list);
    register(root, "cons", c::cons);
    register(root, "first", c::first);
    register(root, "rest", c::rest);
    register(root, "next", c::next_);
    register(root, "count", c::count);
    register(root, "nth", c::nth);
    register(root, "conj", c::conj);
    register(root, "get", c::get);
    register(root, "contains?", c::contains);
    register(root, "empty?", c::empty_p);
    register(root, "seq", c::seq);
    register(root, "reverse", c::reverse);
    register(root, "map", c::map);
    register(root, "filter", c::filter);
    register(root, "reduce", c::reduce);
    register(root, "apply", c::apply_prim);
    register(root, "into", c::into);
    register(root, "mapv", c::mapv);
    register(root, "filterv", c::filterv);
    register(root, "range", c::range);
    register(root, "vec", c::vec_prim);
    register(root, "set", c::set_prim);
    register(root, "hash-map", c::hash_map);
    register(root, "hash-set", c::hash_set);
    register(root, "sort", c::sort);
    register(root, "sort-by", c::sort_by);
    register(root, "concat", c::concat);
    register(root, "assoc", c::assoc);
    register(root, "dissoc", c::dissoc);
    register(root, "disj", c::disj);
    register(root, "keys", c::keys);
    register(root, "vals", c::vals);
    register(root, "merge", c::merge);
    register(root, "inc", c::inc);
    register(root, "dec", c::dec);

    // Type / numeric predicates (reflection.c + numeric.c).
    register(root, "number?", c::number_p);
    register(root, "nil?", c::nil_p);
    register(root, "string?", c::string_p);
    register(root, "keyword?", c::keyword_p);
    register(root, "symbol?", c::symbol_p);
    register(root, "vector?", c::vector_p);
    register(root, "map?", c::map_p);
    register(root, "set?", c::set_p);
    register(root, "list?", c::list_p);
    register(root, "seq?", c::seq_p);
    register(root, "fn?", c::fn_p);
    register(root, "int?", c::int_p);
    register(root, "float?", c::float_p);
    register(root, "boolean?", c::boolean_p);
    register(root, "char?", c::char_p);
    register(root, "coll?", c::coll_p);
    register(root, "even?", c::even_p);
    register(root, "odd?", c::odd_p);
    register(root, "zero?", c::zero_p);
    register(root, "pos?", c::pos_p);
    register(root, "neg?", c::neg_p);
}
