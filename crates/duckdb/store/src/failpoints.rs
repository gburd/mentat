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
//! extension never does): panic at a named point, to test that a panic
//! leaves no transaction open and nothing attached.

use std::cell::RefCell;

thread_local! {
    static ARMED: RefCell<Option<&'static str>> = const { RefCell::new(None) };
}

/// Panic at the next `hit(name)` on this thread (once).
pub fn arm(name: &'static str) {
    ARMED.with(|a| *a.borrow_mut() = Some(name));
}

pub(crate) fn hit(name: &str) {
    if ARMED.with(|a| a.borrow().is_some_and(|n| n == name)) {
        ARMED.with(|a| *a.borrow_mut() = None);
        panic!("failpoint {name}");
    }
}
