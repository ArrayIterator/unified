//! Compliance Center module — GDPR / ISO support: data export — Deletion handlers.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_deletions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let requester_filter = params.filters.get("requester_id").cloned();
        let mut items: Vec<Value> = state
            .deletions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|deletion| {
                if !is_manager && deletion.requester_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if deletion.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(requester) = &requester_filter {
                    if &deletion.requester_id != requester {
                        return false;
                    }
                }
                true
            })
            .map(|deletion| deletion_view(&state, &deletion))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) fn deletion_view(state: &ComplianceState, deletion: &DeletionRequest) -> Value {
    let mut value = serde_json::to_value(deletion).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let items: Vec<DeletionItem> = {
            let mut rows: Vec<DeletionItem> = state
                .deletion_items
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|item| item.deletion_request_id == deletion.id)
                .collect();
            rows.sort_by_key(|item| item.created_at);
            rows
        };
        map.insert(
            "items".to_string(),
            serde_json::to_value(items).unwrap_or(Value::Null),
        );
    }
    value
}

pub(super) async fn create_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateDeletionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        if input.items.is_empty() || input.items.len() > 50 {
            return Err(UnifiedError::bad_request(
                "items must list between 1 and 50 per-module deletion items",
            ));
        }
        let mut seen_modules: Vec<String> = Vec::new();
        for item in &input.items {
            let module_id = require_text(&item.module_id, "items[].module_id", MAX_NAME_LEN)?;
            if seen_modules.contains(&module_id) {
                return Err(UnifiedError::bad_request(format!(
                    "module \"{module_id}\" is listed twice in items; use one item per module"
                )));
            }
            seen_modules.push(module_id);
            require_text(&item.subject, "items[].subject", MAX_DESCRIPTION_LEN)?;
        }
        if state.deletions.iter().any(|entry| {
            entry.value().requester_id == caller.actor
                && matches!(
                    entry.value().status,
                    DeletionStatus::Requested
                        | DeletionStatus::Approved
                        | DeletionStatus::Executing
                )
        }) {
            return Err(UnifiedError::conflict(
                "You already have an open deletion request; wait for it to finish first",
            ));
        }
        let now = Utc::now();
        let deletion = DeletionRequest {
            id: Uuid::now_v7(),
            requester_id: caller.actor.clone(),
            reason,
            status: DeletionStatus::Requested,
            approved_by: None,
            approved_at: None,
            decided_by: None,
            decision_reason: None,
            executed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.deletions.insert(deletion.id, deletion.clone());
        for item in &input.items {
            let row = DeletionItem {
                id: Uuid::now_v7(),
                deletion_request_id: deletion.id,
                module_id: item.module_id.trim().to_string(),
                subject: item.subject.trim().to_string(),
                status: "pending".to_string(),
                detail: None,
                executed_at: None,
                created_at: now,
                updated_at: now,
            };
            state.deletion_items.insert(row.id, row);
        }
        state.emit(
            "compliance.deletion.requested",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "reason": deletion.reason,
                "items": input.items.iter().map(|item| json!({
                    "module": item.module_id,
                    "subject": item.subject,
                })).collect::<Vec<_>>(),
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let deletion = load_deletion(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &deletion.requester_id, &caller.actor)?;
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn approve_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut deletion = load_deletion(&state, &path.into_inner())?;
        if deletion.status != DeletionStatus::Requested {
            return Err(UnifiedError::conflict(format!(
                "Only a requested deletion can be approved (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        if deletion.requester_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "A manager cannot approve their own deletion request; another manager must approve it",
            ));
        }
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        deletion.status = DeletionStatus::Approved;
        deletion.approved_by = Some(caller.actor.clone());
        deletion.approved_at = Some(now);
        deletion.decided_by = Some(caller.actor.clone());
        deletion.decision_reason = reason.clone();
        deletion.updated_at = now;
        state.deletions.insert(deletion.id, deletion.clone());
        state.emit(
            "compliance.deletion.approved",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "approved_by": caller.actor,
                "reason": reason,
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn reject_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut deletion = load_deletion(&state, &path.into_inner())?;
        if deletion.status != DeletionStatus::Requested {
            return Err(UnifiedError::conflict(format!(
                "Only a requested deletion can be rejected (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        deletion.status = DeletionStatus::Rejected;
        deletion.decided_by = Some(caller.actor.clone());
        deletion.decision_reason = reason.clone();
        deletion.updated_at = Utc::now();
        state.deletions.insert(deletion.id, deletion.clone());
        state.emit(
            "compliance.deletion.rejected",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "rejected_by": caller.actor,
                "reason": reason,
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn execute_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut deletion = load_deletion(&state, &path.into_inner())?;
        if deletion.status != DeletionStatus::Approved {
            return Err(UnifiedError::conflict(format!(
                "Only an approved deletion can be executed (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        // Dispatch execution to the owning modules. This module
        // never deletes their rows itself; each owning module
        // deletes its own rows when it answers the event and
        // confirms back through `compliance.deletion.item.executed`
        // (or the manual mark-executed endpoint).
        let items: Vec<DeletionItem> = {
            let mut rows: Vec<DeletionItem> = state
                .deletion_items
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|item| item.deletion_request_id == deletion.id)
                .collect();
            rows.sort_by_key(|item| item.created_at);
            rows
        };
        if items.is_empty() {
            return Err(UnifiedError::conflict(
                "This deletion request has no items to execute",
            ));
        }
        let now = Utc::now();
        for item in &items {
            let mut updated = item.clone();
            updated.status = "dispatched".to_string();
            updated.updated_at = now;
            state.deletion_items.insert(updated.id, updated);
            state.emit(
                "compliance.deletion.item.execute_requested",
                json!({
                    "deletion_id": deletion.id.to_string(),
                    "item_id": item.id.to_string(),
                    "module": item.module_id,
                    "subject": item.subject,
                    "requester_id": deletion.requester_id,
                }),
            );
        }
        deletion.status = DeletionStatus::Executing;
        deletion.updated_at = now;
        state.deletions.insert(deletion.id, deletion.clone());
        state.emit(
            "compliance.deletion.executing",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "executed_by": caller.actor,
                "items_dispatched": items.len(),
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_deletion_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let deletion = load_deletion(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &deletion.requester_id, &caller.actor)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let module_filter = params.filters.get("module").cloned();
        let mut items: Vec<DeletionItem> = state
            .deletion_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                if item.deletion_request_id != deletion.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &item.status != status {
                        return false;
                    }
                }
                if let Some(module) = &module_filter {
                    if &item.module_id != module {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|item| item.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Items may only be confirmed through the manual endpoint while
/// the request is executing. Approval alone dispatches nothing to
/// the owning modules, so confirming an item of a merely approved
/// request would claim work nobody was asked to do.
///
/// This gate applies to the manual endpoint only. The hook
/// listener (`on_deletion_item_executed`) must not apply it:
/// owning modules may confirm synchronously while
/// `execute_deletion` is still dispatching, before the request
/// flips to `executing`.
pub(super) fn can_confirm_item(status: DeletionStatus) -> bool {
    status == DeletionStatus::Executing
}

pub(super) async fn mark_item_executed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<MarkItemExecutedRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeletionItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let (deletion_id, item_id) = path.into_inner();
        let deletion = load_deletion(&state, &deletion_id)?;
        if !can_confirm_item(deletion.status) {
            return Err(UnifiedError::conflict(format!(
                "Deletion items can only be confirmed while the request is executing (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        let item = load_deletion_item(&state, deletion.id, &item_id)?;
        if item.status == "executed" {
            return Err(UnifiedError::conflict("This deletion item is already executed"));
        }
        let detail = match &body.into_inner().detail {
            Some(raw) => Some(require_text(raw, "detail", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        Ok(confirm_item_executed(&state, &item, detail))
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn items_can_only_be_confirmed_while_executing() {
        // P4: approval alone dispatches nothing, so an item of a
        // merely approved request must not be confirmable.
        assert!(can_confirm_item(DeletionStatus::Executing));
        assert!(!can_confirm_item(DeletionStatus::Requested));
        assert!(!can_confirm_item(DeletionStatus::Approved));
        assert!(!can_confirm_item(DeletionStatus::Rejected));
        assert!(!can_confirm_item(DeletionStatus::Executed));
    }
}
