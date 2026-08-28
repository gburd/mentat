//! Primitive installation table. Ports the registration in `src/prim/install.c`.

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
}
