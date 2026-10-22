//! SQL for converting between an instant's Unix microseconds (`i64`) and the
//! `timestamptz` it is stored as. Every read and write of an instant goes
//! through these two, so a value round-trips exactly in every year 1..9999 on
//! every supported pg version (#375).
//!
//! `to_timestamp(x::double precision / 1e6)` loses 1 us outside roughly
//! 1932..2100, and `EXTRACT(EPOCH FROM v) * 1e6` is off by up to 5 us far
//! from 1970 on pg13, where EXTRACT returns double precision (pg14 and later
//! return numeric).

/// The `timestamptz` for `micros`, an SQL expression of Unix microseconds.
/// Integer `/` and `%` truncate toward zero, so the remainder of a negative
/// value is negative: -1 is `to_timestamp(0)` minus 1 us,
/// 1969-12-31 23:59:59.999999.
pub fn sql_instant_from_micros(micros: &str) -> String {
    format!(
        "(to_timestamp(({micros})::bigint / 1000000) \
         + (({micros})::bigint % 1000000) * interval '1 microsecond')"
    )
}

/// The Unix microseconds (`bigint`) of `ts`, an SQL `timestamptz` expression.
/// The whole seconds are an integer (exact even as a double on pg13), and the
/// MICROSECONDS field is seconds * 1e6 + the non-negative fraction.
pub fn sql_micros_from_instant(ts: &str) -> String {
    format!(
        "(EXTRACT(EPOCH FROM date_trunc('second', {ts}))::bigint * 1000000 \
         + EXTRACT(MICROSECONDS FROM {ts})::bigint % 1000000)"
    )
}
