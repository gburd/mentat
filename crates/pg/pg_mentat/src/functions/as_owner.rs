//! Run one statement as a table's owner, the way PostgreSQL runs VACUUM,
//! ANALYZE and REFRESH MATERIALIZED VIEW (src/backend/commands/matview.c,
//! vacuum.c): the owner's userid, a security-restricted operation, and a
//! search_path of `pg_catalog, pg_temp` at a fresh GUC nest level.
//!
//! The 1.19.0 upgrade's clean-up of earlier excisions deletes rows in tables
//! a store's owner owns while the upgrade runs as a superuser. Code the owner
//! attached to those tables (a trigger, an index expression, ...) then runs
//! with the owner's own privileges, not the superuser's (#426). The upgrade
//! also skips stores with such code, so this is the second layer.

use pgrx::prelude::*;
use pgrx::spi::Spi;

/// Run `stmt` (one SQL statement returning one `bigint[]` column, or no
/// rows) as the owner of `rel`, with `$1` = `p1`, `$2` = `p2`, `$3` = `p3`.
/// Returns the first row's value, NULL for none. Superusers only: it exists
/// for the upgrade scripts.
#[pg_extern(schema = "mentat", name = "_as_table_owner")]
fn as_table_owner(
    rel: pg_sys::Oid,
    stmt: &str,
    p1: Option<i64>,
    p2: Option<Vec<i64>>,
    p3: Option<Vec<i64>>,
) -> Option<Vec<i64>> {
    // SAFETY: plain catalog/userid calls on the backend's own state.
    if !unsafe { pg_sys::superuser() } {
        error!("mentat._as_table_owner: only a superuser may call it");
    }
    let owner = Spi::get_one_with_args::<pg_sys::Oid>(
        "SELECT relowner FROM pg_catalog.pg_class WHERE oid = $1",
        &[pgrx::datum::DatumWithOid::from(rel)],
    )
    .ok()
    .flatten()
    .unwrap_or_else(|| error!("mentat._as_table_owner: no relation with oid {rel:?}"));

    let mut save_userid = pg_sys::InvalidOid;
    let mut save_sec_context: i32 = 0;
    // SAFETY: as matview.c does; an ERROR below is cleaned up by the
    // (sub)transaction abort, which restores the userid and the GUC level.
    let save_nestlevel = unsafe {
        pg_sys::GetUserIdAndSecContext(&mut save_userid, &mut save_sec_context);
        pg_sys::SetUserIdAndSecContext(
            owner,
            save_sec_context | pg_sys::SECURITY_RESTRICTED_OPERATION as i32,
        );
        let level = pg_sys::NewGUCNestLevel();
        restrict_search_path();
        level
    };

    let out = Spi::get_one_with_args::<Vec<i64>>(
        stmt,
        &[
            pgrx::datum::DatumWithOid::from(p1),
            pgrx::datum::DatumWithOid::from(p2),
            pgrx::datum::DatumWithOid::from(p3),
        ],
    );

    // SAFETY: undo exactly what was set above, on the success path.
    unsafe {
        pg_sys::AtEOXact_GUC(false, save_nestlevel);
        pg_sys::SetUserIdAndSecContext(save_userid, save_sec_context);
    }
    match out {
        Ok(v) => v,
        Err(pgrx::spi::SpiError::InvalidPosition) => None,
        Err(e) => error!("mentat._as_table_owner: {e}"),
    }
}

/// `RestrictSearchPath()` (PostgreSQL 17+), spelled out for 13-16.
unsafe fn restrict_search_path() {
    #[cfg(any(feature = "pg17", feature = "pg18", feature = "pg19"))]
    pg_sys::RestrictSearchPath();
    #[cfg(not(any(feature = "pg17", feature = "pg18", feature = "pg19")))]
    pg_sys::set_config_option(
        c"search_path".as_ptr(),
        c"pg_catalog, pg_temp".as_ptr(),
        pg_sys::GucContext::PGC_USERSET,
        pg_sys::GucSource::PGC_S_SESSION,
        pg_sys::GucAction::GUC_ACTION_SAVE,
        true,
        0,
        false,
    );
}
