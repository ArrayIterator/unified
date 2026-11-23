//! HTTP handlers for the fleet module.
//!
//! Every handler returns the standard envelope
//! (`{"data": ...}` on success, `{"error": ...}` on failure) through
//! `unified_core::json`. Tenant isolation is enforced here and in
//! the store: the tenant comes from the `X-Unified-Tenant` header
//! (the same header the core tenant middleware uses) or from the
//! request's `tenant_id` field / query parameter. A request without
//! any tenant is rejected with 400 — handlers never fall back to a
//! global view.

use crate::state::{
    DriverInput, DriverUpdate, FleetState, FuelInput, MaintenanceComplete, MaintenanceInput,
    PositionInput, TripInput, TripStatusPatch, VehicleInput, VehicleTypesInput, VehicleUpdate,
};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;

type State = web::Data<Arc<FleetState>>;

/// Resolve the tenant for one request: the `X-Unified-Tenant`
/// header wins, then the explicit value (body field or query
/// parameter). Missing everywhere is a 400.
fn resolve_tenant(req: &HttpRequest, explicit: Option<String>) -> UnifiedResult<String> {
    if let Some(header) = req.headers().get("x-unified-tenant") {
        if let Ok(value) = header.to_str() {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return crate::validation::validate_tenant_id(trimmed);
            }
        }
    }
    match explicit {
        Some(value) => crate::validation::validate_tenant_id(&value),
        None => Err(UnifiedError::bad_request(
            "tenant_id is required (X-Unified-Tenant header, body field, or ?tenant_id=)",
        )),
    }
}

/// Query string as a plain map (only `tenant_id` is read from it).
fn query_tenant(query: &web::Query<HashMap<String, String>>) -> Option<String> {
    query.get("tenant_id").cloned()
}

fn ok(data: Value) -> HttpResponse {
    json::success_response(data, 200)
}

fn created(data: Value) -> HttpResponse {
    json::success_response(data, 201)
}

fn respond(result: UnifiedResult<Value>) -> HttpResponse {
    json::respond(result, false)
}

fn to_value<T: serde::Serialize>(value: T) -> UnifiedResult<Value> {
    serde_json::to_value(value)
        .map_err(|error| UnifiedError::bad_request(format!("Failed to serialise response: {error}")))
}

// -- vehicle types -----------------------------------------------------

async fn list_vehicle_types(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        Ok(json!({ "tenant_id": tenant, "types": state.vehicle_types_for(&tenant) }))
    })();
    respond(result)
}

async fn set_vehicle_types(state: State, req: HttpRequest, body: web::Json<VehicleTypesInput>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let input = body.into_inner();
        let tenant = resolve_tenant(&req, input.tenant_id.clone())?;
        let types = state.set_vehicle_types(&tenant, input.types)?;
        Ok(json!({ "tenant_id": tenant, "types": types }))
    })();
    respond(result)
}

// -- vehicles ----------------------------------------------------------

async fn create_vehicle(state: State, req: HttpRequest, body: web::Json<VehicleInput>) -> HttpResponse {
    let input = body.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.create_vehicle(&tenant, input).and_then(to_value) {
        Ok(data) => created(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_vehicles(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.list_vehicles(&tenant).and_then(to_value)
    })();
    respond(result)
}

async fn get_vehicle(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.get_vehicle(&tenant, &path.into_inner()).and_then(to_value)
    })();
    respond(result)
}

async fn update_vehicle(state: State, req: HttpRequest, path: web::Path<String>, body: web::Json<VehicleUpdate>) -> HttpResponse {
    let input = body.into_inner();
    let id = path.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.update_vehicle(&tenant, &id, input).and_then(to_value) {
        Ok(data) => ok(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn delete_vehicle(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.delete_vehicle(&tenant, &path.into_inner())?;
        Ok(json!({ "deleted": true }))
    })();
    respond(result)
}

async fn vehicle_costs(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.vehicle_costs(&tenant, &path.into_inner())
    })();
    respond(result)
}

// -- GPS positions -----------------------------------------------------

async fn add_position(state: State, req: HttpRequest, path: web::Path<String>, body: web::Json<PositionInput>) -> HttpResponse {
    let input = body.into_inner();
    let vehicle_id = path.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.add_position(&tenant, &vehicle_id, input).and_then(to_value) {
        Ok(data) => created(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_positions(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        let vehicle_id = path.into_inner();
        let positions = state.list_positions(&tenant, &vehicle_id)?;
        Ok(json!({ "vehicle_id": vehicle_id, "tenant_id": tenant, "positions": positions }))
    })();
    respond(result)
}

// -- drivers -----------------------------------------------------------

async fn create_driver(state: State, req: HttpRequest, body: web::Json<DriverInput>) -> HttpResponse {
    let input = body.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state
        .create_driver(&tenant, input)
        .map(|driver| crate::state::FleetState::driver_view(&driver))
        .and_then(to_value)
    {
        Ok(data) => created(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_drivers(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.list_drivers(&tenant).and_then(to_value)
    })();
    respond(result)
}

async fn get_driver(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.get_driver(&tenant, &path.into_inner()).and_then(to_value)
    })();
    respond(result)
}

async fn update_driver(state: State, req: HttpRequest, path: web::Path<String>, body: web::Json<DriverUpdate>) -> HttpResponse {
    let input = body.into_inner();
    let id = path.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.update_driver(&tenant, &id, input).and_then(to_value) {
        Ok(data) => ok(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn delete_driver(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.delete_driver(&tenant, &path.into_inner())?;
        Ok(json!({ "deleted": true }))
    })();
    respond(result)
}

// -- trips -------------------------------------------------------------

async fn create_trip(state: State, req: HttpRequest, body: web::Json<TripInput>) -> HttpResponse {
    let input = body.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.create_trip(&tenant, input).and_then(to_value) {
        Ok(data) => created(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_trips(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.list_trips(&tenant).and_then(to_value)
    })();
    respond(result)
}

async fn get_trip(state: State, req: HttpRequest, path: web::Path<String>, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.get_trip(&tenant, &path.into_inner()).and_then(to_value)
    })();
    respond(result)
}

async fn patch_trip_status(state: State, req: HttpRequest, path: web::Path<String>, body: web::Json<TripStatusPatch>) -> HttpResponse {
    let input = body.into_inner();
    let id = path.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.patch_trip_status(&tenant, &id, input).and_then(to_value) {
        Ok(data) => ok(data),
        Err(error) => json::error_response(&error, false),
    }
}

// -- fuel --------------------------------------------------------------

async fn add_fuel_log(state: State, req: HttpRequest, body: web::Json<FuelInput>) -> HttpResponse {
    let input = body.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.add_fuel_log(&tenant, input).and_then(to_value) {
        Ok(data) => created(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_fuel(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        let vehicle_id = query
            .get("vehicle_id")
            .cloned()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| UnifiedError::bad_request("vehicle_id query parameter is required"))?;
        state.fuel_summary(&tenant, &vehicle_id)
    })();
    respond(result)
}

// -- maintenance -------------------------------------------------------

async fn create_maintenance(state: State, req: HttpRequest, body: web::Json<MaintenanceInput>) -> HttpResponse {
    let input = body.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.create_maintenance(&tenant, input).and_then(to_value) {
        Ok(data) => created(data),
        Err(error) => json::error_response(&error, false),
    }
}

async fn list_maintenance(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.list_maintenance(&tenant).and_then(to_value)
    })();
    respond(result)
}

async fn due_maintenance(state: State, req: HttpRequest, query: web::Query<HashMap<String, String>>) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let tenant = resolve_tenant(&req, query_tenant(&query))?;
        state.due_maintenance(&tenant).and_then(to_value)
    })();
    respond(result)
}

async fn complete_maintenance(state: State, req: HttpRequest, path: web::Path<String>, body: web::Json<MaintenanceComplete>) -> HttpResponse {
    let input = body.into_inner();
    let id = path.into_inner();
    let tenant = match resolve_tenant(&req, input.tenant_id.clone()) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, false),
    };
    match state.complete_maintenance(&tenant, &id, input) {
        Ok(data) => ok(data),
        Err(error) => json::error_response(&error, false),
    }
}

/// Register every fleet route under `/api/v1/fleet/`.
/// Literal segments are registered before `/{id}` segments so they
/// win (for example `/maintenance/due` over `/maintenance/{id}`).
pub(crate) fn configure_routes(cfg: &mut web::ServiceConfig, state: Arc<FleetState>) {
    cfg.app_data(web::Data::new(state));
    cfg.service(
        web::scope("/api/v1/fleet")
            .route("/vehicle-types", web::get().to(list_vehicle_types))
            .route("/vehicle-types", web::put().to(set_vehicle_types))
            .route("/vehicles", web::post().to(create_vehicle))
            .route("/vehicles", web::get().to(list_vehicles))
            .route("/vehicles/{id}/positions", web::post().to(add_position))
            .route("/vehicles/{id}/positions", web::get().to(list_positions))
            .route("/vehicles/{id}/costs", web::get().to(vehicle_costs))
            .route("/vehicles/{id}", web::get().to(get_vehicle))
            .route("/vehicles/{id}", web::put().to(update_vehicle))
            .route("/vehicles/{id}", web::delete().to(delete_vehicle))
            .route("/drivers", web::post().to(create_driver))
            .route("/drivers", web::get().to(list_drivers))
            .route("/drivers/{id}", web::get().to(get_driver))
            .route("/drivers/{id}", web::put().to(update_driver))
            .route("/drivers/{id}", web::delete().to(delete_driver))
            .route("/trips", web::post().to(create_trip))
            .route("/trips", web::get().to(list_trips))
            .route("/trips/{id}", web::get().to(get_trip))
            .route("/trips/{id}/status", web::patch().to(patch_trip_status))
            .route("/fuel", web::post().to(add_fuel_log))
            .route("/fuel", web::get().to(get_fuel))
            .route("/maintenance/due", web::get().to(due_maintenance))
            .route("/maintenance", web::post().to(create_maintenance))
            .route("/maintenance", web::get().to(list_maintenance))
            .route("/maintenance/{id}/complete", web::patch().to(complete_maintenance)),
    );
}
