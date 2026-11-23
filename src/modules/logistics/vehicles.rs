//! Logistics module — shipments, fleet, warehouses, delivery — Vehicle handlers.
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

pub(super) fn valid_vehicle_status(raw: &str) -> UnifiedResult<String> {
    match raw {
        "available" | "in_service" | "maintenance" | "retired" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown vehicle status: {other} (expected available, in_service, maintenance, or retired)"
        ))),
    }
}

pub(super) fn valid_vehicle_type(raw: &str) -> UnifiedResult<String> {
    match raw {
        "van" | "truck" | "motorcycle" | "car" | "other" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown vehicle type: {other} (expected van, truck, motorcycle, car, or other)"
        ))),
    }
}

pub(super) async fn list_vehicles(
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
        let type_filter = params.filters.get("vehicle_type").cloned();
        let driver_filter = params.filters.get("assigned_driver_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Vehicle> = state
            .vehicles
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|vehicle| in_company_scope(&vehicle.company_id, &_scope))
            .filter(|vehicle| {
                if let Some(status) = &status_filter {
                    if &vehicle.status != status {
                        return false;
                    }
                }
                if let Some(vehicle_type) = &type_filter {
                    if &vehicle.vehicle_type != vehicle_type {
                        return false;
                    }
                }
                if let Some(driver_id) = &driver_filter {
                    if vehicle.assigned_driver_id.map(|id| id.to_string()) != Some(driver_id.clone()) {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if vehicle.company_id.map(|id| id.to_string()) != Some(company_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() && !vehicle.plate.to_lowercase().contains(&search) {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.plate.cmp(&b.plate));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateVehicleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let plate = require_text(&input.plate, "plate", MAX_PLATE_LEN)?.to_uppercase();
        if state
            .vehicles
            .iter()
            .any(|entry| entry.value().plate.eq_ignore_ascii_case(&plate))
        {
            return Err(UnifiedError::conflict(format!(
                "A vehicle with plate \"{plate}\" already exists"
            )));
        }
        let vehicle_type = valid_vehicle_type(&input.vehicle_type)?;
        if !input.capacity_kg.is_finite() || input.capacity_kg <= 0.0 || input.capacity_kg > MAX_WEIGHT_KG {
            return Err(UnifiedError::bad_request(format!(
                "capacity_kg must be > 0 and <= {MAX_WEIGHT_KG}"
            )));
        }
        let status = input
            .status
            .as_deref()
            .map(valid_vehicle_status)
            .transpose()?
            .unwrap_or_else(|| "available".to_string());
        if let Some(driver_id) = input.assigned_driver_id {
            if !state.drivers.contains_key(&driver_id) {
                return Err(UnifiedError::bad_request(format!("Driver not found: {driver_id}")));
            }
        }
        let company_id =
            parse_optional_company(&require_company_match(&_tenant, &req, &input.company_id)?)?;
        let now = Utc::now();
        let vehicle = Vehicle {
            id: Uuid::now_v7(),
            plate,
            vehicle_type,
            capacity_kg: input.capacity_kg,
            status,
            assigned_driver_id: input.assigned_driver_id,
            company_id,
            maintenance: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        state.vehicles.insert(vehicle.id, vehicle.clone());
        state.emit(
            "logistics.vehicle.created",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "created_by": caller.actor,
            }),
        );
        Ok(vehicle)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        let vehicle = load_vehicle(&state, &path)?;
        require_record_in_company_scope(&vehicle.company_id, &_scope)?;
        require_record_in_company_scope(&vehicle.company_id, &_scope)?;
        Ok(vehicle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateVehicleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut vehicle = load_vehicle(&state, &path)?;
        require_record_writable_in_company_scope(&vehicle.company_id, &_scope)?;
        let input = body.into_inner();
        if let Some(plate) = &input.plate {
            let plate = require_text(plate, "plate", MAX_PLATE_LEN)?.to_uppercase();
            if state.vehicles.iter().any(|entry| {
                entry.value().id != vehicle.id && entry.value().plate.eq_ignore_ascii_case(&plate)
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A vehicle with plate \"{plate}\" already exists"
                )));
            }
            vehicle.plate = plate;
        }
        if let Some(vehicle_type) = &input.vehicle_type {
            vehicle.vehicle_type = valid_vehicle_type(vehicle_type)?;
        }
        if let Some(capacity) = input.capacity_kg {
            if !capacity.is_finite() || capacity <= 0.0 || capacity > MAX_WEIGHT_KG {
                return Err(UnifiedError::bad_request(format!(
                    "capacity_kg must be > 0 and <= {MAX_WEIGHT_KG}"
                )));
            }
            vehicle.capacity_kg = capacity;
        }
        if let Some(status) = &input.status {
            vehicle.status = valid_vehicle_status(status)?;
        }
        if let Some(driver_id) = input.assigned_driver_id {
            if let Some(id) = driver_id {
                if !state.drivers.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Driver not found: {id}")));
                }
            }
            vehicle.assigned_driver_id = driver_id;
        }
        if let Some(company_id) = &input.company_id {
            vehicle.company_id = match company_id.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => {
                    require_company_match_for(&_scope, &Some(raw.to_string()))?;
                    Some(parse_uuid(raw, "company")?)
                }
            };
        }
        vehicle.updated_at = Utc::now();
        state.vehicles.insert(vehicle.id, vehicle.clone());
        state.emit(
            "logistics.vehicle.updated",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "updated_by": caller.actor,
            }),
        );
        Ok(vehicle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Clear every reference to a vehicle being deleted: the driver's
/// current vehicle and the route assignment. (Same pattern as the
/// vehicle-clear loop in `delete_driver`.)
pub(super) fn clear_vehicle_references(state: &LogisticsState, vehicle_id: Uuid) {
    for mut entry in state.drivers.iter_mut() {
        if entry.value().current_vehicle_id == Some(vehicle_id) {
            entry.value_mut().current_vehicle_id = None;
            entry.value_mut().updated_at = Utc::now();
        }
    }
    for mut entry in state.routes.iter_mut() {
        if entry.value().vehicle_id == Some(vehicle_id) {
            entry.value_mut().vehicle_id = None;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

pub(super) async fn delete_vehicle(
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
        let vehicle = load_vehicle(&state, &path)?;
        require_record_writable_in_company_scope(&vehicle.company_id, &_scope)?;
        if state.shipments.iter().any(|entry| {
            entry.value().vehicle_id == Some(vehicle.id)
                && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "Vehicle is assigned to a live shipment and cannot be deleted",
            ));
        }
        state.vehicles.remove(&vehicle.id);
        clear_vehicle_references(&state, vehicle.id);
        state.emit(
            "logistics.vehicle.deleted",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "vehicle_id": vehicle.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn assign_vehicle(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignVehicleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vehicle> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut vehicle = load_vehicle(&state, &path)?;
        require_record_writable_in_company_scope(&vehicle.company_id, &_scope)?;
        let mut driver = load_driver(&state, &body.driver_id.to_string())?;
        if vehicle.status == "retired" {
            return Err(UnifiedError::conflict(
                "A retired vehicle cannot take a driver assignment",
            ));
        }
        if driver.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Driver is \"{}\"; only an active driver can be assigned",
                driver.status
            )));
        }
        // One driver drives one vehicle at a time: clear any other
        // vehicle that still points at this driver.
        for mut entry in state.vehicles.iter_mut() {
            if entry.value().assigned_driver_id == Some(driver.id) && entry.key() != &vehicle.id {
                entry.value_mut().assigned_driver_id = None;
                entry.value_mut().updated_at = Utc::now();
            }
        }
        vehicle.assigned_driver_id = Some(driver.id);
        vehicle.updated_at = Utc::now();
        state.vehicles.insert(vehicle.id, vehicle.clone());
        driver.current_vehicle_id = Some(vehicle.id);
        driver.updated_at = Utc::now();
        state.drivers.insert(driver.id, driver.clone());
        state.emit(
            "logistics.vehicle.assigned",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "assigned_by": caller.actor,
            }),
        );
        Ok(vehicle)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn list_maintenance(
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
        let vehicle = load_vehicle(&state, &path)?;
        require_record_in_company_scope(&vehicle.company_id, &_scope)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<MaintenanceReminder> = vehicle
            .maintenance
            .iter()
            .cloned()
            .filter(|reminder| {
                if let Some(status) = &status_filter {
                    if &reminder.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.due_date.cmp(&b.due_date));
        let page = paginate(items, &params);
        Ok(serde_json::to_value(page).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn create_maintenance(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateMaintenanceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceReminder> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut vehicle = load_vehicle(&state, &path)?;
        require_record_in_company_scope(&vehicle.company_id, &_scope)?;
        let description = require_text(&body.description, "description", MAX_NOTE_LEN)?;
        let now = Utc::now();
        let mut reminder = MaintenanceReminder {
            id: Uuid::now_v7(),
            description: description.clone(),
            due_date: body.due_date,
            status: "pending".to_string(),
            scheduler_job_id: None,
            scheduler_status: "not_scheduled".to_string(),
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        schedule_maintenance_reminder(&state, vehicle.id, &mut reminder);
        vehicle.maintenance.push(reminder.clone());
        vehicle.updated_at = now;
        state.vehicles.insert(vehicle.id, vehicle.clone());
        state.emit(
            "logistics.vehicle.maintenance_created",
            json!({
                "vehicle_id": vehicle.id.to_string(),
                "plate": vehicle.plate,
                "reminder_id": reminder.id.to_string(),
                "due_date": reminder.due_date.to_rfc3339(),
                "scheduler_status": reminder.scheduler_status,
                "created_by": caller.actor,
            }),
        );
        Ok(reminder)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_vehicle_status_accepts_known_values() {
        for status in ["available", "in_service", "maintenance", "retired"] {
            assert_eq!(valid_vehicle_status(status).unwrap(), status);
        }
        assert!(valid_vehicle_status("flying").is_err());
        assert!(valid_vehicle_status("").is_err());
        assert!(valid_vehicle_status("Available").is_err());
    }

    #[test]
    fn valid_vehicle_type_accepts_known_values() {
        for kind in ["van", "truck", "motorcycle", "car", "other"] {
            assert_eq!(valid_vehicle_type(kind).unwrap(), kind);
        }
        assert!(valid_vehicle_type("boat").is_err());
        assert!(valid_vehicle_type("").is_err());
    }

    fn test_driver_with_vehicle(vehicle_id: Option<Uuid>, user_id: &str) -> Driver {
        let now = Utc::now();
        Driver {
            id: Uuid::now_v7(),
            user_id: user_id.to_string(),
            license_number: "LIC-1".to_string(),
            phone: None,
            status: "active".to_string(),
            current_vehicle_id: vehicle_id,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_route_with_vehicle(vehicle_id: Option<Uuid>) -> DeliveryRoute {
        let now = Utc::now();
        DeliveryRoute {
            id: Uuid::now_v7(),
            name: "Route".to_string(),
            driver_id: None,
            vehicle_id,
            route_date: now,
            start_latitude: None,
            start_longitude: None,
            status: "planned".to_string(),
            company_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn clear_vehicle_references_clears_driver_and_route_links() {
        let state = LogisticsState::default();
        let vehicle_id = Uuid::now_v7();
        let other_vehicle_id = Uuid::now_v7();
        let driver = test_driver_with_vehicle(Some(vehicle_id), "user-1");
        let other_driver = test_driver_with_vehicle(Some(other_vehicle_id), "user-2");
        let route = test_route_with_vehicle(Some(vehicle_id));
        let other_route = test_route_with_vehicle(Some(other_vehicle_id));
        let driver_id = driver.id;
        let other_driver_id = other_driver.id;
        let route_id = route.id;
        let other_route_id = other_route.id;
        state.drivers.insert(driver.id, driver);
        state.drivers.insert(other_driver.id, other_driver);
        state.routes.insert(route.id, route);
        state.routes.insert(other_route.id, other_route);

        clear_vehicle_references(&state, vehicle_id);

        assert_eq!(
            state.drivers.get(&driver_id).unwrap().current_vehicle_id,
            None
        );
        assert_eq!(
            state
                .drivers
                .get(&other_driver_id)
                .unwrap()
                .current_vehicle_id,
            Some(other_vehicle_id)
        );
        assert_eq!(state.routes.get(&route_id).unwrap().vehicle_id, None);
        assert_eq!(
            state.routes.get(&other_route_id).unwrap().vehicle_id,
            Some(other_vehicle_id)
        );
    }
}
