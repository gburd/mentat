#![allow(
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::too_many_arguments,
    unknown_lints
)]
#![allow(clippy::chunks_exact_to_as_chunks, clippy::manual_is_multiple_of)]
//! `mentat-scale`: the embedded-mentat backend of benchmarks/scale.
//!
//!   mentat-scale load  DATA STORE                      bulk load; writes STORE.load.json + STORE.entids
//!   mentat-scale query STORE DATA SCENARIO [ARG]       one query, result rows as JSON
//!   mentat-scale serve STORE DATA                      same, "SCENARIO [ARG]" per stdin line (bench.py check)
//!   mentat-scale bench STORE DATA SCALE REPS CLIENTS,.. SCEN,.. MIN_S MAX_S MIN_N PROBE_S
//!   mentat-scale mixed STORE DATA SCALE REPS READERS SECS [WINDOW_CSV]
//!   mentat-scale coldwarm STORE DATA SCALE SCEN,..     open + first call + 30 warm calls
//!
//! `bench` and `mixed` print raw CSV rows (see ../README.md), one per
//! (scenario, clients, rep). `Store` is `!Sync`, so every client thread owns its
//! own `Store` on the same file. The stores are opened ONCE per process and
//! reused across scenarios and reps, because `Store::open` is O(history): it
//! scans the whole `parts` view. There is exactly one writer, because each
//! `Store` keeps its own in-memory partition map and two writers would hand
//! out the same tx id.
//!
//! Ceiling: if the first (warm-up) call of a scenario takes longer than
//! PROBE_S, the scenario gets one `op=ceiling` row carrying that latency and is
//! skipped for the remaining clients/reps.
use std::fs;
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use mentat::{Binding, QueryInputs, QueryResults, Queryable, Store, TypedValue, Variable};
use serde_json::{json, Value as Json};

const STATES: [&str; 5] = ["open", "in-progress", "closed", "resolved", "reopened"];

fn q(name: &str) -> String {
    let p = format!("{}/../queries/{name}.edn", env!("CARGO_MANIFEST_DIR"));
    fs::read_to_string(&p).unwrap_or_else(|e| panic!("{p}: {e}"))
}

fn read_json(p: &str) -> Json {
    serde_json::from_str(&fs::read_to_string(p).unwrap_or_else(|e| panic!("{p}: {e}"))).unwrap()
}

/// xorshift64*: deterministic, no dependency.
struct Rng(u64);
impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n.max(1)
    }
}

struct Ctx {
    meta: Json,
    load: Json,
    entids: Vec<i64>,
}

impl Ctx {
    fn new(store: &str, data: &str) -> Ctx {
        let raw = fs::read(format!("{store}.entids")).expect("STORE.entids (run load first)");
        let entids = raw
            .chunks_exact(8)
            .map(|c| i64::from_le_bytes(c.try_into().unwrap()))
            .collect();
        Ctx {
            meta: read_json(&format!("{data}/meta.json")),
            load: read_json(&format!("{store}.load.json")),
            entids,
        }
    }
    fn n(&self, k: &str) -> u64 {
        self.meta[k].as_u64().unwrap()
    }
    fn t(&self, k: &str) -> i64 {
        self.load[k].as_i64().unwrap()
    }
}

fn var(n: &str) -> Variable {
    Variable::from_valid_name(n)
}
fn email(u: u64) -> TypedValue {
    TypedValue::typed_string(format!("user{u}@example.com"))
}

/// Run one scenario iteration; returns the number of result rows.
fn run_one(
    store: &Store,
    ctx: &Ctx,
    scen: &str,
    rng: &mut Rng,
    qs: &Queries,
) -> mentat::Result<usize> {
    let users = ctx.n("n_users");
    let scen = match scen {
        // concurrency_sweep / sustained read mix: q1, q2, pull in equal parts.
        "concurrency_sweep" | "sustained" => {
            ["point_lookup", "ref_traversal", "pull"][rng.below(3) as usize]
        }
        s => s,
    };
    let out = match scen {
        "point_lookup" => store.q_once(
            &qs.q1,
            QueryInputs::with_value_sequence(vec![(var("?email"), email(rng.below(users)))]),
        )?,
        "ref_traversal" => store.q_once(
            &qs.q2,
            QueryInputs::with_value_sequence(vec![(var("?email"), email(rng.below(users)))]),
        )?,
        "as_of" => store.q_once_as_of(
            &qs.q2,
            QueryInputs::with_value_sequence(vec![(var("?email"), email(rng.below(users)))]),
            ctx.t("t_mid"),
        )?,
        "aggregate" => store.q_once(&qs.q3, None)?,
        "predicate_scan" => store.q_once(
            &qs.q4,
            QueryInputs::with_value_sequence(vec![(var("?min"), TypedValue::Long(4))]),
        )?,
        "since" => store.q_once_since(&qs.since, None, ctx.t("t_since"))?,
        "input_bindings" => store.q_once(
            &qs.inputs,
            QueryInputs::with_collection(
                var("?email"),
                (0..100).map(|_| email(rng.below(users))).collect(),
            ),
        )?,
        "pull" => {
            let e = ctx.entids[rng.below(ctx.entids.len() as u64) as usize];
            store.q_once(
                &qs.pull,
                QueryInputs::with_value_sequence(vec![(var("?e"), TypedValue::Ref(e))]),
            )?
        }
        other => panic!("unknown scenario {other}"),
    };
    Ok(match out.results {
        QueryResults::Rel(r) => r.row_count(),
        QueryResults::Coll(c) => c.len(),
        QueryResults::Scalar(s) => s.is_some() as usize,
        QueryResults::Tuple(t) => t.is_some() as usize,
    })
}

struct Queries {
    q1: String,
    q2: String,
    q3: String,
    q4: String,
    since: String,
    inputs: String,
    pull: String,
}
impl Queries {
    fn load() -> Queries {
        Queries {
            q1: q("q1"),
            q2: q("q2"),
            q3: q("q3"),
            q4: q("q4"),
            since: q("since"),
            inputs: q("inputs"),
            pull: q("pull"),
        }
    }
}

fn tv_json(v: &TypedValue) -> Json {
    match v {
        TypedValue::Ref(x) | TypedValue::Long(x) => json!(x),
        TypedValue::Boolean(b) => json!(b),
        TypedValue::Double(d) => json!(d.into_inner()),
        TypedValue::Instant(i) => json!(i.to_rfc3339()),
        TypedValue::String(s) => json!(s.as_str()),
        TypedValue::Keyword(k) => json!(k.to_string()),
        TypedValue::Uuid(u) => json!(u.to_string()),
        TypedValue::Bytes(b) => json!(format!("{b:?}")),
    }
}
fn b_json(b: &Binding) -> Json {
    match b {
        Binding::Scalar(v) => tv_json(v),
        Binding::Vec(vs) => Json::Array(vs.iter().map(b_json).collect()),
        Binding::Map(m) => Json::Object(
            m.0.iter()
                .map(|(k, v)| (k.to_string(), b_json(v)))
                .collect(),
        ),
    }
}

/// Run one scenario query with an explicit argument; rows as JSON.
fn query_rows(s: &Store, ctx: &Ctx, qs: &Queries, scen: &str, arg: Option<&str>) -> mentat::Result<Json> {
    let emailv = |a: Option<&str>| {
        QueryInputs::with_value_sequence(vec![(var("?email"), email(a.unwrap().parse().unwrap()))])
    };
    let out = match scen {
        "point_lookup" => s.q_once(&qs.q1, emailv(arg)),
        "ref_traversal" => s.q_once(&qs.q2, emailv(arg)),
        "as_of" => s.q_once_as_of(&qs.q2, emailv(arg), ctx.t("t_mid")),
        "aggregate" => s.q_once(&qs.q3, None),
        "predicate_scan" => s.q_once(
            &qs.q4,
            QueryInputs::with_value_sequence(vec![(var("?min"), TypedValue::Long(arg.unwrap().parse().unwrap()))]),
        ),
        "since" => s.q_once_since(&qs.since, None, ctx.t("t_since")),
        "input_bindings" => s.q_once(
            &qs.inputs,
            QueryInputs::with_collection(
                var("?email"),
                arg.unwrap().split(',').map(|u| email(u.parse().unwrap())).collect(),
            ),
        ),
        "pull" => {
            let e = ctx.entids[arg.unwrap().parse::<usize>().unwrap()];
            s.q_once(&qs.pull, QueryInputs::with_value_sequence(vec![(var("?e"), TypedValue::Ref(e))]))
        }
        other => panic!("unknown scenario {other}"),
    }?;
    let rows: Vec<Json> = match out.results {
        QueryResults::Rel(r) => r.rows().map(|row| Json::Array(row.iter().map(b_json).collect())).collect(),
        QueryResults::Coll(c) => c.iter().map(|b| json!([b_json(b)])).collect(),
        QueryResults::Scalar(s) => s.iter().map(|b| json!([b_json(b)])).collect(),
        QueryResults::Tuple(t) => t.iter().map(|r| Json::Array(r.iter().map(b_json).collect())).collect(),
    };
    Ok(Json::Array(rows))
}

/// `query`: one scenario query, rows as JSON.
fn cmd_query(store: &str, data: &str, scen: &str, arg: Option<&str>) {
    let ctx = Ctx::new(store, data);
    let s = Store::open(store).unwrap();
    println!("{}", query_rows(&s, &ctx, &Queries::load(), scen, arg).unwrap());
}

/// `serve`: open the store once; answer "SCEN [ARG]" lines from stdin with one
/// JSON line each (for bench.py's checks: Store::open is O(history)).
fn cmd_serve(store: &str, data: &str) {
    use std::io::{BufRead, Write};
    let ctx = Ctx::new(store, data);
    let s = Store::open(store).unwrap();
    let qs = Queries::load();
    let out = std::io::stdout();
    for line in std::io::stdin().lock().lines() {
        let line = line.unwrap();
        let mut it = line.split_whitespace();
        let Some(scen) = it.next() else { continue };
        let r = match query_rows(&s, &ctx, &qs, scen, it.next()) {
            Ok(j) => j.to_string(),
            Err(e) => json!({"error": e.to_string()}).to_string(),
        };
        let mut o = out.lock();
        writeln!(o, "{r}").unwrap();
        o.flush().unwrap();
    }
}

/// Tempid -> entid maps the loader fills from tx reports ("u7", "l3", "i42").
#[derive(Default)]
struct Ids {
    u: Vec<i64>,
    l: Vec<i64>,
    i: Vec<i64>,
}
impl Ids {
    fn record(&mut self, tempids: &std::collections::BTreeMap<String, i64>) {
        for (k, &v) in tempids {
            let (kind, n) = k.split_at(1);
            let Ok(n) = n.parse::<usize>() else { continue };
            let vec = match kind {
                "u" => &mut self.u,
                "l" => &mut self.l,
                "i" => &mut self.i,
                _ => continue,
            };
            if vec.len() <= n {
                vec.resize(n + 1, 0);
            }
            vec[n] = v;
        }
    }
    /// Replace every `@@u<n>` / `@@l<n>` / `@@i<n>` with its recorded entid.
    fn subst(&self, line: &str) -> String {
        let mut out = String::with_capacity(line.len() + 64);
        let b = line.as_bytes();
        let mut k = 0;
        while k < b.len() {
            if b[k] == b'@' && b.get(k + 1) == Some(&b'@') {
                // A bare `@@<n>` (datasets from before the u/l/i tags) is an issue.
                let (kind, mut j) = if b[k + 2].is_ascii_digit() { (b'i', k + 2) } else { (b[k + 2], k + 3) };
                let mut n = 0usize;
                while j < b.len() && b[j].is_ascii_digit() {
                    n = n * 10 + (b[j] - b'0') as usize;
                    j += 1;
                }
                let e = match kind {
                    b'u' => self.u[n],
                    b'l' => self.l[n],
                    _ => self.i[n],
                };
                assert!(e != 0, "unresolved @@{}{n}", kind as char);
                out.push_str(&e.to_string());
                k = j;
            } else {
                let c = line[k..].chars().next().unwrap();
                out.push(c);
                k += c.len_utf8();
            }
        }
        out
    }
}

fn shard_files(data: &str, prefix: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(format!("{data}/store"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|f| f.starts_with(prefix))
        .map(|f| format!("{data}/store/{f}"))
        .collect();
    v.sort();
    v
}

fn cmd_load(data: &str, store: &str) {
    let meta = read_json(&format!("{data}/meta.json"));
    let n_issues = meta["n_issues"].as_u64().unwrap() as usize;
    for suf in ["", "-wal", "-shm", ".entids", ".load.json"] {
        let _ = fs::remove_file(format!("{store}{suf}"));
    }
    let t0 = Instant::now();
    let mut s = Store::open(store).unwrap();
    let schema = fs::read_to_string(format!("{data}/schema.edn")).unwrap();
    s.transact(&schema).unwrap();
    let mut ids = Ids::default();
    let mut txs = 0u64;
    let mut t_mid = 0;
    // LOAD_MAX_S: give up (exit 3) once the load has run this long, and record
    // how far it got, so a scale that is too big for this backend becomes a
    // measured ceiling instead of a hang.
    let max_s: f64 = std::env::var("LOAD_MAX_S")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(f64::MAX);
    let per_tx = meta["batch"].as_u64().unwrap_or(500) as f64
        * meta["n_datoms_initial"].as_f64().unwrap()
        / meta["n_issues"].as_f64().unwrap();
    let mut files = vec![format!("{data}/store/base.edn")];
    files.extend(shard_files(data, "issues-"));
    for f in &files {
        let text = fs::read_to_string(f).unwrap_or_else(|e| panic!("{f}: {e}"));
        for line in text.lines().filter(|l| !l.is_empty()) {
            if t0.elapsed().as_secs_f64() > max_s {
                let el = t0.elapsed().as_secs_f64();
                let done = txs as f64 * per_tx;
                let info = json!({"ceiling": true, "elapsed_s": el, "txs": txs,
                                  "datoms_loaded_est": done, "datoms_per_s": done / el,
                                  "n_datoms": meta["n_datoms"]});
                fs::write(format!("{store}.load.json"), info.to_string()).unwrap();
                println!("{info}");
                std::process::exit(3);
            }
            let r = s
                .transact(&ids.subst(line))
                .unwrap_or_else(|e| panic!("{f}: {e}"));
            ids.record(&r.tempids);
            t_mid = r.tx_id;
            txs += 1;
            if txs % 200 == 0 {
                eprintln!("load: {txs} txs {:.0}s", t0.elapsed().as_secs_f64());
            }
        }
    }
    assert!(
        ids.i.len() == n_issues && ids.i.iter().all(|&e| e != 0),
        "an issue tempid was not reported"
    );
    let t_initial = t0.elapsed().as_secs_f64();
    let hist = shard_files(data, "hist-");
    let mut t_since = t_mid;
    for (k, f) in hist.iter().enumerate() {
        if k + 1 == hist.len() {
            t_since = s.last_tx_id();
        }
        for line in fs::read_to_string(f)
            .unwrap()
            .lines()
            .filter(|l| !l.is_empty())
        {
            s.transact(&ids.subst(line))
                .unwrap_or_else(|e| panic!("{f}: {e}"));
            txs += 1;
        }
    }
    let load_s = t0.elapsed().as_secs_f64();
    drop(s);
    let bytes: Vec<u8> = ids.i.iter().flat_map(|e| e.to_le_bytes()).collect();
    fs::write(format!("{store}.entids"), bytes).unwrap();
    let size = fs::metadata(store).map(|m| m.len()).unwrap_or(0);
    let info = json!({"t_mid": t_mid, "t_since": t_since, "load_s": load_s,
                      "initial_load_s": t_initial, "txs": txs, "store_bytes": size,
                      "n_datoms": meta["n_datoms"]});
    fs::write(format!("{store}.load.json"), info.to_string()).unwrap();
    println!("{info}");
}

fn pct(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((p / 100.0) * sorted.len() as f64).ceil() as usize;
    sorted[i.clamp(1, sorted.len()) - 1]
}

fn row(
    meta: &Json,
    scale: &str,
    rep: &str,
    scen: &str,
    clients: usize,
    op: &str,
    lat: &mut [f64],
    wall: f64,
    errors: usize,
) {
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "{scen},embedded,{scale},{},{clients},{op},{},{:.3},{:.3},{:.3},{:.3},{:.2},{errors},{rep}",
        meta["n_datoms"],
        lat.len(),
        pct(lat, 50.0),
        pct(lat, 95.0),
        pct(lat, 99.0),
        lat.last().copied().unwrap_or(0.0),
        lat.len() as f64 / wall.max(1e-9),
    );
}

#[derive(Clone, Copy)]
struct Limits {
    min_s: f64,
    max_s: f64,
    min_n: usize,
    probe_s: f64,
}

/// One client: warm up (bounded), then loop until the limits say stop.
/// Returns (latencies, errors, wall, first_call_ms).
fn client(
    s: &Store,
    ctx: &Ctx,
    scen: &str,
    seed: u64,
    lim: &Limits,
    bar: &Barrier,
) -> (Vec<f64>, usize, f64, f64) {
    let qs = Queries::load();
    let mut rng = Rng::new(seed);
    let mut errors = 0;
    let w0 = Instant::now();
    let mut first = 0.0;
    for i in 0..3 {
        let t = Instant::now();
        // The first call is the probe: a watchdog interrupts SQLite at PROBE_S,
        // so a pathological plan costs PROBE_S, not minutes.
        let (done, rx) = std::sync::mpsc::channel::<()>();
        let h = s.sqlite_ref().get_interrupt_handle();
        let limit = Duration::from_secs_f64(lim.probe_s);
        let wd = (i == 0).then(|| {
            std::thread::spawn(move || {
                if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) = rx.recv_timeout(limit) {
                    h.interrupt();
                }
            })
        });
        // An interrupted query panics inside mentat's projector (it unwraps
        // the row iterator), so the probe must also catch unwinds.
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_one(s, ctx, scen, &mut rng, &qs)
        }));
        if !matches!(r, Ok(Ok(_))) {
            errors += 1;
        }
        let _ = done.send(());
        if let Some(wd) = wd {
            wd.join().unwrap();
        }
        if i == 0 {
            first = t.elapsed().as_secs_f64() * 1e3;
        }
        if first >= lim.probe_s * 1e3 || w0.elapsed().as_secs_f64() > lim.max_s / 4.0 {
            break;
        }
    }
    bar.wait();
    let mut lat = Vec::new();
    let t0 = Instant::now();
    if first >= lim.probe_s * 1e3 {
        return (lat, errors, 0.0, first);
    }
    loop {
        let el = t0.elapsed().as_secs_f64();
        if (lat.len() >= lim.min_n && el >= lim.min_s) || (el >= lim.max_s && !lat.is_empty()) {
            break;
        }
        let t = Instant::now();
        match run_one(s, ctx, scen, &mut rng, &qs) {
            Ok(_) => lat.push(t.elapsed().as_secs_f64() * 1e3),
            Err(_) => errors += 1,
        }
    }
    (lat, errors, t0.elapsed().as_secs_f64(), first)
}

fn open_stores(store: &str, n: usize) -> Vec<Store> {
    let t = Instant::now();
    let v: Vec<Store> = std::thread::scope(|sc| {
        let hs: Vec<_> = (0..n)
            .map(|_| sc.spawn(|| Store::open(store).unwrap()))
            .collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    eprintln!("opened {n} stores in {:.1}s", t.elapsed().as_secs_f64());
    v
}

#[allow(clippy::too_many_arguments)]
fn cmd_bench(
    store: &str,
    data: &str,
    scale: &str,
    reps: usize,
    clients: &[usize],
    scens: &[&str],
    lim: Limits,
) {
    let ctx = Ctx::new(store, data);
    let max_c = *clients.iter().max().unwrap();
    let mut stores = open_stores(store, max_c);
    let mut ceilinged: Vec<String> = Vec::new();
    for rep in 1..=reps {
        for &scen in scens {
            for &c in clients {
                if ceilinged.iter().any(|x| x == scen) {
                    continue;
                }
                let lim_c = Limits {
                    min_n: lim.min_n.div_ceil(c),
                    ..lim
                };
                let bar = Barrier::new(c);
                let res: Vec<_> = std::thread::scope(|sc| {
                    let hs: Vec<_> = stores[..c]
                        .iter_mut()
                        .enumerate()
                        .map(|(i, st)| {
                            let (ctx, lim_c, bar) = (&ctx, &lim_c, &bar);
                            // &mut Store is Send (Store is Send, not Sync): each thread owns its Store.
                            sc.spawn(move || {
                                client(
                                    &*st,
                                    ctx,
                                    scen,
                                    1 + i as u64 + 1000 * rep as u64,
                                    lim_c,
                                    bar,
                                )
                            })
                        })
                        .collect();
                    hs.into_iter().map(|h| h.join().unwrap()).collect()
                });
                let first = res.iter().map(|r| r.3).fold(0.0, f64::max);
                let errors: usize = res.iter().map(|r| r.1).sum();
                if first >= lim.probe_s * 1e3 {
                    // The interrupted probe panicked inside mentat and poisoned
                    // the Store's metadata mutex: reopen those stores.
                    for st in stores[..c].iter_mut() {
                        *st = Store::open(store).unwrap();
                    }
                    row(
                        &ctx.meta,
                        scale,
                        &rep.to_string(),
                        scen,
                        c,
                        "ceiling",
                        &mut [first],
                        first / 1e3,
                        errors,
                    );
                    ceilinged.push(scen.to_string());
                    continue;
                }
                let mut lat: Vec<f64> = res.iter().flat_map(|r| r.0.iter().copied()).collect();
                let wall = res.iter().map(|r| r.2).fold(0.0, f64::max);
                row(
                    &ctx.meta,
                    scale,
                    &rep.to_string(),
                    scen,
                    c,
                    "read",
                    &mut lat,
                    wall,
                    errors,
                );
            }
        }
    }
}

/// `mixed`: one writer thread (state updates / label adds) + READERS threads
/// alternating point_lookup and ref_traversal, for SECS seconds.
fn cmd_mixed(
    store: &str,
    data: &str,
    scale: &str,
    reps: usize,
    readers: usize,
    secs: f64,
    window: Option<&str>,
) {
    for rep in 1..=reps {
        mixed_once(store, data, scale, &rep.to_string(), readers, secs, window);
    }
}

/// 10-second windows of (t_start_s, op, n, p50, p99) for drift analysis.
fn write_windows(path: &str, series: &[(&str, Vec<(f32, f32)>)]) {
    use std::fmt::Write;
    let mut out = String::from("t_s,op,count,p50_ms,p99_ms,throughput_ops_s\n");
    for (op, v) in series {
        let mut by: std::collections::BTreeMap<u32, Vec<f64>> = Default::default();
        for &(t, l) in v {
            by.entry((t / 10.0) as u32).or_default().push(l as f64);
        }
        for (w, mut l) in by {
            l.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let _ = writeln!(
                out,
                "{},{op},{},{:.3},{:.3},{:.2}",
                w * 10,
                l.len(),
                pct(&l, 50.0),
                pct(&l, 99.0),
                l.len() as f64 / 10.0
            );
        }
    }
    fs::write(path, out).unwrap();
}

fn mixed_once(
    store: &str,
    data: &str,
    scale: &str,
    rep: &str,
    readers: usize,
    secs: f64,
    window: Option<&str>,
) {
    // With a window file this is the long `sustained` run; otherwise `write_mixed`.
    let label = if window.is_some() {
        "sustained"
    } else {
        "write_mixed"
    };
    let ctx = Arc::new(Ctx::new(store, data));
    // Open everything before the clock starts (Store::open is O(history)).
    let mut wstore = Some(Store::open(store).unwrap());
    let mut rstores: Vec<Option<Store>> =
        open_stores(store, readers).into_iter().map(Some).collect();
    let start = Instant::now();
    let stop = start + Duration::from_secs_f64(secs);
    let writer = {
        let ctx = ctx.clone();
        let mut s = wstore.take().unwrap();
        std::thread::spawn(move || {
            let mut rng = Rng::new(424242);
            let (mut lat, mut err, mut ts) = (Vec::new(), 0, Vec::new());
            let t0 = Instant::now();
            let labels = ctx.n("n_labels");
            while Instant::now() < stop {
                let e = ctx.entids[rng.below(ctx.entids.len() as u64) as usize];
                let tx = if lat.len() % 2 == 0 {
                    format!(
                        "[[:db/add {e} :issue/state :state/{}]]",
                        STATES[rng.below(5) as usize]
                    )
                } else {
                    format!(
                        "[[:db/add {e} :issue/label (lookup-ref :label/name \"label-{}\")]]",
                        rng.below(labels)
                    )
                };
                let t = Instant::now();
                match s.transact(&tx) {
                    Ok(_) => {
                        let l = t.elapsed().as_secs_f64() * 1e3;
                        lat.push(l);
                        ts.push(((t - start).as_secs_f32(), l as f32));
                    }
                    Err(_) => err += 1,
                }
            }
            (lat, err, t0.elapsed().as_secs_f64(), ts)
        })
    };
    let hs: Vec<_> = (0..readers)
        .map(|c| {
            let ctx = ctx.clone();
            let s = rstores[c].take().unwrap();
            std::thread::spawn(move || {
                let qs = Queries::load();
                let mut rng = Rng::new(7 + c as u64);
                let (mut lat, mut err, mut ts) = (Vec::new(), 0, Vec::new());
                let t0 = Instant::now();
                let mut i = 0;
                while Instant::now() < stop {
                    let scen = if label == "sustained" {
                        "sustained"
                    } else if i % 2 == 0 {
                        "point_lookup"
                    } else {
                        "ref_traversal"
                    };
                    i += 1;
                    let t = Instant::now();
                    match run_one(&s, &ctx, scen, &mut rng, &qs) {
                        Ok(_) => {
                            let l = t.elapsed().as_secs_f64() * 1e3;
                            lat.push(l);
                            ts.push(((t - start).as_secs_f32(), l as f32));
                        }
                        Err(_) => err += 1,
                    }
                }
                (lat, err, t0.elapsed().as_secs_f64(), ts)
            })
        })
        .collect();
    let (mut rl, mut re, mut rw, mut rts) = (Vec::new(), 0, 0f64, Vec::new());
    for h in hs {
        let (l, e, w, t) = h.join().unwrap();
        rl.extend(l);
        re += e;
        rw = rw.max(w);
        rts.extend(t);
    }
    let (mut wl, we, ww, wts) = writer.join().unwrap();
    if let Some(p) = window {
        write_windows(p, &[("read", rts), ("write", wts)]);
    }
    row(
        &ctx.meta, scale, rep, label, readers, "read", &mut rl, rw, re,
    );
    row(&ctx.meta, scale, rep, label, 1, "write", &mut wl, ww, we);
}

/// cold_vs_warm: time Store::open, the first call, then 30 warm calls per scenario.
fn cmd_coldwarm(store: &str, data: &str, scale: &str, scens: &[&str]) {
    let ctx = Ctx::new(store, data);
    let t = Instant::now();
    let s = Store::open(store).unwrap();
    let open_ms = t.elapsed().as_secs_f64() * 1e3;
    row(
        &ctx.meta,
        scale,
        "1",
        "cold_vs_warm",
        1,
        "open",
        &mut [open_ms],
        open_ms / 1e3,
        0,
    );
    let qs = Queries::load();
    let mut rng = Rng::new(99);
    for &scen in scens {
        let t = Instant::now();
        let e = run_one(&s, &ctx, scen, &mut rng, &qs).is_err() as usize;
        let first = t.elapsed().as_secs_f64() * 1e3;
        row(
            &ctx.meta,
            scale,
            "1",
            "cold_vs_warm",
            1,
            &format!("cold_{scen}"),
            &mut [first],
            first / 1e3,
            e,
        );
        let mut lat = Vec::new();
        let t0 = Instant::now();
        for _ in 0..30 {
            let t = Instant::now();
            run_one(&s, &ctx, scen, &mut rng, &qs).unwrap();
            lat.push(t.elapsed().as_secs_f64() * 1e3);
        }
        row(
            &ctx.meta,
            scale,
            "1",
            "cold_vs_warm",
            1,
            &format!("warm_{scen}"),
            &mut lat,
            t0.elapsed().as_secs_f64(),
            0,
        );
    }
}

fn main() {
    // Silence the expected panic message from an interrupted probe.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |i| {
        if !i.to_string().contains("interrupted") {
            hook(i)
        }
    }));
    let a: Vec<String> = std::env::args().collect();
    let a: Vec<&str> = a.iter().map(String::as_str).collect();
    match a.get(1).copied() {
        Some("load") => cmd_load(a[2], a[3]),
        Some("query") => cmd_query(a[2], a[3], a[4], a.get(5).copied()),
        Some("serve") => cmd_serve(a[2], a[3]),
        Some("bench") => {
            let clients: Vec<usize> = a[6].split(',').map(|x| x.parse().unwrap()).collect();
            let scens: Vec<&str> = a[7].split(',').collect();
            let lim = Limits {
                min_s: a[8].parse().unwrap(),
                max_s: a[9].parse().unwrap(),
                min_n: a[10].parse().unwrap(),
                probe_s: a[11].parse().unwrap(),
            };
            cmd_bench(
                a[2],
                a[3],
                a[4],
                a[5].parse().unwrap(),
                &clients,
                &scens,
                lim,
            )
        }
        Some("mixed") => cmd_mixed(
            a[2],
            a[3],
            a[4],
            a[5].parse().unwrap(),
            a[6].parse().unwrap(),
            a[7].parse().unwrap(),
            a.get(8).copied(),
        ),
        Some("coldwarm") => cmd_coldwarm(a[2], a[3], a[4], &a[5].split(',').collect::<Vec<_>>()),
        _ => panic!("usage: see src/main.rs header"),
    }
}
