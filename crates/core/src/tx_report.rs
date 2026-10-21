// Copyright 2018 Mozilla
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

#![allow(dead_code)]

use std::collections::BTreeMap;

use core_traits::Entid;

use crate::{DateTime, Utc};

/// A transaction report summarizes an applied transaction.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialOrd, PartialEq)]
pub struct TxReport {
    /// The transaction ID of the transaction.
    pub tx_id: Entid,

    /// The timestamp when the transaction began to be committed.
    pub tx_instant: DateTime<Utc>,

    /// A map from string literal tempid to resolved or allocated entid.
    ///
    /// Every string literal tempid presented to the transactor either resolves via upsert to an
    /// existing entid, or is allocated a new entid.  (It is possible for multiple distinct string
    /// literal tempids to all unify to a single freshly allocated entid.)
    pub tempids: BTreeMap<String, Entid>,

    /// How many datoms the transaction added to the log, and how many it retracted (#281).
    /// Both leave out the transaction's own `:db/txInstant` datom, so `0, 0` means the
    /// transaction changed nothing. A redundant assertion (the datom is already there) or a
    /// retraction of an absent datom is not counted; replacing a cardinality-one value counts
    /// one assertion and one retraction.
    pub datoms_asserted: usize,
    pub datoms_retracted: usize,
}

impl TxReport {
    pub fn new(
        tx_id: Entid,
        tx_instant: DateTime<Utc>,
        tempids: BTreeMap<String, Entid>,
        datoms_asserted: usize,
        datoms_retracted: usize,
    ) -> TxReport {
        TxReport {
            tx_id,
            tx_instant,
            tempids,
            datoms_asserted,
            datoms_retracted,
        }
    }

    /// True if the transaction asserted and retracted nothing (beyond its `:db/txInstant`).
    pub fn is_empty(&self) -> bool {
        self.datoms_asserted == 0 && self.datoms_retracted == 0
    }
}
