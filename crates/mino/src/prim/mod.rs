//! Primitive installation table. Ports the registration in `src/prim/install.c`.

pub mod collections;
pub mod numeric;
pub mod reflection;
pub mod regex;
pub mod stateful;
pub mod string;

use crate::env::Env;
use crate::symbol::Symbol;
use crate::value::{Prim, PrimFn, Value};

fn register(root: &Env, name: &'static str, f: PrimFn) {
    // A `ns/name` spelling registers a namespaced key (isolated from the bare
    // clojure.core name); otherwise a plain bare key. `/` alone (the division
    // prim) is not a namespace separator.
    let sym = match name.rsplit_once('/') {
        Some((ns, n)) if !ns.is_empty() && !n.is_empty() => Symbol::namespaced(ns, n),
        _ => Symbol::plain(name),
    };
    root.set(sym, Value::Prim(Prim(f, name)));
}

/// Install the core numeric primitives into `root`. Called from `Interp::new`.
pub fn install_core(root: &Env) {
    register(root, "+", numeric::add);
    register(root, "-", numeric::sub);
    register(root, "*", numeric::mul);
    register(root, "/", numeric::div);
    register(root, "+'", numeric::addp);
    register(root, "-'", numeric::subp);
    register(root, "*'", numeric::mulp);
    register(root, "inc'", numeric::incp);
    register(root, "dec'", numeric::decp);
    register(root, "=", numeric::eq);
    register(root, "<", numeric::lt);
    register(root, ">", numeric::gt);
    register(root, "<=", numeric::le);
    register(root, ">=", numeric::ge);
    // mod / rem / quot (numeric.c prim_mqr).
    register(root, "mod", numeric::mod_);
    register(root, "rem", numeric::rem);
    register(root, "quot", numeric::quot);
    // Bitwise (numeric_bit.c).
    register(root, "bit-and", numeric::bit_and);
    register(root, "bit-or", numeric::bit_or);
    register(root, "bit-xor", numeric::bit_xor);
    register(root, "bit-not", numeric::bit_not);
    register(root, "bit-shift-left", numeric::bit_shift_left);
    register(root, "bit-shift-right", numeric::bit_shift_right);
    register(
        root,
        "unsigned-bit-shift-right",
        numeric::unsigned_bit_shift_right,
    );
    // Coercions (numeric_coerce.c + bignum.c + ratio.c).
    register(root, "int", numeric::int_);
    register(root, "long", numeric::long_);
    register(root, "short", numeric::short_);
    register(root, "byte", numeric::byte_);
    register(root, "char", numeric::char_);
    register(root, "double", numeric::double_);
    register(root, "float", numeric::float_);
    register(root, "bigint", numeric::bigint);
    register(root, "numerator", numeric::numerator);
    register(root, "denominator", numeric::denominator);
    register(root, "rationalize", numeric::rationalize);
    register(root, "parse-long", numeric::parse_long);
    register(root, "parse-double", numeric::parse_double);
    // unchecked-* family (numeric.c).
    register(root, "unchecked-add", numeric::unchecked_add);
    register(root, "unchecked-subtract", numeric::unchecked_subtract);
    register(root, "unchecked-multiply", numeric::unchecked_multiply);
    register(root, "unchecked-inc", numeric::unchecked_inc);
    register(root, "unchecked-dec", numeric::unchecked_dec);
    register(root, "unchecked-negate", numeric::unchecked_negate);
    register(root, "unchecked-long", numeric::unchecked_long_cast);
    register(root, "unchecked-int", numeric::unchecked_int_cast);
    register(root, "unchecked-short", numeric::unchecked_short_cast);
    register(root, "unchecked-byte", numeric::unchecked_byte_cast);
    register(root, "unchecked-char", numeric::unchecked_char_cast);
    register(root, "unchecked-float", numeric::unchecked_float_cast);
    register(root, "unchecked-double", numeric::unchecked_double_cast);
    register(root, "unchecked-add-int", numeric::unchecked_add_int);
    register(
        root,
        "unchecked-subtract-int",
        numeric::unchecked_subtract_int,
    );
    register(
        root,
        "unchecked-multiply-int",
        numeric::unchecked_multiply_int,
    );
    register(root, "unchecked-inc-int", numeric::unchecked_inc_int);
    register(root, "unchecked-dec-int", numeric::unchecked_dec_int);
    register(root, "unchecked-negate-int", numeric::unchecked_negate_int);
    register(
        root,
        "unchecked-remainder-int",
        numeric::unchecked_remainder_int,
    );
    register(root, "unchecked-divide-int", numeric::quot);
    // Tier predicates.
    register(root, "bigint?", numeric::bigint_p);
    register(root, "ratio?", numeric::ratio_p);
    register(root, "rational?", numeric::rational_p);
    register(root, "decimal?", numeric::decimal_p);

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
    register(root, "NaN?", c::nan_p);
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
    register(root, "namespace", r::namespace);
    register(root, "hash", r::hash);
    register(root, "class", r::class);
    register(root, "resolve", r::resolve);
    register(root, "eval", r::eval);
    register(root, "not", r::not);
    register(root, "name", r::name);
    register(root, "keyword", r::keyword);
    register(root, "find-keyword", r::find_keyword);
    register(root, "regex?", r::regex_p);
    register(root, "mino-version", r::mino_version);
    register(root, "read-string", r::read_string);
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
    // Core string prims + clojure.string C primitives (string.c). The rest of
    // clojure.string (blank?/capitalize/escape/triml/trimr/reverse/index-of/
    // last-index-of/re-quote-replacement) is defined on top of these by the
    // bundled lib/clojure/string.clj loaded in Interp::load_supplement.
    register(root, "subs", st::subs);
    register(root, "char-at", st::char_at);
    register(root, "upper-case", st::upper_case);
    register(root, "lower-case", st::lower_case);
    register(root, "trim", st::trim);
    register(root, "starts-with?", st::starts_with_p);
    register(root, "ends-with?", st::ends_with_p);
    register(root, "includes?", st::includes_p);
    register(root, "join", st::join);
    register(root, "split", st::split);
    // The clojure.string `replace`/`replace-first` C primitives, under private
    // dash-prefixed names that never collide with a clojure.core var. The
    // bundled lib/clojure/string.clj captures these as `prim-replace` and
    // wraps them with char/regex dispatch under the public bare names.
    register(root, "-string-replace", st::replace);
    register(root, "-string-replace-first", st::replace_first);

    // Regex prims (Task 5.2, prim/regex.c). re-seq/re-matcher/re-groups and
    // the matcher-aware re-find arity are defined in core.clj on top of these.
    use regex as rx;
    register(root, "re-pattern", rx::re_pattern);
    register(root, "re-find", rx::re_find);
    register(root, "re-matches", rx::re_matches);
    register(root, "re-find-from", rx::re_find_from);

    // Atom / stateful prims (Task 5.3, prim/stateful.c). Single-threaded, so
    // swap!/compare-and-set! are plain read-compute-store (no CAS loop).
    use stateful as sf;
    register(root, "atom", sf::atom);
    register(root, "deref", sf::deref);
    register(root, "delay*", sf::delay_star);
    register(root, "realized?", sf::realized_p);
    register(root, "reset!", sf::reset_bang);
    register(root, "reset-vals!", sf::reset_vals_bang);
    register(root, "swap!", sf::swap_bang);
    register(root, "swap-vals!", sf::swap_vals_bang);
    register(root, "compare-and-set!", sf::compare_and_set_bang);
    register(root, "atom?", sf::atom_p);
    register(root, "add-watch", sf::add_watch);
    register(root, "remove-watch", sf::remove_watch);
    register(root, "set-validator!", sf::set_validator);
    register(root, "get-validator", sf::get_validator);

    // Metadata prims (Task 5.3, prim/meta.c). meta/with-meta/vary-meta.
    use reflection as m;
    register(root, "meta", m::meta);
    register(root, "with-meta", m::with_meta);
    register(root, "vary-meta", m::vary_meta);
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
    register(root, "lazy-keep", c::lazy_keep);
    register(root, "lazy-remove", c::lazy_remove);
    register(root, "lazy-map-indexed", c::lazy_map_indexed);
    register(root, "rerun-seq", c::rerun_seq);
    register(root, "__transduce-fuse", c::transduce_fuse);
    register(root, "reduce", c::reduce);
    register(root, "apply", c::apply_prim);
    register(root, "into", c::into);
    register(root, "mapv", c::mapv);
    register(root, "filterv", c::filterv);
    register(root, "range", c::range);
    register(root, "repeat", c::repeat);
    register(root, "vec", c::vec_prim);
    register(root, "set", c::set_prim);
    register(root, "hash-map", c::hash_map);
    register(root, "hash-set", c::hash_set);
    register(root, "sort", c::sort);
    register(root, "sort-by", c::sort_by);
    register(root, "compare", c::compare);
    register(root, "concat", c::concat);
    register(root, "assoc", c::assoc);
    register(root, "dissoc", c::dissoc);
    register(root, "disj", c::disj);
    register(root, "pop", c::pop);
    register(root, "keys", c::keys);
    register(root, "vals", c::vals);
    register(root, "merge", c::merge);
    register(root, "merge-with", c::merge_with);
    register(root, "inc", numeric::inc);
    register(root, "dec", numeric::dec);
}
