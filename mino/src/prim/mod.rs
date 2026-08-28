//! Primitive installation table. Ports the registration in `src/prim/install.c`.

pub mod collections;
pub mod numeric;
pub mod reflection;
pub mod string;

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

    install_eager_seq_prims(root);
    // Type / numeric predicates (reflection.c + numeric.c).
    use collections as c;
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
    register(root, "cons?", c::cons_p);
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

    // Error / reflection prims (Task 3.2, prim/reflection.c + core.clj
    // ex-info/ex-message/ex-data).
    use reflection as r;
    register(root, "throw", r::throw);
    register(root, "ex-info", r::ex_info);
    register(root, "ex-message", r::ex_message);
    register(root, "ex-data", r::ex_data);
    register(root, "gensym", r::gensym);
    register(root, "macroexpand-1", r::macroexpand_1);
    register(root, "macroexpand", r::macroexpand);
    register(root, "not", r::not);
    register(root, "meta", r::meta);
    register(root, "with-meta", r::with_meta);
    register(root, "vary-meta", r::vary_meta);
    register(root, "name", r::name);
    register(root, "keyword", r::keyword);
    register(root, "symbol", r::symbol);
    register(root, "true?", r::true_p);
    register(root, "false?", r::false_p);
    register(root, "some?", r::some_p);
    register(root, "type", r::type_);
    register(root, "mino-installed?", r::mino_installed_p);

    // String / print prims (Task 5.1 subset core.clj + corpus need).
    use string as st;
    register(root, "str", st::str_);
    register(root, "pr-str", st::pr_str);
    register(root, "println", st::println_);
    register(root, "print", st::print_);
    register(root, "prn", st::prn);
}

/// The eager collection/sequence prims the port implements natively. Split
/// out so `Interp` can RE-assert them AFTER core.clj loads: core.clj redefines
/// `map`/`filter`/`concat`/etc. as LAZY seqs built on machinery the port has
/// not ported yet (lazy-seq/chunked cons), so those defns load but throw when
/// called. Re-registering the eager versions makes the working implementation
/// win. ponytail: eager prims shadow lazy core.clj defns until Phase 5 lazy seqs.
pub fn install_eager_seq_prims(root: &Env) {
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
}
