//! `mentat` as a DuckDB loadable extension.
//!
//! Exposes the embedded `mentat` SQLite store (plan §3.1 option (a)) through
//! DuckDB:
//!   - `edn_t(db_path, edn)`                  scalar -> VARCHAR (JSON tx-report).
//!   - `edn_q(db_path, query, options)`       table fn -> rows, all VARCHAR.
//!   - `edn_pull(db_path, pattern, entity)`   scalar -> VARCHAR (JSON map).
//!   - `edn_eval(db_path, script)`            scalar -> VARCHAR (EDN), feature `script`.
//!
//! Stores come from `store_cache` (shared with the SQLite extension): reused
//! per path and checked for staleness before every call. A `Store` is checked
//! out by one call at a time, never shared across DuckDB threads.
//!
//! This crate is EXTENSION-ONLY. The `loadable-extension` feature replaces the
//! DuckDB C API functions; opening a normal `Connection` here panics with
//! "API not initialized" (plan §9 risk 5). Do not add client `Connection` use.

use duckdb::{
    core::{DataChunkHandle, Inserter, LogicalTypeHandle, LogicalTypeId},
    duckdb_entrypoint_c_api,
    ffi::duckdb_string_t,
    types::DuckString,
    vscalar::{ScalarFunctionSignature, VScalar},
    vtab::{arrow::WritableVector, BindInfo, InitInfo, TableFunctionInfo, VTab},
    Connection, Result,
};
use serde_json::{json, Value as Json};
use std::{collections::BTreeSet, error::Error, sync::Mutex};

use mentat::edn::query::{
    Binding as InBinding, OrWhereClause, PatternNonValuePlace, PatternValuePlace,
    VariableOrPlaceholder, WhereClause,
};
use mentat::{
    Binding, HasSchema, QueryInputs, QueryResults, Queryable, Schema, Store, StructuredMap,
    TxReport, TypedValue, ValueType, Variable,
};

type BoxErr = Box<dyn Error>;

#[path = "../../sqlite/ext/src/store_cache.rs"]
mod store_cache;

// ---------------------------------------------------------------------------
// Value rendering (plan §2.4 v1: all-VARCHAR).
// ---------------------------------------------------------------------------

fn value_as_string(value: &TypedValue) -> String {
    use TypedValue::*;
    match value {
        Boolean(b) => b.to_string(),
        Double(d) => d.to_string(),
        Instant(i) => i.to_rfc3339(),
        Keyword(k) => k.to_string(), // `:ns/name`
        Long(l) => l.to_string(),
        Ref(r) => r.to_string(),
        // Inside a vec/map a string is EDN-quoted; a top-level cell is raw (`cell`).
        String(s) => format!("{:?}", s.as_str()),
        Uuid(u) => u.to_string(), // hyphenated
        Bytes(b) => format!("#bytes {:?}", b.to_vec()),
    }
}

fn binding_as_string(value: &Binding) -> String {
    match value {
        Binding::Scalar(v) => value_as_string(v),
        Binding::Vec(v) => {
            let vals: Vec<String> = v.iter().map(binding_as_string).collect();
            format!("[{}]", vals.join(", "))
        }
        Binding::Map(m) => {
            let kvs: Vec<String> =
                m.0.iter()
                    .map(|(k, v)| format!("{} {}", k, binding_as_string(v)))
                    .collect();
            format!("{{{}}}", kvs.join(", "))
        }
    }
}

/// One top-level result cell. A string is returned RAW (`Alice`, not
/// `"Alice"`) so it compares/joins equal to a native DuckDB VARCHAR.
fn cell(b: &Binding) -> String {
    match b {
        Binding::Scalar(TypedValue::String(s)) => s.to_string(),
        other => binding_as_string(other),
    }
}

// ---------------------------------------------------------------------------
// Scalar plumbing shared by edn_t / edn_pull / edn_eval.
// ---------------------------------------------------------------------------

/// Row `i` of VARCHAR column `col`; `None` if NULL.
fn str_arg(input: &DataChunkHandle, col: usize, i: usize) -> Option<String> {
    let v = input.flat_vector(col);
    if v.row_is_null(i as u64) {
        return None;
    }
    let mut s = unsafe { v.as_slice_with_len::<duckdb_string_t>(input.len()) }[i];
    Some(DuckString::new(&mut s).as_str().to_string())
}

/// Row `i` of BIGINT column `col`; `None` if NULL.
fn i64_arg(input: &DataChunkHandle, col: usize, i: usize) -> Option<i64> {
    let v = input.flat_vector(col);
    if v.row_is_null(i as u64) {
        return None;
    }
    Some(unsafe { v.as_slice_with_len::<i64>(input.len()) }[i])
}

/// Fill the VARCHAR output with `f(row)`; `None` -> NULL (any NULL arg).
fn map_rows(
    input: &DataChunkHandle,
    output: &mut dyn WritableVector,
    f: impl Fn(usize) -> Result<Option<String>, BoxErr>,
) -> Result<(), BoxErr> {
    let mut out = output.flat_vector();
    for i in 0..input.len() {
        match f(i)? {
            Some(s) => out.insert(i, s.as_str()),
            None => out.set_null(i),
        }
    }
    Ok(())
}

fn varchar() -> LogicalTypeHandle {
    LogicalTypeId::Varchar.into()
}

// ---------------------------------------------------------------------------
// edn_t(db_path, edn) -> VARCHAR (JSON tx-report). Plan §2, §6, §7.
// ---------------------------------------------------------------------------

fn tx_report_json(report: &TxReport) -> String {
    let tempids: serde_json::Map<String, Json> = report
        .tempids
        .iter()
        .map(|(k, v)| (k.clone(), Json::from(*v)))
        .collect();
    json!({
        "tx_id": report.tx_id,
        "tx_instant": report.tx_instant.to_rfc3339(),
        "tempids": tempids,
    })
    .to_string()
}

struct EdnTransact;

impl VScalar for EdnTransact {
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(edn)) = (str_arg(input, 0, i), str_arg(input, 1, i)) else {
                return Ok(None);
            };
            let report = store_cache::transact(&db, &edn)?;
            Ok(Some(tx_report_json(&report)))
        })
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![varchar(), varchar()],
            varchar(),
        )]
    }

    /// Mutates the store: volatile so DuckDB evaluates it exactly once per row
    /// (never constant-folded or re-evaluated -> no double-applied tx).
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// edn_pull(db_path, pattern, entity) -> VARCHAR (JSON, pg_mentat's shape).
// ---------------------------------------------------------------------------

/// pg_mentat's pull value encoding (`decode_row_typed_value`): refs are
/// `{":db/id": n}`, keywords `":ns/name"`, instants epoch micros, bytes hex.
fn typed_json(tv: &TypedValue) -> Json {
    use TypedValue::*;
    match tv {
        Ref(r) => json!({ ":db/id": r }),
        Boolean(b) => json!(b),
        Long(l) => json!(l),
        Double(d) => json!(d.into_inner()),
        String(s) => json!(s.as_str()),
        Keyword(k) => json!(k.to_string()),
        Instant(t) => json!(t.timestamp_micros()),
        Uuid(u) => json!(u.to_string()),
        Bytes(b) => json!(b
            .iter()
            .map(|x| format!("{x:02x}"))
            .collect::<std::string::String>()),
    }
}

fn binding_json(b: &Binding) -> Json {
    match b {
        Binding::Scalar(tv) => typed_json(tv),
        Binding::Vec(vs) => Json::Array(vs.iter().map(binding_json).collect()),
        Binding::Map(m) => Json::Object(map_json(m)),
    }
}

fn map_json(m: &StructuredMap) -> serde_json::Map<String, Json> {
    m.0.iter()
        .map(|(k, v)| (k.to_string(), binding_json(v)))
        .collect()
}

fn pull_json(db: &str, pattern: &str, eid: i64) -> Result<String, BoxErr> {
    // The pattern is spliced into a `(pull ?e …)` query (reusing mentat's pull
    // grammar), so require it to be exactly one EDN vector: it cannot close the
    // form early and inject clauses.
    if !matches!(
        mentat::edn::parse::value(pattern).map(|v| v.without_spans()),
        Ok(mentat::edn::Value::Vector(_))
    ) {
        return Err(format!(
            "edn_pull: pattern must be an EDN vector like [*] or [:person/name], got {pattern}"
        )
        .into());
    }
    let query = format!("[:find (pull ?e {pattern}) . :in ?e :where [?e _ _]]");
    let inputs = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?e"),
        TypedValue::Ref(eid),
    )]);
    let mut m = match store_cache::read(db, |store| store.q_once(&query, inputs))?.results {
        QueryResults::Scalar(Some(Binding::Map(sm))) => map_json(&sm),
        _ => serde_json::Map::new(),
    };
    // Always present, as a plain integer (pg_mentat does the same).
    m.insert(":db/id".into(), json!(eid));
    Ok(Json::Object(m).to_string())
}

struct EdnPull;

impl VScalar for EdnPull {
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(pat), Some(e)) = (
                str_arg(input, 0, i),
                str_arg(input, 1, i),
                i64_arg(input, 2, i),
            ) else {
                return Ok(None);
            };
            pull_json(&db, &pat, e).map(Some)
        })
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![varchar(), varchar(), LogicalTypeId::Bigint.into()],
            varchar(),
        )]
    }

    /// Reads external, mutable state (the store file): never constant-fold.
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// edn_eval(db_path, script) -> VARCHAR (EDN). Feature `script`.
// ---------------------------------------------------------------------------

#[cfg(feature = "script")]
struct EdnEval;

#[cfg(feature = "script")]
impl VScalar for EdnEval {
    type State = ();

    fn invoke(
        _: &(),
        input: &mut DataChunkHandle,
        output: &mut dyn WritableVector,
    ) -> Result<(), BoxErr> {
        map_rows(input, output, |i| {
            let (Some(db), Some(src)) = (str_arg(input, 0, i), str_arg(input, 1, i)) else {
                return Ok(None);
            };
            // Sandboxed (no host fs prims) + step/heap/depth limits; a no-arg
            // `(mentat.store/open)` opens `db`.
            let mut it = mentat::script::Interpreter::with_default_path(&db);
            let out = it
                .eval_to_string(&src)
                .map_err(|e| format!("edn_eval: {e}"))?;
            Ok(Some(out))
        })
    }

    fn signatures() -> Vec<ScalarFunctionSignature> {
        vec![ScalarFunctionSignature::exact(
            vec![varchar(), varchar()],
            varchar(),
        )]
    }

    /// Scripts can transact: evaluate exactly once per row.
    fn volatile() -> bool {
        true
    }
}

// ---------------------------------------------------------------------------
// edn_q options: {"inputs": [...], "asOf": T, "since": T} (pg_mentat's shape).
// ---------------------------------------------------------------------------

#[derive(Default)]
struct QueryOpts {
    inputs: Option<Vec<Json>>,
    as_of: Option<i64>,
    since: Option<i64>,
}

fn parse_opts(text: Option<&str>) -> Result<QueryOpts, String> {
    let mut opts = QueryOpts::default();
    let text = text.map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Ok(opts);
    }
    let obj = match serde_json::from_str(text) {
        Ok(Json::Null) => return Ok(opts),
        Ok(Json::Object(o)) => o,
        Ok(_) => return Err("edn_q: options must be a JSON object".into()),
        Err(e) => return Err(format!("edn_q: options are not valid JSON: {e}")),
    };
    let tx = |k: &str, v: &Json| {
        v.as_i64()
            .ok_or_else(|| format!("edn_q: \"{k}\" must be an integer tx id, got {v}"))
    };
    for (k, v) in obj {
        match k.as_str() {
            "inputs" => match v {
                Json::Array(a) => opts.inputs = Some(a),
                other => return Err(format!("edn_q: \"inputs\" must be an array, got {other}")),
            },
            "asOf" => opts.as_of = Some(tx(&k, &v)?),
            "since" => opts.since = Some(tx(&k, &v)?),
            other => {
                return Err(format!(
                    "edn_q: unknown option \"{other}\" (expected inputs, asOf, since)"
                ))
            }
        }
    }
    if opts.as_of.is_some() && opts.since.is_some() {
        return Err("edn_q: \"asOf\" and \"since\" are mutually exclusive".into());
    }
    Ok(opts)
}

/// Variables that stand for entities: the entity/tx place of a pattern, or the
/// value place of a `:db.type/ref` attribute. A JSON integer bound to one of
/// these becomes `TypedValue::Ref` (else `Long`, which the algebrizer would
/// treat as a type mismatch in entity position -> silently empty).
/// ponytail: walks patterns/or/not only, not rule bodies; add rules if needed.
fn ref_vars(clauses: &[WhereClause], schema: &Schema, out: &mut BTreeSet<Variable>) {
    for c in clauses {
        match c {
            WhereClause::Pattern(p) => {
                for place in [&p.entity, &p.tx] {
                    if let PatternNonValuePlace::Variable(v) = place {
                        out.insert(v.clone());
                    }
                }
                if let (PatternNonValuePlace::Ident(a), PatternValuePlace::Variable(v)) =
                    (&p.attribute, &p.value)
                {
                    if schema
                        .attribute_for_ident(a)
                        .is_some_and(|(attr, _)| attr.value_type == ValueType::Ref)
                    {
                        out.insert(v.clone());
                    }
                }
            }
            WhereClause::NotJoin(n) => ref_vars(&n.clauses, schema, out),
            WhereClause::OrJoin(o) => {
                for oc in &o.clauses {
                    match oc {
                        OrWhereClause::Clause(c) => ref_vars(std::slice::from_ref(c), schema, out),
                        OrWhereClause::And(cs) => ref_vars(cs, schema, out),
                    }
                }
            }
            _ => {}
        }
    }
}

/// JSON -> TypedValue, mirroring pg_mentat's `bind_input_value`: integer ->
/// Long (Ref for an entity var), float -> Double, bool -> Boolean, `":kw"` ->
/// Keyword, other string -> String.
fn json_to_typed(j: &Json, is_ref: bool) -> Result<TypedValue, String> {
    Ok(match j {
        Json::Bool(b) => TypedValue::Boolean(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) if is_ref => TypedValue::Ref(i),
            (Some(i), _) => TypedValue::Long(i),
            (None, Some(f)) => TypedValue::from(f),
            _ => return Err(format!("edn_q: unrepresentable number {n}")),
        },
        Json::String(s) if s.starts_with(':') => {
            match mentat::edn::parse::value(s).map(|v| v.without_spans()) {
                Ok(mentat::edn::Value::Keyword(k)) => TypedValue::from(k),
                _ => return Err(format!("edn_q: invalid keyword input {s}")),
            }
        }
        Json::String(s) => TypedValue::typed_string(s),
        other => return Err(format!("edn_q: unsupported input value {other}")),
    })
}

/// Build `QueryInputs` from the positional `inputs` array: one element per
/// `:in` binding form (source vars like `$` are not binding forms).
fn build_inputs(store: &Store, query: &str, vals: Vec<Json>) -> Result<QueryInputs, BoxErr> {
    let parsed = mentat::edn::parse::parse_query(query)?;
    if vals.len() != parsed.in_bindings.len() {
        return Err(format!(
            "edn_q: query has {} :in binding(s) but \"inputs\" has {} value(s)",
            parsed.in_bindings.len(),
            vals.len()
        )
        .into());
    }
    let mut refs = BTreeSet::new();
    ref_vars(
        &parsed.where_clauses,
        &store.conn().current_schema(),
        &mut refs,
    );
    let tv = |v: &Variable, j: &Json| json_to_typed(j, refs.contains(v));
    let array = |j: Json, what: &str| match j {
        Json::Array(a) => Ok(a),
        other => Err(format!(
            "edn_q: {what} input must be a JSON array, got {other}"
        )),
    };
    // One tuple row: drop `_` placeholder columns.
    let row = |vps: &[VariableOrPlaceholder], vals: Vec<Json>| {
        if vals.len() != vps.len() {
            return Err(format!(
                "edn_q: tuple needs {} value(s), got {}",
                vps.len(),
                vals.len()
            ));
        }
        let mut vars = Vec::new();
        let mut out = Vec::new();
        for (vp, j) in vps.iter().zip(vals) {
            if let VariableOrPlaceholder::Variable(v) = vp {
                out.push(tv(v, &j)?);
                vars.push(v.clone());
            }
        }
        Ok((vars, out))
    };

    let mut scalars = Vec::new();
    let mut non_scalar = Vec::new();
    for (b, j) in parsed.in_bindings.iter().zip(vals) {
        match b {
            InBinding::BindScalar(v) => scalars.push((v.clone(), tv(v, &j)?)),
            InBinding::BindColl(v) => {
                let xs = array(j, "collection [?x ...]")?;
                let xs = xs.iter().map(|x| tv(v, x)).collect::<Result<_, _>>()?;
                non_scalar.push(QueryInputs::with_collection(v.clone(), xs));
            }
            InBinding::BindTuple(vps) => {
                let (vars, xs) = row(vps, array(j, "tuple [?a ?b]")?)?;
                non_scalar.push(QueryInputs::with_tuple(vars, xs));
            }
            InBinding::BindRel(vps) => {
                let mut vars = Vec::new();
                let mut rows = Vec::new();
                for r in array(j, "relation [[?a ?b]]")? {
                    let (vs, xs) = row(vps, array(r, "relation row")?)?;
                    vars = vs;
                    rows.push(xs);
                }
                if rows.is_empty() {
                    vars = vps.iter().filter_map(|vp| vp.clone().into_var()).collect();
                }
                non_scalar.push(QueryInputs::with_relation(vars, rows));
            }
        }
    }
    // Scalars plus any number of collection/tuple/relation bindings, merged.
    let mut out = QueryInputs::with_value_sequence(scalars);
    for ns in non_scalar {
        out = out.merge(ns).map_err(|e| format!("edn_q: inputs: {e}"))?;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// edn_q(db_path, query, options) -> table (all VARCHAR). Plan §2.2.
// ---------------------------------------------------------------------------

/// Materialized rows-of-columns from a QueryResults (plan §2.2 normalization).
struct EdnQueryBindData {
    rows: Vec<Vec<String>>,
    ncols: usize,
}

struct EdnQueryInitData {
    /// Next row index to emit; guarded so `func` can be re-entered safely.
    cursor: Mutex<usize>,
}

/// Normalize the four QueryResults arms into rows-of-columns of strings.
fn results_to_rows(results: QueryResults) -> Vec<Vec<String>> {
    match results {
        QueryResults::Scalar(v) => v.iter().map(|b| vec![cell(b)]).collect(),
        QueryResults::Coll(vs) => vs.iter().map(|b| vec![cell(b)]).collect(),
        QueryResults::Tuple(vv) => vv
            .iter()
            .map(|row| row.iter().map(cell).collect())
            .collect(),
        QueryResults::Rel(rel) => rel
            .rows()
            .map(|row| row.iter().map(cell).collect())
            .collect(),
    }
}

/// A VARCHAR bind parameter; `None` if SQL NULL.
fn param(bind: &BindInfo, i: u64) -> Option<String> {
    let v = bind.get_parameter(i);
    (!v.is_null()).then(|| v.to_string())
}

struct EdnQuery;

impl VTab for EdnQuery {
    type InitData = EdnQueryInitData;
    type BindData = EdnQueryBindData;

    fn bind(bind: &BindInfo) -> Result<Self::BindData, BoxErr> {
        let db_path = param(bind, 0).ok_or("edn_q: db_path must not be NULL")?;
        let query = param(bind, 1).ok_or("edn_q: query must not be NULL")?;
        let opts = parse_opts(param(bind, 2).as_deref())?;

        // Run the query once to learn the FindSpec columns AND materialize
        // the results (smallest correct diff).
        let output = store_cache::read(&db_path, |store| -> Result<_, BoxErr> {
            let inputs = match opts.inputs {
                Some(vals) => Some(build_inputs(store, &query, vals)?),
                None => None,
            };
            Ok(match (opts.as_of, opts.since) {
                (Some(t), _) => store.q_once_as_of(&query, inputs, t)?,
                (_, Some(t)) => store.q_once_since(&query, inputs, t)?,
                _ => store.q_once(&query, inputs)?,
            })
        })?;

        // Declare one VARCHAR column per FindSpec element, named like the CLI.
        let mut ncols = 0;
        for element in output.spec.columns() {
            bind.add_result_column(&element.to_string(), varchar());
            ncols += 1;
        }

        let rows = results_to_rows(output.results);
        Ok(EdnQueryBindData { rows, ncols })
    }

    fn init(info: &InitInfo) -> Result<Self::InitData, BoxErr> {
        // The whole result set is materialized in one BindData shared across
        // scan threads; a single cursor streams it. Force single-threaded scan
        // so each row is emitted exactly once (parallel scans would each
        // re-emit the full set). ponytail: fine for a materialized result;
        // partition across threads only if huge result sets need it.
        info.set_max_threads(1);
        Ok(EdnQueryInitData {
            cursor: Mutex::new(0),
        })
    }

    fn func(func: &TableFunctionInfo<Self>, output: &mut DataChunkHandle) -> Result<(), BoxErr> {
        let bind_data = func.get_bind_data();
        let init_data = func.get_init_data();
        let mut cursor = init_data.cursor.lock().unwrap();

        // DuckDB's STANDARD_VECTOR_SIZE; the output chunk is pre-sized to this.
        // ponytail: hardcoded 2048 (the crate exposes no capacity accessor at
        // v1.5.5); revisit if a future crate version adds one.
        const VECTOR_SIZE: usize = 2048;
        let remaining = bind_data.rows.len().saturating_sub(*cursor);
        let n = remaining.min(VECTOR_SIZE);

        for col in 0..bind_data.ncols {
            let vec = output.flat_vector(col);
            for r in 0..n {
                vec.insert(r, bind_data.rows[*cursor + r][col].as_str());
            }
        }
        *cursor += n;
        output.set_len(n);
        Ok(())
    }

    fn parameters() -> Option<Vec<LogicalTypeHandle>> {
        Some(vec![varchar(), varchar(), varchar()]) // db_path, query, options
    }
}

#[duckdb_entrypoint_c_api]
pub unsafe fn extension_entrypoint(con: Connection) -> Result<(), Box<dyn Error>> {
    con.register_scalar_function::<EdnTransact>("edn_t")?;
    con.register_table_function::<EdnQuery>("edn_q")?;
    con.register_scalar_function::<EdnPull>("edn_pull")?;
    #[cfg(feature = "script")]
    con.register_scalar_function::<EdnEval>("edn_eval")?;
    Ok(())
}
