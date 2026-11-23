// field_service module — Maintenance plan handlers.
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

use super::constants::{MAX_DESCRIPTION_LEN, MAX_NAME_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::types::{Equipment, EquipmentType, MaintenancePlan, MaintenanceTemplate};
use super::requests::{CreatePlanRequest, UpdatePlanRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, compute_next_due, created, days_until_due, err, load_equipment,
    load_plan, load_template, ok, optional_text, paginate, plan_is_overdue, require_permission,
    require_tenant, require_text, run_due_check,
};
use super::handlers_templates::validate_interval;

// ---------------------------------------------------------------------------
// Maintenance plan handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_plans(
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
        let equipment_filter = params.filters.get("equipment_id").cloned();
        let active_filter = params.filters.get("is_active").cloned();
        let overdue_filter = params.filters.get("overdue").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<MaintenancePlan> = state
            .plans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|plan| {
                if let Some(equipment_id) = &equipment_filter {
                    if plan.equipment_id.to_string() != *equipment_id {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let wanted = active == "true";
                    if plan.is_active != wanted {
                        return false;
                    }
                }
                if let Some(overdue) = &overdue_filter {
                    let wanted = overdue == "true";
                    if plan_is_overdue(&state, plan) != wanted {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        plan.name,
                        plan.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            a.next_due_at
                .cmp(&b.next_due_at)
                .then(a.created_at.cmp(&b.created_at))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Default interval for a new plan, in precedence order: the
/// template's interval, then the equipment type's default, then
/// the settings default. Shared by the (None, None) branch and
/// the one-sided fallback so both agree.
pub(crate) fn default_interval(
    state: &FieldServiceState,
    template: Option<&MaintenanceTemplate>,
    equipment: &Equipment,
) -> (i64, String) {
    if let Some(template) = template {
        return (template.interval_value, template.interval_unit.clone());
    }
    if let Some(entry) = state.equipment_types.get(&equipment.equipment_type_id) {
        return (
            entry.value().default_interval_value,
            entry.value().default_interval_unit.clone(),
        );
    }
    (
        state.settings().default_interval_days,
        "days".to_string(),
    )
}

pub(crate) async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let input = body.into_inner();
        let equipment = load_equipment(&state, &input.equipment_id.to_string())?;
        let template = match input.template_id {
            Some(id) => Some(load_template(&state, &id.to_string())?),
            None => None,
        };
        let (interval_value, interval_unit) = match (&input.interval_value, &input.interval_unit) {
            (None, None) => default_interval(&state, template.as_ref(), &equipment),
            _ => validate_interval(&state, input.interval_value, input.interval_unit.clone())?,
        };
        // When only one side of the interval is given, the other
        // falls back to the template / type default.
        let (interval_value, interval_unit) = if input.interval_value.is_some() ^ input.interval_unit.is_some() {
            let fallback = default_interval(&state, template.as_ref(), &equipment);
            (
                input.interval_value.unwrap_or(fallback.0),
                input.interval_unit
                    .map(|raw| raw.trim().to_lowercase())
                    .unwrap_or(fallback.1),
            )
        } else {
            (interval_value, interval_unit)
        };
        if state.interval_unit(&interval_unit).is_none() {
            return Err(UnifiedError::bad_request(format!(
                "Unknown interval unit: {interval_unit}"
            )));
        }
        let name = match &input.name {
            Some(raw) => require_text(raw, "name", MAX_NAME_LEN)?,
            None => match &template {
                Some(template) => template.name.clone(),
                None => format!("Maintenance plan for {}", equipment.name),
            },
        };
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let usage_at_last_service = match input.usage_at_last_service {
            Some(reading) if !reading.is_finite() || reading < 0.0 => {
                return Err(UnifiedError::bad_request(
                    "usage_at_last_service must be a finite number >= 0",
                ));
            }
            Some(reading) => Some(reading),
            None => equipment.usage_reading,
        };
        let now = Utc::now();
        let mut plan = MaintenancePlan {
            id: Uuid::now_v7(),
            equipment_id: equipment.id,
            template_id: input.template_id,
            name,
            description,
            interval_value,
            interval_unit,
            last_service_at: input.last_service_at,
            usage_at_last_service,
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
                "template_id": plan.template_id.map(|id| id.to_string()),
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

pub(crate) async fn get_plan(
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
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let plan = load_plan(&state, &path.into_inner())?;
        Ok(json!({
            "plan": plan,
            "overdue": plan_is_overdue(&state, &plan),
            "days_until_due": days_until_due(&plan),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut plan = load_plan(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            plan.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            plan.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(interval_value) = input.interval_value {
            if interval_value < 1 || interval_value > 3650 {
                return Err(UnifiedError::bad_request(
                    "interval_value must be between 1 and 3650",
                ));
            }
            plan.interval_value = interval_value;
        }
        if let Some(unit) = input.interval_unit {
            let unit = unit.trim().to_lowercase();
            if state.interval_unit(&unit).is_none() {
                return Err(UnifiedError::bad_request(format!("Unknown interval unit: {unit}")));
            }
            plan.interval_unit = unit;
        }
        if let Some(last_service) = input.last_service_at {
            plan.last_service_at = last_service;
        }
        if let Some(usage) = input.usage_at_last_service {
            match usage {
                Some(value) => {
                    if !value.is_finite() || value < 0.0 {
                        return Err(UnifiedError::bad_request(
                            "usage_at_last_service must be a finite number >= 0",
                        ));
                    }
                    plan.usage_at_last_service = Some(value);
                }
                None => plan.usage_at_last_service = None,
            }
        }
        if let Some(is_active) = input.is_active {
            plan.is_active = is_active;
        }
        let (next_due_at, next_due_usage) = compute_next_due(&state, &plan)?;
        plan.next_due_at = next_due_at;
        plan.next_due_usage = next_due_usage;
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "field_service.plan.updated",
            json!({
                "plan_id": plan.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => ok(plan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_plan(
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
        let plan = load_plan(&state, &path.into_inner())?;
        if state.work_orders.iter().any(|entry| {
            entry.value().plan_id == Some(plan.id) && !entry.value().status.is_terminal()
        }) {
            return Err(UnifiedError::conflict(
                "Maintenance plan has open work orders and cannot be deleted",
            ));
        }
        state.plans.remove(&plan.id);
        state.emit(
            "field_service.plan.deleted",
            json!({
                "plan_id": plan.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "plan_id": plan.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn recompute_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut plan = load_plan(&state, &path.into_inner())?;
        let (next_due_at, next_due_usage) = compute_next_due(&state, &plan)?;
        plan.next_due_at = next_due_at;
        plan.next_due_usage = next_due_usage;
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            "field_service.plan.next_due_reset",
            json!({
                "plan_id": plan.id.to_string(),
                "next_due_at": plan.next_due_at.map(|due| due.to_rfc3339()),
                "next_due_usage": plan.next_due_usage,
                "recomputed_by": caller.actor,
            }),
        );
        Ok(plan)
    })();
    match result {
        Ok(plan) => ok(plan),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_due_plans(
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
        let days: i64 = params
            .filters
            .get("days")
            .and_then(|raw| raw.parse::<i64>().ok())
            .unwrap_or(state.settings().overdue_alert_days)
            .clamp(0, 365);
        let equipment_filter = params.filters.get("equipment_id").cloned();
        let mut items: Vec<Value> = state
            .plans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|plan| {
                if !plan.is_active {
                    return false;
                }
                if let Some(equipment_id) = &equipment_filter {
                    if plan.equipment_id.to_string() != *equipment_id {
                        return false;
                    }
                }
                match days_until_due(plan) {
                    Some(remaining) => remaining <= days,
                    None => plan_is_overdue(&state, plan),
                }
            })
            .map(|plan| {
                json!({
                    "plan": plan,
                    "overdue": plan_is_overdue(&state, &plan),
                    "days_until_due": days_until_due(&plan),
                })
            })
            .collect();
        items.sort_by_key(|item| {
            item.get("days_until_due")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_overdue_plans(
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
        let equipment_filter = params.filters.get("equipment_id").cloned();
        let mut items: Vec<Value> = state
            .plans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|plan| {
                if !plan.is_active || !plan_is_overdue(&state, plan) {
                    return false;
                }
                if let Some(equipment_id) = &equipment_filter {
                    if plan.equipment_id.to_string() != *equipment_id {
                        return false;
                    }
                }
                true
            })
            .map(|plan| {
                json!({
                    "plan": plan,
                    "overdue": true,
                    "days_until_due": days_until_due(&plan),
                })
            })
            .collect();
        items.sort_by_key(|item| {
            item.get("days_until_due")
                .and_then(Value::as_i64)
                .unwrap_or(i64::MAX)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn check_due_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let alerts = run_due_check(&state);
        Ok(json!({
            "checked": true,
            "checked_by": caller.actor,
            "alerts": alerts.len(),
            "alerts_detail": alerts,
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

    fn equipment(equipment_type_id: Uuid) -> Equipment {
        let now = Utc::now();
        Equipment {
            id: Uuid::new_v4(),
            asset_id: None,
            equipment_type_id,
            name: "Pump A".to_string(),
            serial_number: None,
            location: None,
            install_date: None,
            warranty_end: None,
            status: "active".to_string(),
            usage_reading: None,
            usage_unit: None,
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn equipment_type() -> EquipmentType {
        let now = Utc::now();
        EquipmentType {
            id: Uuid::new_v4(),
            slug: "pump".to_string(),
            name: "Pump".to_string(),
            description: None,
            default_interval_value: 30,
            default_interval_unit: "days".to_string(),
            is_system: false,
            created_at: now,
            updated_at: now,
        }
    }

    fn template() -> MaintenanceTemplate {
        let now = Utc::now();
        MaintenanceTemplate {
            id: Uuid::new_v4(),
            name: "Quarterly".to_string(),
            description: None,
            equipment_type_id: None,
            interval_value: 3,
            interval_unit: "months".to_string(),
            checklist_template: Vec::new(),
            default_priority: "normal".to_string(),
            is_system: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn default_interval_prefers_template_then_type_then_settings() {
        let state = FieldServiceState::seeded();
        let equipment_type = equipment_type();
        state
            .equipment_types
            .insert(equipment_type.id, equipment_type.clone());
        let equipment = equipment(equipment_type.id);
        let template = template();

        assert_eq!(
            default_interval(&state, Some(&template), &equipment),
            (3, "months".to_string()),
            "template wins over the equipment-type default"
        );
        assert_eq!(
            default_interval(&state, None, &equipment),
            (30, "days".to_string()),
            "equipment-type default wins over the settings default"
        );
        let other_equipment = equipment(Uuid::new_v4());
        assert_eq!(
            default_interval(&state, None, &other_equipment),
            (state.settings().default_interval_days, "days".to_string()),
            "settings default is the last resort"
        );
    }
}
