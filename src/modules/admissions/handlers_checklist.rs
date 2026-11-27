// admissions module - Document checklist handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_TEXT, PERM_ADMISSIONS_APPLY, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ};
use super::types::{AttachDocument, ChecklistItem, RejectChecklistItem};
use super::state::AdmissionsState;
use super::helpers::{
    caller, caller_company, check_capability, load_application_for_read, matches_search, now,
    paginate, require_id, require_permission, require_tenant, require_text,
};

// Handlers: document checklist

pub(crate) async fn get_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ChecklistItem>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        state.ensure_checklist(&application);
        let params = query.into_inner();
        let items = state.checklist_of(&application.id);
        let items: Vec<ChecklistItem> = items
            .into_iter()
            .filter(|item| {
                matches_search(&format!("{} {}", item.document_key, item.title), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| item.status == *status)
                    && params
                        .filters
                        .get("required")
                        .is_none_or(|required| item.required.to_string() == *required)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn attach_checklist_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<AttachDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let (application_id, item_id) = path.into_inner();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may attach a document",
            ));
        }
        let input = body.into_inner();
        let document_id = require_id(&input.document_id, "document_id")?;
        let mut item = state
            .checklist
            .get(&item_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.application_id != application_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        if item.status == "verified" {
            return Err(UnifiedError::conflict(
                "Checklist item is already verified",
            ));
        }
        item.document_id = Some(document_id.clone());
        item.status = "submitted".to_string();
        item.rejection_reason = None;
        item.updated_at = now();
        state.checklist.insert(item.id.clone(), item.clone());
        state.emit(
            "admissions.document_attached",
            json!({
                "application_id": application.id,
                "checklist_item_id": item.id,
                "document_id": document_id,
            }),
        );
        Ok(item)
    })();
    json::respond(result, debug)
}

pub(crate) async fn verify_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (application_id, item_id) = path.into_inner();
        let mut item = state
            .checklist
            .get(&item_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.application_id != application_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        if item.status != "submitted" {
            return Err(UnifiedError::conflict(
                "Only a submitted checklist item can be verified",
            ));
        }
        item.status = "verified".to_string();
        item.verified_by = Some(caller.actor.clone());
        item.verified_at = Some(now());
        item.rejection_reason = None;
        item.updated_at = now();
        state.checklist.insert(item.id.clone(), item.clone());
        state.emit(
            "admissions.document_verified",
            json!({
                "application_id": application_id,
                "checklist_item_id": item.id,
            }),
        );
        Ok(item)
    })();
    json::respond(result, debug)
}

pub(crate) async fn reject_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<RejectChecklistItem>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (application_id, item_id) = path.into_inner();
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_TEXT)?;
        let mut item = state
            .checklist
            .get(&item_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.application_id != application_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        if item.status != "submitted" {
            return Err(UnifiedError::conflict(
                "Only a submitted checklist item can be rejected",
            ));
        }
        item.status = "rejected".to_string();
        item.rejection_reason = Some(reason);
        item.updated_at = now();
        state.checklist.insert(item.id.clone(), item.clone());
        Ok(item)
    })();
    json::respond(result, debug)
}
