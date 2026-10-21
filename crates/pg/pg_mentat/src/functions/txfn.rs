//! User-defined transaction functions (Codeberg #325).
//!
//! A transaction function is an ident entity carrying a `:db/fn` string: the
//! mino source of a `(fn [db & args] ...)` form that returns tx-data. A
//! transaction invokes it as `[:my/fn arg ...]`. Before the transactor's
//! two-pass pipeline runs, [`expand_user_tx_fns`] replaces each such
//! invocation with the tx-data the function returns (recursively, so a tx-fn
//! may return calls to other tx-fns). The output is ordinary tx-data, so the
//! rest of the transactor -- tempids, upserts, the built-in `:db.fn/cas` /
//! `:db/retractEntity`, cardinality -- handles it unchanged.
//!
//! Safety (the standing rules for in-backend user code):
//! * Runs in the same sandboxed interpreter as `edn_eval`
//!   (`script::build_interpreter`: no host filesystem/network prims) under the
//!   same step / heap / depth GUC limits and interrupt hook.
//! * The fn's reads go through SPI as the calling role; never
//!   `SECURITY DEFINER`.
//! * `mentat.txfn_max_depth` bounds tx-fn recursion; a tx-fn that errors
//!   aborts the whole transaction.
//!
//! The `db` argument is the same db value scripts get (`mentat.store/db`), at
//! the basis *before* this transaction: Datomic's transaction functions see the
//! database value as of the start of the transaction, too.
//!
//! See docs/design/325-user-defined-transaction-functions.md.

use pgrx::spi::Spi;

use crate::functions::script_gucs::TXFN_MAX_DEPTH;

type BoxErr = Box<dyn std::error::Error + Send + Sync>;

/// The `:db/fn` attribute's ident (bootstrap entid 20; see `bootstrap.rs` /
/// `sql/06_bootstrap_data.sql`).
pub use mentat_script::txfn::DB_FN_IDENT;

/// Expand every user tx-fn invocation in the top-level tx vector `value`,
/// using the shared expander (`mentat_script::txfn`, the same code the
/// embedded SQLite and DuckDB backends run). One sandboxed interpreter per
/// transaction, built only when an invocation is found.
pub fn expand_user_tx_fns(value: edn::Value, qs: &str) -> Result<edn::Value, BoxErr> {
    let mut it: Option<mino_rs::Interpreter> = None;
    mentat_script::txfn::expand(
        value,
        TXFN_MAX_DEPTH.get().max(1) as usize,
        |ident| lookup_db_fn(qs, ident).map_err(|e| e.to_string()),
        |form| {
            it.get_or_insert_with(crate::functions::script::build_interpreter)
                .eval_to_string(form)
        },
    )
    .map_err(|e| e.into())
}

/// The `:db/fn` source on the entity whose `:db/ident` is `ident`, if any.
fn lookup_db_fn(qs: &str, ident: &str) -> Result<Option<String>, BoxErr> {
    // Resolve through the store's own idents table (the cache may not have a
    // freshly defined fn yet) and read the live value from the current-state
    // projection of the same store.
    let store_id = super::transact::get_store_id_from_schema(qs)?;
    let sql = format!(
        "SELECT t.v FROM {qs}.idents i \
         JOIN {qs}.idents f ON f.ident = $2 \
         JOIN {qs}.current_text t ON t.e = i.entid AND t.a = f.entid \
         WHERE i.ident = $1 AND t.store_id = $3 \
         LIMIT 1"
    );
    Spi::get_one_with_args::<String>(
        &sql,
        &[
            pgrx::datum::DatumWithOid::from(ident),
            pgrx::datum::DatumWithOid::from(DB_FN_IDENT),
            pgrx::datum::DatumWithOid::from(store_id),
        ],
    )
    .or_else(|e| match e {
        pgrx::spi::SpiError::InvalidPosition => Ok(None),
        other => Err(other),
    })
    .map_err(|e| -> BoxErr { format!(":db.error/txfn-lookup {ident}: {e}").into() })
}
