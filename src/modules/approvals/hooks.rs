// Generic Approvals — Hook-bus listeners.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// `approval.requested` — another module asks for approval of one of
/// its records. Creates the request under the active definition for
/// (`module`, `record_type`) and answers with the `request_id`, or
/// answers `accepted: false` when no definition applies.
pub(crate) fn on_approval_requested(state: Arc<ApprovalsState>, ctx: &mut HookContext) {
    let module = ctx
        .data
        .get("module")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let record_type = ctx
        .data
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let record_id = ctx
        .data
        .get("record_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let requester_id = ctx
        .data
        .get("requester_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let summary = ctx
        .data
        .get("summary")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let amount_minor = ctx
        .data
        .get("amount_minor")
        .and_then(|value| value.as_i64());
    let currency = ctx
        .data
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if module.is_empty() || record_type.is_empty() || record_id.is_empty() || requester_id.is_empty() || summary.is_empty() {
        ctx.add_result(json!({
            "accepted": false,
            "error": "approval.requested requires module, record_type, record_id, requester_id, and summary",
        }));
        return;
    }
    if !valid_record_key(&module, MAX_MODULE_LEN) || !valid_record_key(&record_type, MAX_RECORD_TYPE_LEN) {
        ctx.add_result(json!({
            "accepted": false,
            "error": "module and record_type must be lowercase keys (letters, digits, dots, underscores, hyphens)",
        }));
        return;
    }
    if summary.len() > MAX_SUMMARY_LEN || record_id.len() > MAX_RECORD_ID_LEN {
        ctx.add_result(json!({
            "accepted": false,
            "error": "summary or record_id is too long",
        }));
        return;
    }
    if let Some(amount) = amount_minor {
        if amount < 0 {
            ctx.add_result(json!({
                "accepted": false,
                "error": "amount_minor must not be negative",
            }));
            return;
        }
    }
    let Some(definition) = active_definition_for(&state, &module, &record_type) else {
        ctx.add_result(json!({
            "accepted": false,
            "error": format!("No active approval definition exists for {module} / {record_type}"),
        }));
        return;
    };
    let request = match create_request_record(
        &state,
        &definition,
        module,
        record_type,
        record_id,
        requester_id,
        summary,
        amount_minor,
        currency,
        company_id,
    ) {
        Ok(request) => request,
        Err(error) => {
            ctx.add_result(json!({
                "accepted": false,
                "error": error.to_string(),
            }));
            return;
        }
    };
    let answer = json!({
        "accepted": true,
        "request_id": request.id.to_string(),
        "status": request.status,
    });
    ctx.set("result", answer.clone());
    ctx.add_result(answer);
}

// NOTE: the `approval.status.query` listener was removed: no module
// in the tree ever emitted that event, so the listener was dead
// contract. Request status is available over HTTP instead
// (GET /api/v1/approvals/requests/{id} and the list endpoint).
