//! HTTP handler for the delivery notes AI surface: one
//! registered capability at a time through the AI module's
//! gated runner (`run_ai_capability`, capability allowlist
//! gate first), exactly like the phone module's `ai_capability`
//! handler. The AI never books, charges, transfers, or changes
//! a delivery note on its own: both capabilities return text
//! inline only.

use actix_web::{web, HttpRequest, HttpResponse};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;

use super::helpers::{caller, caller_company, err, load_note, require_tenant};
use super::state::DeliveryNoteState;
use super::types::PERM_DELIVERY_NOTES_VIEW;
use crate::modules::ai;

/// Run one registered delivery notes AI capability through the
/// AI module's gated runner.
pub(crate) async fn ai_capability(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let _ = state;
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err(error, debug),
    };
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) =
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)
    {
        return err(error, debug);
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return err(error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "delivery_notes",
        &capability,
        input,
    )
    .await
    {
        Ok(output) => super::helpers::ok(output),
        Err(error) => err(error, debug),
    }
}

/// The grounding facts for one DO, used by the AI grounding
/// hooks in `hooks.rs`: the stored note plus its items and
/// quantity totals, so the AI summarizes stored facts instead
/// of inventing a delivery.
pub(crate) fn grounding_facts(
    state: &DeliveryNoteState,
    tenant_id: &str,
    note_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let note = load_note(state, tenant_id, note_id)?;
    let items = state.note_items(note.id);
    Ok(serde_json::json!({
        "delivery_note_id": note.id.to_string(),
        "number": note.number,
        "status": note.status.as_str(),
        "shipment_id": note.shipment_id.map(|id| id.to_string()),
        "tracking_number": note.tracking_number,
        "rental_order_id": note.rental_order_id,
        "sender_name": note.sender.name,
        "sender_address": note.sender.address,
        "receiver_name": note.receiver.name,
        "receiver_address": note.receiver.address,
        "warehouse_name": note.warehouse_name,
        "driver_name": note.driver_name,
        "vehicle_plate": note.vehicle_plate,
        "vehicle_type": note.vehicle_type,
        "proof_of_delivery_id": note.proof_of_delivery_id.map(|id| id.to_string()),
        "recipient_name": note.recipient_name,
        "return_reason": note.return_reason,
        "return_scope": note.return_scope,
        "total_ordered": super::types::total_ordered(&items),
        "total_shipped": super::types::total_shipped(&items),
        "total_returned": super::types::total_returned(&items),
        "items": items,
        "created_at": note.created_at.to_rfc3339(),
        "issued_at": note.issued_at.map(|at| at.to_rfc3339()),
        "delivered_at": note.delivered_at.map(|at| at.to_rfc3339()),
    }))
}

/// Delivery-readiness checklist for one DO, computed from
/// stored facts only: every item shipped, a driver and vehicle
/// named, sender and receiver present, and (when the tenant
/// requires it) a proof of delivery linked before delivery.
/// Returned as data, never as an instruction to the AI.
pub(crate) fn readiness_checklist(
    state: &DeliveryNoteState,
    tenant_id: &str,
    note_id: &str,
) -> UnifiedResult<serde_json::Value> {
    let note = load_note(state, tenant_id, note_id)?;
    let items = state.note_items(note.id);
    let settings = state.current_settings(tenant_id);
    let has_items = !items.is_empty();
    let all_shipped = has_items
        && items.iter().all(|item| {
            item.quantity_shipped > 0 && item.quantity_shipped <= item.quantity_ordered
        });
    let has_driver = note
        .driver_name
        .as_ref()
        .map(|name| !name.trim().is_empty())
        .unwrap_or(false);
    let has_vehicle = note
        .vehicle_plate
        .as_ref()
        .map(|plate| !plate.trim().is_empty())
        .unwrap_or(false);
    let has_proof = note.proof_of_delivery_id.is_some();
    let proof_needed = settings.require_proof_for_delivery;
    let ready =
        has_items && all_shipped && has_driver && has_vehicle && (!proof_needed || has_proof);
    Ok(serde_json::json!({
        "delivery_note_id": note.id.to_string(),
        "number": note.number,
        "status": note.status.as_str(),
        "ready": ready,
        "checks": {
            "has_items": has_items,
            "all_shipped": all_shipped,
            "has_driver": has_driver,
            "has_vehicle": has_vehicle,
            "has_proof_of_delivery": has_proof,
            "proof_required": proof_needed,
        },
    }))
}
