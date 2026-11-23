// Global Search — Event names, permission strings, limits, and other constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, Pagination, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;

use super::types::*;
use super::store::*;
use super::service::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// Document kinds the global index accepts. Keep this list in sync with
/// the kinds named in the module documentation above.
pub const SEARCH_KINDS: &[&str] = &[
    "software", "product", "order", "invoice", "license", "user", "company", "domain",
    "task", "article", "ticket", "server", "cluster", "backup",
];

/// Maximum number of documents kept in the in-memory index. Oldest
/// documents (by `updated_at`) are evicted first when the cap is hit.
pub const MAX_DOCUMENTS: usize = 20_000;
