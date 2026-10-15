//! User-defined transaction functions, shared by every backend (Codeberg #325,
//! #333).
//!
//! A transaction function is an ident carrying a `:db/fn` string: the mino
//! source of a `(fn [db & args] ...)` form that returns tx-data. A transaction
//! invokes it as `[:my/fn arg ...]`. Before a transactor processes the
//! transaction, [`expand`] replaces each such invocation with the tx-data the
//! function returns (recursively: a tx-fn may return calls to other tx-fns).
//! The output is ordinary tx-data, so the transactor itself is unchanged.
//!
//! The backend supplies two things:
//!  * `lookup(ident) -> Option<source>`: the live `:db/fn` of the entity whose
//!    `:db/ident` is `ident` (None when it has none, so the call is left for
//!    the transactor to accept or reject);
//!  * `eval(form) -> printed EDN`: evaluate a mino form in the backend's
//!    sandboxed interpreter, where `(mentat.store/db (mentat.store/open))` is
//!    the transacting store at the basis before this transaction.
//!
//! See docs/design/325-user-defined-transaction-functions.md.

/// The ident of the attribute that holds a transaction function's source.
pub const DB_FN_IDENT: &str = ":db/fn";

/// Default bound on tx-fns calling tx-fns (pg_mentat: `mentat.txfn_max_depth`;
/// embedded: `MENTAT_TXFN_MAX_DEPTH`).
pub const DEFAULT_MAX_DEPTH: usize = 16;

type Res<T> = Result<T, String>;

/// Expand every user tx-fn invocation in the top-level tx vector `value`.
/// Input that isn't a vector is returned unchanged (the transactor reports
/// it). No interpreter work happens unless an item could be an invocation.
pub fn expand<L, E>(
    value: edn::Value,
    max_depth: usize,
    mut lookup: L,
    mut eval: E,
) -> Res<edn::Value>
where
    L: FnMut(&str) -> Res<Option<String>>,
    E: FnMut(&str) -> Res<String>,
{
    let edn::Value::Vector(items) = value else {
        return Ok(value);
    };
    if !items.iter().any(|it| invocation_head(it).is_some()) {
        return Ok(edn::Value::Vector(items));
    }
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        expand_into(item, 0, max_depth.max(1), &mut lookup, &mut eval, &mut out)?;
    }
    Ok(edn::Value::Vector(out))
}

/// Expand the tx-fn invocations in EDN text, returning EDN text. The common
/// case (no candidate invocation) returns the input unchanged without
/// re-printing it.
pub fn expand_str<L, E>(edn_tx: &str, max_depth: usize, lookup: L, eval: E) -> Res<String>
where
    L: FnMut(&str) -> Res<Option<String>>,
    E: FnMut(&str) -> Res<String>,
{
    // Cheap textual pre-check: no `[:` means no keyword-headed vector at all.
    if !edn_tx.contains("[:") {
        return Ok(edn_tx.to_string());
    }
    let value = edn::parse::value(edn_tx)
        .map_err(|e| e.to_string())?
        .without_spans();
    let edn::Value::Vector(ref items) = value else {
        return Ok(edn_tx.to_string());
    };
    if !items.iter().any(|it| invocation_head(it).is_some()) {
        return Ok(edn_tx.to_string());
    }
    Ok(format!("{}", expand(value, max_depth, lookup, eval)?))
}

/// `Some(keyword)` if `v` looks like `[:ns/name ...]` and the keyword is not
/// one of the transactor's own list-form operations. Whether it actually names
/// a tx-fn is decided by looking for a `:db/fn` on it.
pub fn invocation_head(v: &edn::Value) -> Option<&edn::symbols::Keyword> {
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

fn expand_into<L, E>(
    item: edn::Value,
    depth: usize,
    max_depth: usize,
    lookup: &mut L,
    eval: &mut E,
    out: &mut Vec<edn::Value>,
) -> Res<()>
where
    L: FnMut(&str) -> Res<Option<String>>,
    E: FnMut(&str) -> Res<String>,
{
    let Some(kw) = invocation_head(&item) else {
        out.push(item);
        return Ok(());
    };
    let ident = format!("{kw}");
    let Some(src) = lookup(&ident).map_err(|e| format!(":db.error/txfn-lookup {ident}: {e}"))?
    else {
        // Not a tx-fn: leave it for the transactor to accept or reject.
        out.push(item);
        return Ok(());
    };
    if depth >= max_depth {
        return Err(format!(
            ":db.error/txfn-depth transaction function {ident} exceeded the tx-fn \
             depth limit ({max_depth}; mentat.txfn_max_depth on PostgreSQL, \
             MENTAT_TXFN_MAX_DEPTH embedded). A tx-fn that returns calls to itself \
             (directly or indirectly) must terminate."
        ));
    }
    let edn::Value::Vector(call) = item else {
        unreachable!("invocation_head only matches vectors")
    };
    // Args are EDN-printed values; mino reads EDN, so they round-trip. Quote
    // them so symbols/lists arrive as data, not code.
    let args: Vec<String> = call[1..].iter().map(|a| format!("(quote {a})")).collect();
    let form = format!(
        "(let [f {src} db (mentat.store/db (mentat.store/open))] (f db {}))",
        args.join(" ")
    );
    let printed = eval(&form)
        .map_err(|e| format!(":db.error/txfn-error transaction function {ident} failed: {e}"))?;
    let produced = edn::parse::value(&printed)
        .map_err(|e| {
            format!(
                ":db.error/txfn-result transaction function {ident} returned \
                 non-EDN tx-data ({e}): {printed}"
            )
        })?
        .without_spans();
    // The fn returns tx-data: a vector (or list) of entities.
    let edn::Value::Vector(entities) = normalize(produced) else {
        return Err(format!(
            ":db.error/txfn-result transaction function {ident} must return a vector \
             of tx-data (e.g. [[:db/add e a v]]), got {printed}"
        ));
    };
    for e in entities {
        expand_into(e, depth + 1, max_depth, lookup, eval, out)?;
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn lookup_none(_: &str) -> Res<Option<String>> {
        Ok(None)
    }

    #[test]
    fn untouched_without_invocations() {
        let s = "[{:a/b 1} [:db/add 1 :a/b 2]]";
        let out = expand_str(s, 4, lookup_none, |_| unreachable!("no eval")).unwrap();
        assert_eq!(out, s);
    }

    #[test]
    fn expands_recursively_and_bounds_depth() {
        let lookup = |id: &str| -> Res<Option<String>> {
            Ok(match id {
                ":t/outer" => Some("OUTER".into()),
                ":t/inner" => Some("INNER".into()),
                ":t/loop" => Some("LOOP".into()),
                _ => None,
            })
        };
        let eval = |form: &str| -> Res<String> {
            Ok(if form.contains("OUTER") {
                "[[:t/inner] {:x/y 1}]".into()
            } else if form.contains("INNER") {
                "([:db/add 1 :x/y 2])".into() // a list result is accepted
            } else {
                "[[:t/loop]]".into()
            })
        };
        let out = expand_str("[[:t/outer] [:t/none 1]]", 4, lookup, eval).unwrap();
        let parsed = |s: &str| edn::parse::value(s).unwrap().without_spans();
        assert_eq!(
            parsed(&out),
            parsed("[[:db/add 1 :x/y 2] {:x/y 1} [:t/none 1]]")
        );
        let err = expand_str("[[:t/loop]]", 3, lookup, eval).unwrap_err();
        assert!(err.contains("txfn-depth"), "{err}");
    }

    #[test]
    fn rejects_non_vector_results() {
        let err =
            expand_str("[[:t/f]]", 4, |_| Ok(Some("F".into())), |_| Ok("42".into())).unwrap_err();
        assert!(err.contains("txfn-result"), "{err}");
    }
}
