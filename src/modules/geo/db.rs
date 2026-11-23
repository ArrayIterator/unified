//! geo - db.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::db::models::GeoOverride;
use diesel::prelude::*;



// ---------------------------------------------------------------------------
// Database helpers (only used when a pool is attached to the context).
// ---------------------------------------------------------------------------
pub(crate) fn db_pool(ctx: &AppContext) -> Option<unified_core::db::DbPool> {
    ctx.db.read().expect("db lock poisoned").clone()
}


pub(crate) fn db_load_overrides(pool: &unified_core::db::DbPool) -> UnifiedResult<Vec<GeoOverride>> {
    use unified_core::db::schema::geo_overrides::dsl as geo_dsl;
    let mut conn = pool.get().map_err(|error| {
        UnifiedError::internal(format!("Cannot get database connection: {error}"))
    })?;
    geo_dsl::geo_overrides
        .select(GeoOverride::as_select())
        .load(&mut conn)
        .map_err(|error| UnifiedError::internal(format!("Cannot load Geo overrides: {error}")))
}
