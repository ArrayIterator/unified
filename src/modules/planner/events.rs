//! Planner - Hook listeners.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, helpers::*, types::*, state::*, handlers_pools::*, forecast::*, handlers_plans::*, module::*};

// Hook listeners.

/// Provider sync completed: refresh the last-sync information of every
/// pool that draws from this connection. The sync payload carries no
/// per-server usage numbers, so it cannot create utilisation samples;
/// those come from POST /pools/{id}/usage.
pub(crate) fn on_provider_sync_completed(state: Arc<PlannerState>, data: &Value) {
    let Some(connection_id) = data.get("connection_id").and_then(|value| value.as_str()) else {
        return;
    };
    let server_count = data
        .get("server_count")
        .and_then(|value| value.as_u64());
    for mut entry in state.pools.iter_mut() {
        let pool = entry.value_mut();
        if pool.provider_connection_id.as_deref() != Some(connection_id) {
            continue;
        }
        pool.last_sync_at = Some(now());
        pool.last_server_count = server_count;
        pool.updated_at = now();
    }
}
