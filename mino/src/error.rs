//! Thrown values. Ports the throw/error payload model from `runtime/error.c`,
//! scoped for Task 1.1 to just carrying a `Value`: eval returns
//! `Result<Value, Throw>`. Phase 3 adds `try`/`catch` and richer diagnostics.

use crate::printer::print_str;
use crate::value::Value;
use gc::Gc;
use std::fmt;

/// A thrown Clojure value.
pub struct Throw(pub Value);

/// Wrap a string message as a thrown `Str` value.
pub fn throw_str(msg: &str) -> Throw {
    Throw(Value::Str(Gc::new(msg.to_string())))
}

impl fmt::Display for Throw {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&print_str(&self.0))
    }
}

impl fmt::Debug for Throw {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Throw({})", print_str(&self.0))
    }
}
