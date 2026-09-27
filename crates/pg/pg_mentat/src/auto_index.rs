//! Automatic index management (1.10.0). The rules live in
//! `mentat._tune_indexes` (sql/27_auto_index.sql); this module collects the
//! evidence, registers the GUCs and drives the amortized runs.
//!
//! Evidence: every compiled query with a range predicate (`<`, `<=`, `>`,
//! `>=`) on a value bound by a constant-attribute pattern counts one hit for
//! (store, attribute, table read). Hits are kept per backend and flushed to
//! `mentat.index_evidence` every `mentat.auto_index_every_n_tx` edn_t calls
//! and by `mentat_tune_indexes`.

use pgrx::prelude::*;
use pgrx::{GucContext, GucFlags, GucRegistry, GucSetting, PostgresGucEnum};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

#[derive(PostgresGucEnum, Clone, Copy, PartialEq, Eq, Debug)]
pub enum AutoIndexMode {
    #[name = c"off"]
    Off,
    #[name = c"schema"]
    Schema,
    #[name = c"adaptive"]
    Adaptive,
}

static AUTO_INDEX: GucSetting<AutoIndexMode> =
    GucSetting::<AutoIndexMode>::new(AutoIndexMode::Schema);
static IDLE_WINDOW_S: GucSetting<i32> = GucSetting::<i32>::new(7 * 24 * 3600);
static EVERY_N_TX: GucSetting<i32> = GucSetting::<i32>::new(1000);
static MIN_QUERIES: GucSetting<i32> = GucSetting::<i32>::new(50);
static MIN_ROWS: GucSetting<i32> = GucSetting::<i32>::new(100_000);
static LOCK_TIMEOUT_MS: GucSetting<i32> = GucSetting::<i32>::new(100);

pub fn register_gucs() {
    GucRegistry::define_enum_guc(
        c"mentat.auto_index",
        c"Automatic index management: off, schema (default) or adaptive.",
        c"schema: only the indexes the extension ships are created automatically; mentat_tune_indexes() acts when called. adaptive: also run mentat_tune_indexes(false) every mentat.auto_index_every_n_tx transactions of a backend. off: collect no evidence; mentat_tune_indexes() does nothing.",
        &AUTO_INDEX,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"mentat.auto_index_idle_window",
        c"A managed index unscanned this long while its table takes writes is dropped.",
        c"Also the minimum age before a managed index may be dropped (hysteresis). Default 7d.",
        &IDLE_WINDOW_S,
        0,
        i32::MAX,
        GucContext::Suset,
        GucFlags::UNIT_S,
    );
    GucRegistry::define_int_guc(
        c"mentat.auto_index_every_n_tx",
        c"Flush index evidence (and, in adaptive mode, tune) every N edn_t calls of a backend.",
        c"0 disables the amortized runs. Default 1000.",
        &EVERY_N_TX,
        0,
        i32::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"mentat.auto_index_min_queries",
        c"Range-predicate queries on an attribute before a range index is created.",
        c"Default 50.",
        &MIN_QUERIES,
        1,
        i32::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );
    GucRegistry::define_int_guc(
        c"mentat.auto_index_lock_timeout",
        c"How long an amortized (edn_t) index build may wait for its table lock before it is skipped.",
        c"Default 100ms. 0 waits indefinitely (the triggering edn_t then waits too).",
        &LOCK_TIMEOUT_MS,
        0,
        i32::MAX,
        GucContext::Suset,
        GucFlags::UNIT_MS,
    );
    GucRegistry::define_int_guc(
        c"mentat.auto_index_min_rows",
        c"Minimum table size (rows) for a managed range index to be created.",
        c"Default 100000.",
        &MIN_ROWS,
        0,
        i32::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );
}

thread_local! {
    // ponytail: per-backend until flushed; a backend that only reads and
    // never runs edn_t or mentat_tune_indexes() never contributes. Shared
    // memory (needs shared_preload_libraries) if that ever matters.
    static EVIDENCE: RefCell<HashMap<(i64, i64, &'static str), i64>> = RefCell::new(HashMap::new());
    static TX_COUNT: Cell<u64> = const { Cell::new(0) };
}

pub fn collecting() -> bool {
    AUTO_INDEX.get() != AutoIndexMode::Off
}

/// Record one compiled range predicate on attribute `attr` read from `table`.
pub fn record_range_use(store_id: i64, attr: i64, table: &'static str) {
    EVIDENCE.with(|e| *e.borrow_mut().entry((store_id, attr, table)).or_insert(0) += 1);
}

/// Flush this backend's evidence (and optionally tune) through the
/// exception-safe SQL entry point: a failure is LOGged, never raised.
fn tick(tune: bool) {
    let (mut s, mut a, mut t, mut h) = (vec![], vec![], vec![], vec![]);
    EVIDENCE.with(|e| {
        for ((store, attr, table), hits) in e.borrow().iter() {
            s.push(*store);
            a.push(*attr);
            t.push(table.to_string());
            h.push(*hits);
        }
    });
    if s.is_empty() && !tune {
        return;
    }
    let ok = Spi::get_one_with_args::<bool>(
        "SELECT mentat._auto_index_tick($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        &[
            s.into(),
            a.into(),
            t.into(),
            h.into(),
            tune.into(),
            i64::from(MIN_QUERIES.get()).into(),
            i64::from(MIN_ROWS.get()).into(),
            i64::from(IDLE_WINDOW_S.get()).into(),
            i64::from(LOCK_TIMEOUT_MS.get()).into(),
        ],
    );
    if let Ok(Some(true)) = ok {
        EVIDENCE.with(|e| e.borrow_mut().clear());
    }
}

/// Called after every successful edn_t: the amortized run.
pub fn after_transact() {
    let n = EVERY_N_TX.get();
    let mode = AUTO_INDEX.get();
    if mode == AutoIndexMode::Off || n <= 0 {
        return;
    }
    let count = TX_COUNT.with(|c| {
        c.set(c.get() + 1);
        c.get()
    });
    if count % n as u64 == 0 {
        tick(mode == AutoIndexMode::Adaptive);
    }
}

/// Create / drop managed indexes (see docs/src/operations.md, "Automatic
/// index management"). `dry_run` (the default) only reports: no index and no
/// registry row changes (this backend's evidence is still flushed).
#[pg_extern]
pub fn mentat_tune_indexes(
    dry_run: default!(bool, true),
) -> Result<
    TableIterator<
        'static,
        (
            name!(action, String),
            name!(index_name, String),
            name!(table_name, String),
            name!(reason, String),
        ),
    >,
    pgrx::spi::SpiError,
> {
    if AUTO_INDEX.get() == AutoIndexMode::Off {
        return Ok(TableIterator::new(Vec::new()));
    }
    tick(false);
    let rows = Spi::connect(|client| {
        let mut out = Vec::new();
        for r in client.select(
            "SELECT action, index_name, table_name, reason FROM mentat._tune_indexes($1, $2, $3, $4)",
            None,
            &[
                dry_run.into(),
                i64::from(MIN_QUERIES.get()).into(),
                i64::from(MIN_ROWS.get()).into(),
                i64::from(IDLE_WINDOW_S.get()).into(),
            ],
        )? {
            out.push((
                r.get::<String>(1)?.unwrap_or_default(),
                r.get::<String>(2)?.unwrap_or_default(),
                r.get::<String>(3)?.unwrap_or_default(),
                r.get::<String>(4)?.unwrap_or_default(),
            ));
        }
        Ok::<_, pgrx::spi::SpiError>(out)
    })?;
    Ok(TableIterator::new(rows))
}
