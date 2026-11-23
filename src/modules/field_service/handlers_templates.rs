// field_service module — Maintenance template handlers.
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

use super::constants::{DEFAULT_INTERVAL_DAYS, MAX_CHECKLIST_ITEMS, MAX_DESCRIPTION_LEN, MAX_NAME_LEN, MAX_TITLE_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::types::{Equipment, MaintenancePlan, MaintenanceTemplate};
use super::requests::{ApplyTemplateRequest, CreateTemplateRequest, UpdateTemplateRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, compute_next_due, created, err, load_equipment, load_template, ok,
    optional_text, paginate, require_permission, require_tenant, require_text,
};

// ---------------------------------------------------------------------------
// Maintenance template handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_templates(
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
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<MaintenanceTemplate> = state
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                if let Some(type_id) = &type_filter {
                    if template.equipment_type_id.map(|id| id.to_string()) != Some(type_id.clone()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        template.name,
                        template.description.clone().unwrap_or_default()
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

pub(crate) fn validate_checklist(items: &Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
    match items {
        Some(raw) => {
            if raw.len() > MAX_CHECKLIST_ITEMS {
                return Err(UnifiedError::bad_request(format!(
                    "checklist_template must contain at most {MAX_CHECKLIST_ITEMS} items"
                )));
            }
            let mut out: Vec<String> = Vec::new();
            for item in raw {
                out.push(require_text(item, "checklist_template item", MAX_TITLE_LEN)?);
            }
            Ok(out)
        }
        None => Ok(Vec::new()),
    }
}

pub(crate) fn validate_interval(
    state: &FieldServiceState,
    value: Option<i64>,
    unit: Option<String>,
) -> UnifiedResult<(i64, String)> {
    let interval_value = value.unwrap_or(DEFAULT_INTERVAL_DAYS);
    if interval_value < 1 || interval_value > 3650 {
        return Err(UnifiedError::bad_request(
            "interval_value must be between 1 and 3650",
        ));
    }
    let interval_unit = match unit {
        Some(raw) => {
            let unit = raw.trim().to_lowercase();
            if state.interval_unit(&unit).is_none() {
                return Err(UnifiedError::bad_request(format!("Unknown interval unit: {unit}")));
            }
            unit
        }
        None => "days".to_string(),
    };
    Ok((interval_value, interval_unit))
}

pub(crate) async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        if let Some(type_id) = input.equipment_type_id {
            if state.equipment_types.get(&type_id).is_none() {
                return Err(UnifiedError::not_found(format!("Equipment type not found: {type_id}")));
            }
        }
        let (interval_value, interval_unit) =
            validate_interval(&state, input.interval_value, input.interval_unit)?;
        let checklist = validate_checklist(&input.checklist_template)?;
        let default_priority = match &input.default_priority {
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
        let template = MaintenanceTemplate {
            id: Uuid::now_v7(),
            name,
            description,
            equipment_type_id: input.equipment_type_id,
            interval_value,
            interval_unit,
            checklist_template: checklist,
            default_priority,
            is_system: false,
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        state.emit(
            "field_service.template.created",
            json!({
                "template_id": template.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => created(template),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        load_template(&state, &path.into_inner())
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceTemplate> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            template.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            template.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(type_id) = input.equipment_type_id {
            match type_id {
                Some(id) => {
                    if state.equipment_types.get(&id).is_none() {
                        return Err(UnifiedError::not_found(format!(
                            "Equipment type not found: {id}"
                        )));
                    }
                    template.equipment_type_id = Some(id);
                }
                None => template.equipment_type_id = None,
            }
        }
        if let Some(interval_value) = input.interval_value {
            if interval_value < 1 || interval_value > 3650 {
                return Err(UnifiedError::bad_request(
                    "interval_value must be between 1 and 3650",
                ));
            }
            template.interval_value = interval_value;
        }
        if let Some(unit) = input.interval_unit {
            let unit = unit.trim().to_lowercase();
            if state.interval_unit(&unit).is_none() {
                return Err(UnifiedError::bad_request(format!("Unknown interval unit: {unit}")));
            }
            template.interval_unit = unit;
        }
        if let Some(checklist) = input.checklist_template {
            template.checklist_template = validate_checklist(&Some(checklist))?;
        }
        if let Some(priority) = input.default_priority {
            let priority = priority.trim().to_string();
            if !state.definition_exists("priority", &priority) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown priority: {priority}"
                )));
            }
            template.default_priority = priority;
        }
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        state.emit(
            "field_service.template.updated",
            json!({
                "template_id": template.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_template(
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
        let template = load_template(&state, &path.into_inner())?;
        if state
            .plans
            .iter()
            .any(|entry| entry.value().template_id == Some(template.id))
        {
            return Err(UnifiedError::conflict(
                "Maintenance template is in use by plans and cannot be deleted",
            ));
        }
        state.templates.remove(&template.id);
        state.emit(
            "field_service.template.deleted",
            json!({
                "template_id": template.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "template_id": template.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a maintenance plan from a template for one equipment
/// item. The template's interval and checklist seed the plan; the
/// plan name defaults to the template name.
pub(crate) async fn apply_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ApplyTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        let equipment = load_equipment(&state, &input.equipment_id.to_string())?;
        if let Some(type_id) = template.equipment_type_id {
            if equipment.equipment_type_id != type_id {
                return Err(UnifiedError::bad_request(
                    "Template does not apply to this equipment type",
                ));
            }
        }
        let name = match &input.name {
            Some(raw) => require_text(raw, "name", MAX_NAME_LEN)?,
            None => template.name.clone(),
        };
        let now = Utc::now();
        let mut plan = MaintenancePlan {
            id: Uuid::now_v7(),
            equipment_id: equipment.id,
            template_id: Some(template.id),
            name,
            description: template.description.clone(),
            interval_value: template.interval_value,
            interval_unit: template.interval_unit.clone(),
            last_service_at: None,
            usage_at_last_service: equipment.usage_reading,
            next_due_at: None,
            next_due_usage: None,
            is_active: true,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        let (next_due_at, next_due_usage) = compute_next_due(&state, &plan)?;
        plan.next_due_at = next_due_at;
        plan.next_due_usage = next_due_usage;
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "field_service.plan.created",
            json!({
                "plan_id": plan.id.to_string(),
                "equipment_id": equipment.id.to_string(),
                "template_id": template.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => created(plan),
        Err(error) => err(error, debug),
    }
}
