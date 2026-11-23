// admissions module — Required document handlers.
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

use super::constants::{MAX_SHORT, MAX_TEXT, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ};
use super::helpers::{
    caller, caller_company, matches_search, new_id, now, optional_text, paginate,
    require_permission, require_tenant, require_text, slugify,
};
use super::state::AdmissionsState;
use super::types::{CreateRequiredDocument, RequiredDocument, UpdateRequiredDocument};
use super::state::AdmissionsState;

// ---------------------------------------------------------------------------
// Handlers: required documents
// ---------------------------------------------------------------------------

pub(crate) async fn list_required_documents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<RequiredDocument>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let documents = state.required_documents_of(&period_id);
        let items: Vec<RequiredDocument> = documents
            .into_iter()
            .filter(|document| {
                matches_search(&format!("{} {}", document.document_key, document.title), &params)
                    && params
                        .filters
                        .get("required")
                        .is_none_or(|required| {
                            document.required.to_string() == *required
                        })
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_required_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRequiredDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RequiredDocument> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Required documents cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let document_key = slugify(&input.document_key);
        if document_key.is_empty() {
            return Err(UnifiedError::bad_request(
                "document_key must contain at least one letter or digit",
            ));
        }
        let title = require_text(&input.title, "title", MAX_SHORT)?;
        let description = optional_text(&input.description, "description", MAX_TEXT)?;
        if state.required_documents.iter().any(|entry| {
            let document = entry.value();
            document.period_id == period_id && document.document_key == document_key
        }) {
            return Err(UnifiedError::conflict(format!(
                "A required document with key \"{document_key}\" already exists in this period",
            )));
        }
        let document = RequiredDocument {
            id: new_id(),
            period_id,
            document_key,
            title,
            description,
            required: input.required.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .required_documents
            .insert(document.id.clone(), document.clone());
        Ok(document)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_required_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateRequiredDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RequiredDocument> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, document_id) = path.into_inner();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Required documents cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let mut document = state
            .required_documents
            .get(&document_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Required document not found"))?;
        if document.period_id != period_id {
            return Err(UnifiedError::not_found("Required document not found"));
        }
        if let Some(title) = input.title {
            document.title = require_text(&title, "title", MAX_SHORT)?;
        }
        if let Some(description) = input.description {
            document.description = match description {
                Some(value) => Some(require_text(&value, "description", MAX_TEXT)?),
                None => None,
            };
        }
        if let Some(required) = input.required {
            document.required = required;
        }
        document.updated_at = now();
        state
            .required_documents
            .insert(document.id.clone(), document.clone());
        Ok(document)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_required_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, document_id) = path.into_inner();
        let document = state
            .required_documents
            .get(&document_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Required document not found"))?;
        if document.period_id != period_id {
            return Err(UnifiedError::not_found("Required document not found"));
        }
        if state.checklist.iter().any(|entry| {
            entry.value().required_document_id == document_id
                && matches!(entry.value().status.as_str(), "submitted" | "verified")
        }) {
            return Err(UnifiedError::conflict(
                "Required document still has submitted or verified checklist items; keep it and mark it not required instead",
            ));
        }
        state
            .checklist
            .retain(|_, item| item.required_document_id != document_id);
        state
            .required_documents
            .remove(document_id.as_str())
            .map(|_| json!({ "deleted": true, "id": document_id }))
            .ok_or_else(|| UnifiedError::not_found("Required document not found"))
    })();
    json::respond(result, debug)
}
