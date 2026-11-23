// field_service module — Definition and settings handlers.
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

use super::constants::{DEFINITION_KINDS, MAX_LABEL_LEN, PERM_FIELD_SERVICE_MANAGE, PERM_FIELD_SERVICE_VIEW};
use super::types::{DefinitionValue, FieldServiceSettings, IntervalUnitDef};
use super::requests::{AddDefinitionRequest, UpdateSettingsRequest};
use super::state::FieldServiceState;
use super::helpers::{
    caller, caller_company, created, err, ok, require_permission, require_tenant, require_text,
    valid_slug,
};

// ---------------------------------------------------------------------------
// Definition and settings handlers
// ---------------------------------------------------------------------------

pub(crate) async fn list_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let mut registries = serde_json::Map::new();
        for kind in DEFINITION_KINDS {
            if kind == "interval_unit" {
                let mut units: Vec<IntervalUnitDef> = state
                    .interval_units
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect();
                units.sort_by(|a, b| a.unit.cmp(&b.unit));
                registries.insert(kind.to_string(), serde_json::to_value(units).unwrap_or(Value::Null));
            } else {
                let values: Vec<DefinitionValue> = state
                    .definitions
                    .get(kind)
                    .map(|entry| entry.value().clone())
                    .unwrap_or_default();
                registries.insert(kind.to_string(), serde_json::to_value(values).unwrap_or(Value::Null));
            }
        }
        Ok(json!({ "registries": registries }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn add_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddDefinitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let kind = path.into_inner();
        if !DEFINITION_KINDS.contains(&kind.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown definition kind: {kind}"
            )));
        }
        let input = body.into_inner();
        let value = input.value.trim().to_lowercase();
        if !valid_slug(&value) {
            return Err(UnifiedError::bad_request(
                "value must be lowercase letters, digits, \"_\", or \"-\" (max 64 characters)",
            ));
        }
        let label = match &input.label {
            Some(raw) => require_text(raw, "label", MAX_LABEL_LEN)?,
            None => value.clone(),
        };
        if kind == "interval_unit" {
            let days_per_unit = match input.days_per_unit {
                Some(days) if (1..=366).contains(&days) => days,
                Some(_) => {
                    return Err(UnifiedError::bad_request(
                        "days_per_unit must be between 1 and 366",
                    ));
                }
                None => {
                    return Err(UnifiedError::bad_request(
                        "interval_unit requires days_per_unit (1-366)",
                    ));
                }
            };
            if state.interval_units.get(&value).is_some() {
                return Err(UnifiedError::conflict(format!(
                    "Interval unit already exists: {value}"
                )));
            }
            let unit = IntervalUnitDef {
                unit: value.clone(),
                label,
                days_per_unit: Some(days_per_unit),
                is_calendar_month: false,
                is_usage_based: false,
                is_system: false,
            };
            state.interval_units.insert(value.clone(), unit.clone());
            state.emit(
                "field_service.definition.created",
                json!({
                    "kind": kind,
                    "value": value,
                    "created_by": caller.actor,
                }),
            );
            return Ok(serde_json::to_value(unit).unwrap_or(Value::Null));
        }
        let mut values = state
            .definitions
            .get(&kind)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        if values.iter().any(|item| item.value == value) {
            return Err(UnifiedError::conflict(format!(
                "Definition value already exists in {kind}: {value}"
            )));
        }
        values.push(DefinitionValue {
            value: value.clone(),
            label,
            is_system: false,
        });
        state.definitions.insert(kind.clone(), values.clone());
        state.emit(
            "field_service.definition.created",
            json!({
                "kind": kind,
                "value": value,
                "created_by": caller.actor,
            }),
        );
        Ok(serde_json::to_value(values).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Whether a `priority` definition value is still referenced:
/// by any work order, by any template's `default_priority`, or
/// by the settings `default_priority`.
pub(crate) fn priority_in_use(state: &FieldServiceState, value: &str) -> bool {
    state
        .work_orders
        .iter()
        .any(|entry| entry.value().priority == value)
        || state
            .templates
            .iter()
            .any(|entry| entry.value().default_priority == value)
        || state.settings().default_priority == value
}

pub(crate) async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let (kind, value) = path.into_inner();
        if !DEFINITION_KINDS.contains(&kind.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown definition kind: {kind}"
            )));
        }
        let value = value.trim().to_lowercase();
        if kind == "interval_unit" {
            let unit = state.interval_unit(&value).ok_or_else(|| {
                UnifiedError::not_found(format!("Interval unit not found: {value}"))
            })?;
            if unit.is_system {
                return Err(UnifiedError::conflict(
                    "A system interval unit cannot be deleted",
                ));
            }
            if state
                .plans
                .iter()
                .any(|entry| entry.value().interval_unit == value)
                || state
                    .templates
                    .iter()
                    .any(|entry| entry.value().interval_unit == value)
                || state
                    .equipment_types
                    .iter()
                    .any(|entry| entry.value().default_interval_unit == value)
            {
                return Err(UnifiedError::conflict(
                    "Interval unit is in use by plans, templates, or equipment types",
                ));
            }
            state.interval_units.remove(&value);
            return Ok(json!({ "deleted": true, "kind": kind, "value": value }));
        }
        let mut values = state
            .definitions
            .get(&kind)
            .map(|entry| entry.value().clone())
            .unwrap_or_default();
        let position = values
            .iter()
            .position(|item| item.value == value)
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Definition value not found in {kind}: {value}"))
            })?;
        if values[position].is_system {
            return Err(UnifiedError::conflict(format!(
                "A system value of {kind} cannot be deleted"
            )));
        }
        let in_use = match kind.as_str() {
            "work_order_type" => state
                .work_orders
                .iter()
                .any(|entry| entry.value().order_type == value),
            "priority" => priority_in_use(&state, &value),
            "equipment_status" => state
                .equipment
                .iter()
                .any(|entry| entry.value().status == value),
            _ => false,
        };
        if in_use {
            return Err(UnifiedError::conflict(format!(
                "Definition value is in use and cannot be deleted: {value}"
            )));
        }
        values.remove(position);
        state.definitions.insert(kind.clone(), values);
        state.emit(
            "field_service.definition.deleted",
            json!({
                "kind": kind,
                "value": value,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "kind": kind, "value": value }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldServiceSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldServiceSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(max_jobs) = input.max_concurrent_jobs_per_technician {
            if !(1..=20).contains(&max_jobs) {
                return Err(UnifiedError::bad_request(
                    "max_concurrent_jobs_per_technician must be between 1 and 20",
                ));
            }
            settings.max_concurrent_jobs_per_technician = max_jobs;
        }
        if let Some(days) = input.overdue_alert_days {
            if !(0..=365).contains(&days) {
                return Err(UnifiedError::bad_request(
                    "overdue_alert_days must be between 0 and 365",
                ));
            }
            settings.overdue_alert_days = days;
        }
        if let Some(days) = input.default_interval_days {
            if !(1..=3650).contains(&days) {
                return Err(UnifiedError::bad_request(
                    "default_interval_days must be between 1 and 3650",
                ));
            }
            settings.default_interval_days = days;
        }
        if let Some(priority) = input.default_priority {
            let priority = priority.trim().to_string();
            if !state.definition_exists("priority", &priority) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown priority: {priority}"
                )));
            }
            settings.default_priority = priority;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "field_service.settings.updated",
            json!({
                "updated_by": caller.actor,
                "settings": settings,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::field_service::{MaintenanceTemplate, WorkOrder, WorkOrderStatus};

    fn template_with_default_priority(priority: &str) -> MaintenanceTemplate {
        let now = Utc::now();
        MaintenanceTemplate {
            id: Uuid::new_v4(),
            name: "Quarterly".to_string(),
            description: None,
            equipment_type_id: None,
            interval_value: 90,
            interval_unit: "days".to_string(),
            checklist_template: Vec::new(),
            default_priority: priority.to_string(),
            is_system: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn priority_in_use_scans_templates_and_settings_not_just_work_orders() {
        let state = FieldServiceState::seeded();
        assert!(!priority_in_use(&state, "urgent"));

        let template = template_with_default_priority("urgent");
        state.templates.insert(template.id, template);
        assert!(
            priority_in_use(&state, "urgent"),
            "a template default priority blocks deletion"
        );

        let mut settings = state.settings();
        settings.default_priority = "critical".to_string();
        *state.settings.write().expect("settings lock poisoned") = settings;
        assert!(
            priority_in_use(&state, "critical"),
            "the settings default priority blocks deletion"
        );

        let now = Utc::now();
        state.work_orders.insert(
            Uuid::new_v4(),
            WorkOrder {
                id: Uuid::new_v4(),
                equipment_id: Uuid::new_v4(),
                plan_id: None,
                project_id: None,
                title: "Fix pump".to_string(),
                description: None,
                order_type: "corrective".to_string(),
                priority: "high".to_string(),
                status: WorkOrderStatus::Open,
                assignee_id: None,
                scheduled_at: None,
                due_at: None,
                completed_at: None,
                verified_at: None,
                closed_at: None,
                cancelled_at: None,
                created_by: "test".to_string(),
                created_at: now,
                updated_at: now,
            },
        );
        assert!(priority_in_use(&state, "high"));
        assert!(!priority_in_use(&state, "low"));
    }
}
