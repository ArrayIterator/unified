//! Logistics module - shipments, fleet, warehouses, delivery - Driver handlers.
//!
//! Split from `src/modules/logistics.rs`; behaviour unchanged.

use super::constants::*;
use super::delivery_routes::*;
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

pub(super) fn valid_driver_status(raw: &str) -> UnifiedResult<String> {
    match raw {
        "active" | "inactive" | "on_leave" => Ok(raw.to_string()),
        other => Err(UnifiedError::bad_request(format!(
            "Unknown driver status: {other} (expected active, inactive, or on_leave)"
        ))),
    }
}

pub(super) async fn list_drivers(
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
        let user_filter = params.filters.get("user_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Driver> = state
            .drivers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|driver| {
                if let Some(status) = &status_filter {
                    if &driver.status != status {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &driver.user_id != user_id {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !driver.license_number.to_lowercase().contains(&search)
                    && !driver.user_id.to_lowercase().contains(&search)
                {
                    return false;
                }
                true
            })
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

pub(super) async fn create_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    body: web::Json<CreateDriverRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Driver> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_CREATE)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", MAX_NAME_LEN)?;
        if state
            .drivers
            .iter()
            .any(|entry| entry.value().user_id == user_id)
        {
            return Err(UnifiedError::conflict(format!(
                "A driver record already exists for user \"{user_id}\""
            )));
        }
        let license_number = require_text(&input.license_number, "license_number", MAX_LICENSE_LEN)?;
        let phone = optional_text(&input.phone, "phone", MAX_PHONE_LEN)?;
        let status = input
            .status
            .as_deref()
            .map(valid_driver_status)
            .transpose()?
            .unwrap_or_else(|| "active".to_string());
        if let Some(vehicle_id) = input.current_vehicle_id {
            if !state.vehicles.contains_key(&vehicle_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Vehicle not found: {vehicle_id}"
                )));
            }
        }
        let now = Utc::now();
        let driver = Driver {
            id: Uuid::now_v7(),
            user_id,
            license_number,
            phone,
            status,
            current_vehicle_id: input.current_vehicle_id,
            created_at: now,
            updated_at: now,
        };
        state.drivers.insert(driver.id, driver.clone());
        state.emit(
            "logistics.driver.created",
            json!({
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "created_by": caller.actor,
            }),
        );
        Ok(driver)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn get_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Driver> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_VIEW)?;
        load_driver(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(super) async fn update_driver(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<LogisticsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDriverRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Driver> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_LOGISTICS_MANAGE)?;
        let mut driver = load_driver(&state, &path)?;
        let input = body.into_inner();
        if let Some(license) = &input.license_number {
            driver.license_number = require_text(license, "license_number", MAX_LICENSE_LEN)?;
        }
        if let Some(phone) = &input.phone {
            driver.phone = match phone.as_deref() {
                None => None,
                Some(raw) if raw.trim().is_empty() => None,
                Some(raw) => Some(require_text(raw, "phone", MAX_PHONE_LEN)?),
            };
        }
        if let Some(status) = &input.status {
            driver.status = valid_driver_status(status)?;
        }
        if let Some(vehicle_id) = input.current_vehicle_id {
            if let Some(id) = vehicle_id {
                if !state.vehicles.contains_key(&id) {
                    return Err(UnifiedError::bad_request(format!("Vehicle not found: {id}")));
                }
            }
            driver.current_vehicle_id = vehicle_id;
        }
        driver.updated_at = Utc::now();
        state.drivers.insert(driver.id, driver.clone());
        state.emit(
            "logistics.driver.updated",
            json!({
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "updated_by": caller.actor,
            }),
        );
        Ok(driver)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Clear every reference to a driver being deleted: the vehicle
/// assignment and the route assignment.
pub(super) fn clear_driver_references(state: &LogisticsState, driver_id: Uuid) {
    for mut entry in state.vehicles.iter_mut() {
        if entry.value().assigned_driver_id == Some(driver_id) {
            entry.value_mut().assigned_driver_id = None;
            entry.value_mut().updated_at = Utc::now();
        }
    }
    for mut entry in state.routes.iter_mut() {
        if entry.value().driver_id == Some(driver_id) {
            entry.value_mut().driver_id = None;
            entry.value_mut().updated_at = Utc::now();
        }
    }
}

pub(super) async fn delete_driver(
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
        let driver = load_driver(&state, &path)?;
        if state.shipments.iter().any(|entry| {
            entry.value().driver_id == Some(driver.id) && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "Driver is assigned to a live shipment and cannot be deleted",
            ));
        }
        state.drivers.remove(&driver.id);
        clear_driver_references(&state, driver.id);
        state.emit(
            "logistics.driver.deleted",
            json!({
                "driver_id": driver.id.to_string(),
                "user_id": driver.user_id,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "driver_id": driver.id.to_string() }))
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
    fn valid_driver_status_accepts_known_values() {
        for status in ["active", "inactive", "on_leave"] {
            assert_eq!(valid_driver_status(status).unwrap(), status);
        }
        assert!(valid_driver_status("retired").is_err());
        assert!(valid_driver_status("").is_err());
        assert!(valid_driver_status("Active").is_err());
    }

    fn test_driver() -> Driver {
        let now = Utc::now();
        Driver {
            id: Uuid::now_v7(),
            user_id: "user-1".to_string(),
            license_number: "LIC-1".to_string(),
            phone: None,
            status: "active".to_string(),
            current_vehicle_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn test_vehicle(driver_id: Option<Uuid>) -> Vehicle {
        let now = Utc::now();
        Vehicle {
            id: Uuid::now_v7(),
            plate: "B 1234 CD".to_string(),
            vehicle_type: "van".to_string(),
            capacity_kg: 1000.0,
            status: "available".to_string(),
            assigned_driver_id: driver_id,
            company_id: None,
            maintenance: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn test_route(driver_id: Option<Uuid>) -> DeliveryRoute {
        let now = Utc::now();
        DeliveryRoute {
            id: Uuid::now_v7(),
            name: "Route".to_string(),
            driver_id,
            vehicle_id: None,
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
    fn clear_driver_references_clears_vehicle_and_route_links() {
        let state = LogisticsState::default();
        let driver = test_driver();
        let other_driver = test_driver();
        let vehicle = test_vehicle(Some(driver.id));
        let other_vehicle = test_vehicle(Some(other_driver.id));
        let route = test_route(Some(driver.id));
        let other_route = test_route(Some(other_driver.id));
        let vehicle_id = vehicle.id;
        let other_vehicle_id = other_vehicle.id;
        let route_id = route.id;
        let other_route_id = other_route.id;
        state.drivers.insert(driver.id, driver.clone());
        state.drivers.insert(other_driver.id, other_driver);
        state.vehicles.insert(vehicle.id, vehicle);
        state.vehicles.insert(other_vehicle.id, other_vehicle);
        state.routes.insert(route.id, route);
        state.routes.insert(other_route.id, other_route);

        clear_driver_references(&state, driver.id);

        assert_eq!(
            state.vehicles.get(&vehicle_id).unwrap().assigned_driver_id,
            None
        );
        assert_eq!(
            state.vehicles.get(&other_vehicle_id).unwrap().assigned_driver_id,
            Some(other_driver.id)
        );
        assert_eq!(state.routes.get(&route_id).unwrap().driver_id, None);
        assert_eq!(
            state.routes.get(&other_route_id).unwrap().driver_id,
            Some(other_driver.id)
        );
    }
}
