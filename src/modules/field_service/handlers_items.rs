// field_service module — Work order checklist item handlers.
//
// Split from the former single-file module `src/modules/field_service.rs`.
// The module root (`field_service.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_CHECKLIST_ITEMS, MAX_NOTES_LEN, MAX_TITLE_LEN, PERM_FIELD_SERVICE_VIEW};
use super::types::WorkOrderItem;
use super::requests::{CreateItemRequest, UpdateItemRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, can_act_on_order, created, err, items_of, load_item, load_work_order,
    ok, optional_text, paginate, require_permission, require_tenant, require_text,
};
use super::types::WorkOrderItem;

// ---------------------------------------------------------------------------
// Work order checklist item handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_work_order_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        Ok(paginate(items_of(&state, order.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot gain checklist items",
            ));
        }
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let existing = items_of(&state, order.id);
        if existing.len() >= MAX_CHECKLIST_ITEMS {
            return Err(UnifiedError::bad_request(format!(
                "A work order holds at most {MAX_CHECKLIST_ITEMS} checklist items"
            )));
        }
        let position = input
            .position
            .unwrap_or_else(|| existing.iter().map(|item| item.position).max().unwrap_or(-1) + 1);
        if position < 0 || position > 10_000 {
            return Err(UnifiedError::bad_request(
                "position must be between 0 and 10000",
            ));
        }
        let now = Utc::now();
        let item = WorkOrderItem {
            id: Uuid::now_v7(),
            work_order_id: order.id,
            title,
            position,
            is_done: false,
            done_by: None,
            done_at: None,
            notes,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item.clone());
        state.emit(
            "field_service.work_order.item.created",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (order_id, item_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let mut item = load_item(&state, order.id, &item_id)?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            item.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(position) = input.position {
            if position < 0 || position > 10_000 {
                return Err(UnifiedError::bad_request(
                    "position must be between 0 and 10000",
                ));
            }
            item.position = position;
        }
        if let Some(notes) = input.notes {
            item.notes = match notes {
                Some(raw) => Some(require_text(&raw, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        if let Some(is_done) = input.is_done {
            if is_done && !item.is_done {
                item.is_done = true;
                item.done_by = Some(caller.actor.clone());
                item.done_at = Some(Utc::now());
            } else if !is_done {
                item.is_done = false;
                item.done_by = None;
                item.done_at = None;
            }
        }
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        state.emit(
            "field_service.work_order.item.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (order_id, item_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let item = load_item(&state, order.id, &item_id)?;
        state.items.remove(&item.id);
        state.emit(
            "field_service.work_order.item.deleted",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "item_id": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn complete_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderItem> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (order_id, item_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let mut item = load_item(&state, order.id, &item_id)?;
        if item.is_done {
            return Err(UnifiedError::conflict("Checklist item is already done"));
        }
        item.is_done = true;
        item.done_by = Some(caller.actor.clone());
        item.done_at = Some(Utc::now());
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        state.emit(
            "field_service.work_order.item.completed",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "done_by": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}
