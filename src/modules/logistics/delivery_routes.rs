//! Logistics module — shipments, fleet, warehouses, delivery — Delivery route handlers.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
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

pub(super) fn valid_route_status(raw: &str) -> UnifiedResult<String> {
    match raw {
        "planned" | "active" | "completed" | "cancelled" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown route status: {other} (expected planned, active, completed, or cancelled)"
        ))),
    }
}

pub(super) async fn list_routes(
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
        let driver_filter = params.filters.get("driver_id").cloned();
        let vehicle_filter = params.filters.get("vehicle_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<DeliveryRoute> = state
            .routes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|route| in_company_scope(&route.company_id, &_scope))
            .filter(|route| {
                if let Some(status) = &status_filter {
                    if &route.status != status {
                        return false;
                    }
                }
                if let Some(driver_id) = &driver_filter {
                    if route.driver_id.map(|id| id.to_string()) != Some(driver_id.clone()) {
                        return false;
                    }
                }
                if let Some(vehicle_id) = &vehicle_filter {
                    if route.vehicle_id.map(|id| id.to_string()) != Some(vehicle_id.clone()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if route.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() && !route.name.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.route_date.cmp(&a.route_date));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateRouteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        if let Some(latitude) = input.start_latitude {
            validate_latitude(latitude, "start_latitude")?;
        }
        if let Some(longitude) = input.start_longitude {
            validate_longitude(longitude, "start_longitude")?;
        }
        if input.start_latitude.is_some() != input.start_longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "start_latitude and start_longitude must be provided together",
            ));
        }
        if let Some(driver_id) = input.driver_id {
            if !state.drivers.contains_key(&driver_id) {
                return Err(UnifiedError::bad_request(format!("Driver not found: {driver_id}")));
            }
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if !state.vehicles.contains_key(&vehicle_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Vehicle not found: {vehicle_id}"
                )));
            }
        }
        let status = input
            .status
            .as_deref()
            .map(valid_route_status)
            .transpose()?
            .unwrap_or_else(|| "planned".to_string());
        let company_id =
            parse_optional_company(&require_company_match(&_tenant, &req, &input.company_id)?)?;
        let now = Utc::now();
        let route = DeliveryRoute {
            id: Uuid::now_v7(),
            name,
            driver_id: input.driver_id,
            vehicle_id: input.vehicle_id,
            route_date: input.route_date,
            start_latitude: input.start_latitude,
            start_longitude: input.start_longitude,
            status,
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.routes.insert(route.id, route.clone());
        state.emit(
            "logistics.route.created",
            json!({
                "route_id": route.id.to_string(),
                "name": route.name,
                "created_by": caller.actor,
            }),
        );
        Ok(route)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let route = load_route(&state, &path)?;
        require_record_in_company_scope(&route.company_id, &_scope)?;
        require_record_in_company_scope(&route.company_id, &_scope)?;
        Ok(route)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRouteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut route = load_route(&state, &path)?;
        require_record_writable_in_company_scope(&route.company_id, &_scope)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "A {} route cannot be edited",
                route.status
            )));
        }
        let input = body.into_inner();
        if let Some(name) = &input.name {
            route.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(driver_id) = input.driver_id {
            if let Some(id) = driver_id {
                if !state.drivers.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Driver not found: {id}")));
                }
            }
            route.driver_id = driver_id;
        }
        if let Some(vehicle_id) = input.vehicle_id {
            if let Some(id) = vehicle_id {
                if !state.vehicles.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Vehicle not found: {id}")));
                }
            }
            route.vehicle_id = vehicle_id;
        }
        if let Some(route_date) = input.route_date {
            route.route_date = route_date;
        }
        if let Some(latitude) = input.start_latitude {
            if let Some(value) = latitude {
                validate_latitude(value, "start_latitude")?;
            }
            route.start_latitude = latitude;
        }
        if let Some(longitude) = input.start_longitude {
            if let Some(value) = longitude {
                validate_longitude(value, "start_longitude")?;
            }
            route.start_longitude = longitude;
        }
        if route.start_latitude.is_some() != route.start_longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "start_latitude and start_longitude must be provided together",
            ));
        }
        if let Some(status) = &input.status {
            route.status = valid_route_status(status)?;
        }
        if let Some(company_id) = &input.company_id {
            route.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => {
                    require_company_match_for(&_scope, &Some(raw.to_string()))?;
                    Some(parse_uuid(raw, "company")?)
                }
            };
        }
        route.updated_at = Utc::now();
        state.routes.insert(route.id, route.clone());
        state.emit(
            "logistics.route.updated",
            json!({
                "route_id": route.id.to_string(),
                "name": route.name,
                "updated_by": caller.actor,
            }),
        );
        Ok(route)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_route(
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
        let route = load_route(&state, &path)?;
        require_record_writable_in_company_scope(&route.company_id, &_scope)?;
        if route.status == "active" {
            return Err(UnifiedError::conflict(
                "An active route cannot be deleted; complete or cancel it first",
            ));
        }
        state.routes.remove(&route.id);
        let stale: Vec<Uuid> = state
            .route_stops
            .iter()
            .filter(|entry| entry.value().route_id == route.id)
            .map(|entry| *entry.key())
            .collect();
        for id in stale {
            state.route_stops.remove(&id);
        }
        state.emit(
            "logistics.route.deleted",
            json!({
                "route_id": route.id.to_string(),
                "name": route.name,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "route_id": route.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn assign_route(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignRouteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeliveryRoute> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut route = load_route(&state, &path)?;
        require_record_writable_in_company_scope(&route.company_id, &_scope)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "A {} route cannot be assigned",
                route.status
            )));
        }
        let driver = load_driver(&state, &body.driver_id.to_string())?;
        let vehicle = load_vehicle(&state, &body.vehicle_id.to_string())?;
        if driver.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Driver is \"{}\"; only an active driver can be assigned",
                driver.status
            )));
        }
        if vehicle.status == "retired" {
            return Err(UnifiedError::conflict(
                "A retired vehicle cannot be assigned to a route",
            ));
        }
        route.driver_id = Some(driver.id);
        route.vehicle_id = Some(vehicle.id);
        route.updated_at = Utc::now();
        state.routes.insert(route.id, route.clone());
        state.emit(
            "logistics.route.assigned",
            json!({
                "route_id": route.id.to_string(),
                "driver_id": driver.id.to_string(),
                "vehicle_id": vehicle.id.to_string(),
                "assigned_by": caller.actor,
            }),
        );
        Ok(route)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_route_stops(
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
        let route = load_route(&state, &path)?;
        require_record_in_company_scope(&route.company_id, &_scope)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<RouteStop> = state
            .route_stops
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stop| {
                if stop.route_id != route.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &stop.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|stop| stop.sequence);
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_route_stop(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRouteStopRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RouteStop> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let route = load_route(&state, &path)?;
        require_record_in_company_scope(&route.company_id, &_scope)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "Stops cannot be added to a {} route",
                route.status
            )));
        }
        let input = body.into_inner();
        let address = require_text(&input.address, "address", MAX_ADDRESS_LEN)?;
        if let Some(latitude) = input.latitude {
            validate_latitude(latitude, "latitude")?;
        }
        if let Some(longitude) = input.longitude {
            validate_longitude(longitude, "longitude")?;
        }
        if input.latitude.is_some() != input.longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be provided together",
            ));
        }
        if let Some(shipment_id) = input.shipment_id {
            if !state.shipments.contains_key(&shipment_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Shipment not found: {shipment_id}"
                )));
            }
        }
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let sequence = state
            .route_stops
            .iter()
            .filter(|entry| entry.value().route_id == route.id)
            .map(|entry| entry.value().sequence)
            .max()
            .unwrap_or(0)
            + 1;
        let now = Utc::now();
        let stop = RouteStop {
            id: Uuid::now_v7(),
            route_id: route.id,
            shipment_id: input.shipment_id,
            sequence,
            address,
            latitude: input.latitude,
            longitude: input.longitude,
            status: "pending".to_string(),
            note,
            created_at: now,
            updated_at: now,
        };
        state.route_stops.insert(stop.id, stop.clone());
        state.emit(
            "logistics.route_stop.created",
            json!({
                "stop_id": stop.id.to_string(),
                "route_id": route.id.to_string(),
                "sequence": sequence,
                "created_by": caller.actor,
            }),
        );
        Ok(stop)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_route_stop(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateRouteStopRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RouteStop> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let (route_raw, stop_raw) = path.into_inner();
        let route = load_route(&state, &route_raw)?;
        require_record_in_company_scope(&route.company_id, &_scope)?;
        let mut stop = load_route_stop(&state, route.id, &stop_raw)?;
        let input = body.into_inner();
        if let Some(shipment_id) = input.shipment_id {
            if let Some(id) = shipment_id {
                if !state.shipments.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Shipment not found: {id}")));
                }
            }
            stop.shipment_id = shipment_id;
        }
        if let Some(address) = &input.address {
            stop.address = require_text(address, "address", MAX_ADDRESS_LEN)?;
        }
        if let Some(latitude) = input.latitude {
            if let Some(value) = latitude {
                validate_latitude(value, "latitude")?;
            }
            stop.latitude = latitude;
        }
        if let Some(longitude) = input.longitude {
            if let Some(value) = longitude {
                validate_longitude(value, "longitude")?;
            }
            stop.longitude = longitude;
        }
        if stop.latitude.is_some() != stop.longitude.is_some() {
            return Err(UnifiedError::bad_request(
                "latitude and longitude must be provided together",
            ));
        }
        if let Some(status) = &input.status {
            match status.as_str() {
                "pending" | "visited" | "skipped" => stop.status = status.clone(),
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown stop status: {other} (expected pending, visited, or skipped)"
                    )));
                }
            }
        }
        if let Some(note) = &input.note {
            stop.note = match note.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "note", MAX_NOTE_LEN)?),
            };
        }
        stop.updated_at = Utc::now();
        state.route_stops.insert(stop.id, stop.clone());
        state.emit(
            "logistics.route_stop.updated",
            json!({
                "stop_id": stop.id.to_string(),
                "route_id": route.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(stop)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn delete_route_stop(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let (route_raw, stop_raw) = path.into_inner();
        let route = load_route(&state, &route_raw)?;
        require_record_in_company_scope(&route.company_id, &_scope)?;
        let stop = load_route_stop(&state, route.id, &stop_raw)?;
        state.route_stops.remove(&stop.id);
        // Renumber the remaining stops so sequences stay contiguous.
        let mut remaining: Vec<RouteStop> = state
            .route_stops
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.route_id == route.id)
            .collect();
        remaining.sort_by_key(|entry| entry.sequence);
        for (index, mut entry) in remaining.into_iter().enumerate() {
            entry.sequence = (index + 1) as i64;
            entry.updated_at = Utc::now();
            state.route_stops.insert(entry.id, entry);
        }
        state.emit(
            "logistics.route_stop.deleted",
            json!({
                "stop_id": stop.id.to_string(),
                "route_id": route.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "stop_id": stop.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Order stops by nearest neighbour from the route start (or the
/// first stop, when the route has no start coordinates) and assign
/// contiguous 1-based sequences on the returned vec, so the rows
/// stored in state and the vec returned to the caller always agree.
/// Callers must check every stop carries coordinates first; this
/// helper assumes they do.
pub(super) fn reorder_stops(
    route: &DeliveryRoute,
    stops: Vec<RouteStop>,
) -> UnifiedResult<Vec<RouteStop>> {
    if stops.is_empty() {
        return Ok(Vec::new());
    }
    let mut stops = stops;
    stops.sort_by_key(|stop| stop.sequence);
    let (mut current_lat, mut current_lng) = match (route.start_latitude, route.start_longitude) {
        (Some(lat), Some(lng)) => (lat, lng),
        _ => (
            stops[0].latitude.expect("coordinates checked above"),
            stops[0].longitude.expect("coordinates checked above"),
        ),
    };
    let mut remaining = stops;
    let mut ordered: Vec<RouteStop> = Vec::with_capacity(remaining.len());
    while !remaining.is_empty() {
        let mut best_index = 0;
        let mut best_distance = f64::MAX;
        for (index, stop) in remaining.iter().enumerate() {
            let distance = haversine_km(
                current_lat,
                current_lng,
                stop.latitude.expect("coordinates checked above"),
                stop.longitude.expect("coordinates checked above"),
            );
            if distance < best_distance {
                best_distance = distance;
                best_index = index;
            }
        }
        let next = remaining.remove(best_index);
        current_lat = next.latitude.expect("coordinates checked above");
        current_lng = next.longitude.expect("coordinates checked above");
        ordered.push(next);
    }
    for (index, stop) in ordered.iter_mut().enumerate() {
        stop.sequence = (index + 1) as i64;
        stop.updated_at = Utc::now();
    }
    Ok(ordered)
}

pub(super) async fn reorder_route(
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
        let route = load_route(&state, &path)?;
        require_record_writable_in_company_scope(&route.company_id, &_scope)?;
        if route.status == "completed" || route.status == "cancelled" {
            return Err(UnifiedError::conflict(format!(
                "A {} route cannot be reordered",
                route.status
            )));
        }
        let mut stops: Vec<RouteStop> = state
            .route_stops
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stop| stop.route_id == route.id)
            .collect();
        stops.sort_by_key(|stop| stop.sequence);
        if stops.len() < 2 {
            return Ok(json!({
                "route_id": route.id.to_string(),
                "method": "nearest_neighbour",
                "reordered": false,
                "reason": "Fewer than two stops; nothing to reorder.",
            }));
        }
        // Nearest-neighbour heuristic only (see the route note in
        // the module header). Every stop must carry coordinates;
        // otherwise the reorder is refused rather than guessed.
        for stop in &stops {
            if stop.latitude.is_none() || stop.longitude.is_none() {
                return Err(UnifiedError::unprocessable(
                    "All stops need latitude and longitude before a reorder; at least one stop has no coordinates",
                ));
            }
        }
        let ordered = reorder_stops(&route, stops)?;
        for stop in &ordered {
            state.route_stops.insert(stop.id, stop.clone());
        }
        state.emit(
            "logistics.route.reordered",
            json!({
                "route_id": route.id.to_string(),
                "method": "nearest_neighbour",
                "stop_count": ordered.len(),
                "reordered_by": caller.actor,
            }),
        );
        Ok(json!({
            "route_id": route.id.to_string(),
            "method": "nearest_neighbour",
            "reordered": true,
            "stop_count": ordered.len(),
            "stops": ordered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_route_status_accepts_known_values() {
        for status in ["planned", "active", "completed", "cancelled"] {
            assert_eq!(valid_route_status(status).unwrap(), status);
        }
        assert!(valid_route_status("delayed").is_err());
        assert!(valid_route_status("").is_err());
        assert!(valid_route_status("Active").is_err());
    }

    fn test_route() -> DeliveryRoute {
        let now = Utc::now();
        DeliveryRoute {
            id: Uuid::now_v7(),
            name: "Morning run".to_string(),
            driver_id: None,
            vehicle_id: None,
            route_date: now,
            start_latitude: Some(0.0),
            start_longitude: Some(0.0),
            status: "planned".to_string(),
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_stop(route_id: Uuid, sequence: i64, latitude: f64, longitude: f64) -> RouteStop {
        let now = Utc::now();
        RouteStop {
            id: Uuid::now_v7(),
            route_id,
            shipment_id: None,
            sequence,
            address: format!("Stop at {latitude},{longitude}"),
            latitude: Some(latitude),
            longitude: Some(longitude),
            status: "pending".to_string(),
            note: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn reorder_stops_returns_nearest_neighbour_order_with_fresh_sequences() {
        let route = test_route();
        // Stored order is far, near, mid; nearest neighbour from
        // (0, 0) must visit near (0, 1), then mid (0, 2), then far
        // (0, 10) — and the returned vec must carry the new
        // sequences 1, 2, 3, matching what is stored.
        let far = test_stop(route.id, 1, 0.0, 10.0);
        let near = test_stop(route.id, 2, 0.0, 1.0);
        let mid = test_stop(route.id, 3, 0.0, 2.0);
        let ordered = reorder_stops(&route, vec![far, near.clone(), mid.clone()])
            .expect("reorder succeeds");
        assert_eq!(ordered.len(), 3);
        assert_eq!(ordered[0].id, near.id);
        assert_eq!(ordered[1].id, mid.id);
        assert_eq!(ordered[2].address, "Stop at 0,10");
        for (index, stop) in ordered.iter().enumerate() {
            assert_eq!(stop.sequence, (index + 1) as i64);
        }
    }

    #[test]
    fn reorder_stops_empty_input_returns_empty() {
        let route = test_route();
        let ordered = reorder_stops(&route, vec![]).expect("reorder succeeds");
        assert!(ordered.is_empty());
    }
}
