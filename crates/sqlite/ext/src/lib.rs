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

use mentat::edn::query::{Element, FindSpec};
use mentat::{
    Binding, QueryInputs, QueryResults, Queryable, StructuredMap, TemporalBound, TxReport,
    TypedValue, Variable,
};

type Res<T> = Result<T, String>;

mod store_cache;

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
// Stores come from `store_cache` (reused per path, checked for staleness);
// `edn_eval` opens its own through the interpreter.
//
// ponytail: the JSON encoders and the inputs contract below are shared in
// spirit with crates/duckdb/src/lib.rs (a cdylib, so not a dependency). Hoist
// both into `mentat` when a third consumer appears.
// ---------------------------------------------------------------------------

/// Prefix an error with the SQL function name, once.
fn ctx(f: &'static str) -> impl Fn(String) -> String {
    move |e| {
        if e.starts_with(f) {
            e
        } else {
            format!("{f}: {e}")
        }
    }
}

fn transact_json(db: &str, edn: &str) -> Res<String> {
    let report = store_cache::transact(db, edn).map_err(ctx("edn_t"))?;
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
    let query = format!("[:find (pull ?e {pattern}) . :in ?e :where [?e _ _]]");
    let inputs = QueryInputs::with_value_sequence(vec![(
        Variable::from_valid_name("?e"),
        TypedValue::Ref(eid),
    )]);
    let results = store_cache::read(db, |store| store.q_once(&query, inputs))
        .map_err(ctx("edn_pull"))?
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
    let out = store_cache::read(db, |store| {
        let (inputs, temporal) =
            mentat::options_from_json(&store.conn().current_schema(), query, &opts)
                .map_err(|e| format!("edn_q: {e}"))?;
        match temporal {
            Some(TemporalBound::AsOf(t)) => store.q_once_as_of(query, inputs, t),
            Some(TemporalBound::Since(t)) => store.q_once_since(query, inputs, t),
            None => store.q_once(query, inputs),
        }
        .map_err(|e| e.to_string())
    })
    .map_err(ctx("edn_q"))?;
    Ok(results_json(&out.spec, out.results).to_string())
}

// ---------------------------------------------------------------------------
// edn_q options: {"inputs": [...], "asOf": T, "since": T}; {} / NULL / '' = none.
// Parsed by mentat::options_from_json, shared with the DuckDB extension and CLI.
// ---------------------------------------------------------------------------

fn parse_opts(text: Option<&str>) -> Res<Json> {
    let text = text.map(str::trim).unwrap_or("");
    if text.is_empty() {
        return Ok(Json::Null);
    }
    serde_json::from_str(text).map_err(|e| format!("edn_q: options are not valid JSON: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opts_text() {
        for empty in [None, Some(""), Some("  "), Some("null")] {
            assert_eq!(parse_opts(empty).unwrap(), Json::Null, "{empty:?}");
        }
        assert_eq!(
            parse_opts(Some(r#"{"since": 9}"#)).unwrap(),
            json!({"since": 9})
        );
        assert!(parse_opts(Some("{nope"))
            .unwrap_err()
            .contains("not valid JSON"));
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
