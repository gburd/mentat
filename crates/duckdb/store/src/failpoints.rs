// Copyright 2026 Greg Burd
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Test-only (feature `failpoints`, which the test harness enables and the
//! extension never does): run a hook, or panic, at a named point, to test
//! that a panic leaves no transaction open and nothing attached, and to look
//! at state mid-operation.

use std::cell::RefCell;

type Hook = (&'static str, Box<dyn FnOnce()>);

thread_local! {
    static ARMED: RefCell<Option<Hook>> = const { RefCell::new(None) };
}

/// Panic at the next `hit(name)` on this thread (once).
pub fn arm(name: &'static str) {
    run_at(name, move || panic!("failpoint {name}"));
}

/// Run `f` at the next `hit(name)` on this thread (once).
pub fn run_at(name: &'static str, f: impl FnOnce() + 'static) {
    ARMED.with(|a| *a.borrow_mut() = Some((name, Box::new(f))));
}

pub(crate) fn hit(name: &str) {
    let f = ARMED.with(|a| {
        let mut a = a.borrow_mut();
        match a.as_ref() {
            Some((n, _)) if *n == name => a.take().map(|(_, f)| f),
            _ => None,
        }
    });
    if let Some(f) = f {
        f();
    }
}
