// OKR / Goals - Hook-bus listeners.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::module::*;

/// Refresh the roll-up of every key result linked to the completed
/// task id. Unknown task ids are still recorded in
/// `completed_tasks` so a link created later picks the completion
/// up on its next refresh.
pub(crate) fn on_task_completed(state: OkrState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let task_id = data
        .get("task_id")
        .and_then(Value::as_str)
        .or_else(|| data.get("id").and_then(Value::as_str))
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(task_id) = task_id else {
        return;
    };
    state.completed_tasks.insert(task_id, Utc::now());
    let affected: Vec<Uuid> = state
        .links
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|link| link.link_type == "task" && link.target_id == task_id)
        .map(|link| link.key_result_id)
        .collect();
    for key_result_id in affected {
        refresh_rollup(&state, key_result_id);
    }
}

/// Emit an audit-shaped event on the hook bus: the audit module
/// reads `actor_id`, `resource`, and `id` from the payload. The
/// audit module only records events listed in its `AUDITED_EVENTS`,
/// so the integrator must add `okrs.kr.override` there.
pub(crate) fn emit_audit(state: &OkrState, event: &str, actor: &str, resource: &str, extra: Value) {
    let mut payload = match extra {
        Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    payload.insert("actor_id".to_string(), Value::String(actor.to_string()));
    payload.insert("resource".to_string(), Value::String(resource.to_string()));
    state.emit(event, Value::Object(payload));
}
