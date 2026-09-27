//! Datomic-in-Clojure-style scripting layer for pg_mentat, backed by the live
//! Postgres-resident store.
//!
//! This module implements the shared [`mentat_script::ScriptBackend`] trait
//! over the extension's own `#[pg_extern]` engine functions (`transact`,
//! `query`, `pull`, `entity`) reached through SPI, and builds the sandboxed
//! [`mino_rs`] interpreter for `edn_eval`. The backend-independent
//! `mentat.store/*` surface — the db-value shape, argument parsing, the
//! tx-report / inst / uuid value builders, and the prim registration — lives in
//! the `mentat_script` crate (plan § 1.19). What stays here is (a) the engine
//! storage impl, (b) the `serde_json::Value` → mino `Value` conversion, and
//! (c) the Task-1c sandboxing (`sandboxed()` + GUC limits + check hook).
//!
//! # Temporal honesty — pg_mentat's advantage
//!
//! Unlike Mentat's SQLite algebrizer, pg_mentat's `edn_q` accepts
//! `{"asOf": T}` / `{"since": T}` temporal inputs, so arbitrary Datalog `q`
//! runs faithfully against a historical basis: an as-of/since db forwards its
//! bound into the query inputs.
//!
//! Gated behind the `script` feature; nothing here compiles for a default build.

use mino_rs::collections::map::PSet;
use mino_rs::collections::vector::PVec;
use mino_rs::symbol::Symbol;
use mino_rs::{Gc, Value};

use pgrx::prelude::*;

use serde_json::Value as J;

use mentat_script::{DbRef, ScriptBackend, TxReport};

/// A fresh sandboxed scripting interpreter with the `mentat.store/*` prims
/// registered over the [`PgBackend`].
///
/// Built per `edn_eval` call. Uses [`Interpreter::sandboxed`](mino_rs::Interpreter::sandboxed):
/// the language, regex, bignum, atoms and the in-memory store are installed,
/// but every host-filesystem prim (`slurp`, `spit`, `rm-rf`, `mkdir-p`,
/// `file-exists?`) and the file-backed store are *absent* (unbound) — so a
/// hostile `edn_eval` caller cannot touch the server's disk. The
/// `mentat.store/*` prims are SPI-backed and run as the calling role, exactly
/// like `edn_q`/`edn_t`; sandboxing only removes host access,
/// not the store surface.
///
/// Three `PGC_SUSET` GUCs bound CPU, memory and stack; the check hook wires
/// mino's step counter to `check_for_interrupts!` (so `statement_timeout` and
/// `pg_cancel_backend` work) and to `stack_is_too_deep()` as a second line
/// against runaway recursion. See `register_script_gucs`. (Task 1c — MUST be
/// preserved: the shared `install()` only adds the store surface; the sandbox
/// and limits stay here.)
pub fn build_interpreter() -> mino_rs::Interpreter {
    use crate::functions::script_gucs::{
        SCRIPT_MAX_DEPTH, SCRIPT_MAX_HEAP_BYTES, SCRIPT_MAX_STEPS,
    };
    let mut it = mino_rs::Interpreter::sandboxed();
    let backend: std::rc::Rc<std::cell::RefCell<dyn ScriptBackend>> =
        std::rc::Rc::new(std::cell::RefCell::new(PgBackend));
    mentat_script::install(&mut it, backend);
    it.set_limits(mino_rs::Limits {
        steps: Some(SCRIPT_MAX_STEPS.get() as u64),
        heap_bytes: Some(SCRIPT_MAX_HEAP_BYTES.get() as u64),
        depth: Some(SCRIPT_MAX_DEPTH.get() as u32),
    });
    it.set_check_hook(Box::new(|| {
        pgrx::check_for_interrupts!();
        if unsafe { pgrx::pg_sys::stack_is_too_deep() } {
            return Err(mino_rs::error::throw_str("edn_eval: stack depth limit"));
        }
        Ok(())
    }));
    it
}

/// Evaluate a mino script and return its result as EDN (`pr-str`) text.
///
/// Mirrors `edn_t`/`edn_q`: EDN/text in, EDN text out. A mino
/// exception surfaces as a clean Postgres ERROR carrying the mino message.
#[pg_extern(name = "edn_eval")]
pub fn mentat_eval(script: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let mut it = build_interpreter();
    it.eval_to_string(script)
        .map_err(|e| Box::<dyn std::error::Error + Send + Sync>::from(format!("edn_eval: {e}")))
}

// Deprecated pre-1.9.0 name, emitted only in `script` builds (this module is
// cfg-gated). Same VOLATILE STRICT as the C function it wraps. The other three
// deprecated wrappers are in sql/26_deprecated_names.sql.
pgrx::extension_sql!(
    r"
CREATE FUNCTION public.mentat_eval(script TEXT)
RETURNS TEXT
LANGUAGE SQL VOLATILE STRICT
AS $$ SELECT public.edn_eval(script); $$;
COMMENT ON FUNCTION public.mentat_eval(TEXT) IS 'Deprecated since 1.9.0: use edn_eval';
",
    name = "deprecated_mentat_eval",
    requires = [mentat_eval],
);

// ---------------------------------------------------------------------------
// ScriptBackend over the pg_mentat engine
// ---------------------------------------------------------------------------

/// The pg_mentat storage backend. There is exactly one database (the one you
/// are connected to), reached through the extension's engine functions over
/// SPI; the backend holds no connection.
struct PgBackend;

impl ScriptBackend for PgBackend {
    fn open(&mut self, _path: Option<&str>) -> Result<i64, String> {
        // Exactly one database; a trivial conn handle.
        Ok(1)
    }

    fn close(&mut self, _conn: i64) -> Result<(), String> {
        // No owned connection to close; no-op keeps API parity with Mentat.
        Ok(())
    }

    fn basis_tx(&self, _conn: i64) -> Result<i64, String> {
        current_basis_tx()
    }

    fn transact(&mut self, _conn: i64, edn: &str) -> Result<TxReport, String> {
        let report = super::transact::mentat_transact(edn).map_err(|e| e.to_string())?;
        json_to_tx_report(&report)
    }

    fn with(&mut self, db: &DbRef, edn: &str) -> Result<(i64, TxReport), String> {
        let report = super::transact::mentat_with(edn).map_err(|e| e.to_string())?;
        let j: J = serde_json::from_str(&report).map_err(|e| format!("bad engine JSON: {e}"))?;
        let basis_after = j
            .get("db-after")
            .and_then(|d| d.get("basis-t"))
            .and_then(J::as_i64)
            .unwrap_or(db.basis_tx);
        Ok((basis_after, json_to_tx_report(&report)?))
    }

    fn q(&self, db: &DbRef, query_edn: &str) -> Result<Value, String> {
        let out = super::query::mentat_query(query_edn, pgrx::JsonB(temporal_inputs(db)))
            .map_err(|e| e.to_string())?;
        Ok(query_result_value(&out.0))
    }

    fn q_with_inputs(&self, db: &DbRef, query_edn: &str, inputs: &[J]) -> Result<Value, String> {
        // edn_q's own options shape: the temporal bound plus positional inputs.
        let mut opts = temporal_inputs(db);
        if let J::Object(ref mut o) = opts {
            o.insert("inputs".to_string(), J::Array(inputs.to_vec()));
        }
        let out =
            super::query::mentat_query(query_edn, pgrx::JsonB(opts)).map_err(|e| e.to_string())?;
        Ok(query_result_value(&out.0))
    }

    fn pull(&self, _db: &DbRef, eid: i64, pattern_edn: &str) -> Result<Value, String> {
        let out = super::pull::mentat_pull(pattern_edn, eid).map_err(|e| e.to_string())?;
        Ok(json_to_mino(&out.0))
    }

    fn entity(&self, _db: &DbRef, eid: i64) -> Result<Value, String> {
        let out = super::entity::mentat_entity(eid).map_err(|e| e.to_string())?;
        Ok(json_to_mino(&out.0))
    }

    fn read(&self, db: &DbRef, eid: i64, attr: &str) -> Result<Value, String> {
        // A collection query -- the value(s) of that attr on that eid.
        let query = format!("[:find [?v ...] :where [{eid} {attr} ?v]]");
        let out = super::query::mentat_query(&query, pgrx::JsonB(temporal_inputs(db)))
            .map_err(|e| e.to_string())?;
        // FindColl envelope: {"result": [..]}. Cardinality-one collapses to the
        // scalar; many stays a set; absent -> nil.
        let vals = out
            .0
            .get("result")
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default();
        match vals.len() {
            0 => Ok(Value::Nil),
            1 => Ok(json_to_mino(&vals[0])),
            _ => {
                let mut set = PSet::empty();
                for v in &vals {
                    set = set.conj(json_to_mino(v));
                }
                Ok(Value::Set(Gc::new(set)))
            }
        }
    }

    fn entities(&self, db: &DbRef, attr: &str) -> Result<Value, String> {
        let query = format!("[:find [?e ...] :where [?e {attr} _]]");
        let out = super::query::mentat_query(&query, pgrx::JsonB(temporal_inputs(db)))
            .map_err(|e| e.to_string())?;
        let eids = out
            .0
            .get("result")
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default();
        let mut set = PSet::empty();
        for e in &eids {
            set = set.conj(json_to_mino(e));
        }
        Ok(Value::Set(Gc::new(set)))
    }

    fn datoms(&self, db: &DbRef) -> Result<Value, String> {
        // Full EAVT scan; the engine applies the temporal bound from inputs.
        let query = "[:find ?e ?a ?v ?tx ?added :where [?e ?a ?v ?tx ?added]]";
        let mut inputs = temporal_inputs(db);
        // A datoms listing wants the raw datom log, so include retractions for a
        // since/current window; as-of reconstructs the live set.
        if db.as_of.is_none() {
            if let J::Object(ref mut o) = inputs {
                o.insert("history".to_string(), J::from(true));
            }
        }
        let out =
            super::query::mentat_query(query, pgrx::JsonB(inputs)).map_err(|e| e.to_string())?;
        // FindRel envelope: {"results": [[e,a,v,tx,added], ...]}.
        let rows = out
            .0
            .get("results")
            .and_then(J::as_array)
            .cloned()
            .unwrap_or_default();
        let tuples: Vec<Value> = rows.iter().map(json_to_mino).collect();
        Ok(Value::Vector(Gc::new(PVec::from_vec(tuples))))
    }

    fn resolve_eid(&self, db: &DbRef, arg: &Value) -> Result<i64, String> {
        match arg {
            Value::Int(n) => Ok(*n),
            Value::Keyword(sym) => {
                let ident = sym_to_kw_str(sym);
                crate::cache::get_cache()
                    .resolve_ident(&ident)
                    .ok_or_else(|| format!("unknown ident {ident}"))
            }
            Value::Vector(v) if v.len() == 2 => {
                // Lookup ref [:attr val] -> [:find ?e . :where [?e :attr val]].
                let attr = v.nth(0).unwrap();
                let val = v.nth(1).unwrap();
                let attr_kw = match attr {
                    Value::Keyword(sym) => sym_to_kw_str(sym),
                    _ => return Err("lookup-ref attr must be a keyword".into()),
                };
                let query = format!(
                    "[:find ?e . :where [?e {attr_kw} {}]]",
                    mino_rs::printer::print_str(val)
                );
                let out = super::query::mentat_query(&query, pgrx::JsonB(temporal_inputs(db)))
                    .map_err(|e| e.to_string())?;
                out.0.get("result").and_then(J::as_i64).ok_or_else(|| {
                    format!(
                        "lookup-ref {} resolved to no entity",
                        mino_rs::printer::print_str(arg)
                    )
                })
            }
            _ => Err(format!(
                "eid must be an integer, ident keyword, or [:attr val] lookup-ref, got {}",
                mino_rs::printer::print_str(arg)
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Engine access over SPI
// ---------------------------------------------------------------------------

/// The current basis: the max committed transaction id, via SPI.
fn current_basis_tx() -> Result<i64, String> {
    Spi::get_one::<i64>("SELECT COALESCE(MAX(tx), 0) FROM mentat.transactions")
        .map(|o| o.unwrap_or(0))
        .map_err(|e| format!("current basis tx: {e}"))
}

/// The query-inputs JSON carrying a db value's temporal bound (empty for the
/// current basis). pg_mentat's `edn_q` reads `asOf`/`since` from this.
fn temporal_inputs(db: &DbRef) -> J {
    let mut obj = serde_json::Map::new();
    if let Some(t) = db.as_of {
        obj.insert("asOf".to_string(), J::from(t));
    }
    if let Some(t) = db.since {
        obj.insert("since".to_string(), J::from(t));
    }
    J::Object(obj)
}

// ---------------------------------------------------------------------------
// Value conversions: engine JSON -> mino Value / shared TxReport
// ---------------------------------------------------------------------------

/// Convert an engine tx-report (JSON text) to the shared [`TxReport`] shape.
fn json_to_tx_report(report: &str) -> Result<TxReport, String> {
    let j: J = serde_json::from_str(report).map_err(|e| format!("bad engine JSON: {e}"))?;
    let tx_id = j
        .get("db-after")
        .and_then(|d| d.get("basis-t"))
        .and_then(J::as_i64)
        .unwrap_or(0);
    let mut tempids = Vec::new();
    if let Some(obj) = j.get("tempids").and_then(J::as_object) {
        for (k, v) in obj {
            if let Some(n) = v.as_i64() {
                tempids.push((k.clone(), n));
            }
        }
    }
    Ok(TxReport { tx_id, tempids })
}

/// A `edn_q` JSON envelope as a mino value in Datomic result shape:
/// scalar find -> the value; coll `[?x ...]` -> a vector; tuple `[?a ?b]` -> a
/// vector; relation `?a ?b` -> a set of tuple-vectors.
fn query_result_value(env: &J) -> Value {
    // Relation: has "results" (and "columns"); a set of tuple vectors.
    if let Some(rows) = env.get("results").and_then(J::as_array) {
        let mut set = PSet::empty();
        for row in rows {
            set = set.conj(json_to_mino(row));
        }
        return Value::Set(Gc::new(set));
    }
    // Scalar / coll / tuple: a single "result" value.
    match env.get("result") {
        Some(v) => json_to_mino(v),
        None => Value::Nil,
    }
}

/// A `serde_json::Value` as a mino `Value`, RECURSIVELY. Restores keyword
/// typing (a string starting with `:` is a keyword) and maps object keys the
/// same way, so pull/entity maps keyed by `:person/name` become keyword-keyed
/// mino maps. Nested arrays/objects recurse (pull results are not collapsed).
fn json_to_mino(v: &J) -> Value {
    match v {
        J::Null => Value::Nil,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => str_to_mino(s),
        J::Array(items) => Value::Vector(Gc::new(PVec::from_vec(
            items.iter().map(json_to_mino).collect(),
        ))),
        J::Object(entries) => {
            let mut m = mino_rs::collections::map::PMap::empty();
            for (k, val) in entries {
                m = m.assoc(str_to_mino(k), json_to_mino(val));
            }
            Value::Map(Gc::new(m))
        }
    }
}

/// A JSON string as a mino value: a leading `:` makes it a keyword. Otherwise a
/// plain string.
fn str_to_mino(s: &str) -> Value {
    if let Some(rest) = s.strip_prefix(':') {
        if !rest.is_empty() {
            return Value::Keyword(kw_from_str(rest));
        }
    }
    Value::Str(Gc::new(s.to_string()))
}

/// Build a mino keyword symbol from `ns/name` or `name`.
fn kw_from_str(s: &str) -> Symbol {
    match s.split_once('/') {
        Some((ns, name)) if !ns.is_empty() && !name.is_empty() => Symbol::namespaced(ns, name),
        _ => Symbol::plain(s),
    }
}

/// `:ns/name` / `:name` text for a mino keyword symbol (the ABI fed to the
/// engine).
fn sym_to_kw_str(sym: &Symbol) -> String {
    match sym.ns.as_deref() {
        Some(ns) => format!(":{ns}/{}", sym.name),
        None => format!(":{}", sym.name),
    }
}

// ---------------------------------------------------------------------------
// Pure-value-conversion unit tests (no Postgres/SPI). These exercise the JSON
// -> mino bridge, which is the non-trivial logic that stays in this module.
// Run under plain `cargo test -p pg_mentat --features script`.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod value_tests {
    use super::*;
    use mino_rs::printer::print_str;
    use serde_json::json;

    fn edn(v: &Value) -> String {
        print_str(v)
    }

    #[test]
    fn scalar_and_keyword_restore() {
        assert_eq!(edn(&json_to_mino(&json!(42))), "42");
        assert_eq!(edn(&json_to_mino(&json!(true))), "true");
        assert_eq!(edn(&json_to_mino(&json!("Alice"))), "\"Alice\"");
        assert_eq!(edn(&json_to_mino(&json!(":person/name"))), ":person/name");
        assert_eq!(edn(&json_to_mino(&json!(":db.type/ref"))), ":db.type/ref");
        assert_eq!(edn(&json_to_mino(&json!(":name"))), ":name");
        assert_eq!(edn(&json_to_mino(&json!(":"))), "\":\"");
    }

    #[test]
    fn nested_map_is_not_collapsed() {
        let j = json!({":person/name": "Alice", ":person/friend": {":person/name": "Bob"}});
        let s = edn(&json_to_mino(&j));
        assert!(s.contains(":person/name \"Alice\""), "{s}");
        assert!(s.contains(":person/friend {"), "nested map preserved: {s}");
        assert!(
            s.contains(":person/name \"Bob\""),
            "nested value preserved: {s}"
        );
    }

    #[test]
    fn query_shapes_map_to_datomic() {
        let rel = json!({"columns": ["?n"], "results": [["Alice"], ["Bob"]], "result": [["Alice"], ["Bob"]]});
        let s = edn(&query_result_value(&rel));
        assert!(s.starts_with("#{"), "relation -> set: {s}");
        assert!(s.contains("[\"Alice\"]") && s.contains("[\"Bob\"]"), "{s}");

        let scalar = json!({"result": "Alice"});
        assert_eq!(edn(&query_result_value(&scalar)), "\"Alice\"");

        let coll = json!({"result": ["Alice", "Bob"]});
        assert_eq!(edn(&query_result_value(&coll)), "[\"Alice\" \"Bob\"]");

        assert_eq!(edn(&query_result_value(&json!({"result": null}))), "nil");
    }

    #[test]
    fn temporal_inputs_carry_the_bound() {
        let current = temporal_inputs(&DbRef {
            conn: 1,
            basis_tx: 9,
            as_of: None,
            since: None,
        });
        assert_eq!(current, json!({}));
        let as_of = temporal_inputs(&DbRef {
            conn: 1,
            basis_tx: 9,
            as_of: Some(7),
            since: None,
        });
        assert_eq!(as_of, json!({"asOf": 7}));
        let since = temporal_inputs(&DbRef {
            conn: 1,
            basis_tx: 9,
            as_of: None,
            since: Some(3),
        });
        assert_eq!(since, json!({"since": 3}));
    }

    #[test]
    fn tx_report_projects_the_report() {
        let report = r#"{"db-before":{"basis-t":1},"db-after":{"basis-t":268435458},"tx-data":[],"tempids":{"t":268435460},"schema_changed":false}"#;
        let r = json_to_tx_report(report).unwrap();
        assert_eq!(r.tx_id, 268435458);
        assert_eq!(r.tempids, vec![("t".to_string(), 268435460)]);
    }

    #[test]
    fn float_and_null_and_nested_vec() {
        assert_eq!(edn(&json_to_mino(&json!(3.5))), "3.5");
        assert_eq!(edn(&json_to_mino(&json!(null))), "nil");
        let j = json!([{":a": 1}, {":a": 2}]);
        let s = edn(&json_to_mino(&j));
        assert!(s.starts_with('['), "{s}");
        assert!(s.contains(":a 1") && s.contains(":a 2"), "{s}");
    }

    #[test]
    fn keyword_arg_prints_back_as_ident_text() {
        assert_eq!(
            sym_to_kw_str(&Symbol::namespaced("person", "name")),
            ":person/name"
        );
        assert_eq!(sym_to_kw_str(&Symbol::plain("name")), ":name");
    }

    #[test]
    fn keyword_round_trips_json_to_mino_and_back() {
        for ident in [":db.type/ref", ":person/name", ":done"] {
            let v = json_to_mino(&json!(ident));
            assert_eq!(edn(&v), ident, "round-trip for {ident}");
        }
    }

    #[test]
    fn empty_relation_is_the_empty_set() {
        let rel = json!({"columns": ["?n"], "results": [], "result": []});
        assert_eq!(edn(&query_result_value(&rel)), "#{}");
    }
}
