// routes module — Views.
//
// Split from the former single-file module `src/modules/routes.rs`.
// The module root (`routes.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::routes::{
    OverrideSource, RedirectEntry, RouteChange, RouteEntry, RouteRegistry,
    ROUTE_CHANGED_EVENT,
};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

/// One route as listed by the API: registry data plus the override
/// value currently in force at each layer, so the admin UI can show
/// what clearing an override falls back to.
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteView {
    pub(crate) name: String,
    pub(crate) default_path: String,
    pub(crate) current_path: String,
    pub(crate) source: OverrideSource,
    pub(crate) description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) updated_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) previous_path: Option<String>,
}

impl From<RouteEntry> for RouteView {
    fn from(entry: RouteEntry) -> Self {
        Self {
            name: entry.name,
            default_path: entry.default_path,
            current_path: entry.current_path,
            source: entry.source,
            description: entry.description,
            updated_at: entry.updated_at,
            previous_path: entry.previous_path,
        }
    }
}

/// One override change as returned by PUT / DELETE, including the
/// redirect the old path now serves (when the path actually changed).
#[derive(Debug, Clone, Serialize)]
pub(crate) struct RouteChangeView {
    pub(crate) route: String,
    pub(crate) old_path: String,
    pub(crate) new_path: String,
    pub(crate) source: OverrideSource,
    pub(crate) changed: bool,
    pub(crate) changed_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) redirect_from: Option<String>,
}

impl From<RouteChange> for RouteChangeView {
    fn from(change: RouteChange) -> Self {
        let redirect_from = if change.changed {
            Some(change.old_path.clone())
        } else {
            None
        };
        Self {
            route: change.route,
            old_path: change.old_path,
            new_path: change.new_path,
            source: change.source,
            changed: change.changed,
            changed_at: change.changed_at,
            redirect_from,
        }
    }
}
