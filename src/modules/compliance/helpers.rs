//! Compliance Center module — GDPR / ISO support: data export — Helpers.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::state::*;
use super::types::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use uuid::Uuid;

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
pub(super) struct Caller {
    pub(super) actor: String,
    pub(super) role: String,
}

pub(super) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
pub(super) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(super) fn require_permission(
    ctx: &AppContext,
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, task 316).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before caller identity or any capability
/// check.
///
/// The records in this module carry no company field, so the guard
/// is a fail-closed gate only: real per-record partitioning needs a
/// `tenant_id` on the record (C11 follow-up), not a filter here.
pub(super) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(super) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Fail closed (403) when no company resolves for a mutating
/// handler: the tenant context's company first, the verified
/// caller's server-resolved company otherwise. An unresolved caller
/// writes nothing (the shared `CallerScope::Unresolved` rule) — the
/// records in this module carry no company field to stamp, so there
/// is no global-row write to fall back to.
pub(super) fn require_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?.ok_or_else(|| {
        UnifiedError::forbidden(
            "No company scope resolved for this request: writes require a resolved company",
        )
    })
}

pub(super) fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_COMPLIANCE_MANAGE)
}

pub(super) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(super) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(super) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(super) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(super) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(super) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(super) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(super) fn load_export(state: &ComplianceState, raw_id: &str) -> UnifiedResult<ExportRequest> {
    let id = parse_uuid(raw_id, "export request")?;
    state
        .exports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Export request not found: {id}")))
}

pub(super) fn load_deletion(
    state: &ComplianceState,
    raw_id: &str,
) -> UnifiedResult<DeletionRequest> {
    let id = parse_uuid(raw_id, "deletion request")?;
    state
        .deletions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Deletion request not found: {id}")))
}

pub(super) fn load_deletion_item(
    state: &ComplianceState,
    deletion_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<DeletionItem> {
    let id = parse_uuid(raw_id, "deletion item")?;
    state
        .deletion_items
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|item| item.deletion_request_id == deletion_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Deletion item not found: {id}")))
}

pub(super) fn load_consent(state: &ComplianceState, raw_id: &str) -> UnifiedResult<ConsentLog> {
    let id = parse_uuid(raw_id, "consent record")?;
    state
        .consents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Consent record not found: {id}")))
}

pub(super) fn load_policy(state: &ComplianceState, raw_id: &str) -> UnifiedResult<RetentionPolicy> {
    let id = parse_uuid(raw_id, "retention policy")?;
    state
        .retention_policies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Retention policy not found: {id}")))
}

pub(super) fn load_processing_record(
    state: &ComplianceState,
    raw_id: &str,
) -> UnifiedResult<ProcessingRecord> {
    let id = parse_uuid(raw_id, "processing record")?;
    state
        .processing_records
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Processing record not found: {id}")))
}

pub(super) fn load_checklist(
    state: &ComplianceState,
    raw_id: &str,
) -> UnifiedResult<ComplianceChecklist> {
    let id = parse_uuid(raw_id, "checklist")?;
    state
        .checklists
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Checklist not found: {id}")))
}

pub(super) fn load_breach(state: &ComplianceState, raw_id: &str) -> UnifiedResult<BreachLog> {
    let id = parse_uuid(raw_id, "breach")?;
    state
        .breaches
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Breach not found: {id}")))
}

/// Effective export status at read time: a `ready` export whose
/// download link expired reads as `expired` (the stored status is
/// unchanged until the next write).
pub(super) fn export_view(export: &ExportRequest, now: DateTime<Utc>) -> Value {
    let mut value = serde_json::to_value(export).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let effective = match export.status {
            ExportStatus::Ready => match export.download_expires_at {
                Some(expires_at) if now >= expires_at => ExportStatus::Expired,
                _ => ExportStatus::Ready,
            },
            other => other,
        };
        map.insert(
            "status".to_string(),
            Value::String(effective.as_str().to_string()),
        );
    }
    value
}

/// A requester may only see their own export / deletion requests
/// unless they hold the manage capability.
pub(super) fn check_own_or_manage(
    ctx: &AppContext,
    role: &str,
    owner_id: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if owner_id == actor || can_manage(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Request not found"))
    }
}

pub(super) fn valid_checklist_status(status: &str) -> bool {
    matches!(status, "open" | "in_progress" | "done")
}

pub(super) fn valid_breach_status(status: &str) -> bool {
    matches!(
        status,
        "reported" | "investigating" | "contained" | "notified" | "closed"
    )
}

pub(super) fn valid_notification_status(status: &str) -> bool {
    matches!(status, "not_started" | "in_progress" | "notified")
}

/// Mark every item of a deletion request `executed` when all of
/// them are confirmed, and move the request itself to `executed`.
/// Called after any single item is confirmed.
pub(super) fn maybe_finish_deletion(state: &ComplianceState, deletion_id: Uuid) {
    let items: Vec<DeletionItem> = state
        .deletion_items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.deletion_request_id == deletion_id)
        .collect();
    if items.is_empty() {
        return;
    }
    if !items.iter().all(|item| item.status == "executed") {
        return;
    }
    let Some(mut request) = state
        .deletions
        .get_mut(&deletion_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if request.status == DeletionStatus::Executed {
        return;
    }
    let now = Utc::now();
    request.status = DeletionStatus::Executed;
    request.executed_at = Some(now);
    request.updated_at = now;
    state.deletions.insert(request.id, request.clone());
    state.emit(
        "compliance.deletion.executed",
        json!({
            "deletion_id": request.id.to_string(),
            "requester_id": request.requester_id,
            "executed_at": now.to_rfc3339(),
            "items": items.iter().map(|item| json!({
                "item_id": item.id.to_string(),
                "module": item.module_id,
                "subject": item.subject,
                "detail": item.detail,
            })).collect::<Vec<_>>(),
        }),
    );
}

/// Confirm one deletion item as executed (shared by the hook
/// listener and the manual mark-executed endpoint).
///
/// This only writes the row (and finishes the request when every
/// item is done). It deliberately does NOT emit
/// `compliance.deletion.item.executed`: that event is the inbound
/// confirmation channel from the owning modules, and re-emitting
/// it here would re-enter this module's own listener
/// (`on_deletion_item_executed`). Completion of the whole request
/// is still broadcast by `maybe_finish_deletion` through
/// `compliance.deletion.executed`.
pub(super) fn confirm_item_executed(
    state: &ComplianceState,
    item: &DeletionItem,
    detail: Option<String>,
) -> DeletionItem {
    let now = Utc::now();
    let mut updated = item.clone();
    updated.status = "executed".to_string();
    if detail.is_some() {
        updated.detail = detail;
    }
    updated.executed_at = Some(now);
    updated.updated_at = now;
    state.deletion_items.insert(updated.id, updated.clone());
    maybe_finish_deletion(state, updated.deletion_request_id);
    updated
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::{HookBus, HookContext};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };

    #[test]
    fn checklist_breach_and_notification_statuses() {
        assert!(valid_checklist_status("open"));
        assert!(valid_checklist_status("in_progress"));
        assert!(valid_checklist_status("done"));
        assert!(!valid_checklist_status("closed"));
        assert!(!valid_checklist_status(""));
        assert!(valid_breach_status("reported"));
        assert!(valid_breach_status("investigating"));
        assert!(valid_breach_status("contained"));
        assert!(valid_breach_status("notified"));
        assert!(valid_breach_status("closed"));
        assert!(!valid_breach_status("open"));
        assert!(valid_notification_status("not_started"));
        assert!(valid_notification_status("in_progress"));
        assert!(valid_notification_status("notified"));
        assert!(!valid_notification_status("done"));
    }

    #[test]
    fn opt_string_reads_string_values_only() {
        let data = json!({"name": "ada", "age": 36, "nick": null});
        assert_eq!(opt_string(&data, "name").as_deref(), Some("ada"));
        assert_eq!(opt_string(&data, "missing"), None);
        assert_eq!(opt_string(&data, "age"), None);
        assert_eq!(opt_string(&data, "nick"), None);
    }

    #[test]
    fn export_view_expires_ready_downloads() {
        let now = Utc::now();
        let past = now - Duration::hours(1);
        let future = now + Duration::hours(1);
        let base = ExportRequest {
            id: Uuid::new_v4(),
            requester_id: "user-1".to_string(),
            scope: vec![],
            status: ExportStatus::Ready,
            storage_target: None,
            storage_key: None,
            download_expires_at: Some(past),
            failure_reason: None,
            created_at: now,
            updated_at: now,
        };
        assert_eq!(export_view(&base, now)["status"].as_str(), Some("expired"));
        let unexpired = ExportRequest {
            download_expires_at: Some(future),
            ..base.clone()
        };
        assert_eq!(
            export_view(&unexpired, now)["status"].as_str(),
            Some("ready")
        );
        let failed = ExportRequest {
            status: ExportStatus::Failed,
            download_expires_at: Some(past),
            ..base
        };
        assert_eq!(export_view(&failed, now)["status"].as_str(), Some("failed"));
    }

    #[test]
    fn paginate_slices_and_reports_totals() {
        let params = PaginationParams {
            page: Some(2),
            per_page: Some(2),
            ..Default::default()
        };
        let page = paginate(vec![1, 2, 3, 4, 5], &params);
        assert_eq!(page.items, vec![3, 4]);
        assert_eq!(page.pagination.total, 5);
        assert_eq!(page.pagination.total_pages, 3);
    }

    #[test]
    fn confirm_item_executed_does_not_emit_the_confirmation_event() {
        // P3: confirming an item must not re-emit
        // `compliance.deletion.item.executed`. The event is the
        // inbound confirmation channel from the owning modules;
        // emitting it here would re-enter the module's own
        // listener. Completion of the whole request is still
        // broadcast through `compliance.deletion.executed`.
        let bus = Arc::new(HookBus::new());
        let state = ComplianceState::default();
        *state.hooks.write().expect("hooks lock poisoned") = Some(Arc::clone(&bus));

        let item_deliveries = Arc::new(AtomicUsize::new(0));
        let counted_items = Arc::clone(&item_deliveries);
        bus.register(
            DELETION_ITEM_EXECUTED,
            "test-counter",
            Arc::new(move |_ctx: &mut HookContext| {
                counted_items.fetch_add(1, Ordering::SeqCst);
            }),
        );
        let request_deliveries = Arc::new(AtomicUsize::new(0));
        let counted_requests = Arc::clone(&request_deliveries);
        bus.register(
            "compliance.deletion.executed",
            "test-counter",
            Arc::new(move |_ctx: &mut HookContext| {
                counted_requests.fetch_add(1, Ordering::SeqCst);
            }),
        );

        let now = Utc::now();
        let deletion = DeletionRequest {
            id: Uuid::new_v4(),
            requester_id: "user-1".to_string(),
            reason: "leave".to_string(),
            status: DeletionStatus::Executing,
            approved_by: Some("manager-9".to_string()),
            approved_at: Some(now),
            decided_by: Some("manager-9".to_string()),
            decision_reason: Some("approved".to_string()),
            executed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.deletions.insert(deletion.id, deletion.clone());
        let item = DeletionItem {
            id: Uuid::new_v4(),
            deletion_request_id: deletion.id,
            module_id: "community".to_string(),
            subject: "all posts".to_string(),
            status: "dispatched".to_string(),
            detail: None,
            executed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.deletion_items.insert(item.id, item.clone());

        let updated = confirm_item_executed(&state, &item, Some("3 posts".to_string()));
        assert_eq!(updated.status, "executed");
        assert_eq!(updated.detail.as_deref(), Some("3 posts"));
        assert!(updated.executed_at.is_some());
        let stored_request = state
            .deletions
            .get(&deletion.id)
            .expect("request row")
            .value()
            .clone();
        assert_eq!(stored_request.status, DeletionStatus::Executed);
        // No re-emit of the per-item event, but the request-level
        // completion event still went out exactly once.
        assert_eq!(item_deliveries.load(Ordering::SeqCst), 0);
        assert_eq!(request_deliveries.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(tenant.company_id.as_deref(), Some("company-1"));
        let company = caller_company(&tenant, &req).expect("company must resolve");
        assert_eq!(company.as_deref(), Some("company-1"));
        assert_eq!(
            require_company(&tenant, &req).expect("company must resolve"),
            "company-1"
        );
    }

    #[test]
    fn require_company_fails_closed_without_a_resolved_company() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: String::new(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(tenant.company_id.is_none());
        assert!(
            caller_company(&tenant, &req)
                .expect("mismatch check must pass")
                .is_none()
        );
        assert!(require_company(&tenant, &req).is_err());
    }

    #[test]
    fn caller_company_refuses_a_verified_company_mismatch() {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "company-1".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company("company-2".to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
