//! Logistics module — shipments, fleet, warehouses, delivery — Warehouse + stock movement handlers.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::hooks::*;
use super::module::*;
use super::rates::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_warehouses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let params = query.into_inner();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Warehouse> = state
            .warehouses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|warehouse| in_company_scope(&warehouse.company_id, &_scope))
            .filter(|warehouse| {
                if let Some(company_id) = &company_filter {
                    if warehouse.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() && !warehouse.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateWarehouseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Warehouse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let address = require_text(&input.address, "address", MAX_ADDRESS_LEN)?;
        if input.capacity < 0 {
            return Err(UnifiedError::bad_request("capacity must be >= 0"));
        }
        let threshold = input.default_low_stock_threshold.unwrap_or(0);
        if threshold < 0 {
            return Err(UnifiedError::bad_request(
                "default_low_stock_threshold must be >= 0",
            ));
        }
        let manager_user_id = optional_text(&input.manager_user_id, "manager_user_id", MAX_NAME_LEN)?;
        let company_id =
            parse_optional_company(&require_company_match(&_tenant, &req, &input.company_id)?)?;
        let now = Utc::now();
        let warehouse = Warehouse {
            id: Uuid::now_v7(),
            name,
            address,
            capacity: input.capacity,
            manager_user_id,
            default_low_stock_threshold: threshold,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.warehouses.insert(warehouse.id, warehouse.clone());
        state.emit(
            "logistics.warehouse.created",
            json!({
                "warehouse_id": warehouse.id.to_string(),
                "name": warehouse.name,
                "created_by": caller.actor,
            }),
        );
        Ok(warehouse)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Warehouse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let warehouse = load_warehouse(&state, &path)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        Ok(warehouse)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWarehouseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Warehouse> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut warehouse = load_warehouse(&state, &path)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        let input = body.into_inner();
        if let Some(name) = &input.name {
            warehouse.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(address) = &input.address {
            warehouse.address = require_text(address, "address", MAX_ADDRESS_LEN)?;
        }
        if let Some(capacity) = input.capacity {
            if capacity < 0 {
                return Err(UnifiedError::bad_request("capacity must be >= 0"));
            }
            warehouse.capacity = capacity;
        }
        if let Some(manager) = &input.manager_user_id {
            warehouse.manager_user_id = match manager.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "manager_user_id", MAX_NAME_LEN)?),
            };
        }
        if let Some(threshold) = input.default_low_stock_threshold {
            if threshold < 0 {
                return Err(UnifiedError::bad_request(
                    "default_low_stock_threshold must be >= 0",
                ));
            }
            warehouse.default_low_stock_threshold = threshold;
        }
        if let Some(company_id) = &input.company_id {
            warehouse.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => {
                    require_company_match_for(&_scope, &Some(raw.to_string()))?;
                    Some(parse_uuid(raw, "company")?)
                }
            };
        }
        warehouse.updated_at = Utc::now();
        state.warehouses.insert(warehouse.id, warehouse.clone());
        state.emit(
            "logistics.warehouse.updated",
            json!({
                "warehouse_id": warehouse.id.to_string(),
                "name": warehouse.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(warehouse)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_warehouse(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let warehouse = load_warehouse(&state, &path)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        if state
            .stock_movements
            .iter()
            .any(|entry| entry.value().warehouse_id == warehouse.id)
        {
            return Err(UnifiedError::conflict(
                "Warehouse has stock movements and cannot be deleted",
            ));
        }
        state.warehouses.remove(&warehouse.id);
        state.emit(
            "logistics.warehouse.deleted",
            json!({
                "warehouse_id": warehouse.id.to_string(),
                "name": warehouse.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "warehouse_id": warehouse.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_stock_movements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let warehouse = load_warehouse(&state, &path)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        let params = query.into_inner();
        let direction_filter = params.filters.get("direction").cloned();
        let asset_filter = params.filters.get("asset_id").cloned();
        let mut items: Vec<StockMovement> = state
            .stock_movements
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|movement| {
                if movement.warehouse_id != warehouse.id {
                    return false;
                }
                if let Some(direction) = &direction_filter {
                    if &movement.direction != direction {
                        return false;
                    }
                }
                if let Some(asset_id) = &asset_filter {
                    if movement.asset_id.to_string() != *asset_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_stock_movement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateStockMovementRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StockMovement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let warehouse = load_warehouse(&state, &path)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        let input = body.into_inner();
        let direction = match input.direction.as_str() {
            "in" | "out" => input.direction.clone(),
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown direction: {other} (expected in or out)"
                )));
            }
        };
        if input.quantity <= 0 {
            return Err(UnifiedError::bad_request("quantity must be > 0"));
        }
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        if let Some(shipment_id) = input.shipment_id {
            if !state.shipments.contains_key(&shipment_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Shipment not found: {shipment_id}"
                )));
            }
        }
        let on_hand = state.stock_on_hand(warehouse.id, input.asset_id);
        if direction == "out" && input.quantity > on_hand {
            return Err(UnifiedError::conflict(format!(
                "Cannot move out {} units: only {on_hand} on hand in this warehouse for this item",
                input.quantity
            )));
        }
        let quantity_after = if direction == "in" {
            on_hand + input.quantity
        } else {
            on_hand - input.quantity
        };
        let movement = StockMovement {
            id: Uuid::now_v7(),
            warehouse_id: warehouse.id,
            asset_id: input.asset_id,
            direction: direction.clone(),
            quantity: input.quantity,
            quantity_after,
            reason,
            shipment_id: input.shipment_id,
            actor: caller.actor.clone(),
            created_at: Utc::now(),
        };
        state.stock_movements.insert(movement.id, movement.clone());
        state.emit(
            "logistics.stock.moved",
            json!({
                "movement_id": movement.id.to_string(),
                "warehouse_id": warehouse.id.to_string(),
                "asset_id": input.asset_id.to_string(),
                "direction": direction,
                "quantity": input.quantity,
                "quantity_after": quantity_after,
                "actor": caller.actor,
            }),
        );
        // Low-stock alert: when the balance after the move is at or
        // below the threshold, notify the warehouse manager and
        // emit the low-stock event. The assets module owns the item
        // master data, so this alert carries ids only, no item
        // names or prices.
        let threshold = input
            .low_stock_threshold
            .unwrap_or(warehouse.default_low_stock_threshold);
        if quantity_after <= threshold {
            if let Some(manager) = &warehouse.manager_user_id {
                state.notify(
                    "logistics.stock.low",
                    manager,
                    "Low stock in warehouse",
                    &format!(
                        "Item {} in warehouse \"{}\" is at {quantity_after} units (threshold {threshold}).",
                        input.asset_id, warehouse.name
                    ),
                    Some(format!("/api/v1/logistics/warehouses/{}", warehouse.id)),
                );
            }
            state.emit(
                "logistics.stock.low",
                json!({
                    "warehouse_id": warehouse.id.to_string(),
                    "asset_id": input.asset_id.to_string(),
                    "quantity_after": quantity_after,
                    "threshold": threshold,
                }),
            );
        }
        Ok(movement)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_stock_movement(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StockMovement> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let (warehouse_raw, movement_raw) = path.into_inner();
        let warehouse = load_warehouse(&state, &warehouse_raw)?;
        require_record_in_company_scope(&warehouse.company_id, &_scope)?;
        let movement_id = parse_uuid(&movement_raw, "stock movement")?;
        state
            .stock_movements
            .get(&movement_id)
            .map(|entry| entry.value().clone())
            .filter(|movement| movement.warehouse_id == warehouse.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Stock movement not found: {movement_id}"))
            })
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
