//! Compliance Center module — GDPR / ISO support: data export — Checklist handlers.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
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

pub(super) async fn list_checklists(
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
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ComplianceChecklist> = state
            .checklists
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                if let Some(status) = &status_filter {
                    if &item.status != status {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!(
                        "{} {}",
                        item.title,
                        item.description.clone().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&search)
                {
                    return false;
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

pub(super) async fn create_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateChecklistRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ComplianceChecklist> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let status = match &input.status {
            Some(raw) => {
                let checked = raw.trim().to_lowercase();
                if !valid_checklist_status(&checked) {
                    return Err(UnifiedError::bad_request(
                        "status must be \"open\", \"in_progress\", or \"done\"",
                    ));
                }
                checked
            }
            None => "open".to_string(),
        };
        let (target, key) = match (&input.evidence_storage_target, &input.evidence_storage_key) {
            (Some(target), Some(key)) => (
                Some(require_text(target, "evidence_storage_target", MAX_STORAGE_KEY_LEN)?),
                Some(require_text(key, "evidence_storage_key", MAX_STORAGE_KEY_LEN)?),
            ),
            (None, None) => (None, None),
            _ => {
                return Err(UnifiedError::bad_request(
                    "evidence_storage_target and evidence_storage_key must be given together (storage module reference)",
                ))
            }
        };
        let now = Utc::now();
        let item = ComplianceChecklist {
            id: Uuid::now_v7(),
            title,
            description,
            status,
            evidence_storage_target: target,
            evidence_storage_key: key,
            due_date: input.due_date,
            created_at: now,
            updated_at: now,
        };
        state.checklists.insert(item.id, item.clone());
        state.emit(
            "compliance.checklist.created",
            json!({ "checklist_id": item.id.to_string(), "title": item.title }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ComplianceChecklist> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        load_checklist(&state, &path.into_inner())
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateChecklistRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ComplianceChecklist> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut item = load_checklist(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            item.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            item.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(status) = &input.status {
            let checked = status.trim().to_lowercase();
            if !valid_checklist_status(&checked) {
                return Err(UnifiedError::bad_request(
                    "status must be \"open\", \"in_progress\", or \"done\"",
                ));
            }
            item.status = checked;
        }
        if let Some(target) = &input.evidence_storage_target {
            item.evidence_storage_target = match target {
                Some(raw) => Some(require_text(raw, "evidence_storage_target", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(key) = &input.evidence_storage_key {
            item.evidence_storage_key = match key {
                Some(raw) => Some(require_text(raw, "evidence_storage_key", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if item.evidence_storage_key.is_some() != item.evidence_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "evidence_storage_target and evidence_storage_key must be given together",
            ));
        }
        if let Some(due_date) = &input.due_date {
            item.due_date = due_date.clone();
        }
        item.updated_at = Utc::now();
        state.checklists.insert(item.id, item.clone());
        state.emit(
            "compliance.checklist.updated",
            json!({ "checklist_id": item.id.to_string(), "status": item.status }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_checklist(
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
        let item = load_checklist(&state, &path.into_inner())?;
        let id = item.id;
        state.checklists.remove(&id);
        Ok(json!({ "deleted": true, "checklist_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
