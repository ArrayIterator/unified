// field_service module — Work order part handlers.
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

use super::constants::{MAX_NAME_LEN, MAX_PARTS_PER_ORDER, MAX_UNIT_LEN, PERM_FIELD_SERVICE_VIEW};
use super::types::WorkOrderPart;
use super::requests::{CreatePartRequest, UpdatePartRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, can_act_on_order, created, err, load_part, load_work_order, ok,
    paginate, parts_of, require_permission, require_tenant, require_text,
};

// ---------------------------------------------------------------------------
// Work order part handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_work_order_parts(
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
        Ok(paginate(parts_of(&state, order.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn validate_part_line(input: &CreatePartRequest) -> UnifiedResult<(String, i64, String)> {
    let part_name = require_text(&input.part_name, "part_name", MAX_NAME_LEN)?;
    let quantity = input.quantity.unwrap_or(1);
    if quantity < 1 || quantity > 100_000 {
        return Err(UnifiedError::bad_request(
            "quantity must be between 1 and 100000",
        ));
    }
    let unit = match &input.unit {
        Some(raw) => require_text(raw, "unit", MAX_UNIT_LEN)?,
        None => "unit".to_string(),
    };
    Ok((part_name, quantity, unit))
}

pub(crate) async fn add_work_order_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePartRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderPart> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot gain parts",
            ));
        }
        if parts_of(&state, order.id).len() >= MAX_PARTS_PER_ORDER {
            return Err(UnifiedError::bad_request(format!(
                "A work order holds at most {MAX_PARTS_PER_ORDER} parts"
            )));
        }
        let input = body.into_inner();
        let (part_name, quantity, unit) = validate_part_line(&input)?;
        let now = Utc::now();
        let part = WorkOrderPart {
            id: Uuid::now_v7(),
            work_order_id: order.id,
            stock_id: input.stock_id,
            part_name,
            quantity,
            unit,
            status: "required".to_string(),
            consumed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.parts.insert(part.id, part.clone());
        state.emit(
            "field_service.work_order.part.created",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(part)
    })();
    match result {
        Ok(part) => created(part),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_work_order_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdatePartRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderPart> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        let (order_id, part_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let mut part = load_part(&state, order.id, &part_id)?;
        if part.status == "consumption_requested" {
            return Err(UnifiedError::conflict(
                "A part whose consumption was requested cannot be edited",
            ));
        }
        let input = body.into_inner();
        if let Some(part_name) = input.part_name {
            part.part_name = require_text(&part_name, "part_name", MAX_NAME_LEN)?;
        }
        if let Some(quantity) = input.quantity {
            if quantity < 1 || quantity > 100_000 {
                return Err(UnifiedError::bad_request(
                    "quantity must be between 1 and 100000",
                ));
            }
            part.quantity = quantity;
        }
        if let Some(unit) = input.unit {
            part.unit = require_text(&unit, "unit", MAX_UNIT_LEN)?;
        }
        if let Some(status) = input.status {
            match status.trim() {
                "required" | "reserved" | "returned" => part.status = status.trim().to_string(),
                "consumption_requested" => {
                    return Err(UnifiedError::bad_request(
                        "Use POST /consume to request consumption of a part",
                    ));
                }
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown part status: {other}"
                    )));
                }
            }
        }
        part.updated_at = Utc::now();
        state.parts.insert(part.id, part.clone());
        state.emit(
            "field_service.work_order.part.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(part)
    })();
    match result {
        Ok(part) => ok(part),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_work_order_part(
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
        let (order_id, part_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let part = load_part(&state, order.id, &part_id)?;
        if part.status == "consumption_requested" {
            return Err(UnifiedError::conflict(
                "A part whose consumption was requested cannot be deleted",
            ));
        }
        state.parts.remove(&part.id);
        state.emit(
            "field_service.work_order.part.deleted",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "part_id": part.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Request stock consumption for a part. This module never touches
/// stock levels itself: it emits `assets.stock.adjust_requested`
/// with a negative delta and records the part as
/// `consumption_requested`. The assets module has no listener for
/// that event yet, so the answer is reported honestly as
/// `stock_handler_answered`.
pub(crate) async fn consume_work_order_part(
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
        let (order_id, part_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot consume parts",
            ));
        }
        let mut part = load_part(&state, order.id, &part_id)?;
        if part.status == "consumption_requested" {
            return Err(UnifiedError::conflict(
                "Consumption was already requested for this part",
            ));
        }
        let stock_answered = match part.stock_id {
            Some(stock_id) => state.emit_request(
                "assets.stock.adjust_requested",
                json!({
                    "stock_id": stock_id.to_string(),
                    "asset_id": null,
                    "delta": -part.quantity,
                    "reason": format!(
                        "Consumed for field service work order {} (part \"{}\")",
                        order.id, part.part_name
                    ),
                    "requested_by": caller.actor,
                    "work_order_id": order.id.to_string(),
                    "part_id": part.id.to_string(),
                }),
            ),
            None => false,
        };
        part.status = "consumption_requested".to_string();
        part.consumed_at = Some(Utc::now());
        part.updated_at = Utc::now();
        state.parts.insert(part.id, part.clone());
        state.emit(
            "field_service.work_order.part.consumed",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "stock_id": part.stock_id.map(|id| id.to_string()),
                "quantity": part.quantity,
                "stock_handler_answered": stock_answered,
                "consumed_by": caller.actor,
            }),
        );
        Ok(json!({
            "part": part,
            "stock_adjustment_requested": part.stock_id.is_some(),
            "stock_handler_answered": stock_answered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
