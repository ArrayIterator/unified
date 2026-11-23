//! geo - store.

use unified_core::context::AppContext;
use unified_core::db::models::GeoOverride;
use dashmap::DashMap;
use std::sync::{ Arc, OnceLock };
use uuid::Uuid;



// ---------------------------------------------------------------------------
// In-memory store (used when no database pool is attached to the context).
// ---------------------------------------------------------------------------
pub(crate) static OVERRIDES: OnceLock<DashMap<Uuid, GeoOverride>> = OnceLock::new();


pub(crate) fn overrides_store() -> &'static DashMap<Uuid, GeoOverride> {
    OVERRIDES.get_or_init(DashMap::new)
}


/// Application context stashed at `init` so the synchronous `geo.lookup`
/// hook handler can resolve lookups without an async context.
pub(crate) static GEO_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();
