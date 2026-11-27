//! Hook handlers for user deletion, payment capture/refund,
//! logistics delivery, and contract signing.

use super::state::RentalState;
use super::types::{DepositStatus, DELETED_USER_ID};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;
use uuid::Uuid;

/// Marker for anonymized rows is `deleted-user` (see types.rs).

// User deletion (users module owns the event)

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the forums module reads). The user's orders are
/// anonymized to `deleted-user` so the rental history stays
/// auditable without the identity; damage reports and
/// deliveries they recorded are anonymized the same way.
pub(crate) fn on_user_deleted(state: RentalState, ctx: &mut HookContext) {
    let user_id = ctx
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(user_id) = user_id else {
        ctx.add_result(json!({ "rental": "skipped", "reason": "user.deleted carries no user_id" }));
        return;
    };
    let now = Utc::now();
    let mut anonymized_orders = 0i64;
    for mut entry in state.orders.iter_mut() {
        if entry.value().customer_id == user_id {
            entry.value_mut().customer_id = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized_orders += 1;
        }
    }
    let mut anonymized_reports = 0i64;
    for mut entry in state.damage_reports.iter_mut() {
        let mut changed = false;
        if entry.value().reported_by == user_id {
            entry.value_mut().reported_by = DELETED_USER_ID.to_string();
            changed = true;
        }
        if entry.value().resolved_by.as_deref() == Some(user_id.as_str()) {
            entry.value_mut().resolved_by = Some(DELETED_USER_ID.to_string());
            changed = true;
        }
        if changed {
            entry.value_mut().updated_at = now;
            anonymized_reports += 1;
        }
    }
    let mut anonymized_deliveries = 0i64;
    for mut entry in state.deliveries.iter_mut() {
        if entry.value().recorded_by == user_id {
            entry.value_mut().recorded_by = DELETED_USER_ID.to_string();
            anonymized_deliveries += 1;
        }
    }
    ctx.add_result(json!({
        "rental": "user_deleted",
        "user_id": user_id,
        "anonymized_orders": anonymized_orders,
        "anonymized_reports": anonymized_reports,
        "anonymized_deliveries": anonymized_deliveries,
    }));
}

// Payments (payments module owns the events)

/// Find the order a payment event refers to. Payment payloads
/// carry the order id under `rental_order_id` (falling back to
/// `order_id` and to `reference_id`, the shapes other modules
/// use for cross-module references).
fn order_id_from(ctx: &HookContext) -> Option<Uuid> {
    for key in ["rental_order_id", "order_id", "reference_id"] {
        if let Some(raw) = ctx.get(key).and_then(|value| value.as_str()) {
            if let Ok(id) = Uuid::parse_str(raw) {
                return Some(id);
            }
        }
    }
    None
}

/// `payment.captured` marks the deposit of the referenced order
/// as held. The payment module owns the money movement; this
/// module only records the deposit state so the return flow
/// knows what is held. Events that name no rental order are
/// skipped, not failed.
pub(crate) fn on_payment_captured(state: RentalState, ctx: &mut HookContext) {
    let Some(order_id) = order_id_from(ctx) else {
        ctx.add_result(
            json!({ "rental": "skipped", "reason": "payment.captured carries no rental order id" }),
        );
        return;
    };
    let Some(mut order) = state.orders.get_mut(&order_id) else {
        ctx.add_result(json!({ "rental": "skipped", "order_id": order_id.to_string(), "reason": "no rental order with that id" }));
        return;
    };
    if order.deposit_status == DepositStatus::Pending {
        order.deposit_status = DepositStatus::Held;
        order.updated_at = Utc::now();
        ctx.add_result(json!({ "rental": "deposit_held", "order_id": order_id.to_string() }));
    } else {
        ctx.add_result(json!({ "rental": "skipped", "order_id": order_id.to_string(), "reason": "deposit is not pending" }));
    }
}

/// `payment.refunded` marks the deposit of the referenced order
/// as refunded. A partial refund (the payload carries
/// `amount_minor` below the held deposit) is recorded as
/// partially refunded with the deducted remainder kept on the
/// order.
pub(crate) fn on_payment_refunded(state: RentalState, ctx: &mut HookContext) {
    let Some(order_id) = order_id_from(ctx) else {
        ctx.add_result(
            json!({ "rental": "skipped", "reason": "payment.refunded carries no rental order id" }),
        );
        return;
    };
    let Some(mut order) = state.orders.get_mut(&order_id) else {
        ctx.add_result(json!({ "rental": "skipped", "order_id": order_id.to_string(), "reason": "no rental order with that id" }));
        return;
    };
    let refunded = ctx.get("amount_minor").and_then(|value| value.as_i64());
    match refunded {
        Some(amount) if amount < order.deposit_amount_minor && amount >= 0 => {
            order.deposit_deducted_minor = order.deposit_amount_minor - amount;
            order.deposit_status = DepositStatus::PartiallyRefunded;
            order.updated_at = Utc::now();
            ctx.add_result(json!({ "rental": "deposit_partially_refunded", "order_id": order_id.to_string(), "refunded_minor": amount }));
        }
        _ => {
            order.deposit_deducted_minor = 0;
            order.deposit_status = DepositStatus::Refunded;
            order.updated_at = Utc::now();
            ctx.add_result(
                json!({ "rental": "deposit_refunded", "order_id": order_id.to_string() }),
            );
        }
    }
}

// Logistics delivery (logistics module owns the event)

/// `logistics.shipment.delivered` records the shipment id on the
/// referenced rental order so the hand-out / return flow can be
/// traced to the delivery that carried the unit. The payload
/// carries the order id under `rental_order_id` (falling back to
/// `order_id`) and the shipment id under `shipment_id` (falling
/// back to `id`). Shipments for other modules are skipped.
pub(crate) fn on_shipment_delivered(state: RentalState, ctx: &mut HookContext) {
    let Some(order_id) = order_id_from(ctx) else {
        ctx.add_result(json!({ "rental": "skipped", "reason": "logistics.shipment.delivered carries no rental order id" }));
        return;
    };
    let shipment_id = ctx
        .get("shipment_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(mut order) = state.orders.get_mut(&order_id) else {
        ctx.add_result(json!({ "rental": "skipped", "order_id": order_id.to_string(), "reason": "no rental order with that id" }));
        return;
    };
    order.shipment_id = shipment_id.clone();
    order.updated_at = Utc::now();
    ctx.add_result(json!({ "rental": "shipment_delivered", "order_id": order_id.to_string(), "shipment_id": shipment_id }));
}

// Contract signing (contracts module owns the event)

/// `contracts.signed` marks the rental agreement signed on every
/// order linked to that contract id. The payload carries the
/// contract id under `contract_id` (falling back to `id`).
pub(crate) fn on_contract_signed(state: RentalState, ctx: &mut HookContext) {
    let contract_id = ctx
        .get("contract_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(contract_id) = contract_id else {
        ctx.add_result(
            json!({ "rental": "skipped", "reason": "contracts.signed carries no contract_id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut signed = 0i64;
    for mut entry in state.orders.iter_mut() {
        if entry.value().contract_id.as_deref() == Some(contract_id.as_str())
            && !entry.value().agreement_signed
        {
            entry.value_mut().agreement_signed = true;
            entry.value_mut().updated_at = now;
            signed += 1;
        }
    }
    ctx.add_result(
        json!({ "rental": "agreement_signed", "contract_id": contract_id, "orders": signed }),
    );
}
