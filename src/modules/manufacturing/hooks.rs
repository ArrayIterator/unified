//! manufacturing - hooks.

use unified_core::hooks::HookContext;
use serde_json::{json, Value};
use super::{constants::*, types::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



// Hook listeners
/// Record a stock confirmation from the inventory side and mark
/// the matching reservations confirmed.
pub(crate) fn on_stock_confirmed(state: ManufacturingState, ctx: &mut HookContext) {
    let reference = ctx
        .get("reference_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let work_order_id = ctx
        .get("work_order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reservation_id = ctx
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let component_product_id = ctx
        .get("component_product_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let quantity = ctx.get("quantity").and_then(|v| v.as_f64());
    let status = ctx
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("confirmed")
        .to_string();
    let notice = StockNotice {
        id: new_id(),
        kind: "confirmed".to_string(),
        work_order_id: work_order_id.clone(),
        reservation_id: reservation_id.clone(),
        component_product_id: component_product_id.clone(),
        quantity,
        message: ctx.get("message").and_then(|v| v.as_str()).map(str::to_string),
        received_at: now(),
    };
    state.stock_notices.insert(notice.id.clone(), notice);
    if status == "confirmed" {
        if let Some(reservation_id) = reservation_id {
            if let Some(mut reservation) = state.reservations.get_mut(&reservation_id) {
                reservation.status = "reserved".to_string();
                reservation.confirmation_reference = if reference.is_empty() {
                    None
                } else {
                    Some(reference)
                };
                reservation.stock_unknown = false;
                reservation.updated_at = now();
            }
        } else if let Some(work_order_id) = work_order_id {
            for mut entry in state.reservations.iter_mut() {
                let reservation = entry.value_mut();
                if reservation.work_order_id == work_order_id
                    && reservation.status == "pending_confirmation"
                {
                    reservation.status = "reserved".to_string();
                    reservation.stock_unknown = false;
                    reservation.updated_at = now();
                }
            }
        }
    }
}


/// Record a stock shortage notice from the inventory side and
/// mark the matching reservations short.
pub(crate) fn on_stock_short(state: ManufacturingState, ctx: &mut HookContext) {
    let work_order_id = ctx
        .get("work_order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reservation_id = ctx
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let component_product_id = ctx
        .get("component_product_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let quantity = ctx
        .get("quantity_short")
        .or_else(|| ctx.get("short_quantity"))
        .and_then(|v| v.as_f64());
    let notice = StockNotice {
        id: new_id(),
        kind: "short".to_string(),
        work_order_id: work_order_id.clone(),
        reservation_id: reservation_id.clone(),
        component_product_id: component_product_id.clone(),
        quantity,
        message: ctx.get("message").and_then(|v| v.as_str()).map(str::to_string),
        received_at: now(),
    };
    state.stock_notices.insert(notice.id.clone(), notice);
    if let Some(reservation_id) = reservation_id {
        if let Some(mut reservation) = state.reservations.get_mut(&reservation_id) {
            reservation.status = "short".to_string();
            reservation.stock_unknown = false;
            reservation.updated_at = now();
        }
    }
    if let (Some(work_order_id), Some(product_id), Some(short)) =
        (work_order_id, component_product_id, quantity)
    {
        if let Some(mut order) = state.work_orders.get_mut(&work_order_id) {
            for req in order.requirements.iter_mut() {
                if req.component_product_id == product_id {
                    req.quantity_short = short;
                }
            }
            order.updated_at = now();
        }
    }
}


/// Record a quality inspection outcome on the work order.
///
/// Listens for the canonical `quality.inspection.completed`
/// (payload: `{inspection_id, point_id, target_ref, target_kind,
/// result, inspector_id, blocked, alert_id?}` where `target_ref`
/// is the work order id and `target_kind` is `work_order`).
/// Inspections for other target kinds are ignored - they belong
/// to another module's record. A `work_order_id` key is still
/// accepted as a fallback for the target, for emitters that send
/// the old shape. When the completion carries `blocked: true`
/// the block is named in the inspection notes, so the work order
/// shows why it must not proceed until the alert is resolved.
pub(crate) fn on_inspection_completed(state: ManufacturingState, ctx: &mut HookContext) {
    if let Some(target_kind) = ctx.get("target_kind").and_then(|v| v.as_str()) {
        if target_kind != QUALITY_TARGET_KIND_WORK_ORDER {
            return;
        }
    }
    let Some(work_order_id) = ctx
        .get("target_ref")
        .and_then(|v| v.as_str())
        .or_else(|| ctx.get("work_order_id").and_then(|v| v.as_str()))
    else {
        return;
    };
    let outcome = ctx
        .get("result")
        .or_else(|| ctx.get("outcome"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let blocked = ctx
        .get("blocked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let alert_id = ctx.get("alert_id").and_then(|v| v.as_str());
    let notes = ctx.get("notes").and_then(|v| v.as_str()).map(str::to_string);
    if let Some(mut order) = state.work_orders.get_mut(work_order_id) {
        order.inspection_status = Some(outcome);
        order.inspection_notes = match (notes, blocked, alert_id) {
            (Some(notes), true, Some(alert_id)) => Some(format!(
                "{notes} Blocked by quality alert {alert_id} until the alert is resolved."
            )),
            (Some(notes), true, None) => Some(format!(
                "{notes} Blocked by a quality alert until the alert is resolved."
            )),
            (Some(notes), false, _) => Some(notes),
            (None, true, Some(alert_id)) => Some(format!(
                "Blocked by quality alert {alert_id} until the alert is resolved."
            )),
            (None, true, None) => {
                Some("Blocked by a quality alert until the alert is resolved.".to_string())
            }
            (None, false, _) => order.inspection_notes.clone(),
        };
        order.updated_at = now();
    }
}


/// Ask the quality module which blocking alerts cover a target
/// (`quality.blocks.query`, the hook form of quality's
/// `GET /blocks`). Returns the blocking alerts as JSON values
/// (each carrying at least `id`, `target_ref`, `target_kind`,
/// `message`). An empty list means either no block exists or the
/// quality module is not registered - in the latter case there is
/// no owner of blocks at all, so there is nothing to refuse on.
pub(crate) fn quality_blocks_for(
    state: &ManufacturingState,
    target_ref: &str,
    target_kind: &str,
) -> Vec<Value> {
    let Some(bus) = state.bus() else {
        return Vec::new();
    };
    let context = bus.emit(
        QUALITY_BLOCKS_QUERY,
        json!({ "target_ref": target_ref, "target_kind": target_kind }),
    );
    let mut blocks: Vec<Value> = Vec::new();
    let mut read = |value: &Value| {
        if let Some(list) = value.get("blocks").and_then(|v| v.as_array()) {
            blocks.extend(list.iter().cloned());
        }
    };
    read(&context.data);
    for result in &context.results {
        read(result);
    }
    blocks
}
