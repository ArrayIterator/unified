//! HTTP handlers: onboarding checklists.

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
// Handlers: onboarding / offboarding checklists
// ---------------------------------------------------------------------------

pub(crate) async fn create_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateChecklist>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let employee_id = path.as_str().to_string();
        if !state.employees.contains_key(employee_id.as_str()) {
            return Err(UnifiedError::not_found("Employee not found"));
        }
        let input = body.into_inner();
        if !matches!(input.kind.as_str(), "onboarding" | "offboarding") {
            return Err(UnifiedError::bad_request(
                "kind must be one of: onboarding, offboarding",
            ));
        }
        if state.checklists.iter().any(|entry| {
            let checklist = entry.value();
            checklist.employee_id == employee_id
                && checklist.kind == input.kind
                && checklist.status != "completed"
        }) {
            return Err(UnifiedError::conflict(format!(
                "An open {} checklist already exists for this employee",
                input.kind
            )));
        }
        let checklist = Checklist {
            id: new_id(),
            employee_id: employee_id.clone(),
            kind: input.kind,
            status: "pending".to_string(),
            completed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .checklists
            .insert(checklist.id.clone(), checklist.clone());
        let mut items: Vec<ChecklistItem> = Vec::new();
        for title in input.items {
            let item = ChecklistItem {
                id: new_id(),
                checklist_id: checklist.id.clone(),
                title: require_text(&title, "items entry", 500)?,
                completed: false,
                completed_by: None,
                completed_at: None,
                created_at: now(),
                updated_at: now(),
            };
            state.checklist_items.insert(item.id.clone(), item.clone());
            items.push(item);
        }
        Ok(json!({
            "checklist": checklist,
            "items": items,
        }))
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn list_checklists_for_employee(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let employee = load_employee_for_read(&state, &ctx, &caller, path.as_str())?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .checklists
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|checklist| {
                checklist.employee_id == employee.id
                    && params
                        .filters
                        .get("kind")
                        .is_none_or(|kind| checklist.kind == *kind)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| checklist.status == *status)
            })
            .map(|checklist| {
                let items: Vec<ChecklistItem> = state
                    .checklist_items
                    .iter()
                    .map(|entry| entry.value().clone())
                    .filter(|item| item.checklist_id == checklist.id)
                    .collect();
                let done = items.iter().filter(|item| item.completed).count();
                json!({
                    "checklist": checklist,
                    "items": items,
                    "completed_items": done,
                    "total_items": items.len(),
                })
            })
            .collect();
        items.sort_by(|a, b| {
            b.get("checklist")
                .and_then(|v| v.get("created_at"))
                .and_then(|v| v.as_str())
                .cmp(
                    &a.get("checklist")
                        .and_then(|v| v.get("created_at"))
                        .and_then(|v| v.as_str()),
                )
        });
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

pub(crate) async fn get_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let checklist = state
            .checklists
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        let employee = state
            .employees
            .get(checklist.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        require_read_employee(&ctx, &caller, &employee)?;
        let mut items: Vec<ChecklistItem> = state
            .checklist_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.checklist_id == checklist.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let done = items.iter().filter(|item| item.completed).count();
        let total = items.len();
        Ok(json!({
            "checklist": checklist,
            "items": items,
            "completed_items": done,
            "total_items": total,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn add_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateChecklistItem>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let checklist_id = path.as_str().to_string();
        let checklist = state
            .checklists
            .get(checklist_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        if checklist.status == "completed" {
            return Err(UnifiedError::conflict(
                "Items cannot be added to a completed checklist",
            ));
        }
        let input = body.into_inner();
        let item = ChecklistItem {
            id: new_id(),
            checklist_id,
            title: require_text(&input.title, "title", 500)?,
            completed: false,
            completed_by: None,
            completed_at: None,
            created_at: now(),
            updated_at: now(),
        };
        state.checklist_items.insert(item.id.clone(), item.clone());
        if let Some(mut checklist) = state.checklists.get_mut(item.checklist_id.as_str()) {
            if checklist.status == "pending" {
                checklist.status = "in_progress".to_string();
                checklist.updated_at = now();
            }
        }
        Ok(item)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn complete_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_READ)?;
        let item_id = path.as_str().to_string();
        let item = state
            .checklist_items
            .get(item_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        let checklist = state
            .checklists
            .get(item.checklist_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        let employee = state
            .employees
            .get(checklist.employee_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
        // The employee themself, or HR with `hr:manage`.
        if employee.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        }
        if item.completed {
            return Err(UnifiedError::conflict("Checklist item is already completed"));
        }
        let mut item = state
            .checklist_items
            .get_mut(item_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        item.completed = true;
        item.completed_by = Some(caller.actor.clone());
        item.completed_at = Some(now());
        item.updated_at = now();
        let completed = item.clone();
        drop(item);
        if let Some(mut checklist) = state.checklists.get_mut(checklist.id.as_str()) {
            if checklist.status == "pending" {
                checklist.status = "in_progress".to_string();
                checklist.updated_at = now();
            }
        }
        Ok(completed)
    })();
    json::respond(result, debug)
}

pub(crate) async fn complete_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HrState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Checklist> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HR_MANAGE)?;
        let checklist_id = path.as_str().to_string();
        let checklist = state
            .checklists
            .get(checklist_id.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        if checklist.status == "completed" {
            return Err(UnifiedError::conflict("Checklist is already completed"));
        }
        let items: Vec<ChecklistItem> = state
            .checklist_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.checklist_id == checklist_id)
            .collect();
        if items.is_empty() {
            return Err(UnifiedError::conflict(
                "A checklist with no items cannot be completed",
            ));
        }
        if items.iter().any(|item| !item.completed) {
            return Err(UnifiedError::conflict(
                "All checklist items must be completed first",
            ));
        }
        let mut checklist = state
            .checklists
            .get_mut(checklist_id.as_str())
            .ok_or_else(|| UnifiedError::not_found("Checklist not found"))?;
        checklist.status = "completed".to_string();
        checklist.completed_at = Some(now());
        checklist.updated_at = now();
        let completed = checklist.clone();
        drop(checklist);
        if completed.kind == "onboarding" {
            state.emit(
                "hr.onboarding_completed",
                json!({
                    "checklist_id": completed.id,
                    "employee_id": completed.employee_id,
                }),
            );
        }
        Ok(completed)
    })();
    json::respond(result, debug)
}
