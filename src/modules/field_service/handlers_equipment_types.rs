// field_service module — Equipment type handlers.
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

use super::constants::{DEFAULT_INTERVAL_DAYS, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::types::{Equipment, EquipmentType};
use super::requests::{CreateEquipmentTypeRequest, UpdateEquipmentTypeRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, created, err, load_equipment_type, ok, optional_text, paginate,
    require_permission, require_tenant, require_text, valid_slug,
};

// ---------------------------------------------------------------------------
// Equipment type handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_equipment_types(
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
        let slug_filter = params.filters.get("slug").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<EquipmentType> = state
            .equipment_types
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|equipment_type| {
                if let Some(slug) = &slug_filter {
                    if &equipment_type.slug != slug {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        equipment_type.slug,
                        equipment_type.name,
                        equipment_type.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateEquipmentTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EquipmentType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let input = body.into_inner();
        let slug = input.slug.trim().to_lowercase();
        if !valid_slug(&slug) {
            return Err(UnifiedError::bad_request(
                "slug must be lowercase letters, digits, \"_\", or \"-\" (max 64 characters)",
            ));
        }
        if state
            .equipment_types
            .iter()
            .any(|entry| entry.value().slug == slug)
        {
            return Err(UnifiedError::conflict(format!(
                "Equipment type slug already exists: {slug}"
            )));
        }
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let interval_value = input.default_interval_value.unwrap_or(DEFAULT_INTERVAL_DAYS);
        if interval_value < 1 || interval_value > 3650 {
            return Err(UnifiedError::bad_request(
                "default_interval_value must be between 1 and 3650",
            ));
        }
        let interval_unit = match &input.default_interval_unit {
            Some(unit) => {
                let unit = unit.trim().to_lowercase();
                if state.interval_unit(&unit).is_none() {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown interval unit: {unit}"
                    )));
                }
                unit
            }
            None => "days".to_string(),
        };
        let now = Utc::now();
        let equipment_type = EquipmentType {
            id: Uuid::now_v7(),
            slug,
            name,
            description,
            default_interval_value: interval_value,
            default_interval_unit: interval_unit,
            is_system: false,
            created_at: now,
            updated_at: now,
        };
        state.equipment_types.insert(equipment_type.id, equipment_type.clone());
        state.emit(
            "field_service.equipment_type.created",
            json!({
                "equipment_type_id": equipment_type.id.to_string(),
                "slug": equipment_type.slug,
                "created_by": caller.actor,
            }),
        );
        Ok(equipment_type)
    })();
    match result {
        Ok(equipment_type) => created(equipment_type),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EquipmentType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        load_equipment_type(&state, &path.into_inner())
    })();
    match result {
        Ok(equipment_type) => ok(equipment_type),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEquipmentTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EquipmentType> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut equipment_type = load_equipment_type(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            equipment_type.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            equipment_type.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(interval_value) = input.default_interval_value {
            if interval_value < 1 || interval_value > 3650 {
                return Err(UnifiedError::bad_request(
                    "default_interval_value must be between 1 and 3650",
                ));
            }
            equipment_type.default_interval_value = interval_value;
        }
        if let Some(unit) = input.default_interval_unit {
            let unit = unit.trim().to_lowercase();
            if state.interval_unit(&unit).is_none() {
                return Err(UnifiedError::bad_request(format!("Unknown interval unit: {unit}")));
            }
            equipment_type.default_interval_unit = unit;
        }
        equipment_type.updated_at = Utc::now();
        state.equipment_types.insert(equipment_type.id, equipment_type.clone());
        state.emit(
            "field_service.equipment_type.updated",
            json!({
                "equipment_type_id": equipment_type.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(equipment_type)
    })();
    match result {
        Ok(equipment_type) => ok(equipment_type),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_equipment_type(
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
        let equipment_type = load_equipment_type(&state, &path.into_inner())?;
        if state
            .equipment
            .iter()
            .any(|entry| entry.value().equipment_type_id == equipment_type.id)
        {
            return Err(UnifiedError::conflict(
                "Equipment type is in use by equipment and cannot be deleted",
            ));
        }
        state.equipment_types.remove(&equipment_type.id);
        state.emit(
            "field_service.equipment_type.deleted",
            json!({
                "equipment_type_id": equipment_type.id.to_string(),
                "slug": equipment_type.slug,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "equipment_type_id": equipment_type.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
