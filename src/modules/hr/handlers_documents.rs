//! HTTP handlers: employee documents.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Handlers: employee documents (metadata only)
// ---------------------------------------------------------------------------

pub(crate) async fn list_documents_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<EmployeeDocument>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<EmployeeDocument> = state
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| {
                document.employee_id == employee.id
                    && params
                        .filters
                        .get("document_type")
                        .is_none_or(|kind| document.document_type == *kind)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateEmployeeDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EmployeeDocument> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee_id = path.as_str().to_string();
        let employee = state
            .employees
            .get(employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        // The employee themself, or HR with `hr:manage`.
        if employee.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        let input = body.into_inner();
        let document_type = match input.document_type.as_deref() {
            Some(kind)
                if matches!(
                    kind,
                    "contract" | "id" | "passport" | "certificate" | "other"
                ) =>
            {
                kind.to_string()
            }
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "document_type must be one of: contract, id, passport, certificate, other",
                ))
            }
            None => "other".to_string(),
        };
        let storage_file_id = require_text(&input.storage_file_id, "storage_file_id", 255)?;
        // The file itself lives in Storage. Ask Storage whether the
        // file exists; a definite "no" rejects the metadata, while
        // no answer is "unknown" — the metadata is kept but flagged
        // unverified (`storage_verified: false`), never treated as
        // confirmed to exist.
        let storage_verified = match state.storage_file_status(&storage_file_id) {
            Some(false) => {
                return Err(UnifiedError::not_found(format!(
                    "Storage file {storage_file_id} not found"
                )));
            }
            Some(true) => true,
            None => false,
        };
        if let Some(size) = input.size_bytes {
            if size < 0 {
                return Err(UnifiedError::bad_request("size_bytes must not be negative"));
            }
        }
        let document = EmployeeDocument {
            id: new_id(),
            employee_id,
            title: require_text(&input.title, "title", 255)?,
            document_type,
            storage_file_id,
            storage_verified,
            mime_type: optional_text(&input.mime_type, "mime_type", 128)?,
            size_bytes: input.size_bytes,
            uploaded_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        state.documents.insert(document.id.clone(), document.clone());
        Ok(document)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EmployeeDocument> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let document = state
            .documents
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Document not found"))?;
        let employee = state
            .employees
            .get(document.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        Ok(document)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let document_id = path.as_str().to_string();
        state
            .documents
            .remove(document_id.as_str())
            .map(|_| json!({ "deleted": true, "id": document_id }))
            .ok_or_else(|| UnifiedError::not_found("Document not found"))
    })();
    json::respond(result, debug)
}
