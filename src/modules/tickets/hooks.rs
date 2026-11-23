// Tickets module — hook handlers.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

pub(crate) fn on_ticket_create_requested(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let input = CreateTicket {
        title: data
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        body: data
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        user_id: data
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        company_id: data
            .get("company_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        priority: data
            .get("priority")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        category: data
            .get("category")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        product: data
            .get("product")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        department_id: data
            .get("department_id")
            .and_then(|v| v.as_str())
            .map(str::to_string),
    };
    match state.create_ticket(input) {
        Ok(ticket) => {
            let value = serde_json::to_value(&ticket).unwrap_or(Value::Null);
            ctx.add_result(value.clone());
            state.emit("ticket.created", value);
        }
        Err(error) => {
            ctx.add_result(json!({ "error": error.message }));
        }
    }
}

/// Run the SLA scan when another module asks for it through the bus
/// (`ticket.sla.check_requested`).
pub(crate) fn on_ticket_sla_check_requested(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let report = state.run_sla_scan();
    ctx.add_result(serde_json::to_value(&report).unwrap_or(Value::Null));
}

/// Answer the Departments module's delete guard
/// (`department.delete_check`) with how many tickets still belong to
/// the department.
pub(crate) fn on_department_delete_check(state: Arc<TicketsState>, ctx: &mut HookContext) {
    let Some(department_id) = ctx.get("department_id").and_then(|v| v.as_str()) else {
        return;
    };
    let ticket_count = state
        .tickets
        .iter()
        .filter(|entry| entry.value().department_id.as_deref() == Some(department_id))
        .count() as u64;
    ctx.add_result(json!({
        "department_id": department_id,
        "ticket_count": ticket_count,
    }));
}
