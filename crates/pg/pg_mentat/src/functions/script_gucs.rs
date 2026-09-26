//! Resource-limit GUCs for the `edn_eval` scripting surface (§ 1.1).
//!
//! `edn_eval` is intentionally callable by every role (no `REVOKE`, not
//! `SECURITY DEFINER`), so the sandbox plus these limits are the entire defense
//! against a hostile script. Each GUC is `PGC_SUSET`: a superuser or
//! `ALTER SYSTEM` sets them, an ordinary role cannot raise them for its own
//! session. `statement_timeout` still applies on top, via the interpreter's
//! interrupt check hook.
//!
//! Gated behind the `script` feature; nothing here compiles for a default build.

use pgrx::{GucContext, GucFlags, GucRegistry, GucSetting};

/// Max eval steps for one `edn_eval` call. Bounds CPU: `(loop [] (recur))`
/// throws `:eval/limit` once the counter is exhausted. Default 10,000,000.
pub static SCRIPT_MAX_STEPS: GucSetting<i32> = GucSetting::<i32>::new(10_000_000);

/// Max bytes a single `edn_eval` call may charge to its allocation budget.
/// Bounds memory in one step: `(range 100000000000)` throws `:eval/limit`
/// before allocating. Default 67108864 (64 MiB).
pub static SCRIPT_MAX_HEAP_BYTES: GucSetting<i32> = GucSetting::<i32>::new(67_108_864);

/// Max eval/apply nesting depth for one `edn_eval` call. Bounds stack:
/// deep non-tail recursion throws `:eval/limit` instead of overflowing the
/// Rust stack (which would SIGABRT the backend and crash the server into
/// recovery). Default 2000.
pub static SCRIPT_MAX_DEPTH: GucSetting<i32> = GucSetting::<i32>::new(2000);

/// Register the three `edn_eval` limit GUCs. Called from `_PG_init`.
pub fn register_script_gucs() {
    GucRegistry::define_int_guc(
        c"mentat.script_max_steps",
        c"Max eval steps for one edn_eval call.",
        c"Bounds CPU for the edn_eval scripting surface. A script exceeding this many interpreter steps (e.g. an infinite (loop [] (recur))) aborts with an :eval/limit error. Default 10000000. PGC_SUSET: only a superuser or ALTER SYSTEM may raise it. statement_timeout still applies on top.",
        &SCRIPT_MAX_STEPS,
        0,
        i32::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"mentat.script_max_heap_bytes",
        c"Max allocation-budget bytes for one edn_eval call.",
        c"Bounds memory for the edn_eval scripting surface. A single bulk-allocating step (e.g. (range 100000000000)) that would exceed this many bytes aborts with an :eval/limit error before allocating. Default 67108864 (64 MiB). PGC_SUSET: only a superuser or ALTER SYSTEM may raise it.",
        &SCRIPT_MAX_HEAP_BYTES,
        0,
        i32::MAX,
        GucContext::Suset,
        GucFlags::default(),
    );

    GucRegistry::define_int_guc(
        c"mentat.script_max_depth",
        c"Max eval/apply nesting depth for one edn_eval call.",
        c"Bounds stack for the edn_eval scripting surface. Deep non-tail recursion aborts with an :eval/limit error instead of overflowing the Rust stack (which would crash the backend and force crash recovery). Default 2000. PGC_SUSET: only a superuser or ALTER SYSTEM may raise it.",
        &SCRIPT_MAX_DEPTH,
        1,
        100_000,
        GucContext::Suset,
        GucFlags::default(),
    );
}
