//! Logistics module — shipments, fleet, warehouses, delivery — Shipment handlers.
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
use super::state::*;
use super::tracking::*;
use super::types::*;
use super::vehicles::*;
use super::warehouses::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

pub(super) async fn list_shipments(
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
        let status_filter = params.filters.get("status").cloned();
        let warehouse_filter = params.filters.get("warehouse_id").cloned();
        let driver_filter = params.filters.get("driver_id").cloned();
        let vehicle_filter = params.filters.get("vehicle_id").cloned();
        let order_filter = params.filters.get("order_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Shipment> = state
            .shipments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|shipment| in_company_scope(&shipment.company_id, &_scope))
            .filter(|shipment| {
                if let Some(status) = &status_filter {
                    if shipment.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(warehouse_id) = &warehouse_filter {
                    if shipment.warehouse_id.map(|id| id.to_string()) != Some(warehouse_id.clone()) {
                        return false;
                    }
                }
                if let Some(driver_id) = &driver_filter {
                    if shipment.driver_id.map(|id| id.to_string()) != Some(driver_id.clone()) {
                        return false;
                    }
                }
                if let Some(vehicle_id) = &vehicle_filter {
                    if shipment.vehicle_id.map(|id| id.to_string()) != Some(vehicle_id.clone()) {
                        return false;
                    }
                }
                if let Some(order_id) = &order_filter {
                    if shipment.order_id.as_deref() != Some(order_id.as_str()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if shipment.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        shipment.tracking_number,
                        shipment.origin.address,
                        shipment.destination.address
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
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

pub(super) async fn create_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateShipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let origin = validate_endpoint(&input.origin, "origin")?;
        let destination = validate_endpoint(&input.destination, "destination")?;
        validate_dimensions(input.weight_kg, input.length_cm, input.width_cm, input.height_cm)?;
        validate_window(input.window_start, input.window_end)?;
        let zone_origin = require_text(&input.zone_origin, "zone_origin", MAX_ZONE_LEN)?;
        let zone_destination =
            require_text(&input.zone_destination, "zone_destination", MAX_ZONE_LEN)?;
        let service_level = input
            .service_level
            .as_deref()
            .map(|value| require_text(value, "service_level", MAX_SERVICE_LEVEL_LEN))
            .transpose()?
            .unwrap_or_else(|| "standard".to_string());
        let currency = input
            .currency
            .as_deref()
            .map(valid_currency)
            .transpose()?
            .unwrap_or_else(|| "USD".to_string());
        let company_id =
            parse_optional_company(&require_company_match(&_tenant, &req, &input.company_id)?)?;
        if let Some(driver_id) = input.driver_id {
            if !state.drivers.contains_key(&driver_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Driver not found: {driver_id}"
                )));
            }
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if !state.vehicles.contains_key(&vehicle_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Vehicle not found: {vehicle_id}"
                )));
            }
        }
        if let Some(warehouse_id) = input.warehouse_id {
            if !state.warehouses.contains_key(&warehouse_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Warehouse not found: {warehouse_id}"
                )));
            }
        }
        // Order link: confirmed by the store, denied by the store,
        // or unverified when no handler answers (see the header).
        let mut order_id: Option<String> = None;
        let mut order_link_status = "none".to_string();
        let mut order_warning: Option<String> = None;
        if let Some(raw_order_id) = &input.order_id {
            let trimmed = raw_order_id.trim();
            if !trimmed.is_empty() {
                match lookup_order(&ctx, trimmed) {
                    Some(true) => {
                        order_id = Some(trimmed.to_string());
                        order_link_status = "linked".to_string();
                    }
                    Some(false) => {
                        return Err(UnifiedError::unprocessable(format!(
                            "Order not found in the store: {trimmed}"
                        )));
                    }
                    None => {
                        order_id = Some(trimmed.to_string());
                        order_link_status = "unverified".to_string();
                        order_warning = Some(
                            "Order link is unverified: the store module did not answer the store.order.lookup query. The shipment is not treated as linked to a real order until the store confirms it.".to_string(),
                        );
                    }
                }
            }
        }
        let tracking_number = generate_tracking_number(&state)?;
        let token = generate_tracking_token();
        let now = Utc::now();
        let shipment = Shipment {
            id: Uuid::now_v7(),
            tracking_number: tracking_number.clone(),
            tracking_token_hash: sha256_hex(&token),
            order_id,
            order_link_status,
            company_id,
            status: ShipmentStatus::Created,
            origin,
            destination,
            weight_kg: input.weight_kg,
            length_cm: input.length_cm,
            width_cm: input.width_cm,
            height_cm: input.height_cm,
            zone_origin,
            zone_destination,
            service_level,
            driver_id: input.driver_id,
            vehicle_id: input.vehicle_id,
            warehouse_id: input.warehouse_id,
            scheduled_at: input.scheduled_at,
            window_start: input.window_start,
            window_end: input.window_end,
            estimated_delivery_at: input.estimated_delivery_at,
            shipping_cost: None,
            currency,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.shipments.insert(shipment.id, shipment.clone());
        state.add_timeline(
            shipment.id,
            "status_change",
            None,
            Some("created"),
            &caller.actor,
            None,
            None,
        );
        state.emit(
            "logistics.shipment.created",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "order_id": shipment.order_id,
                "order_link_status": shipment.order_link_status,
                "status": "created",
                "created_by": caller.actor,
            }),
        );
        // The verification token is returned once, inside the
        // creation response only. It is never stored raw and never
        // appears in any later response.
        let mut value = serde_json::to_value(&shipment).unwrap_or(Value::Null);
        if let Value::Object(map) = &mut value {
            map.insert(
                "tracking_token".to_string(),
                Value::String(token),
            );
            if let Some(warning) = order_warning {
                map.insert("order_link_warning".to_string(), Value::String(warning));
            }
        }
        Ok(value)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let shipment = load_shipment(&state, &path)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateShipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_writable_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status != ShipmentStatus::Created {
            return Err(UnifiedError::conflict(format!(
                "Only a shipment in \"created\" status can be edited; this shipment is \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(origin) = &input.origin {
            shipment.origin = validate_endpoint(origin, "origin")?;
        }
        if let Some(destination) = &input.destination {
            shipment.destination = validate_endpoint(destination, "destination")?;
        }
        let weight = input.weight_kg.unwrap_or(shipment.weight_kg);
        let length = input.length_cm.unwrap_or(shipment.length_cm);
        let width = input.width_cm.unwrap_or(shipment.width_cm);
        let height = input.height_cm.unwrap_or(shipment.height_cm);
        validate_dimensions(weight, length, width, height)?;
        shipment.weight_kg = weight;
        shipment.length_cm = length;
        shipment.width_cm = width;
        shipment.height_cm = height;
        if let Some(zone) = &input.zone_origin {
            shipment.zone_origin = require_text(zone, "zone_origin", MAX_ZONE_LEN)?;
        }
        if let Some(zone) = &input.zone_destination {
            shipment.zone_destination = require_text(zone, "zone_destination", MAX_ZONE_LEN)?;
        }
        if let Some(level) = &input.service_level {
            shipment.service_level = require_text(level, "service_level", MAX_SERVICE_LEVEL_LEN)?;
        }
        if let Some(driver_id) = input.driver_id {
            if let Some(id) = driver_id {
                if !state.drivers.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Driver not found: {id}")));
                }
            }
            shipment.driver_id = driver_id;
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if let Some(id) = vehicle_id {
                if !state.vehicles.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Vehicle not found: {id}")));
                }
            }
            shipment.vehicle_id = vehicle_id;
        }
        if let Some(warehouse_id) = input.warehouse_id {
            if let Some(id) = warehouse_id {
                if !state.warehouses.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!(
                        "Warehouse not found: {id}"
                    )));
                }
            }
            shipment.warehouse_id = warehouse_id;
        }
        if let Some(scheduled_at) = input.scheduled_at {
            shipment.scheduled_at = scheduled_at;
        }
        if let Some(window_start) = input.window_start {
            shipment.window_start = window_start;
        }
        if let Some(window_end) = input.window_end {
            shipment.window_end = window_end;
        }
        validate_window(shipment.window_start, shipment.window_end)?;
        if let Some(estimated) = input.estimated_delivery_at {
            shipment.estimated_delivery_at = estimated;
        }
        if let Some(currency) = &input.currency {
            shipment.currency = valid_currency(currency)?;
        }
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        state.emit(
            "logistics.shipment.updated",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "status": shipment.status.as_str(),
                "updated_by": caller.actor,
            }),
        );
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let shipment = load_shipment(&state, &path)?;
        require_record_writable_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status != ShipmentStatus::Created {
            return Err(UnifiedError::conflict(format!(
                "Only a shipment in \"created\" status can be deleted; this shipment is \"{}\". Cancel it instead.",
                shipment.status.as_str()
            )));
        }
        state.shipments.remove(&shipment.id);
        let stale: Vec<Uuid> = state
            .timeline
            .iter()
            .filter(|entry| entry.value().shipment_id == shipment.id)
            .map(|entry| *entry.key())
            .collect();
        for id in stale {
            state.timeline.remove(&id);
        }
        // No row may keep pointing at the deleted shipment:
        // exceptions, proofs, and stock movements are removed,
        // and route stops keep their stop but lose the link.
        remove_shipment_references(&state, shipment.id);
        state.emit(
            "logistics.shipment.deleted",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "shipment_id": shipment.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Remove every row that references a shipment being deleted.
/// Exceptions, proofs, and stock movements for the shipment are
/// removed outright; route stops survive but lose the link (their
/// `shipment_id` is cleared), so no dangling reference remains.
/// Timeline rows are removed by the caller, before this runs.
pub(super) fn remove_shipment_references(state: &LogisticsState, shipment_id: Uuid) {
    let stale_exceptions: Vec<Uuid> = state
        .exceptions
        .iter()
        .filter(|entry| entry.value().shipment_id == shipment_id)
        .map(|entry| *entry.key())
        .collect();
    for id in stale_exceptions {
        state.exceptions.remove(&id);
    }
    let stale_proofs: Vec<Uuid> = state
        .proofs
        .iter()
        .filter(|entry| entry.value().shipment_id == shipment_id)
        .map(|entry| *entry.key())
        .collect();
    for id in stale_proofs {
        state.proofs.remove(&id);
    }
    let stale_movements: Vec<Uuid> = state
        .stock_movements
        .iter()
        .filter(|entry| entry.value().shipment_id == Some(shipment_id))
        .map(|entry| *entry.key())
        .collect();
    for id in stale_movements {
        state.stock_movements.remove(&id);
    }
    for mut entry in state.route_stops.iter_mut() {
        if entry.value().shipment_id == Some(shipment_id) {
            entry.value_mut().shipment_id = None;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

/// True when an exception is still open. Only `open` counts as
/// open: no code path ever sets any other in-progress status, so
/// the resolve / return guards accept `open` only.
pub(super) fn exception_is_open(status: &str) -> bool {
    status == "open"
}

/// Move a shipment through the state machine. The transition is
/// rejected with 409 when the target is not in
/// `allowed_transitions(from)` — the check is the single gate for
/// every status change, including the exception and proof paths,
/// which call the same table through their own endpoints.
pub(super) fn apply_transition(
    state: &LogisticsState,
    shipment: &mut Shipment,
    to: ShipmentStatus,
    actor: &str,
    reason: Option<&str>,
    location: Option<String>,
    note: Option<String>,
) -> UnifiedResult<()> {
    let from = shipment.status;
    if from.is_terminal() {
        return Err(UnifiedError::conflict(format!(
            "Shipment is in terminal status \"{}\"; no transition is possible",
            from.as_str()
        )));
    }
    if !allowed_transitions(from).contains(&to) {
        let allowed: Vec<&str> = allowed_transitions(from)
            .iter()
            .map(|status| status.as_str())
            .collect();
        return Err(UnifiedError::conflict(format!(
            "Illegal transition from \"{}\" to \"{}\"; allowed: {}",
            from.as_str(),
            to.as_str(),
            allowed.join(", ")
        )));
    }
    if matches!(to, ShipmentStatus::Cancelled | ShipmentStatus::Exception | ShipmentStatus::Returned)
        && reason.map(str::trim).unwrap_or("").is_empty()
    {
        return Err(UnifiedError::bad_request(
            "A reason is required for this transition",
        ));
    }
    shipment.status = to;
    shipment.updated_at = Utc::now();
    state.shipments.insert(shipment.id, shipment.clone());
    let mut timeline_note = note;
    if let Some(reason) = reason {
        if !reason.trim().is_empty() {
            timeline_note = Some(match timeline_note {
                Some(existing) => format!("{reason} — {existing}"),
                None => reason.to_string(),
            });
        }
    }
    state.add_timeline(
        shipment.id,
        "status_change",
        Some(from.as_str()),
        Some(to.as_str()),
        actor,
        location,
        timeline_note,
    );
    state.emit(
        "logistics.shipment.status_changed",
        json!({
            "shipment_id": shipment.id.to_string(),
            "tracking_number": shipment.tracking_number,
            "from_status": from.as_str(),
            "to_status": to.as_str(),
            "actor": actor,
        }),
    );
    match to {
        ShipmentStatus::Cancelled => state.emit(
            "logistics.shipment.cancelled",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "reason": reason,
            }),
        ),
        ShipmentStatus::Delivered => state.emit(
            "logistics.shipment.delivered",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
            }),
        ),
        ShipmentStatus::Returned => state.emit(
            "logistics.shipment.returned",
            json!({
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "reason": reason,
            }),
        ),
        _ => {}
    }
    Ok(())
}

pub(super) async fn transition_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_writable_in_company_scope(&shipment.company_id, &_scope)?;
        can_operate_shipment(&ctx, &state, &caller, &shipment)?;
        let input = body.into_inner();
        let to = ShipmentStatus::parse(&input.to_status).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown status: {}", input.to_status))
        })?;
        if to == ShipmentStatus::Delivered {
            return Err(UnifiedError::bad_request(
                "Deliveries are recorded through the proof of delivery endpoint, not through a raw transition",
            ));
        }
        if to == ShipmentStatus::Exception {
            return Err(UnifiedError::bad_request(
                "Exceptions are opened through the exceptions endpoint, which records the exception row",
            ));
        }
        let location = optional_text(&input.location, "location", MAX_ADDRESS_LEN)?;
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        apply_transition(
            &state,
            &mut shipment,
            to,
            &caller.actor,
            reason.as_deref(),
            location,
            note,
        )?;
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_shipment_timeline(
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
        let shipment = load_shipment(&state, &path)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<ShipmentTimelineEntry> = state
            .timeline
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.shipment_id == shipment.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn link_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<LinkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_writable_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot link an order to a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        let order_id = require_text(&body.order_id, "order_id", MAX_NAME_LEN)?;
        let mut warning: Option<String> = None;
        match lookup_order(&ctx, &order_id) {
            Some(true) => {
                shipment.order_link_status = "linked".to_string();
            }
            Some(false) => {
                return Err(UnifiedError::unprocessable(format!(
                    "Order not found in the store: {order_id}"
                )));
            }
            None => {
                shipment.order_link_status = "unverified".to_string();
                warning = Some(
                    "Order link is unverified: the store module did not answer the store.order.lookup query. The shipment is not treated as linked to a real order until the store confirms it.".to_string(),
                );
            }
        }
        shipment.order_id = Some(order_id.clone());
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        state.add_timeline(
            shipment.id,
            "order_linked",
            None,
            None,
            &caller.actor,
            None,
            Some(format!(
                "Linked order {order_id} ({})",
                shipment.order_link_status
            )),
        );
        let mut value = serde_json::to_value(&shipment).unwrap_or(Value::Null);
        if let (Value::Object(map), Some(warning)) = (&mut value, warning) {
            map.insert("order_link_warning".to_string(), Value::String(warning));
        }
        Ok(value)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn schedule_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_writable_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot schedule a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        validate_window(input.window_start, input.window_end)?;
        shipment.scheduled_at = Some(input.scheduled_at);
        shipment.window_start = input.window_start;
        shipment.window_end = input.window_end;
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        state.add_timeline(
            shipment.id,
            "rescheduled",
            None,
            None,
            &caller.actor,
            None,
            Some(format!(
                "Delivery scheduled for {}",
                input.scheduled_at.to_rfc3339()
            )),
        );
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn reschedule_shipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RescheduleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Shipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_writable_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot reschedule a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        validate_window(input.window_start, input.window_end)?;
        let previous = shipment
            .scheduled_at
            .map(|value| value.to_rfc3339())
            .unwrap_or_else(|| "unscheduled".to_string());
        shipment.scheduled_at = Some(input.scheduled_at);
        shipment.window_start = input.window_start;
        shipment.window_end = input.window_end;
        shipment.updated_at = Utc::now();
        state.shipments.insert(shipment.id, shipment.clone());
        // Rescheduling is audited: the old and new times stay in
        // the timeline, and the customer is notified.
        state.add_timeline(
            shipment.id,
            "rescheduled",
            None,
            None,
            &caller.actor,
            None,
            Some(format!(
                "Rescheduled from {previous} to {}: {reason}",
                input.scheduled_at.to_rfc3339()
            )),
        );
        state.notify(
            "logistics.shipment.rescheduled",
            &shipment.created_by,
            "Delivery rescheduled",
            &format!(
                "Shipment {} was rescheduled to {}. Reason: {reason}",
                shipment.tracking_number,
                input.scheduled_at.to_rfc3339()
            ),
            Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
        );
        Ok(shipment)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_proof(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateProofRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        can_operate_shipment(&ctx, &state, &caller, &shipment)?;
        if shipment.status != ShipmentStatus::OutForDelivery {
            return Err(UnifiedError::conflict(format!(
                "Proof of delivery requires status \"out_for_delivery\"; this shipment is \"{}\"",
                shipment.status.as_str()
            )));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_NAME_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", 1024)?;
        if !valid_sha256_hex(&input.content_sha256) {
            return Err(UnifiedError::bad_request(
                "content_sha256 must be a 64-character lowercase hex SHA-256 digest",
            ));
        }
        let signature_target = optional_text(
            &input.signature_storage_target,
            "signature_storage_target",
            MAX_NAME_LEN,
        )?;
        let signature_key =
            optional_text(&input.signature_storage_key, "signature_storage_key", 1024)?;
        if signature_target.is_some() != signature_key.is_some() {
            return Err(UnifiedError::bad_request(
                "signature_storage_target and signature_storage_key must be provided together",
            ));
        }
        let recipient_name = require_text(&input.recipient_name, "recipient_name", MAX_NAME_LEN)?;
        let delivered_at = input.delivered_at.unwrap_or_else(Utc::now);
        let now = Utc::now();
        let proof = ProofOfDelivery {
            id: Uuid::now_v7(),
            shipment_id: shipment.id,
            storage_target,
            storage_key,
            content_sha256: input.content_sha256,
            signature_storage_target: signature_target,
            signature_storage_key: signature_key,
            recipient_name: recipient_name.clone(),
            delivered_at,
            actor: caller.actor.clone(),
            created_at: now,
        };
        state.proofs.insert(proof.id, proof.clone());
        // Proof closes the delivery: the state machine moves the
        // shipment to `delivered` through the same gate as any
        // other transition (out_for_delivery -> delivered is the
        // only legal entry to delivered).
        apply_transition(
            &state,
            &mut shipment,
            ShipmentStatus::Delivered,
            &caller.actor,
            None,
            None,
            Some(format!("Proof of delivery recorded for {recipient_name}")),
        )?;
        state.add_timeline(
            shipment.id,
            "proof_recorded",
            Some("out_for_delivery"),
            Some("delivered"),
            &caller.actor,
            None,
            input.note.clone(),
        );
        state.emit(
            "logistics.proof.created",
            json!({
                "proof_id": proof.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "recipient_name": recipient_name,
                "delivered_at": delivered_at.to_rfc3339(),
            }),
        );
        state.notify(
            "logistics.shipment.delivered",
            &shipment.created_by,
            "Shipment delivered",
            &format!(
                "Shipment {} was delivered to {recipient_name}.",
                shipment.tracking_number
            ),
            Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
        );
        Ok(serde_json::to_value(&proof).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_proofs(
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
        let shipment = load_shipment(&state, &path)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        let params = query.into_inner();
        let mut items: Vec<ProofOfDelivery> = state
            .proofs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|proof| proof.shipment_id == shipment.id)
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

pub(super) async fn create_exception(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateExceptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut shipment = load_shipment(&state, &path)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        can_operate_shipment(&ctx, &state, &caller, &shipment)?;
        let input = body.into_inner();
        let exception_type = match input.exception_type.as_str() {
            "damaged" | "refused" | "address_issue" | "weather" | "lost" | "other" => {
                input.exception_type.clone()
            }
            other => {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown exception type: {other} (expected damaged, refused, address_issue, weather, lost, or other)"
                )));
            }
        };
        let description = require_text(&input.description, "description", MAX_NOTE_LEN)?;
        let location = optional_text(&input.location, "location", MAX_ADDRESS_LEN)?;
        // The exception opens only from a live, non-terminal status
        // and only when no exception is already open for this
        // shipment. The state-machine gate is checked BEFORE the
        // exception row is stored, so a rejected transition can
        // never leave an orphan exception behind.
        if shipment.status.is_terminal() {
            return Err(UnifiedError::conflict(format!(
                "Cannot open an exception on a shipment in terminal status \"{}\"",
                shipment.status.as_str()
            )));
        }
        if !allowed_transitions(shipment.status).contains(&ShipmentStatus::Exception) {
            return Err(UnifiedError::conflict(format!(
                "Cannot open an exception while the shipment is \"{}\"; exceptions open from picked_up, in_transit, or out_for_delivery",
                shipment.status.as_str()
            )));
        }
        if state.exceptions.iter().any(|entry| {
            entry.value().shipment_id == shipment.id
                && exception_is_open(entry.value().status.as_str())
        }) {
            return Err(UnifiedError::conflict(
                "This shipment already has an open exception",
            ));
        }
        let previous_status = shipment.status.as_str().to_string();
        let now = Utc::now();
        let exception = DeliveryException {
            id: Uuid::now_v7(),
            shipment_id: shipment.id,
            exception_type: exception_type.clone(),
            description: description.clone(),
            status: "open".to_string(),
            previous_status: previous_status.clone(),
            resolution: None,
            actor: caller.actor.clone(),
            resolved_at: None,
            created_at: now,
            updated_at: now,
        };
        state.exceptions.insert(exception.id, exception.clone());
        apply_transition(
            &state,
            &mut shipment,
            ShipmentStatus::Exception,
            &caller.actor,
            Some(&description),
            location,
            Some(format!("Exception opened: {exception_type}")),
        )?;
        state.add_timeline(
            shipment.id,
            "exception_opened",
            Some(&previous_status),
            Some("exception"),
            &caller.actor,
            None,
            Some(format!("{exception_type}: {description}")),
        );
        state.emit(
            "logistics.exception.created",
            json!({
                "exception_id": exception.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "type": exception_type,
                "previous_status": previous_status,
            }),
        );
        state.notify(
            "logistics.exception.created",
            &shipment.created_by,
            "Delivery exception",
            &format!(
                "Shipment {} hit a delivery exception ({exception_type}): {description}",
                shipment.tracking_number
            ),
            Some(format!("/api/v1/logistics/shipments/{}", shipment.id)),
        );
        Ok(serde_json::to_value(&exception).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_exceptions(
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
        let shipment = load_shipment(&state, &path)?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<DeliveryException> = state
            .exceptions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|exception| {
                if exception.shipment_id != shipment.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &exception.status != status {
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

pub(super) async fn resolve_exception(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ResolveExceptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryException> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut exception = load_exception(&state, &path)?;
        if !exception_is_open(exception.status.as_str()) {
            return Err(UnifiedError::conflict(format!(
                "Exception is already \"{}\"; only open exceptions can be resolved",
                exception.status
            )));
        }
        let input = body.into_inner();
        let resolution = require_text(&input.resolution, "resolution", MAX_NOTE_LEN)?;
        let mut shipment = load_shipment(&state, &exception.shipment_id.to_string())?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status != ShipmentStatus::Exception {
            return Err(UnifiedError::conflict(format!(
                "Shipment is \"{}\", not in exception status; the exception cannot be resolved",
                shipment.status.as_str()
            )));
        }
        let now = Utc::now();
        exception.status = "resolved".to_string();
        exception.resolution = Some(resolution.clone());
        exception.resolved_at = Some(now);
        exception.updated_at = now;
        state.exceptions.insert(exception.id, exception.clone());
        if input.resume_delivery {
            apply_transition(
                &state,
                &mut shipment,
                ShipmentStatus::InTransit,
                &caller.actor,
                Some(&resolution),
                None,
                Some("Exception resolved; delivery resumed".to_string()),
            )?;
        } else {
            apply_transition(
                &state,
                &mut shipment,
                ShipmentStatus::Returned,
                &caller.actor,
                Some(&resolution),
                None,
                Some("Exception resolved; shipment returned".to_string()),
            )?;
            if let Some(mut entry) = state.exceptions.get_mut(&exception.id) {
                entry.status = "returned".to_string();
            }
            exception.status = "returned".to_string();
        }
        state.add_timeline(
            shipment.id,
            "exception_resolved",
            Some("exception"),
            Some(shipment.status.as_str()),
            &caller.actor,
            None,
            Some(resolution.clone()),
        );
        state.emit(
            "logistics.exception.resolved",
            json!({
                "exception_id": exception.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "resolution": resolution,
                "resumed": input.resume_delivery,
            }),
        );
        Ok(exception)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn return_exception(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReturnExceptionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryException> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut exception = load_exception(&state, &path)?;
        if !exception_is_open(exception.status.as_str()) {
            return Err(UnifiedError::conflict(format!(
                "Exception is already \"{}\"; only open exceptions can be returned",
                exception.status
            )));
        }
        let input = body.into_inner();
        let reason = input
            .reason
            .as_deref()
            .map(|value| require_text(value, "reason", MAX_REASON_LEN))
            .transpose()?
            .unwrap_or_else(|| "Returned to sender".to_string());
        let mut shipment = load_shipment(&state, &exception.shipment_id.to_string())?;
        require_record_in_company_scope(&shipment.company_id, &_scope)?;
        if shipment.status != ShipmentStatus::Exception {
            return Err(UnifiedError::conflict(format!(
                "Shipment is \"{}\", not in exception status; the exception cannot be returned",
                shipment.status.as_str()
            )));
        }
        let now = Utc::now();
        exception.status = "returned".to_string();
        exception.resolution = Some(reason.clone());
        exception.resolved_at = Some(now);
        exception.updated_at = now;
        state.exceptions.insert(exception.id, exception.clone());
        apply_transition(
            &state,
            &mut shipment,
            ShipmentStatus::Returned,
            &caller.actor,
            Some(&reason),
            None,
            Some("Shipment returned to sender".to_string()),
        )?;
        state.emit(
            "logistics.exception.returned",
            json!({
                "exception_id": exception.id.to_string(),
                "shipment_id": shipment.id.to_string(),
                "tracking_number": shipment.tracking_number,
                "reason": reason,
            }),
        );
        Ok(exception)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_exception(shipment_id: Uuid) -> DeliveryException {
        let now = Utc::now();
        DeliveryException {
            id: Uuid::now_v7(),
            shipment_id,
            exception_type: "lost".to_string(),
            description: "parcel lost".to_string(),
            status: "open".to_string(),
            previous_status: "in_transit".to_string(),
            resolution: None,
            actor: "tester".to_string(),
            resolved_at: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_proof(shipment_id: Uuid) -> ProofOfDelivery {
        let now = Utc::now();
        ProofOfDelivery {
            id: Uuid::now_v7(),
            shipment_id,
            storage_target: "local".to_string(),
            storage_key: "proofs/1.jpg".to_string(),
            content_sha256: "a".repeat(64),
            signature_storage_target: None,
            signature_storage_key: None,
            recipient_name: "Recipient".to_string(),
            delivered_at: now,
            actor: "tester".to_string(),
            created_at: now,
        }
    }

    fn test_movement(shipment_id: Option<Uuid>) -> StockMovement {
        StockMovement {
            id: Uuid::now_v7(),
            warehouse_id: Uuid::now_v7(),
            asset_id: Uuid::now_v7(),
            direction: "out".to_string(),
            quantity: 1,
            quantity_after: 0,
            reason: "shipment".to_string(),
            shipment_id,
            actor: "tester".to_string(),
            created_at: Utc::now(),
        }
    }

    fn test_stop(shipment_id: Option<Uuid>) -> RouteStop {
        let now = Utc::now();
        RouteStop {
            id: Uuid::now_v7(),
            route_id: Uuid::now_v7(),
            shipment_id,
            sequence: 1,
            address: "1 Main Street".to_string(),
            latitude: Some(0.0),
            longitude: Some(0.0),
            status: "pending".to_string(),
            note: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn exception_is_open_accepts_only_open() {
        assert!(exception_is_open("open"));
        assert!(!exception_is_open("investigating"));
        assert!(!exception_is_open("resolved"));
        assert!(!exception_is_open("returned"));
        assert!(!exception_is_open(""));
    }

    #[test]
    fn remove_shipment_references_clears_everything_pointing_at_the_shipment() {
        let state = LogisticsState::default();
        let shipment_id = Uuid::now_v7();
        let other_id = Uuid::now_v7();
        let exception = test_exception(shipment_id);
        let other_exception = test_exception(other_id);
        state.exceptions.insert(exception.id, exception);
        state.exceptions.insert(other_exception.id, other_exception);
        let proof = test_proof(shipment_id);
        let other_proof = test_proof(other_id);
        state.proofs.insert(proof.id, proof);
        state.proofs.insert(other_proof.id, other_proof);
        let movement = test_movement(Some(shipment_id));
        let unlinked_movement = test_movement(None);
        state.stock_movements.insert(movement.id, movement);
        state
            .stock_movements
            .insert(unlinked_movement.id, unlinked_movement);
        let stop = test_stop(Some(shipment_id));
        let other_stop = test_stop(Some(other_id));
        let stop_id = stop.id;
        let other_stop_id = other_stop.id;
        state.route_stops.insert(stop.id, stop);
        state.route_stops.insert(other_stop.id, other_stop);

        remove_shipment_references(&state, shipment_id);

        assert!(state
            .exceptions
            .iter()
            .all(|entry| entry.value().shipment_id == other_id));
        assert_eq!(state.exceptions.len(), 1);
        assert!(state.proofs.iter().all(|entry| entry.value().shipment_id == other_id));
        assert_eq!(state.proofs.len(), 1);
        assert!(state
            .stock_movements
            .iter()
            .all(|entry| entry.value().shipment_id.is_none()));
        assert_eq!(state.stock_movements.len(), 1);
        // The stop itself survives, but the link is cleared.
        let kept = state.route_stops.get(&stop_id).expect("stop survives");
        assert_eq!(kept.shipment_id, None);
        let untouched = state
            .route_stops
            .get(&other_stop_id)
            .expect("other stop survives");
        assert_eq!(untouched.shipment_id, Some(other_id));
    }
}
