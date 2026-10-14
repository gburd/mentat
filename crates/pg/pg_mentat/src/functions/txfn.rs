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

/// The `:db/fn` attribute's ident and entid (bootstrap; see
/// `bootstrap.rs` / `sql/06_bootstrap_data.sql`).
pub const DB_FN_IDENT: &str = ":db/fn";

/// Expand every user tx-fn invocation in the top-level tx vector `value`.
/// Non-vector input is returned unchanged (the caller reports the error).
pub fn expand_user_tx_fns(value: edn::Value, qs: &str) -> Result<edn::Value, BoxErr> {
    let edn::Value::Vector(items) = value else {
        return Ok(value);
    };
    // Cheap pre-check: build no interpreter for the common case of no tx-fns.
    if !items.iter().any(|it| invocation_head(it).is_some()) {
        return Ok(edn::Value::Vector(items));
    }
    let mut cx = Expander {
        qs,
        it: None,
        max_depth: TXFN_MAX_DEPTH.get().max(1) as usize,
    };
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        cx.expand_into(item, 0, &mut out)?;
    }
    Ok(edn::Value::Vector(out))
}

/// `Some(keyword)` if `v` looks like `[:ns/name ...]` and the keyword is not
/// one of the transactor's own list-form operations. Whether it actually names
/// a tx-fn is decided by looking for a `:db/fn` on it.
fn invocation_head(v: &edn::Value) -> Option<&edn::symbols::Keyword> {
    let edn::Value::Vector(vec) = v else {
        return None;
    };
    let edn::Value::Keyword(kw) = vec.first()? else {
        return None;
    };
    let reserved = matches!(
        (kw.namespace(), kw.name()),
        (Some("db"), "add")
            | (Some("db"), "retract")
            | (Some("db"), "cas")
            | (Some("db"), "retractEntity")
            | (Some("db.fn"), _)
            | (None, "retractEntity")
    );
    (!reserved && kw.namespace().is_some()).then_some(kw)
}

struct Expander<'a> {
    qs: &'a str,
    /// One sandboxed interpreter for the whole transaction, built lazily.
    it: Option<mino_rs::Interpreter>,
    max_depth: usize,
}

impl Expander<'_> {
    fn expand_into(
        &mut self,
        item: edn::Value,
        depth: usize,
        out: &mut Vec<edn::Value>,
    ) -> Result<(), BoxErr> {
        let Some(kw) = invocation_head(&item) else {
            out.push(item);
            return Ok(());
        };
        let ident = format!("{kw}");
        let Some(src) = lookup_db_fn(self.qs, &ident)? else {
            // Not a tx-fn: leave it for the transactor to accept or reject.
            out.push(item);
            return Ok(());
        };
        if depth >= self.max_depth {
            return Err(format!(
                ":db.error/txfn-depth transaction function {ident} exceeded \
                 mentat.txfn_max_depth ({}). A tx-fn that returns calls to \
                 itself (directly or indirectly) must terminate.",
                self.max_depth
            )
            .into());
        }
        let edn::Value::Vector(call) = item else {
            unreachable!("invocation_head only matches vectors")
        };
        let args: Vec<String> = call[1..].iter().map(|a| format!("{a}")).collect();
        let produced = self.call(&ident, &src, &args)?;
        // The fn returns tx-data: a vector (or list) of entities.
        let edn::Value::Vector(entities) = produced else {
            return Err(format!(
                ":db.error/txfn-result transaction function {ident} must return a vector \
                 of tx-data (e.g. [[:db/add e a v]]), got {produced}"
            )
            .into());
        };
        for e in entities {
            self.expand_into(e, depth + 1, out)?;
        }
        Ok(())
    }

    /// Evaluate `(src db arg...)` and parse its printed result back into EDN.
    fn call(&mut self, ident: &str, src: &str, args: &[String]) -> Result<edn::Value, BoxErr> {
        let it = self
            .it
            .get_or_insert_with(crate::functions::script::build_interpreter);
        // Args are EDN-printed values; mino reads EDN, so they round-trip.
        // Quote them so symbols/lists arrive as data, not code.
        let quoted: Vec<String> = args.iter().map(|a| format!("(quote {a})")).collect();
        let form = format!(
            "(let [f {src} db (mentat.store/db (mentat.store/open))] (f db {}))",
            quoted.join(" ")
        );
        let printed = it.eval_to_string(&form).map_err(|e| -> BoxErr {
            format!(":db.error/txfn-error transaction function {ident} failed: {e}").into()
        })?;
        let v = edn::parse::value(&printed).map_err(|e| -> BoxErr {
            format!(
                ":db.error/txfn-result transaction function {ident} returned \
                 non-EDN tx-data ({e}): {printed}"
            )
            .into()
        })?;
        Ok(normalize(v.without_spans()))
    }
}

/// mino prints a list for `(list ...)`/seq results; accept a list of entities
/// (and list-form entities) wherever a vector is expected.
fn normalize(v: edn::Value) -> edn::Value {
    match v {
        edn::Value::List(l) => edn::Value::Vector(l.into_iter().map(normalize).collect()),
        edn::Value::Vector(vs) => edn::Value::Vector(vs.into_iter().map(normalize).collect()),
        other => other,
    }
}

/// The `:db/fn` source on the entity whose `:db/ident` is `ident`, if any.
fn lookup_db_fn(qs: &str, ident: &str) -> Result<Option<String>, BoxErr> {
    // Resolve through the store's own idents table (the cache may not have a
    // freshly defined fn yet) and read the live value from the current-state
    // projection. Every store's datoms live in mentat.current_* keyed by
    // store_id (resolved the same way the transactor resolves it).
    let store_id = super::transact::get_store_id_from_schema(qs)?;
    let sql = format!(
        "SELECT t.v FROM {qs}.idents i \
         JOIN {qs}.idents f ON f.ident = $2 \
         JOIN mentat.current_text t ON t.e = i.entid AND t.a = f.entid \
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
