//! `mentat` as a SQLite loadable extension.
//!
//! ```sql
//! .load ./libmentat_sqlite            -- entrypoint sqlite3_mentatsqlite_init
//! SELECT edn_t(db_path, edn);           -- JSON tx-report
//! SELECT edn_q(db_path, query, opts);   -- JSON result (pg_mentat's edn_q shape)
//! SELECT edn_pull(db_path, pattern, e); -- JSON map (pg_mentat's edn_pull shape)
//! SELECT edn_eval(db_path, script);     -- EDN text (sandboxed mino)
//! ```
//!
//! # Two SQLites (see README)
//!
//! The *host* SQLite (the `sqlite3` CLI or any app that loads us) is reached
//! ONLY through the raw `sqlite3_api_routines` pointer handed to the
//! entrypoint, via the handful of slots below. The embedded mentat engine
//! keeps its own statically bundled SQLite (rusqlite `bundled`) for the store
//! file named by `db_path`. rustc's cdylib version script exports only our
//! `#[no_mangle]` entrypoint, so the bundled `sqlite3_*` symbols stay local
//! and never interpose on the host's.
//!
//! Every entrypoint body runs under `catch_unwind`: errors and panics become
//! `sqlite3_result_error`, never an unwind across the C boundary.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::Path;
use std::sync::atomic::{AtomicPtr, Ordering};

use serde_json::{json, Value as Json};

use mentat::edn::query::{
    Binding as InBinding, Element, FindSpec, OrWhereClause, PatternNonValuePlace,
    PatternValuePlace, VariableOrPlaceholder, WhereClause,
};
use mentat::{
    Binding, HasSchema, QueryInputs, QueryResults, Queryable, Schema, Store, StructuredMap,
    TxReport, TypedValue, ValueType, Variable,
};

type Res<T> = Result<T, String>;

// ---------------------------------------------------------------------------
// Host SQLite C API, via the sqlite3_api_routines pointer.
// ---------------------------------------------------------------------------

type Ctx = *mut c_void; // sqlite3_context*
type Val = *mut c_void; // sqlite3_value*
type XFunc = unsafe extern "C" fn(Ctx, c_int, *mut Val);

/// Slot indices into `struct sqlite3_api_routines` (sqlite3ext.h). The struct
/// is append-only across SQLite releases, so these never move; every slot used
/// here exists since SQLite 3.7.16 (below the 3.30 floor checked at load).
mod slot {
    pub const LIBVERSION_NUMBER: usize = 67;
    pub const MALLOC: usize = 68;
    pub const RESULT_ERROR: usize = 80;
    pub const RESULT_NULL: usize = 84;
    pub const RESULT_TEXT: usize = 85;
    pub const VALUE_BYTES: usize = 103;
    pub const VALUE_INT64: usize = 107;
    pub const VALUE_TEXT: usize = 109;
    pub const VALUE_TYPE: usize = 113;
    pub const CONTEXT_DB_HANDLE: usize = 149;
    pub const CREATE_FUNCTION_V2: usize = 162;
    pub const DB_FILENAME: usize = 180;
}

const SQLITE_OK: c_int = 0;
const SQLITE_ERROR: c_int = 1;
const SQLITE_INTEGER: c_int = 1;
const SQLITE_NULL: c_int = 5;
const SQLITE_UTF8: c_int = 1;
/// Not callable from views, triggers, CHECK/DEFAULT/index/generated columns:
/// these touch the filesystem, so an untrusted schema must not reach them.
const SQLITE_DIRECTONLY: c_int = 0x0008_0000; // 3.30.0+
const SQLITE_TRANSIENT: isize = -1;

static API: AtomicPtr<*const c_void> = AtomicPtr::new(std::ptr::null_mut());

/// The host function in slot `i`, as fn-pointer type `F`.
unsafe fn api<F: Copy>(i: usize) -> F {
    let p = *API.load(Ordering::Acquire).add(i);
    std::mem::transmute_copy::<*const c_void, F>(&p)
}

/// Argument `i` as text; `None` for SQL NULL.
unsafe fn arg_text(argv: *mut Val, i: usize) -> Option<String> {
    let v = *argv.add(i);
    if api::<unsafe extern "C" fn(Val) -> c_int>(slot::VALUE_TYPE)(v) == SQLITE_NULL {
        return None;
    }
    // value_text before value_bytes (sqlite docs: the conversion sets the length).
    let p = api::<unsafe extern "C" fn(Val) -> *const u8>(slot::VALUE_TEXT)(v);
    let n = api::<unsafe extern "C" fn(Val) -> c_int>(slot::VALUE_BYTES)(v);
    if p.is_null() {
        return Some(String::new());
    }
    let bytes = std::slice::from_raw_parts(p, n.max(0) as usize);
    Some(String::from_utf8_lossy(bytes).into_owned())
}

/// Argument `i` as an INTEGER; `Ok(None)` for NULL, error for any other type.
unsafe fn arg_int(argv: *mut Val, i: usize, what: &str) -> Res<Option<i64>> {
    let v = *argv.add(i);
    match api::<unsafe extern "C" fn(Val) -> c_int>(slot::VALUE_TYPE)(v) {
        SQLITE_NULL => Ok(None),
        SQLITE_INTEGER => Ok(Some(api::<unsafe extern "C" fn(Val) -> i64>(
            slot::VALUE_INT64,
        )(v))),
        _ => Err(format!("{what} must be an INTEGER")),
    }
}

/// Refuse a `db_path` that is the host's own main database: two SQLite
/// library copies on one file break POSIX locking (sqlite.org/howtocorrupt.html
/// §2.2.1). ponytail: checks `main` only, not ATTACHed dbs (db_name() is 3.39+).
unsafe fn guard_host_file(ctx: Ctx, db_path: &str) -> Res<()> {
    let db = api::<unsafe extern "C" fn(Ctx) -> *mut c_void>(slot::CONTEXT_DB_HANDLE)(ctx);
    let f = api::<unsafe extern "C" fn(*mut c_void, *const c_char) -> *const c_char>(
        slot::DB_FILENAME,
    )(db, c"main".as_ptr());
    if f.is_null() || *f == 0 || db_path.is_empty() {
        return Ok(()); // host is :memory:/temp, or the store is in-memory
    }
    let host = CStr::from_ptr(f).to_string_lossy().into_owned();
    if same_file(&host, db_path) {
        return Err(format!(
            "db_path {db_path:?} is the host's own database; give mentat a separate store file"
        ));
    }
    Ok(())
}

fn same_file(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        let p = Path::new(p);
        p.canonicalize().or_else(|_| std::path::absolute(p))
    };
    matches!((norm(a), norm(b)), (Ok(x), Ok(y)) if x == y)
}

/// Run `f` (panic-safe) and hand its result to SQLite: text, NULL, or error.
unsafe fn run(ctx: Ctx, f: impl FnOnce() -> Res<Option<String>>) {
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|p| {
        let msg = p
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| p.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        Err(format!("mentat: internal panic: {msg}"))
    });
    let err = |m: &str| {
        let n = c_int::try_from(m.len()).unwrap_or(c_int::MAX);
        api::<unsafe extern "C" fn(Ctx, *const c_char, c_int)>(slot::RESULT_ERROR)(
            ctx,
            m.as_ptr().cast(),
            n,
        )
    };
    match r {
        Ok(Some(s)) => match c_int::try_from(s.len()) {
            Ok(n) => api::<unsafe extern "C" fn(Ctx, *const c_char, c_int, isize)>(
                slot::RESULT_TEXT,
            )(ctx, s.as_ptr().cast(), n, SQLITE_TRANSIENT),
            Err(_) => err("mentat: result exceeds 2 GiB"),
        },
        Ok(None) => api::<unsafe extern "C" fn(Ctx)>(slot::RESULT_NULL)(ctx),
        Err(e) => err(&e),
    }
}

unsafe extern "C" fn x_edn_t(ctx: Ctx, _argc: c_int, argv: *mut Val) {
    run(ctx, || {
        let (Some(db), Some(edn)) = (arg_text(argv, 0), arg_text(argv, 1)) else {
            return Ok(None);
        };
        guard_host_file(ctx, &db).map_err(|e| format!("edn_t: {e}"))?;
        transact_json(&db, &edn).map(Some)
    })
}

unsafe extern "C" fn x_edn_q(ctx: Ctx, _argc: c_int, argv: *mut Val) {
    run(ctx, || {
        let (Some(db), Some(query)) = (arg_text(argv, 0), arg_text(argv, 1)) else {
            return Ok(None);
        };
        guard_host_file(ctx, &db).map_err(|e| format!("edn_q: {e}"))?;
        query_json(&db, &query, arg_text(argv, 2).as_deref()).map(Some)
    })
}

unsafe extern "C" fn x_edn_pull(ctx: Ctx, _argc: c_int, argv: *mut Val) {
    run(ctx, || {
        let e = arg_int(argv, 2, "edn_pull: entity")?;
        let (Some(db), Some(pat), Some(e)) = (arg_text(argv, 0), arg_text(argv, 1), e) else {
            return Ok(None);
        };
        guard_host_file(ctx, &db).map_err(|e| format!("edn_pull: {e}"))?;
        pull_json(&db, &pat, e).map(Some)
    })
}

unsafe extern "C" fn x_edn_eval(ctx: Ctx, _argc: c_int, argv: *mut Val) {
    run(ctx, || {
        let (Some(db), Some(src)) = (arg_text(argv, 0), arg_text(argv, 1)) else {
            return Ok(None);
        };
        guard_host_file(ctx, &db).map_err(|e| format!("edn_eval: {e}"))?;
        eval_edn(&db, &src).map(Some)
    })
}

/// Write `msg` to `*pz_err` (sqlite3_malloc'd, as SQLite frees it).
unsafe fn set_err(pz_err: *mut *mut c_char, msg: &str) {
    if pz_err.is_null() {
        return;
    }
    let p = api::<unsafe extern "C" fn(c_int) -> *mut c_char>(slot::MALLOC)(msg.len() as c_int + 1);
    if !p.is_null() {
        std::ptr::copy_nonoverlapping(msg.as_ptr().cast(), p, msg.len());
        *p.add(msg.len()) = 0;
        *pz_err = p;
    }
}

/// Extension entrypoint. SQLite derives `sqlite3_mentatsqlite_init` from the
/// file name `libmentat_sqlite.so`, so `.load ./libmentat_sqlite` needs no
/// explicit entrypoint argument.
///
/// # Safety
/// Called by SQLite's extension loader with a valid `db` and `api`.
#[no_mangle]
pub unsafe extern "C" fn sqlite3_mentatsqlite_init(
    db: *mut c_void,
    pz_err: *mut *mut c_char,
    p_api: *const *const c_void,
) -> c_int {
    if p_api.is_null() {
        return SQLITE_ERROR; // statically linked hosts have no api table
    }
    API.store(p_api.cast_mut(), Ordering::Release);
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let version = api::<unsafe extern "C" fn() -> c_int>(slot::LIBVERSION_NUMBER)();
        if version < 3_030_000 {
            set_err(
                pz_err,
                "mentat: needs host SQLite >= 3.30.0 (SQLITE_DIRECTONLY)",
            );
            return SQLITE_ERROR;
        }
        type CreateFn = unsafe extern "C" fn(
            *mut c_void,
            *const c_char,
            c_int,
            c_int,
            *mut c_void,
            Option<XFunc>,
            Option<XFunc>,
            Option<unsafe extern "C" fn(Ctx)>,
            Option<unsafe extern "C" fn(*mut c_void)>,
        ) -> c_int;
        let create = api::<CreateFn>(slot::CREATE_FUNCTION_V2);
        let fns: [(&CStr, c_int, XFunc); 4] = [
            (c"edn_t", 2, x_edn_t),
            (c"edn_q", 3, x_edn_q),
            (c"edn_pull", 3, x_edn_pull),
            (c"edn_eval", 2, x_edn_eval),
        ];
        for (name, nargs, f) in fns {
            // NOT SQLITE_DETERMINISTIC: the store changes between calls.
            let flags = SQLITE_UTF8 | SQLITE_DIRECTONLY;
            let rc = create(
                db,
                name.as_ptr(),
                nargs,
                flags,
                std::ptr::null_mut(),
                Some(f),
                None,
                None,
                None,
            );
            if rc != SQLITE_OK {
                set_err(
                    pz_err,
                    &format!("mentat: registering {name:?} failed ({rc})"),
                );
                return rc;
            }
        }
        SQLITE_OK
    }));
    r.unwrap_or(SQLITE_ERROR)
}

// ---------------------------------------------------------------------------
// The four functions, as plain Rust over the embedded store (unit-testable).
// Per-call `Store::open`: open -> use -> drop within one invocation.
//
// ponytail: the JSON encoders and the inputs contract below are shared in
// spirit with crates/duckdb/src/lib.rs (a cdylib, so not a dependency). Hoist
// both into `mentat` when a third consumer appears.
// ---------------------------------------------------------------------------

fn open(db: &str, f: &str) -> Res<Store> {
    Store::open(db).map_err(|e| format!("{f}: opening store {db:?}: {e}"))
}

fn transact_json(db: &str, edn: &str) -> Res<String> {
    let report = open(db, "edn_t")?
        .transact(edn)
        .map_err(|e| format!("edn_t: {e}"))?;
    Ok(tx_report_json(&report))
}

/// Same as the DuckDB extension's `edn_t`.
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

fn eval_edn(db: &str, src: &str) -> Res<String> {
    // Sandboxed (no host fs prims) + step/heap/depth limits; a no-arg
    // `(mentat.store/open)` opens `db`.
    mentat::script::Interpreter::with_default_path(db)
        .eval_to_string(src)
        .map_err(|e| format!("edn_eval: {e}"))
}

fn pull_json(db: &str, pattern: &str, eid: i64) -> Res<String> {
    // The pattern is spliced into `(pull ?e …)` to reuse mentat's pull grammar,
    // so it must be exactly one EDN vector (cannot close the form and inject).
    if !matches!(
        mentat::edn::parse::value(pattern).map(|v| v.without_spans()),
        Ok(mentat::edn::Value::Vector(_))
    ) {
        return Err(format!(
            "edn_pull: pattern must be an EDN vector like [*] or [:person/name], got {pattern}"
        ));
    }
    let store = open(db, "edn_pull")?;
    let query = format!("[:find (pull ?e {pattern}) . :in ?e :where [?e _ _]]");
    let inputs = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?e"),
        TypedValue::Ref(eid),
    )]);
    let results = store
        .q_once(&query, inputs)
        .map_err(|e| format!("edn_pull: {e}"))?
        .results;
    let mut m = match results {
        QueryResults::Scalar(Some(Binding::Map(sm))) => map_json(&sm),
        _ => serde_json::Map::new(),
    };
    m.insert(":db/id".into(), json!(eid)); // always, as pg_mentat does
    Ok(Json::Object(m).to_string())
}

/// pg_mentat's pull value encoding (`decode_row_typed_value`): refs
/// `{":db/id": n}`, keywords `":ns/name"`, instants epoch micros, bytes hex.
fn pull_value(tv: &TypedValue) -> Json {
    match tv {
        TypedValue::Ref(r) => json!({ ":db/id": r }),
        TypedValue::Instant(t) => json!(t.timestamp_micros()),
        other => query_value(other),
    }
}

fn pull_binding(b: &Binding) -> Json {
    match b {
        Binding::Scalar(tv) => pull_value(tv),
        Binding::Vec(vs) => Json::Array(vs.iter().map(pull_binding).collect()),
        Binding::Map(m) => Json::Object(map_json(m)),
    }
}

fn map_json(m: &StructuredMap) -> serde_json::Map<String, Json> {
    m.0.iter()
        .map(|(k, v)| (k.to_string(), pull_binding(v)))
        .collect()
}

/// pg_mentat's query value encoding (`build_value_decode_expr` +
/// `decode_text_result`): refs are plain integers, instants ISO-8601 UTC.
fn query_value(tv: &TypedValue) -> Json {
    match tv {
        TypedValue::Ref(r) => json!(r),
        TypedValue::Boolean(b) => json!(b),
        TypedValue::Long(l) => json!(l),
        TypedValue::Double(d) => json!(d.into_inner()),
        TypedValue::String(s) => json!(s.as_str()),
        TypedValue::Keyword(k) => json!(k.to_string()),
        TypedValue::Instant(t) => json!(t.format("%Y-%m-%dT%H:%M:%S%.6fZ").to_string()),
        TypedValue::Uuid(u) => json!(u.to_string()),
        TypedValue::Bytes(b) => json!(b.iter().map(|x| format!("{x:02x}")).collect::<String>()),
    }
}

/// A result cell: a pull element nests as its JSON object (as pg does).
fn query_binding(b: &Binding) -> Json {
    match b {
        Binding::Scalar(tv) => query_value(tv),
        Binding::Vec(vs) => Json::Array(vs.iter().map(query_binding).collect()),
        Binding::Map(m) => Json::Object(map_json(m)),
    }
}

/// pg_mentat's `format_find_response` shapes.
fn results_json(spec: &FindSpec, results: QueryResults) -> Json {
    let row = |r: &[Binding]| Json::Array(r.iter().map(query_binding).collect());
    match results {
        QueryResults::Scalar(b) => json!({ "result": b.as_ref().map(query_binding) }),
        QueryResults::Coll(bs) => {
            json!({ "result": bs.iter().map(query_binding).collect::<Vec<_>>() })
        }
        QueryResults::Tuple(t) => json!({ "result": t.as_deref().map(row) }),
        QueryResults::Rel(rel) => {
            let cols: Vec<String> = spec.columns().map(|e| e.to_string()).collect();
            let rows: Vec<Json> = rel.rows().map(row).collect();
            let lone_agg =
                cols.len() == 1 && matches!(spec.columns().next(), Some(Element::Aggregate(_)));
            if lone_agg && rows.len() == 1 {
                return json!({ "result": rows[0][0] });
            }
            json!({ "columns": cols, "results": rows, "result": rows })
        }
    }
}

fn query_json(db: &str, query: &str, opts: Option<&str>) -> Res<String> {
    let opts = parse_opts(opts)?;
    let store = open(db, "edn_q")?;
    let inputs = opts
        .inputs
        .map(|vals| build_inputs(&store, query, vals))
        .transpose()?;
    let out = match (opts.as_of, opts.since) {
        (Some(t), _) => store.q_once_as_of(query, inputs, t),
        (_, Some(t)) => store.q_once_since(query, inputs, t),
        _ => store.q_once(query, inputs),
    }
    .map_err(|e| format!("edn_q: {e}"))?;
    Ok(results_json(&out.spec, out.results).to_string())
}

// ---------------------------------------------------------------------------
// edn_q options: {"inputs": [...], "asOf": T, "since": T}; {} / NULL / '' = none.
// ---------------------------------------------------------------------------

#[derive(Debug, Default, PartialEq)]
struct QueryOpts {
    inputs: Option<Vec<Json>>,
    as_of: Option<i64>,
    since: Option<i64>,
}

fn parse_opts(text: Option<&str>) -> Res<QueryOpts> {
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
/// value place of a `:db.type/ref` attribute. A JSON integer bound to one
/// becomes `TypedValue::Ref` (a `Long` in entity position is a type mismatch,
/// i.e. a silently empty result).
/// ponytail: walks patterns/or/not only, not rule bodies; add rules if needed.
fn ref_vars(clauses: &[WhereClause], schema: &Schema, out: &mut Vec<Variable>) {
    for c in clauses {
        match c {
            WhereClause::Pattern(p) => {
                for place in [&p.entity, &p.tx] {
                    if let PatternNonValuePlace::Variable(v) = place {
                        out.push(v.clone());
                    }
                }
                if let (PatternNonValuePlace::Ident(a), PatternValuePlace::Variable(v)) =
                    (&p.attribute, &p.value)
                {
                    if schema
                        .attribute_for_ident(a)
                        .is_some_and(|(attr, _)| attr.value_type == ValueType::Ref)
                    {
                        out.push(v.clone());
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
fn json_to_typed(j: &Json, is_ref: bool) -> Res<TypedValue> {
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

/// `QueryInputs` from the positional `inputs` array: one element per `:in`
/// binding form (source vars like `$` are not binding forms).
fn build_inputs(store: &Store, query: &str, vals: Vec<Json>) -> Res<QueryInputs> {
    let parsed = mentat::edn::parse::parse_query(query).map_err(|e| format!("edn_q: {e}"))?;
    if vals.len() != parsed.in_bindings.len() {
        return Err(format!(
            "edn_q: query has {} :in binding(s) but \"inputs\" has {} value(s)",
            parsed.in_bindings.len(),
            vals.len()
        ));
    }
    let mut refs = Vec::new();
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
    // One tuple row; `_` placeholder columns are dropped.
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
                let xs = xs.iter().map(|x| tv(v, x)).collect::<Res<_>>()?;
                non_scalar.push(QueryInputs::with_collection(v.clone(), xs));
            }
            InBinding::BindTuple(vps) => {
                let (vars, xs) = row(vps, array(j, "tuple [?a ?b]")?)?;
                non_scalar.push(QueryInputs::with_tuple(vars, xs));
            }
            InBinding::BindRel(vps) => {
                let mut vars: Vec<Variable> =
                    vps.iter().filter_map(|vp| vp.clone().into_var()).collect();
                let mut rows = Vec::new();
                for r in array(j, "relation [[?a ?b]]")? {
                    let (vs, xs) = row(vps, array(r, "relation row")?)?;
                    vars = vs;
                    rows.push(xs);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opts_contract() {
        for empty in [None, Some(""), Some("  "), Some("null"), Some("{}")] {
            assert_eq!(
                parse_opts(empty).unwrap(),
                QueryOpts::default(),
                "{empty:?}"
            );
        }
        let o = parse_opts(Some(r#"{"inputs":["Alice", 3], "asOf": 7}"#)).unwrap();
        assert_eq!(o.inputs.unwrap(), vec![json!("Alice"), json!(3)]);
        assert_eq!((o.as_of, o.since), (Some(7), None));
        assert_eq!(parse_opts(Some(r#"{"since": 9}"#)).unwrap().since, Some(9));
        for (bad, why) in [
            ("{nope", "not valid JSON"),
            ("[1]", "must be a JSON object"),
            (r#"{"bogus":1}"#, r#"unknown option "bogus""#),
            (r#"{"inputs":"x"}"#, "must be an array"),
            (r#"{"asOf":"x"}"#, "integer tx id"),
            (r#"{"asOf":1,"since":2}"#, "mutually exclusive"),
        ] {
            let e = parse_opts(Some(bad)).unwrap_err();
            assert!(e.contains(why), "{bad}: {e}");
        }
    }

    #[test]
    fn json_input_values() {
        assert_eq!(
            json_to_typed(&json!(5), false).unwrap(),
            TypedValue::Long(5)
        );
        assert_eq!(json_to_typed(&json!(5), true).unwrap(), TypedValue::Ref(5));
        assert_eq!(
            json_to_typed(&json!(1.5), false).unwrap(),
            TypedValue::from(1.5)
        );
        assert_eq!(
            json_to_typed(&json!(true), false).unwrap(),
            TypedValue::Boolean(true)
        );
        assert_eq!(
            json_to_typed(&json!(":person/name"), false).unwrap(),
            TypedValue::typed_ns_keyword("person", "name")
        );
        assert_eq!(
            json_to_typed(&json!("Alice"), false).unwrap(),
            TypedValue::typed_string("Alice")
        );
        assert!(json_to_typed(&json!(null), false).is_err());
        assert!(json_to_typed(&json!(":"), false).is_err());
    }

    /// End to end over a real store file: tx-report, every result shape, the
    /// inputs contract, as-of, pull and eval.
    #[test]
    fn functions_over_a_store_file() {
        let path =
            std::env::temp_dir().join(format!("mentat_sqlite_ext_{}.db", std::process::id()));
        let db = path.to_str().unwrap();
        let schema = r#"[{:db/ident :person/name :db/valueType :db.type/string :db/cardinality :db.cardinality/one}
                         {:db/ident :person/age  :db/valueType :db.type/long   :db/cardinality :db.cardinality/one}]"#;
        let r: Json = serde_json::from_str(&transact_json(db, schema).unwrap()).unwrap();
        assert!(r["tx_id"].as_i64().unwrap() > 0 && r["tx_instant"].is_string());
        let r: Json = serde_json::from_str(
            &transact_json(
                db,
                r#"[{:db/id "a" :person/name "Alice" :person/age 30} {:person/name "Bob"}]"#,
            )
            .unwrap(),
        )
        .unwrap();
        let alice = r["tempids"]["a"].as_i64().unwrap();
        let tx1 = r["tx_id"].as_i64().unwrap();
        transact_json(db, &format!("[[:db/add {alice} :person/age 31]]")).unwrap();

        let q = |query: &str, opts: &str| -> Json {
            serde_json::from_str(&query_json(db, query, Some(opts)).unwrap()).unwrap()
        };
        let rel = q("[:find ?e ?n :where [?e :person/name ?n]]", "{}");
        assert_eq!(rel["columns"], json!(["?e", "?n"]));
        assert_eq!(rel["results"].as_array().unwrap().len(), 2);
        assert_eq!(rel["results"], rel["result"]);
        assert_eq!(
            q("[:find (count ?e) :where [?e :person/name _]]", "")["result"],
            json!(2)
        );
        assert_eq!(
            q(
                "[:find ?e . :in ?n :where [?e :person/name ?n]]",
                r#"{"inputs":["Alice"]}"#
            )["result"],
            json!(alice)
        );
        assert_eq!(
            q(
                "[:find [?n ...] :in [?x ...] :where [?e :person/name ?x] [?e :person/name ?n]]",
                r#"{"inputs":[["Alice","Zed"]]}"#
            )["result"],
            json!(["Alice"])
        );
        let age = "[:find ?a . :in ?e :where [?e :person/age ?a]]";
        assert_eq!(
            q(age, &format!(r#"{{"inputs":[{alice}]}}"#))["result"],
            json!(31)
        );
        assert_eq!(
            q(age, &format!(r#"{{"inputs":[{alice}],"asOf":{tx1}}}"#))["result"],
            json!(30)
        );
        assert_eq!(
            q(
                "[:find [?n ?a] :where [?e :person/name ?n] [?e :person/age ?a]]",
                ""
            )["result"],
            json!(["Alice", 31])
        );
        assert!(query_json(
            db,
            "[:find ?e :in ?a ?b :where [?e :person/name ?a]]",
            Some(r#"{"inputs":[1]}"#)
        )
        .unwrap_err()
        .contains("2 :in binding(s)"));

        let p: Json = serde_json::from_str(&pull_json(db, "[*]", alice).unwrap()).unwrap();
        assert_eq!(
            (&p[":person/name"], &p[":db/id"]),
            (&json!("Alice"), &json!(alice))
        );
        assert!(pull_json(db, "[*]] :where [(x)]", alice).is_err());

        let out = eval_edn(
            db,
            r#"(def c (mentat.store/open)) (mentat.store/transact c [{:person/name "Carol"}])
               (count (mentat.store/q (mentat.store/db c) (quote [:find ?n :where [_ :person/name ?n]])))"#,
        )
        .unwrap();
        assert_eq!(out, "3");
        assert!(eval_edn(db, r#"(slurp "/etc/passwd")"#).is_err());

        for ext in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{db}{ext}"));
        }
    }

    #[test]
    fn same_file_detection() {
        let dir = std::env::temp_dir();
        let a = dir.join("x.db");
        let a = a.to_str().unwrap();
        assert!(same_file(a, &format!("{}/./x.db", dir.display())));
        assert!(!same_file(a, &format!("{}/y.db", dir.display())));
    }
}
