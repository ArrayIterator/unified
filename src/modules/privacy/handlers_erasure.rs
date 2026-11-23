// Handlers: right-to-erasure requests.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::json;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: right-to-erasure requests
// ---------------------------------------------------------------------------

pub(crate) async fn create_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    body: web::Json<CreateErasureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 256)?;
        // Self-service: the subject user may request their own
        // erasure; anyone else needs privacy:write.
        if caller.actor != user_id {
            require_permission(&ctx, &caller.role, PERM_PRIVACY_WRITE)?;
        }
        let modules = validate_module_list(&input.modules)?;
        let reason = require_text(&input.reason, "reason", 1000)?;
        let record = ErasureRequest {
            id: new_id(),
            user_id: user_id.clone(),
            modules,
            reason,
            status: "pending".to_string(),
            requested_by: caller.actor.clone(),
            reviewed_by: None,
            reviewed_at: None,
            rejection_reason: None,
            fulfilled_at: None,
            anonymization_id: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.emit(
            EVENT_COMPLIANCE_ERASURE_REQUESTED,
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
                "actor_id": caller.actor,
                "reason": record.reason,
            }),
        );
        state.audit(
            EVENT_PRIVACY_ERASURE_REQUESTED,
            &caller.actor,
            "privacy.erasure.requested",
            &format!("user:{user_id}"),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_erasure_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ErasureRequest>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        let params = query.into_inner();
        let mut items: Vec<ErasureRequest> = state
            .erasure_requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&format!("{} {}", record.user_id, record.status), &params)
                    && params
                        .filters
                        .get("user_id")
                        .is_none_or(|user_id| record.user_id == *user_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| record.status == *status)
            })
            .collect();
        sort_desc_by_created(&mut items, |record| record.created_at);
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_READ)?;
        state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))
    })();
    json::respond(result, debug)
}

pub(crate) async fn approve_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let mut record = state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))?;
        if record.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending erasure request can be approved (current status: {})",
                record.status
            )));
        }
        record.status = "approved".to_string();
        record.reviewed_by = Some(caller.actor.clone());
        record.reviewed_at = Some(now());
        record.updated_at = now();
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.audit(
            "privacy.erasure.approved",
            &caller.actor,
            "privacy.erasure.approved",
            &format!("user:{}", record.user_id),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "modules": record.modules,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}

pub(crate) async fn fulfill_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        // The load-check-anonymize-mark-fulfilled transition runs
        // under a per-request guard in
        // `PrivacyState::fulfill_erasure_request`, so a concurrent
        // second fulfill gets a 409 conflict instead of running
        // anonymization a second time.
        state.fulfill_erasure_request(path.as_str(), &caller.actor)
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_erasure_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PrivacyState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RejectErasureInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ErasureRequest> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PRIVACY_ADMIN)?;
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", 1000)?;
        let mut record = state
            .erasure_requests
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Erasure request not found"))?;
        if record.status != "pending" && record.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending or approved erasure request can be rejected (current status: {})",
                record.status
            )));
        }
        record.status = "rejected".to_string();
        record.reviewed_by = Some(caller.actor.clone());
        record.reviewed_at = Some(now());
        record.rejection_reason = Some(reason);
        record.updated_at = now();
        state
            .erasure_requests
            .insert(record.id.clone(), record.clone());
        state.audit(
            "privacy.erasure.rejected",
            &caller.actor,
            "privacy.erasure.rejected",
            &format!("user:{}", record.user_id),
            json!({
                "user_id": record.user_id,
                "erasure_request_id": record.id,
                "rejection_reason": record.rejection_reason,
            }),
        );
        Ok(record)
    })();
    json::respond(result, debug)
}
