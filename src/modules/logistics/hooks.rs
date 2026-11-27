//! Logistics module - shipments, fleet, warehouses, delivery - Hook handlers.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
use super::drivers::*;
use super::helpers::*;
use super::module::*;
use super::rates::*;
use super::requests::*;
use super::shipments::*;
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use unified_core::hooks::HookContext;
use unified_core::json;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// `order.paid` - no shipment is auto-created (a paid order may be
/// for digital goods). The handler only reports which linked
/// shipments exist for the order, so callers can see the linkage.
pub(super) fn on_order_paid(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "order.paid carries no order id",
        }));
        return;
    };
    let linked: Vec<String> = state
        .shipments
        .iter()
        .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
        .map(|entry| entry.value().tracking_number.clone())
        .collect();
    ctx.add_result(json!({
        "logistics": "seen",
        "order_id": order_id,
        "linked_shipments": linked,
    }));
}

/// `store.order.cancelled` - cancel every linked shipment still in
/// `created` status (reason "linked order cancelled") and notify
/// the shipment creator. Shipments already moving are left alone:
/// cancelling a truck already on the road is an operator decision,
/// not an automatic one.
pub(super) fn on_store_order_cancelled(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let order_id = ctx
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        })
        .or_else(|| {
            ctx.get("order")
                .and_then(|order| order.get("id"))
                .and_then(|id| id.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "store.order.cancelled carries no order id",
        }));
        return;
    };
    let mut cancelled: Vec<String> = Vec::new();
    let mut left_alone: Vec<String> = Vec::new();
    let linked: Vec<Shipment> = state
        .shipments
        .iter()
        .filter(|entry| entry.value().order_id.as_deref() == Some(order_id.as_str()))
        .map(|entry| entry.value().clone())
        .collect();
    for mut shipment in linked {
        if shipment.status == ShipmentStatus::Created {
            if apply_transition(
                &state,
                &mut shipment,
                ShipmentStatus::Cancelled,
                "system:store.order.cancelled",
                Some("linked order cancelled"),
                None,
                None,
            )
            .is_ok()
            {
                state.notify(
                    "logistics.shipment.cancelled",
                    &shipment.created_by,
                    "Shipment cancelled",
                    &format!(
                        "Shipment {} was cancelled because its linked order was cancelled.",
                        shipment.tracking_number
                    ),
                    Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
                );
                cancelled.push(shipment.tracking_number);
            }
        } else {
            left_alone.push(shipment.tracking_number);
        }
    }
    ctx.add_result(json!({
        "logistics": "handled",
        "order_id": order_id,
        "cancelled_shipments": cancelled,
        "left_alone": left_alone,
    }));
}

/// `assets.stock.low` - when the payload names a warehouse this
/// module knows (`warehouse_id`), notify that warehouse's manager
/// and emit `logistics.stock.low`. Payloads without a warehouse
/// this module knows are ignored (the assets module owns the item
/// master data; this module only alerts for its own warehouses).
pub(super) fn on_assets_stock_low(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let warehouse_id = ctx
        .get("warehouse_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(warehouse_id) = warehouse_id else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "assets.stock.low carries no warehouse_id",
        }));
        return;
    };
    let Some(warehouse) = state
        .warehouses
        .get(&warehouse_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "warehouse is not a logistics warehouse",
        }));
        return;
    };
    let asset_id = ctx
        .get("asset_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let quantity = ctx
        .get("quantity_on_hand")
        .and_then(Value::as_i64)
        .or_else(|| ctx.get("quantity_after").and_then(Value::as_i64));
    if let Some(manager) = &warehouse.manager_user_id {
        state.notify(
            "logistics.stock.low",
            manager,
            "Low stock in warehouse",
            &format!(
                "Item {asset_id} in warehouse \"{}\" is low (on hand: {}).",
                warehouse.name,
                quantity.map(|value| value.to_string()).unwrap_or_else(|| "unknown".to_string())
            ),
            Some(format!("/api/v1/logistics/warehouses/{}", warehouse.id)),
        );
    }
    state.emit(
        "logistics.stock.low",
        json!({
            "warehouse_id": warehouse.id.to_string(),
            "asset_id": asset_id,
            "quantity_after": quantity,
        }),
    );
    ctx.add_result(json!({
        "logistics": "handled",
        "warehouse_id": warehouse.id.to_string(),
        "asset_id": asset_id,
    }));
}

/// `logistics.maintenance.remind_requested` - the scheduler's
/// dispatch for a `logistics.maintenance.remind` job. Marks the
/// reminder due and notifies the assigned driver (or the vehicle
/// record's assignee when the driver row cannot be found).
pub(super) fn on_maintenance_remind_requested(state: Arc<LogisticsState>, ctx: &mut HookContext) {
    let vehicle_id = ctx
        .get("vehicle_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let reminder_id = ctx
        .get("reminder_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let (Some(vehicle_id), Some(reminder_id)) = (vehicle_id, reminder_id) else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "maintenance reminder payload is missing vehicle_id or reminder_id",
        }));
        return;
    };
    let Some(mut vehicle) = state
        .vehicles
        .get(&vehicle_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "vehicle not found for maintenance reminder",
        }));
        return;
    };
    let mut found = false;
    for reminder in vehicle.maintenance.iter_mut() {
        if reminder.id == reminder_id && reminder.status == "pending" {
            reminder.status = "due".to_string();
            reminder.updated_at = Utc::now();
            found = true;
        }
    }
    if !found {
        ctx.add_result(json!({
            "logistics": "skipped",
            "reason": "maintenance reminder not found or already handled",
        }));
        return;
    }
    state.vehicles.insert(vehicle.id, vehicle.clone());
    let due_description = vehicle
        .maintenance
        .iter()
        .find(|reminder| reminder.id == reminder_id)
        .map(|reminder| reminder.description.clone())
        .unwrap_or_default();
    if let Some(driver_id) = vehicle.assigned_driver_id {
        if let Some(driver) = state.drivers.get(&driver_id) {
            state.notify(
                "logistics.vehicle.maintenance_due",
                &driver.value().user_id,
                "Vehicle maintenance due",
                &format!(
                    "Maintenance is due for vehicle {}: {due_description}",
                    vehicle.plate
                ),
                Some(format!("/api/v1/logistics/vehicles/{}", vehicle.id)),
            );
        }
    }
    state.emit(
        "logistics.vehicle.maintenance_due",
        json!({
            "vehicle_id": vehicle.id.to_string(),
            "plate": vehicle.plate,
            "reminder_id": reminder_id.to_string(),
            "description": due_description,
        }),
    );
    ctx.add_result(json!({
        "logistics": "handled",
        "vehicle_id": vehicle.id.to_string(),
        "reminder_id": reminder_id.to_string(),
    }));
}
