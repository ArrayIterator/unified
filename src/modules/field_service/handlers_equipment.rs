// field_service module - Equipment handlers.
//
// Split from the former single-file module `src/modules/field_service.rs`.
// The module root (`field_service.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_DESCRIPTION_LEN, MAX_LOCATION_LEN, MAX_NAME_LEN, MAX_SERIAL_LEN, MAX_TITLE_LEN, MAX_UNIT_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::types::{Equipment, WorkOrder, WorkOrderStatus};
use super::requests::{CreateEquipmentRequest, ReportFailureRequest, UpdateEquipmentRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, created, err, load_equipment, ok, optional_text, paginate,
    require_permission, require_tenant, require_text,
};

// Equipment handlers

pub(crate) async fn list_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let params = query.into_inner();
        let type_filter = params.filters.get("equipment_type_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let location_filter = params.filters.get("location").cloned();
        let asset_filter = params.filters.get("asset_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Equipment> = state
            .equipment
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|equipment| {
                if let Some(type_id) = &type_filter {
                    if equipment.equipment_type_id.to_string() != *type_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &equipment.status != status {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if equipment.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if let Some(asset_id) = &asset_filter {
                    if equipment.asset_id.map(|id| id.to_string()) != Some(asset_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        equipment.name,
                        equipment.serial_number.clone().unwrap_or_default(),
                        equipment.location.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "install_date", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "install_date" => a.install_date.cmp(&b.install_date),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateEquipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Equipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let input = body.into_inner();
        if state.equipment_types.get(&input.equipment_type_id).is_none() {
            return Err(UnifiedError::not_found(format!(
                "Equipment type not found: {}",
                input.equipment_type_id
            )));
        }
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let serial_number = optional_text(&input.serial_number, "serial_number", MAX_SERIAL_LEN)?;
        if let Some(serial) = &serial_number {
            if state
                .equipment
                .iter()
                .any(|entry| entry.value().serial_number.as_deref() == Some(serial.as_str()))
            {
                return Err(UnifiedError::conflict(format!(
                    "Serial number already registered: {serial}"
                )));
            }
        }
        let location = optional_text(&input.location, "location", MAX_LOCATION_LEN)?;
        let status = match &input.status {
            Some(raw) => {
                let status = raw.trim().to_string();
                if !state.definition_exists("equipment_status", &status) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown equipment status: {status}"
                    )));
                }
                status
            }
            None => "active".to_string(),
        };
        let usage_reading = match input.usage_reading {
            Some(reading) if !reading.is_finite() || reading < 0.0 => {
                return Err(UnifiedError::bad_request(
                    "usage_reading must be a finite number >= 0",
                ));
            }
            Some(reading) => Some(reading),
            None => None,
        };
        let usage_unit = optional_text(&input.usage_unit, "usage_unit", MAX_UNIT_LEN)?;
        let now = Utc::now();
        let equipment = Equipment {
            id: Uuid::now_v7(),
            asset_id: input.asset_id,
            equipment_type_id: input.equipment_type_id,
            name,
            serial_number,
            location,
            install_date: input.install_date,
            warranty_end: input.warranty_end,
            status,
            usage_reading,
            usage_unit,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.equipment.insert(equipment.id, equipment.clone());
        state.emit(
            "field_service.equipment.created",
            json!({
                "equipment_id": equipment.id.to_string(),
                "asset_id": equipment.asset_id.map(|id| id.to_string()),
                "equipment_type_id": equipment.equipment_type_id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(equipment)
    })();
    match result {
        Ok(equipment) => created(equipment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Equipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        load_equipment(&state, &path.into_inner())
    })();
    match result {
        Ok(equipment) => ok(equipment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEquipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Equipment> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut equipment = load_equipment(&state, &path.into_inner())?;
        if equipment.status == "retired" {
            return Err(UnifiedError::conflict("Retired equipment cannot be updated"));
        }
        let input = body.into_inner();
        if let Some(type_id) = input.equipment_type_id {
            if state.equipment_types.get(&type_id).is_none() {
                return Err(UnifiedError::not_found(format!("Equipment type not found: {type_id}")));
            }
            equipment.equipment_type_id = type_id;
        }
        if let Some(name) = input.name {
            equipment.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(serial) = input.serial_number {
            match serial {
                Some(raw) => {
                    let serial = require_text(&raw, "serial_number", MAX_SERIAL_LEN)?;
                    if state.equipment.iter().any(|entry| {
                        entry.value().serial_number.as_deref() == Some(serial.as_str())
                            && entry.value().id != equipment.id
                    }) {
                        return Err(UnifiedError::conflict(format!(
                            "Serial number already registered: {serial}"
                        )));
                    }
                    equipment.serial_number = Some(serial);
                }
                None => equipment.serial_number = None,
            }
        }
        if let Some(location) = input.location {
            equipment.location = match location {
                Some(raw) => Some(require_text(&raw, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(install_date) = input.install_date {
            equipment.install_date = install_date;
        }
        if let Some(warranty_end) = input.warranty_end {
            equipment.warranty_end = warranty_end;
        }
        if let Some(status) = input.status {
            let status = status.trim().to_string();
            if !state.definition_exists("equipment_status", &status) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown equipment status: {status}"
                )));
            }
            equipment.status = status;
        }
        if let Some(reading) = input.usage_reading {
            match reading {
                Some(value) => {
                    if !value.is_finite() || value < 0.0 {
                        return Err(UnifiedError::bad_request(
                            "usage_reading must be a finite number >= 0",
                        ));
                    }
                    equipment.usage_reading = Some(value);
                }
                None => equipment.usage_reading = None,
            }
        }
        if let Some(unit) = input.usage_unit {
            equipment.usage_unit = match unit {
                Some(raw) => Some(require_text(&raw, "usage_unit", MAX_UNIT_LEN)?),
                None => None,
            };
        }
        equipment.updated_at = Utc::now();
        state.equipment.insert(equipment.id, equipment.clone());
        state.emit(
            "field_service.equipment.updated",
            json!({
                "equipment_id": equipment.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(equipment)
    })();
    match result {
        Ok(equipment) => ok(equipment),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let equipment = load_equipment(&state, &path.into_inner())?;
        if state
            .plans
            .iter()
            .any(|entry| entry.value().equipment_id == equipment.id && entry.value().is_active)
        {
            return Err(UnifiedError::conflict(
                "Equipment has active maintenance plans and cannot be deleted",
            ));
        }
        if state.work_orders.iter().any(|entry| {
            entry.value().equipment_id == equipment.id
                && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "Equipment has open work orders and cannot be deleted",
            ));
        }
        state.equipment.remove(&equipment.id);
        state.emit(
            "field_service.equipment.deleted",
            json!({
                "equipment_id": equipment.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "equipment_id": equipment.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Report a failure on an equipment item: opens a corrective work
/// order and requests an incident through the incidents module.
/// The incident request is an event (`incidents.create_requested`)
/// with no listener yet, so the answer is reported honestly as
/// `incident_requested` / `incident_handler_answered`.
pub(crate) async fn report_failure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportFailureRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let equipment = load_equipment(&state, &path.into_inner())?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let severity = match &input.severity {
            Some(raw) => {
                let severity = raw.trim().to_lowercase();
                match severity.as_str() {
                    "critical" | "high" | "medium" | "low" => severity,
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "severity must be \"critical\", \"high\", \"medium\", or \"low\"",
                        ));
                    }
                }
            }
            None => "high".to_string(),
        };
        let priority = match &input.priority {
            Some(raw) => {
                let priority = raw.trim().to_string();
                if !state.definition_exists("priority", &priority) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown priority: {priority}"
                    )));
                }
                priority
            }
            None => state.settings().default_priority,
        };
        let now = Utc::now();
        let work_order = WorkOrder {
            id: Uuid::now_v7(),
            equipment_id: equipment.id,
            plan_id: None,
            project_id: None,
            title: title.clone(),
            description: description.clone(),
            order_type: "corrective".to_string(),
            priority,
            status: WorkOrderStatus::Open,
            assignee_id: None,
            scheduled_at: None,
            due_at: None,
            completed_at: None,
            verified_at: None,
            closed_at: None,
            cancelled_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.work_orders.insert(work_order.id, work_order.clone());
        state.emit(
            "field_service.work_order.created",
            json!({
                "work_order_id": work_order.id.to_string(),
                "equipment_id": equipment.id.to_string(),
                "order_type": "corrective",
                "created_by": caller.actor,
            }),
        );
        let incident_answered = state.emit_request(
            "incidents.create_requested",
            json!({
                "title": title,
                "description": description.clone().unwrap_or_else(|| format!(
                    "Equipment failure reported for \"{}\" ({})",
                    equipment.name, equipment.id
                )),
                "severity": severity,
                "source": "field_service",
                "work_order_id": work_order.id.to_string(),
                "equipment_id": equipment.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        state.emit(
            "field_service.equipment.failure_reported",
            json!({
                "equipment_id": equipment.id.to_string(),
                "work_order_id": work_order.id.to_string(),
                "severity": severity,
                "incident_requested": true,
                "incident_handler_answered": incident_answered,
                "reported_by": caller.actor,
            }),
        );
        Ok(json!({
            "equipment": equipment,
            "work_order": work_order,
            "incident_requested": true,
            "incident_handler_answered": incident_answered,
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}
