//! Hook handlers for user deletion, logistics shipment and
//! proof events, rental returns, and the AI grounding hooks.
//!
//! Auto-advance (logistics -> delivery notes): a shipment
//! moving out (picked up, in transit, out for delivery) moves
//! an issued DO to in transit; a proof of delivery recorded
//! for the shipment links itself to the DO and moves an
//! in-transit DO to delivered; a returned shipment returns its
//! DOs; a cancelled shipment cancels DOs that have not been
//! delivered. Draft DOs are never auto-advanced - a draft is
//! not released yet, so the warehouse issues it first and the
//! next shipment event moves it. Delivery through the shipment
//! event alone never bypasses the proof gate: a DO advances to
//! delivered from a shipment event only when a proof is already
//! linked (the proof event itself is what normally links it).
//! Auto-advance is per-tenant data
//! (`auto_advance_from_shipment` in settings); a tenant that
//! turns it off keeps full manual control.

use super::handlers_ai::{grounding_facts, readiness_checklist};
use super::handlers_notes::apply_transition;
use super::state::DeliveryNoteState;
use super::types::{
    DeliveryNoteStatus, DELETED_USER_ID, EVENT_AI_READINESS_REQUESTED, EVENT_AI_SUMMARIZE_REQUESTED,
};
use chrono::Utc;
use serde_json::json;
use unified_core::hooks::HookContext;
use uuid::Uuid;

/// Read a shipment id from a logistics payload
/// (`shipment_id`, falling back to `id`).
fn shipment_id_from(ctx: &HookContext) -> Option<Uuid> {
    for key in ["shipment_id", "id"] {
        if let Some(raw) = ctx.get(key).and_then(|value| value.as_str()) {
            if let Ok(id) = Uuid::parse_str(raw) {
                return Some(id);
            }
        }
    }
    None
}

/// True when the DO's tenant has auto-advance switched on.
fn auto_advance_on(state: &DeliveryNoteState, tenant_id: &str) -> bool {
    state.current_settings(tenant_id).auto_advance_from_shipment
}

// User deletion (users module owns the event)

/// `user.deleted` carries `user_id` (falling back to `id`, the
/// same shape the phone module reads). The user's DOs are
/// anonymized to `deleted-user` so the delivery history stays
/// auditable without the identity. DOs are never deleted: a
/// delivery note must stay on record.
pub(crate) fn on_user_deleted(state: DeliveryNoteState, ctx: &mut HookContext) {
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
        ctx.add_result(
            json!({ "delivery_notes": "skipped", "reason": "user.deleted carries no user_id" }),
        );
        return;
    };
    let now = Utc::now();
    let mut anonymized = 0i64;
    for mut entry in state.notes.iter_mut() {
        if entry.value().created_by == user_id {
            entry.value_mut().created_by = DELETED_USER_ID.to_string();
            entry.value_mut().updated_at = now;
            anonymized += 1;
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "user_deleted",
        "user_id": user_id,
        "anonymized_notes": anonymized,
    }));
}

// Logistics shipment movement (logistics module owns the events)

/// `logistics.shipment.status_changed` carries
/// `shipment_id`, `from_status`, and `to_status`. A shipment
/// moving out (`picked_up`, `in_transit`, `out_for_delivery`)
/// moves an issued DO of that shipment to in transit. Draft
/// DOs are left alone (see the module header), and DOs already
/// past in transit are left alone.
pub(crate) fn on_shipment_status_changed(state: DeliveryNoteState, ctx: &mut HookContext) {
    let Some(shipment_id) = shipment_id_from(ctx) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "logistics.shipment.status_changed carries no shipment_id" }));
        return;
    };
    let to_status = ctx
        .get("to_status")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let moving_out = matches!(
        to_status.as_str(),
        "picked_up" | "in_transit" | "out_for_delivery"
    );
    if !moving_out {
        ctx.add_result(json!({ "delivery_notes": "skipped", "shipment_id": shipment_id.to_string(), "reason": "shipment status does not move a delivery note" }));
        return;
    }
    let mut advanced: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for mut note in state.notes_for_shipment(shipment_id) {
        if !auto_advance_on(&state, &note.tenant_id) {
            skipped.push(note.number.clone());
            continue;
        }
        if note.status == DeliveryNoteStatus::Issued {
            if apply_transition(
                &state,
                &mut note,
                DeliveryNoteStatus::InTransit,
                "system:logistics.shipment.status_changed",
                Some(format!("Shipment moved to {to_status}")),
            )
            .is_ok()
            {
                advanced.push(note.number);
            } else {
                skipped.push(note.number);
            }
        } else {
            skipped.push(note.number);
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "shipment_status_changed",
        "shipment_id": shipment_id.to_string(),
        "to_status": to_status,
        "advanced_to_in_transit": advanced,
        "skipped": skipped,
    }));
}

/// `logistics.shipment.delivered` carries `shipment_id` and
/// `tracking_number`. An in-transit DO with a proof already
/// linked advances to delivered; one without a proof stays in
/// transit - the proof event (below) is what normally links
/// the proof and completes the delivery, so the proof gate is
/// never bypassed by this event alone.
pub(crate) fn on_shipment_delivered(state: DeliveryNoteState, ctx: &mut HookContext) {
    let Some(shipment_id) = shipment_id_from(ctx) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "logistics.shipment.delivered carries no shipment_id" }));
        return;
    };
    let mut delivered: Vec<String> = Vec::new();
    let mut awaiting_proof: Vec<String> = Vec::new();
    for mut note in state.notes_for_shipment(shipment_id) {
        if !auto_advance_on(&state, &note.tenant_id) {
            awaiting_proof.push(note.number.clone());
            continue;
        }
        if note.status != DeliveryNoteStatus::InTransit {
            continue;
        }
        if note.proof_of_delivery_id.is_none() {
            awaiting_proof.push(note.number.clone());
            continue;
        }
        if apply_transition(
            &state,
            &mut note,
            DeliveryNoteStatus::Delivered,
            "system:logistics.shipment.delivered",
            None,
        )
        .is_ok()
        {
            delivered.push(note.number);
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "shipment_delivered",
        "shipment_id": shipment_id.to_string(),
        "delivered": delivered,
        "awaiting_proof": awaiting_proof,
    }));
}

/// `logistics.proof.created` carries `proof_id`,
/// `shipment_id`, `recipient_name`, and `delivered_at`. The
/// proof links itself to every DO of the shipment (proof id,
/// recipient name, and signed-at time when the DO does not
/// already carry them), and an in-transit DO advances to
/// delivered. The logistics `ProofOfDelivery` carries a
/// signature storage reference but no signer name or
/// signed-at time, so the DO stores the recipient metadata
/// itself (see the deliver endpoint).
pub(crate) fn on_proof_created(state: DeliveryNoteState, ctx: &mut HookContext) {
    let Some(shipment_id) = shipment_id_from(ctx) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "logistics.proof.created carries no shipment_id" }));
        return;
    };
    let proof_id = ctx
        .get("proof_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(proof_id) = proof_id else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "logistics.proof.created carries no proof_id" }));
        return;
    };
    let recipient_name = ctx
        .get("recipient_name")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let delivered_at = ctx
        .get("delivered_at")
        .and_then(|value| value.as_str())
        .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    let now = Utc::now();
    let mut linked: Vec<String> = Vec::new();
    let mut delivered: Vec<String> = Vec::new();
    for mut note in state.notes_for_shipment(shipment_id) {
        let mut changed = false;
        if note.proof_of_delivery_id.is_none() {
            note.proof_of_delivery_id = Some(proof_id);
            changed = true;
        }
        if note.recipient_name.is_none() {
            if let Some(name) = &recipient_name {
                note.recipient_name = Some(name.clone());
                changed = true;
            }
        }
        if note.recipient_signed_at.is_none() {
            if let Some(at) = delivered_at {
                note.recipient_signed_at = Some(at);
                changed = true;
            }
        }
        if changed {
            note.updated_at = now;
            state.notes.insert(note.id, note.clone());
        }
        linked.push(note.number.clone());
        if !auto_advance_on(&state, &note.tenant_id) {
            continue;
        }
        if note.status == DeliveryNoteStatus::InTransit {
            if apply_transition(
                &state,
                &mut note,
                DeliveryNoteStatus::Delivered,
                "system:logistics.proof.created",
                None,
            )
            .is_ok()
            {
                delivered.push(note.number);
            }
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "proof_created",
        "shipment_id": shipment_id.to_string(),
        "proof_id": proof_id.to_string(),
        "linked_notes": linked,
        "delivered": delivered,
    }));
}

/// `logistics.shipment.returned` carries `shipment_id` and a
/// `reason`. Every in-transit or delivered DO of the shipment
/// returns (full scope - the whole shipment came back) with
/// that reason. Returned quantities stay as recorded: the
/// shipment return is a document-level event, not a recount.
pub(crate) fn on_shipment_returned(state: DeliveryNoteState, ctx: &mut HookContext) {
    let Some(shipment_id) = shipment_id_from(ctx) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "logistics.shipment.returned carries no shipment_id" }));
        return;
    };
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("shipment returned")
        .to_string();
    let mut returned: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for mut note in state.notes_for_shipment(shipment_id) {
        if !auto_advance_on(&state, &note.tenant_id) {
            skipped.push(note.number.clone());
            continue;
        }
        if note.status == DeliveryNoteStatus::InTransit
            || note.status == DeliveryNoteStatus::Delivered
        {
            note.return_reason = Some(reason.clone());
            note.return_scope = "full".to_string();
            note.returned_at = Some(Utc::now());
            note.updated_at = Utc::now();
            state.notes.insert(note.id, note.clone());
            if apply_transition(
                &state,
                &mut note,
                DeliveryNoteStatus::Returned,
                "system:logistics.shipment.returned",
                Some(reason.clone()),
            )
            .is_ok()
            {
                returned.push(note.number);
            } else {
                skipped.push(note.number);
            }
        } else {
            skipped.push(note.number);
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "shipment_returned",
        "shipment_id": shipment_id.to_string(),
        "reason": reason,
        "returned": returned,
        "skipped": skipped,
    }));
}

/// `logistics.shipment.cancelled` carries `shipment_id` and a
/// `reason`. DOs that have not been delivered or returned are
/// cancelled with that reason; delivered and returned DOs are
/// history and stay.
pub(crate) fn on_shipment_cancelled(state: DeliveryNoteState, ctx: &mut HookContext) {
    let Some(shipment_id) = shipment_id_from(ctx) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "logistics.shipment.cancelled carries no shipment_id" }));
        return;
    };
    let reason = ctx
        .get("reason")
        .and_then(|value| value.as_str())
        .unwrap_or("shipment cancelled")
        .to_string();
    let mut cancelled: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for mut note in state.notes_for_shipment(shipment_id) {
        if !auto_advance_on(&state, &note.tenant_id) {
            skipped.push(note.number.clone());
            continue;
        }
        if matches!(
            note.status,
            DeliveryNoteStatus::Draft | DeliveryNoteStatus::Issued | DeliveryNoteStatus::InTransit
        ) {
            if apply_transition(
                &state,
                &mut note,
                DeliveryNoteStatus::Cancelled,
                "system:logistics.shipment.cancelled",
                Some(reason.clone()),
            )
            .is_ok()
            {
                cancelled.push(note.number);
            } else {
                skipped.push(note.number);
            }
        } else {
            skipped.push(note.number);
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "shipment_cancelled",
        "shipment_id": shipment_id.to_string(),
        "reason": reason,
        "cancelled": cancelled,
        "skipped": skipped,
    }));
}

// Rental returns (rental module owns the event)

/// `rental.order.returned` carries `order_id`,
/// `delivery_id`, `late_fee_minor`, and `returned_at`. Every
/// in-transit or delivered DO linked to that rental order
/// returns (full scope - the rental unit came back) with the
/// rental delivery recorded in the event detail. DOs with no
/// rental link are skipped, not failed.
pub(crate) fn on_rental_order_returned(state: DeliveryNoteState, ctx: &mut HookContext) {
    let order_id = ctx
        .get("order_id")
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .or_else(|| {
            ctx.get("rental_order_id")
                .and_then(|value| value.as_str())
                .map(str::to_string)
        });
    let Some(order_id) = order_id else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "rental.order.returned carries no order_id" }));
        return;
    };
    let delivery_id = ctx
        .get("delivery_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut returned: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for mut note in state.notes_for_rental_order(&order_id) {
        if note.status == DeliveryNoteStatus::InTransit
            || note.status == DeliveryNoteStatus::Delivered
        {
            note.return_reason = Some("rental order returned".to_string());
            note.return_scope = "full".to_string();
            note.returned_at = Some(Utc::now());
            note.updated_at = Utc::now();
            state.notes.insert(note.id, note.clone());
            let detail = delivery_id
                .as_ref()
                .map(|id| format!("rental delivery {id}"));
            if apply_transition(
                &state,
                &mut note,
                DeliveryNoteStatus::Returned,
                "system:rental.order.returned",
                detail,
            )
            .is_ok()
            {
                returned.push(note.number);
            } else {
                skipped.push(note.number);
            }
        } else {
            skipped.push(note.number);
        }
    }
    ctx.add_result(json!({
        "delivery_notes": "rental_order_returned",
        "rental_order_id": order_id,
        "rental_delivery_id": delivery_id,
        "returned": returned,
        "skipped": skipped,
    }));
}

// AI grounding hooks (the AI module's runner emits these)

/// Ground the `summarize_delivery_note` capability with the
/// DO's stored facts. The payload carries `tenant_id` and
/// `delivery_note_id`; without either the hook is skipped.
pub(crate) fn on_ai_summarize_requested(state: DeliveryNoteState, ctx: &mut HookContext) {
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let note_id = ctx
        .get("delivery_note_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(tenant_id), Some(note_id)) = (tenant_id, note_id) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "AI summarize request carries no tenant_id or delivery_note_id" }));
        return;
    };
    match grounding_facts(&state, &tenant_id, &note_id) {
        Ok(facts) => {
            ctx.add_result(json!({ "delivery_notes": "grounded", "facts": facts }));
        }
        Err(error) => {
            ctx.add_result(json!({ "delivery_notes": "skipped", "reason": error.message }));
        }
    }
}

/// Ground the `check_delivery_readiness` capability with the
/// DO's stored-facts checklist.
pub(crate) fn on_ai_readiness_requested(state: DeliveryNoteState, ctx: &mut HookContext) {
    let tenant_id = ctx
        .get("tenant_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let note_id = ctx
        .get("delivery_note_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(tenant_id), Some(note_id)) = (tenant_id, note_id) else {
        ctx.add_result(json!({ "delivery_notes": "skipped", "reason": "AI readiness request carries no tenant_id or delivery_note_id" }));
        return;
    };
    match readiness_checklist(&state, &tenant_id, &note_id) {
        Ok(checklist) => {
            ctx.add_result(json!({ "delivery_notes": "grounded", "checklist": checklist }));
        }
        Err(error) => {
            ctx.add_result(json!({ "delivery_notes": "skipped", "reason": error.message }));
        }
    }
}

/// The AI grounding hook the AI module's runner emits for this
/// module's summary capability.
pub(crate) fn ai_summarize_event() -> &'static str {
    EVENT_AI_SUMMARIZE_REQUESTED
}

/// The AI grounding hook for the delivery-readiness capability.
pub(crate) fn ai_readiness_event() -> &'static str {
    EVENT_AI_READINESS_REQUESTED
}
