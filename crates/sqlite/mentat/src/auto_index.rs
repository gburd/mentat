// Copyright 2026 the Mentat authors
//
// Licensed under the Apache License, Version 2.0 (the "License"); you may not use
// this file except in compliance with the License. You may obtain a copy of the
// License at http://www.apache.org/licenses/LICENSE-2.0
// Unless required by applicable law or agreed to in writing, software distributed
// under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES OR
// CONDITIONS OF ANY KIND, either express or implied. See the License for the
// specific language governing permissions and limitations under the License.

//! Automatic index management for the embedded store.
//!
//! `datoms` always has EAVT and AEVT indexes. The schema's `:db/index` and
//! `:db/unique` add partial AVET indexes (`WHERE index_avet IS NOT 0`), but the
//! query SQL never states that predicate, so SQLite can't use them: a
//! value-filtered pattern like `[?u :user/email "x"]` would scan every
//! `:user/email` datom through AEVT. So every `:db/index` / `:db/unique`
//! attribute also gets a *schema* value index (below), created and dropped
//! with the flag by `mentat_db::db::sync_schema_indexes`, in every mode.
//!
//! In [`AutoIndex::Adaptive`] mode the store also counts, per attribute, the queries
//! whose pattern pins that attribute's value (a constant or a bound `:in`
//! scalar). When an attribute reaches `min_uses` queries within one tuning
//! period, mentat creates
//! `idx_auto_avet_<a> ON datoms (a, v, e) WHERE a = <a>`. The planner uses
//! that without statistics, because the query already says `a = <a>`. There's
//! no `value_type_tag` column: an attribute has one value type, and the SQL
//! doesn't pin the tag when the type is known, so a tag column between `a` and
//! `v` would stop the index serving `v = ?`.
//! Indexes mentat creates are listed in `mentat_managed_indexes`, with `kind`
//! `schema` or `adaptive`. An adaptive index is
//! dropped when a whole period passes without a use *and* its last use is at
//! least `idle` old; a schema index only when its attribute loses the flag. That gap between "`min_uses` in a period" and "no use for
//! `idle`" is the hysteresis. Nothing outside that table is ever dropped.
//! An index whose average value matches over 5% of the attribute's datoms is
//! dropped straight after it's built: SQLite would probe it and lose to the
//! AEVT scan (the scale suite's `:issue/state`, 5 values).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use core_traits::Entid;
use mentat_core::{HasSchema, Keyword, Schema};
use mentat_db::db;
use public_traits::errors::Result;

/// How mentat manages secondary indexes on `datoms`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AutoIndex {
    /// No adaptive indexes, and [`Store::tune_indexes`] does nothing. Schema
    /// value indexes still follow `:db/index` / `:db/unique` (they're part of
    /// the schema, like SQLite's own indexes).
    ///
    /// [`Store::tune_indexes`]: crate::Store::tune_indexes
    Off,
    /// Only the schema's indexes: a value index per `:db/index` /
    /// `:db/unique` attribute. [`Store::tune_indexes`] drops any adaptive
    /// index mentat created earlier. This is the default.
    ///
    /// [`Store::tune_indexes`]: crate::Store::tune_indexes
    #[default]
    Schema,
    /// Schema indexes, plus per-attribute value indexes created and dropped
    /// from the query workload. Tuning runs automatically every
    /// [`PERIOD`] queries, and when an attribute reaches `min_uses`.
    Adaptive,
}

/// One index change made (or, in a dry run, proposed) by `tune_indexes`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IndexAction {
    Create {
        index: String,
        attribute: Entid,
        ident: Option<Keyword>,
        /// Value-filtered queries on the attribute in this period.
        uses: u32,
    },
    Drop {
        index: String,
        attribute: Entid,
        ident: Option<Keyword>,
    },
}

impl fmt::Display for IndexAction {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let who = |a: &Entid, i: &Option<Keyword>| match i {
            Some(k) => format!("{k}"),
            None => a.to_string(),
        };
        match self {
            IndexAction::Create {
                index,
                attribute,
                ident,
                uses,
            } => write!(
                f,
                "create {index} on {} ({uses} value-filtered queries)",
                who(attribute, ident)
            ),
            IndexAction::Drop {
                index,
                attribute,
                ident,
            } => write!(f, "drop {index} on {} (idle)", who(attribute, ident)),
        }
    }
}

/// Queries between automatic tuning runs in `Adaptive` mode.
pub const PERIOD: u32 = 1000;
/// User attributes only: bootstrap attributes are few and cached.
const USER0: Entid = 0x10000;

#[derive(Debug)]
pub(crate) struct Advisor {
    pub(crate) mode: AutoIndex,
    pub(crate) min_uses: u32,
    pub(crate) idle: Duration,
    uses: BTreeMap<Entid, u32>,
    queries: u32,
    /// Attributes whose index turned out unselective; not retried by this Store.
    rejected: BTreeSet<Entid>,
    /// Attributes with a managed index, as of the last tuning run.
    indexed: BTreeSet<Entid>,
}

impl Default for Advisor {
    fn default() -> Advisor {
        let mode = match std::env::var("MENTAT_AUTO_INDEX").as_deref() {
            Ok("off") => AutoIndex::Off,
            Ok("adaptive") => AutoIndex::Adaptive,
            _ => AutoIndex::default(),
        };
        Advisor {
            mode,
            min_uses: 16,
            idle: Duration::from_secs(24 * 3600),
            uses: BTreeMap::new(),
            queries: 0,
            rejected: BTreeSet::new(),
            indexed: BTreeSet::new(),
        }
    }
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `(name, attribute, last_used, is_schema)` for every index mentat created.
fn registry(conn: &rusqlite::Connection) -> Result<Vec<(String, Entid, i64, bool)>> {
    if !db::has_table(conn, "mentat_managed_indexes")? {
        return Ok(vec![]);
    }
    let mut stmt = conn.prepare("SELECT name, a, last_used, kind FROM mentat_managed_indexes")?;
    let rows: Vec<(String, Entid, i64, bool)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, String>(3)? == "schema",
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    // Only ever act on names mentat generates, whatever the table says.
    Ok(rows
        .into_iter()
        .filter(|(n, a, _, _)| *n == db::value_index_name(*a))
        .collect())
}

/// From the index's fresh `sqlite_stat1` row (`nrow rows/a rows/(a,v) 1`): is
/// the average value rare enough that probing the index beats the AEVT scan?
// ponytail: fixed 5% cut from the scale suite (a 20%-per-value enum got
// slower, a unique email 4x faster); per-query costing if it misjudges.
fn selective(conn: &rusqlite::Connection, index: &str) -> Result<bool> {
    let stat: Option<String> = conn
        .query_row(
            "SELECT stat FROM sqlite_stat1 WHERE idx = ?",
            [index],
            |r| r.get(0),
        )
        .ok();
    let n: Vec<u64> = stat
        .unwrap_or_default()
        .split_whitespace()
        .filter_map(|x| x.parse().ok())
        .collect();
    Ok(match n[..] {
        [rows, _, per_value, ..] => per_value * 20 <= rows.max(1),
        _ => true, // No statistics (e.g. an empty attribute): keep it.
    })
}

impl Advisor {
    /// Count one query's value-filtered attributes. True when a tuning run is
    /// due: every `PERIOD` queries, or as soon as an attribute without an index
    /// reaches `min_uses`.
    pub(crate) fn record(&mut self, attrs: &BTreeSet<Entid>) -> bool {
        self.queries += 1;
        let mut due = self.queries >= PERIOD;
        for a in attrs {
            let n = self.uses.entry(*a).or_insert(0);
            *n += 1;
            due |= *n == self.min_uses && !self.indexed.contains(a) && !self.rejected.contains(a);
        }
        due
    }

    pub(crate) fn tune(
        &mut self,
        conn: &rusqlite::Connection,
        schema: &Schema,
        dry_run: bool,
    ) -> Result<Vec<IndexAction>> {
        if self.mode == AutoIndex::Off {
            return Ok(vec![]);
        }
        let tx = conn.unchecked_transaction()?;
        if !dry_run {
            // Existing stores, and anything a transaction missed.
            db::sync_schema_indexes(&tx, schema)?;
        }
        let managed = registry(&tx)?;
        let ident = |a: Entid| schema.get_ident(a).cloned();
        let now = now();
        let mut actions = vec![];
        let have: BTreeSet<Entid> = managed.iter().map(|(_, a, _, _)| *a).collect();
        if self.mode == AutoIndex::Adaptive {
            for (&a, &uses) in &self.uses {
                let eligible = a >= USER0
                    && schema
                        .attribute_for_entid(a)
                        .is_some_and(|attr| !attr.fulltext);
                if uses >= self.min_uses
                    && eligible
                    && !have.contains(&a)
                    && !self.rejected.contains(&a)
                {
                    actions.push(IndexAction::Create {
                        index: db::value_index_name(a),
                        attribute: a,
                        ident: ident(a),
                        uses,
                    });
                }
            }
        }
        for (index, a, last_used, is_schema) in &managed {
            if *is_schema {
                continue; // Follows the schema flag, not the workload.
            }
            let used = self.uses.get(a).copied().unwrap_or(0) > 0;
            let idle = now.saturating_sub(*last_used) as u64 >= self.idle.as_secs();
            if self.mode == AutoIndex::Schema || (!used && idle) {
                actions.push(IndexAction::Drop {
                    index: index.clone(),
                    attribute: *a,
                    ident: ident(*a),
                });
            }
        }
        if dry_run {
            return Ok(actions);
        }

        for (_, a, _, _) in &managed {
            if self.uses.get(a).copied().unwrap_or(0) > 0 {
                tx.execute(
                    "UPDATE mentat_managed_indexes SET last_used = ? WHERE a = ?",
                    rusqlite::params![now, a],
                )?;
            }
        }
        let mut rejected = BTreeSet::new();
        for action in &actions {
            match action {
                IndexAction::Create {
                    index, attribute, ..
                } => {
                    // Built and fully ANALYZEd (O(attribute), like the build):
                    // with no stat1 row the planner may probe it last, after
                    // joining everything else, and `selective` reads it.
                    db::create_value_index(&tx, *attribute, "adaptive")?;
                    if !selective(&tx, index)? {
                        db::drop_value_index(&tx, *attribute)?;
                        rejected.insert(*attribute);
                    }
                }
                IndexAction::Drop { attribute, .. } => {
                    db::drop_value_index(&tx, *attribute)?;
                }
            }
        }
        tx.commit()?;
        self.indexed = registry(conn)?.into_iter().map(|(_, a, _, _)| a).collect();
        actions.retain(
            |a| !matches!(a, IndexAction::Create { attribute, .. } if rejected.contains(attribute)),
        );
        self.rejected.extend(rejected);
        // A new period.
        self.uses.clear();
        self.queries = 0;
        Ok(actions)
    }
}
