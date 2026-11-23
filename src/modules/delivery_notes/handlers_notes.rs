//! HTTP handlers for delivery notes: list, create (standalone
//! and from a shipment), read, update, delete, and the status
//! actions (issue, advance, deliver, return, cancel).

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, created, err, load_note, note_event, note_view, ok, optional_text, paginate,
    parse_date_filter, parse_uuid, require_address, require_label, require_notes, require_reason,
    require_reference, require_text, caller_company, require_company_for_write, in_company_scope_uuid,
    require_company_change_uuid, require_company_match_uuid, require_note_in_company_scope,
    require_tenant,
};
use super::state::DeliveryNoteState;
use super::types::{
    can_transition, snapshot_items, validate_return_quantities, AdvanceRequest, CancelRequest,
    CreateDeliveryNoteRequest, CreateFromShipmentRequest, DeliverRequest, DeliveryNote,
    DeliveryNoteStatus, DeliveryParty, ReturnRequest, UpdateDeliveryNoteRequest, EVENT_CANCELLED,
    EVENT_CREATED, EVENT_DELETED, EVENT_DELIVERED, EVENT_IN_TRANSIT, EVENT_ISSUED, EVENT_RETURNED,
    EVENT_UPDATED, MAX_LABEL_LEN, MAX_NOTES_LEN, MAX_REFERENCE_LEN,
    PERM_DELIVERY_NOTES_MANAGE, PERM_DELIVERY_NOTES_VIEW,
};

/// Path carrying a delivery note id.
#[derive(Debug, Deserialize)]
struct NotePath {
    note_id: String,
}

/// Path carrying a shipment id.
#[derive(Debug, Deserialize)]
struct ShipmentPath {
    shipment_id: String,
}

fn validate_party(
    input: &super::types::DeliveryPartyInput,
    field: &str,
) -> UnifiedResult<DeliveryParty> {
    Ok(DeliveryParty {
        name: require_text(&input.name, &format!("{field}.name"), MAX_LABEL_LEN)?,
        address: require_address(&input.address)?,
        phone: optional_text(&input.phone, &format!("{field}.phone"), MAX_LABEL_LEN)?,
        email: optional_text(&input.email, &format!("{field}.email"), MAX_LABEL_LEN)?,
    })
}

fn validate_party_update(
    input: &super::types::DeliveryPartyInput,
    field: &str,
) -> UnifiedResult<DeliveryParty> {
    validate_party(input, field)
}

/// Ask the logistics module whether a shipment id names a real
/// shipment, through the `logistics.shipment.lookup` hook query.
/// No handler answers that query today, so an unanswered query
/// records the link as `unverified` (the same convention the
/// logistics module itself uses for unverified store order
/// links) and the response carries a warning — the DO is still
/// created, because the snapshot in the request body is the
/// document's source of truth, not the lookup.
fn lookup_shipment(state: &DeliveryNoteState, shipment_id: Uuid) -> (String, Option<String>) {
    let Some(context) = state.query(
        "logistics.shipment.lookup",
        json!({ "shipment_id": shipment_id.to_string() }),
    ) else {
        return (
            "unverified".to_string(),
            Some(
                "Shipment could not be verified: logistics.shipment.lookup has no handler yet"
                    .to_string(),
            ),
        );
    };
    let found = context
        .get("found")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if found {
        ("linked".to_string(), None)
    } else {
        (
            "unverified".to_string(),
            Some(format!(
                "Shipment {shipment_id} was not confirmed by logistics"
            )),
        )
    }
}

/// Build and store a DO from validated inputs plus a snapshot
/// of its items. Shared by the standalone create and the
/// from-shipment create so both paths freeze the same snapshot.
#[allow(clippy::too_many_arguments)]
fn build_note(
    state: &DeliveryNoteState,
    tenant_id: &str,
    actor: &str,
    shipment_id: Option<Uuid>,
    shipment_link_status: String,
    order_id: Option<String>,
    rental_order_id: Option<String>,
    company_id: Option<Uuid>,
    sender: DeliveryParty,
    receiver: DeliveryParty,
    warehouse_id: Option<Uuid>,
    warehouse_name: Option<String>,
    driver_id: Option<Uuid>,
    driver_name: Option<String>,
    vehicle_id: Option<Uuid>,
    vehicle_plate: Option<String>,
    vehicle_type: Option<String>,
    tracking_number: Option<String>,
    notes: Option<String>,
    item_inputs: &[super::types::DeliveryNoteItemInput],
) -> UnifiedResult<DeliveryNote> {
    let now = Utc::now();
    let settings = state.current_settings(tenant_id);
    let number = state.next_number(tenant_id, &settings, now);
    let note_id = Uuid::new_v4();
    let items =
        snapshot_items(item_inputs, tenant_id, note_id, now).map_err(UnifiedError::bad_request)?;
    let note = DeliveryNote {
        id: note_id,
        tenant_id: tenant_id.to_string(),
        number,
        shipment_id,
        shipment_link_status,
        order_id,
        rental_order_id,
        company_id,
        status: DeliveryNoteStatus::Draft,
        sender,
        receiver,
        warehouse_id,
        warehouse_name,
        driver_id,
        driver_name,
        vehicle_id,
        vehicle_plate,
        vehicle_type,
        tracking_number,
        proof_of_delivery_id: None,
        recipient_name: None,
        recipient_signature_ref: None,
        recipient_signed_at: None,
        notes,
        return_reason: None,
        return_scope: "none".to_string(),
        returned_at: None,
        issued_at: None,
        delivered_at: None,
        cancelled_at: None,
        created_by: actor.to_string(),
        created_at: now,
        updated_at: now,
    };
    for item in &items {
        state.items.insert(item.id, item.clone());
    }
    state.notes.insert(note.id, note.clone());
    Ok(note)
}

/// List DOs (paginated; filter by status, shipment_id,
/// rental_order_id, receiver, company_id, and created date
/// range `from` / `to`).
pub(crate) async fn list_notes(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let params = params.into_inner();
        let status_filter = params
            .filters
            .get("status")
            .and_then(|raw| DeliveryNoteStatus::from_str(raw));
        if params.filters.contains_key("status") && status_filter.is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Unknown status filter: {}",
                params.filters.get("status").unwrap_or(&String::new())
            )));
        }
        let shipment_filter = params.filters.get("shipment_id").cloned();
        let rental_filter = params.filters.get("rental_order_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let receiver_filter = params
            .filters
            .get("receiver")
            .map(|value| value.to_lowercase());
        let from_filter = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to_filter = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut rows: Vec<DeliveryNote> = state
            .notes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|note| {
                if note.tenant_id != tenant_id {
                    return false;
                }
                if !in_company_scope_uuid(&note.company_id, &_scope) {
                    return false;
                }
                if let Some(status) = status_filter {
                    if note.status != status {
                        return false;
                    }
                }
                if let Some(shipment_id) = &shipment_filter {
                    if note.shipment_id.map(|id| id.to_string()) != Some(shipment_id.clone()) {
                        return false;
                    }
                }
                if let Some(rental_order_id) = &rental_filter {
                    if note.rental_order_id.as_deref() != Some(rental_order_id.as_str()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if note.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if let Some(receiver) = &receiver_filter {
                    if !note
                        .receiver
                        .name
                        .to_lowercase()
                        .contains(receiver.as_str())
                    {
                        return false;
                    }
                }
                if let Some(from) = from_filter {
                    if note.created_at < from {
                        return false;
                    }
                }
                if let Some(to) = to_filter {
                    if note.created_at > to {
                        return false;
                    }
                }
                if let Some(search) = &search {
                    let haystack = format!(
                        "{} {} {}",
                        note.number, note.receiver.name, note.sender.name
                    )
                    .to_lowercase();
                    if !haystack.contains(search.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = rows.iter().map(|note| note_view(&state, note)).collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a standalone draft DO with a full snapshot
/// (parties, driver, vehicle, warehouse, items).
pub(crate) async fn create_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateDeliveryNoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let input = body.into_inner();
        let sender = validate_party(&input.sender, "sender")?;
        let receiver = validate_party(&input.receiver, "receiver")?;
        let warehouse_name = optional_text(&input.warehouse_name, "warehouse_name", MAX_LABEL_LEN)?;
        let driver_name = optional_text(&input.driver_name, "driver_name", MAX_LABEL_LEN)?;
        let vehicle_plate = optional_text(&input.vehicle_plate, "vehicle_plate", 20)?;
        let vehicle_type = optional_text(&input.vehicle_type, "vehicle_type", MAX_LABEL_LEN)?;
        let tracking_number =
            optional_text(&input.tracking_number, "tracking_number", MAX_REFERENCE_LEN)?;
        let order_id = optional_text(&input.order_id, "order_id", MAX_REFERENCE_LEN)?;
        let rental_order_id =
            optional_text(&input.rental_order_id, "rental_order_id", MAX_REFERENCE_LEN)?;
        let notes = require_notes(&input.notes)?;
        let (link_status, warning) = match input.shipment_id {
            Some(shipment_id) => lookup_shipment(&state, shipment_id),
            None => ("none".to_string(), None),
        };
        let company_id = require_company_match_uuid(&_tenant, &req, &input.company_id)?;
        let note = build_note(
            &state,
            &tenant_id,
            &caller.actor,
            input.shipment_id,
            link_status,
            order_id,
            rental_order_id,
            company_id,
            sender,
            receiver,
            input.warehouse_id,
            warehouse_name,
            input.driver_id,
            driver_name,
            input.vehicle_id,
            vehicle_plate,
            vehicle_type,
            tracking_number,
            notes,
            &input.items,
        )?;
        note_event(&state, EVENT_CREATED, &note, &caller.actor, None);
        let mut view = note_view(&state, &note);
        if let (Value::Object(map), Some(warning)) = (&mut view, warning) {
            map.insert("warning".to_string(), Value::from(warning));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Create a draft DO from a logistics shipment id. The shipment
/// is linked by id only (no foreign key); the snapshot — parties,
/// driver, vehicle, warehouse, items — comes from the request
/// body, so the DO freezes the shipment's state at creation
/// time and later shipment edits never rewrite its history.
pub(crate) async fn create_note_from_shipment(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ShipmentPath>,
    body: web::Json<CreateFromShipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let shipment_id = parse_uuid(&path.shipment_id, "shipment")?;
        let input = body.into_inner();
        let sender = validate_party(&input.sender, "sender")?;
        let receiver = validate_party(&input.receiver, "receiver")?;
        let warehouse_name = optional_text(&input.warehouse_name, "warehouse_name", MAX_LABEL_LEN)?;
        let driver_name = optional_text(&input.driver_name, "driver_name", MAX_LABEL_LEN)?;
        let vehicle_plate = optional_text(&input.vehicle_plate, "vehicle_plate", 20)?;
        let vehicle_type = optional_text(&input.vehicle_type, "vehicle_type", MAX_LABEL_LEN)?;
        let rental_order_id =
            optional_text(&input.rental_order_id, "rental_order_id", MAX_REFERENCE_LEN)?;
        let notes = require_notes(&input.notes)?;
        let (link_status, warning) = lookup_shipment(&state, shipment_id);
        let company_id = require_company_match_uuid(&_tenant, &req, &input.company_id)?;
        let note = build_note(
            &state,
            &tenant_id,
            &caller.actor,
            Some(shipment_id),
            link_status,
            None,
            rental_order_id,
            company_id,
            sender,
            receiver,
            input.warehouse_id,
            warehouse_name,
            input.driver_id,
            driver_name,
            input.vehicle_id,
            vehicle_plate,
            vehicle_type,
            None,
            notes,
            &input.items,
        )?;
        note_event(&state, EVENT_CREATED, &note, &caller.actor, None);
        let mut view = note_view(&state, &note);
        if let (Value::Object(map), Some(warning)) = (&mut view, warning) {
            map.insert("warning".to_string(), Value::from(warning));
        }
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one DO with its items and quantity totals.
pub(crate) async fn get_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_in_company_scope(&note.company_id, &_scope)?;
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a draft DO's snapshot (parties, driver, vehicle,
/// warehouse, notes, links). Only drafts may be edited: once a
/// DO is issued its snapshot is history and must not change.
pub(crate) async fn update_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<UpdateDeliveryNoteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft delivery note can be updated (current status: {})",
                note.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(sender) = &input.sender {
            note.sender = validate_party_update(sender, "sender")?;
        }
        if let Some(receiver) = &input.receiver {
            note.receiver = validate_party_update(receiver, "receiver")?;
        }
        if let Some(warehouse_id) = input.warehouse_id {
            note.warehouse_id = warehouse_id;
        }
        if let Some(warehouse_name) = input.warehouse_name {
            note.warehouse_name = match warehouse_name {
                Some(name) => Some(require_label(&name)?),
                None => None,
            };
        }
        if let Some(driver_id) = input.driver_id {
            note.driver_id = driver_id;
        }
        if let Some(driver_name) = input.driver_name {
            note.driver_name = match driver_name {
                Some(name) => Some(require_label(&name)?),
                None => None,
            };
        }
        if let Some(vehicle_id) = input.vehicle_id {
            note.vehicle_id = vehicle_id;
        }
        if let Some(vehicle_plate) = input.vehicle_plate {
            note.vehicle_plate = match vehicle_plate {
                Some(plate) => Some(require_text(&plate, "vehicle_plate", 20)?),
                None => None,
            };
        }
        if let Some(vehicle_type) = input.vehicle_type {
            note.vehicle_type = match vehicle_type {
                Some(kind) => Some(require_label(&kind)?),
                None => None,
            };
        }
        if let Some(notes) = input.notes {
            note.notes = match notes {
                Some(text) => Some(require_text(&text, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        if let Some(rental_order_id) = input.rental_order_id {
            note.rental_order_id = match rental_order_id {
                Some(id) => Some(require_reference(&id, "rental_order_id")?),
                None => None,
            };
        }
        if let Some(company_id) = input.company_id {
            note.company_id = require_company_change_uuid(&_scope, &company_id)?;
        }
        note.updated_at = Utc::now();
        state.notes.insert(note.id, note.clone());
        note_event(&state, EVENT_UPDATED, &note, &caller.actor, None);
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a draft DO with its items. Issued DOs are never
/// deleted, only cancelled, so the document trail stays intact.
pub(crate) async fn delete_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft delivery note can be deleted (current status: {})",
                note.status.as_str()
            )));
        }
        state.remove_items_of(note.id);
        state.notes.remove(&note.id);
        note_event(&state, EVENT_DELETED, &note, &caller.actor, None);
        Ok(json!({ "deleted": note.id.to_string(), "number": note.number }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Move a DO one step forward in the status machine. Used by
/// the hook listeners as well as by this handler: the same
/// `apply_transition` is the single place a status change is
/// validated, stamped, stored, and announced.
pub(crate) fn apply_transition(
    state: &DeliveryNoteState,
    note: &mut DeliveryNote,
    to: DeliveryNoteStatus,
    actor: &str,
    detail: Option<String>,
) -> UnifiedResult<()> {
    if !can_transition(note.status, to) {
        return Err(UnifiedError::conflict(format!(
            "Delivery note {} cannot move from {} to {}",
            note.number,
            note.status.as_str(),
            to.as_str()
        )));
    }
    let now = Utc::now();
    note.status = to;
    note.updated_at = now;
    match to {
        DeliveryNoteStatus::Issued => note.issued_at = Some(now),
        DeliveryNoteStatus::Delivered => note.delivered_at = Some(now),
        DeliveryNoteStatus::Cancelled => note.cancelled_at = Some(now),
        DeliveryNoteStatus::Returned => note.returned_at = Some(now),
        _ => {}
    }
    state.notes.insert(note.id, note.clone());
    let event = match to {
        DeliveryNoteStatus::Issued => EVENT_ISSUED,
        DeliveryNoteStatus::InTransit => EVENT_IN_TRANSIT,
        DeliveryNoteStatus::Delivered => EVENT_DELIVERED,
        DeliveryNoteStatus::Returned => EVENT_RETURNED,
        DeliveryNoteStatus::Cancelled => EVENT_CANCELLED,
        DeliveryNoteStatus::Draft => EVENT_UPDATED,
    };
    note_event(state, event, note, actor, detail);
    if let DeliveryNoteStatus::Delivered = to {
        state.notify(
            "delivery_notes.delivered",
            &note.created_by,
            "Delivery note delivered",
            &format!(
                "Delivery note {} was delivered to {}.",
                note.number, note.receiver.name
            ),
            Some(format!("/api/v1/delivery-notes/{}", note.id)),
        );
    }
    Ok(())
}

/// Issue a draft DO (draft -> issued). Issuing freezes the
/// document: after this point only status actions may change it.
pub(crate) async fn issue_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        apply_transition(
            &state,
            &mut note,
            DeliveryNoteStatus::Issued,
            &caller.actor,
            None,
        )?;
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Advance a DO: with no `target` it moves to its next forward
/// status (draft -> issued, issued -> in_transit, in_transit
/// -> delivered); with a `target` it moves to that status when
/// the machine allows it. Delivering through this endpoint
/// still needs a linked proof of delivery — use the deliver
/// endpoint to attach one.
pub(crate) async fn advance_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<AdvanceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        let input = body.into_inner();
        let target = match &input.target {
            Some(raw) => DeliveryNoteStatus::from_str(raw.trim()).ok_or_else(|| {
                UnifiedError::bad_request(format!("Unknown delivery note status: {raw}"))
            })?,
            None => match note.status {
                DeliveryNoteStatus::Draft => DeliveryNoteStatus::Issued,
                DeliveryNoteStatus::Issued => DeliveryNoteStatus::InTransit,
                DeliveryNoteStatus::InTransit => DeliveryNoteStatus::Delivered,
                other => {
                    return Err(UnifiedError::conflict(format!(
                        "Delivery note in status {} has no next status",
                        other.as_str()
                    )))
                }
            },
        };
        if target == DeliveryNoteStatus::Delivered {
            let settings = state.current_settings(&tenant_id);
            if settings.require_proof_for_delivery && note.proof_of_delivery_id.is_none() {
                return Err(UnifiedError::conflict(
                    "A delivery note cannot be delivered without a linked proof of delivery: use the deliver endpoint with a proof_of_delivery_id",
                ));
            }
        }
        let detail = input.note.map(|text| text.trim().to_string());
        apply_transition(&state, &mut note, target, &caller.actor, detail)?;
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Deliver a DO: link the logistics proof of delivery and
/// record the recipient signature metadata, then move
/// in_transit -> delivered. The proof id is stored by
/// reference; the signature bytes stay in the storage layer.
pub(crate) async fn deliver_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<DeliverRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::InTransit {
            return Err(UnifiedError::conflict(format!(
                "Only an in-transit delivery note can be delivered (current status: {})",
                note.status.as_str()
            )));
        }
        let input = body.into_inner();
        let recipient_name = require_label(&input.recipient_name)?;
        let signature_ref = match &input.recipient_signature_ref {
            Some(reference) => Some(require_reference(reference, "recipient_signature_ref")?),
            None => None,
        };
        let signed_at = input.recipient_signed_at.unwrap_or_else(Utc::now);
        note.proof_of_delivery_id = Some(input.proof_of_delivery_id);
        note.recipient_name = Some(recipient_name);
        note.recipient_signature_ref = signature_ref;
        note.recipient_signed_at = Some(signed_at);
        note.updated_at = Utc::now();
        state.notes.insert(note.id, note.clone());
        apply_transition(
            &state,
            &mut note,
            DeliveryNoteStatus::Delivered,
            &caller.actor,
            input.note,
        )?;
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Mark a DO returned (partial or full): per-item returned
/// quantities plus the reason. The scope is computed from the
/// quantities, never taken from the caller. A delivered or
/// in-transit DO may be returned; a returned DO may take a
/// second return for the remaining units.
pub(crate) async fn return_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<ReturnRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        if note.status != DeliveryNoteStatus::InTransit
            && note.status != DeliveryNoteStatus::Delivered
            && note.status != DeliveryNoteStatus::Returned
        {
            return Err(UnifiedError::conflict(format!(
                "Only an in-transit, delivered, or already returned delivery note can be returned (current status: {})",
                note.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = require_reason(&input.reason)?;
        let items = state.note_items(note.id);
        let returned: Vec<(Uuid, i64)> = input
            .items
            .iter()
            .map(|row| (row.item_id, row.quantity))
            .collect();
        let scope =
            validate_return_quantities(&items, &returned).map_err(UnifiedError::bad_request)?;
        for (item_id, quantity) in &returned {
            if let Some(mut item) = state.items.get_mut(item_id) {
                item.quantity_returned =
                    (item.quantity_returned + *quantity).min(item.quantity_shipped);
                item.updated_at = Utc::now();
            }
        }
        if let Some(rental_order_id) = input.rental_order_id {
            note.rental_order_id = Some(require_reference(&rental_order_id, "rental_order_id")?);
        }
        note.return_reason = Some(reason.clone());
        note.return_scope = scope.to_string();
        note.returned_at = Some(Utc::now());
        note.updated_at = Utc::now();
        state.notes.insert(note.id, note.clone());
        if note.status != DeliveryNoteStatus::Returned {
            apply_transition(
                &state,
                &mut note,
                DeliveryNoteStatus::Returned,
                &caller.actor,
                Some(reason),
            )?;
        } else {
            // Already returned: a second return only adds the
            // remaining quantities, so announce the update
            // without re-running the machine.
            note_event(&state, EVENT_RETURNED, &note, &caller.actor, Some(reason));
        }
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Cancel a DO that has not been delivered or returned. The
/// row stays (cancelled is history), so the number is never
/// reused.
pub(crate) async fn cancel_note(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<NotePath>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_MANAGE)?;
        let mut note = load_note(&state, &tenant_id, &path.note_id)?;
        require_note_writable_in_company_scope(&note.company_id, &_scope)?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(text) => Some(require_reason(text)?),
            None => None,
        };
        apply_transition(
            &state,
            &mut note,
            DeliveryNoteStatus::Cancelled,
            &caller.actor,
            reason,
        )?;
        Ok(note_view(&state, &note))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the DOs of one shipment (paginated).
pub(crate) async fn list_notes_for_shipment(
    state: web::Data<DeliveryNoteState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<ShipmentPath>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let tenant_id = _tenant.tenant_id.to_string();
        let caller = caller(&req)?;
        super::helpers::require_permission(&ctx, &caller.role, PERM_DELIVERY_NOTES_VIEW)?;
        let shipment_id = parse_uuid(&path.shipment_id, "shipment")?;
        let params = params.into_inner();
        let mut rows: Vec<DeliveryNote> = state
            .notes_for_shipment(shipment_id)
            .into_iter()
            .filter(|note| {
                note.tenant_id == tenant_id && in_company_scope_uuid(&note.company_id, &_scope)
            })
            .collect();
        rows.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        let views: Vec<Value> = rows.iter().map(|note| note_view(&state, note)).collect();
        Ok(serde_json::to_value(paginate(views, &params)).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
