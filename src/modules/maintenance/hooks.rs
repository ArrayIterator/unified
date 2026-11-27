// Maintenance module - hook handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// Hook handlers

pub(crate) fn on_maintenance_check(state: Arc<MaintenanceState>, _ctx: &mut HookContext) {
    // Runs the time-based transitions without an HTTP request (for
    // example from a scheduler job emitting `maintenance.check`).
    // The global flag is synced on the next HTTP handler call, which
    // always runs `refresh` with the application context.
    state.refresh(None);
}
