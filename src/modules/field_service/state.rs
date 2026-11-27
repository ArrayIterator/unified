// field_service module - Shared state.
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

use super::types::{DefinitionValue, DispatchAssignment, Equipment, EquipmentType, FieldServiceSettings, IntervalUnitDef, MaintenancePlan, MaintenanceTemplate, Signoff, WorkOrder, WorkOrderItem, WorkOrderPart};

// Shared state

/// In-memory stores for field service. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000077_field_service`; until that migration is
/// executed, field service data does not survive a restart.
#[derive(Clone)]
pub struct FieldServiceState {
    pub equipment_types: Arc<DashMap<Uuid, EquipmentType>>,
    pub equipment: Arc<DashMap<Uuid, Equipment>>,
    pub templates: Arc<DashMap<Uuid, MaintenanceTemplate>>,
    pub plans: Arc<DashMap<Uuid, MaintenancePlan>>,
    pub work_orders: Arc<DashMap<Uuid, WorkOrder>>,
    pub items: Arc<DashMap<Uuid, WorkOrderItem>>,
    pub parts: Arc<DashMap<Uuid, WorkOrderPart>>,
    pub dispatch_assignments: Arc<DashMap<Uuid, DispatchAssignment>>,
    pub signoffs: Arc<DashMap<Uuid, Signoff>>,
    /// Admin-editable value registries, keyed by kind
    /// (`work_order_type`, `priority`, `equipment_status`).
    pub definitions: Arc<DashMap<String, Vec<DefinitionValue>>>,
    /// Interval unit definitions, keyed by unit slug.
    pub interval_units: Arc<DashMap<String, IntervalUnitDef>>,
    pub settings: Arc<RwLock<FieldServiceSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl FieldServiceState {
    /// Seeded state: the configurable defaults for every registry.
    /// Seeded values are flagged `is_system` and stay editable;
    /// custom values added by the admin are flagged otherwise.
    pub fn seeded() -> Self {
        let state = Self {
            equipment_types: Arc::new(DashMap::new()),
            equipment: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            plans: Arc::new(DashMap::new()),
            work_orders: Arc::new(DashMap::new()),
            items: Arc::new(DashMap::new()),
            parts: Arc::new(DashMap::new()),
            dispatch_assignments: Arc::new(DashMap::new()),
            signoffs: Arc::new(DashMap::new()),
            definitions: Arc::new(DashMap::new()),
            interval_units: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(FieldServiceSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        };
        state.definitions.insert(
            "work_order_type".to_string(),
            vec![
                DefinitionValue { value: "preventive".to_string(), label: "Preventive".to_string(), is_system: true },
                DefinitionValue { value: "corrective".to_string(), label: "Corrective".to_string(), is_system: true },
                DefinitionValue { value: "inspection".to_string(), label: "Inspection".to_string(), is_system: true },
            ],
        );
        state.definitions.insert(
            "priority".to_string(),
            vec![
                DefinitionValue { value: "low".to_string(), label: "Low".to_string(), is_system: true },
                DefinitionValue { value: "normal".to_string(), label: "Normal".to_string(), is_system: true },
                DefinitionValue { value: "high".to_string(), label: "High".to_string(), is_system: true },
                DefinitionValue { value: "urgent".to_string(), label: "Urgent".to_string(), is_system: true },
            ],
        );
        state.definitions.insert(
            "equipment_status".to_string(),
            vec![
                DefinitionValue { value: "active".to_string(), label: "Active".to_string(), is_system: true },
                DefinitionValue { value: "out_of_service".to_string(), label: "Out of service".to_string(), is_system: true },
                DefinitionValue { value: "retired".to_string(), label: "Retired".to_string(), is_system: true },
            ],
        );
        for (unit, label, days, calendar_month, usage_based) in [
            ("days", "Days", Some(1), false, false),
            ("weeks", "Weeks", Some(7), false, false),
            ("months", "Months", None, true, false),
            ("usage_hours", "Usage hours", None, false, true),
        ] {
            state.interval_units.insert(
                unit.to_string(),
                IntervalUnitDef {
                    unit: unit.to_string(),
                    label: label.to_string(),
                    days_per_unit: days,
                    is_calendar_month: calendar_month,
                    is_usage_based: usage_based,
                    is_system: true,
                },
            );
        }
        let now = Utc::now();
        for (slug, name, description, interval_value, interval_unit) in [
            ("hvac", "HVAC", "Heating, ventilation, and air conditioning units.", 90, "days"),
            ("electrical", "Electrical", "Electrical panels, wiring, and power systems.", 180, "days"),
            ("plumbing", "Plumbing", "Pipes, pumps, and water systems.", 180, "days"),
            ("machinery", "Machinery", "Production and workshop machinery.", 90, "days"),
            ("vehicle", "Vehicle", "Service vehicles and fleet equipment.", 180, "days"),
        ] {
            let equipment_type = EquipmentType {
                id: Uuid::now_v7(),
                slug: slug.to_string(),
                name: name.to_string(),
                description: Some(description.to_string()),
                default_interval_value: interval_value,
                default_interval_unit: interval_unit.to_string(),
                is_system: true,
                created_at: now,
                updated_at: now,
            };
            state.equipment_types.insert(equipment_type.id, equipment_type);
        }
        for (name, description, interval_value, interval_unit, checklist, priority) in [
            (
                "Quarterly inspection",
                "Routine quarterly inspection of the equipment.",
                90,
                "days",
                vec![
                    "Inspect exterior and housing".to_string(),
                    "Check safety guards and labels".to_string(),
                    "Lubricate moving parts".to_string(),
                    "Record the meter reading".to_string(),
                ],
                "normal",
            ),
            (
                "Annual service",
                "Full annual service of the equipment.",
                12,
                "months",
                vec![
                    "Replace filters and consumables".to_string(),
                    "Test safety shut-offs".to_string(),
                    "Check calibration".to_string(),
                    "Record the meter reading".to_string(),
                ],
                "normal",
            ),
        ] {
            let template = MaintenanceTemplate {
                id: Uuid::now_v7(),
                name: name.to_string(),
                description: Some(description.to_string()),
                equipment_type_id: None,
                interval_value,
                interval_unit: interval_unit.to_string(),
                checklist_template: checklist,
                default_priority: priority.to_string(),
                is_system: true,
                created_at: now,
                updated_at: now,
            };
            state.templates.insert(template.id, template);
        }
        state
    }

    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit an event and report whether any listener answered.
    /// Used for cross-module requests (time logging, incidents,
    /// stock adjustments) so handlers can report honestly when
    /// no listener exists yet.
    pub(crate) fn emit_request(&self, event: &str, data: Value) -> bool {
        match self.bus() {
            Some(bus) => !bus.emit(event, data).results.is_empty(),
            None => false,
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(crate) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
        priority: &str,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "field_service",
                "category": "field_service",
                "priority": priority,
                "link": link,
            }),
        );
    }

    /// True when `value` is a registered value of registry `kind`.
    pub(crate) fn definition_exists(&self, kind: &str, value: &str) -> bool {
        self.definitions
            .get(kind)
            .map(|entry| entry.value().iter().any(|item| item.value == value))
            .unwrap_or(false)
    }

    pub(crate) fn interval_unit(&self, unit: &str) -> Option<IntervalUnitDef> {
        self.interval_units
            .get(unit)
            .map(|entry| entry.value().clone())
    }

    pub(crate) fn settings(&self) -> FieldServiceSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    /// Active (not released / completed) dispatch assignments of
    /// one technician.
    pub(crate) fn active_assignments_of(&self, technician_id: &str) -> Vec<DispatchAssignment> {
        let mut rows: Vec<DispatchAssignment> = self
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.technician_id == technician_id && row.status == "active")
            .collect();
        rows.sort_by_key(|row| row.sequence);
        rows
    }

    /// The active dispatch assignment of one work order, if any.
    pub(crate) fn active_assignment_of_order(&self, work_order_id: Uuid) -> Option<DispatchAssignment> {
        self.dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .find(|row| row.work_order_id == work_order_id && row.status == "active")
    }
}

impl Default for FieldServiceState {
    fn default() -> Self {
        Self::seeded()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_state_knows_the_built_in_interval_units() {
        let state = FieldServiceState::seeded();
        let days = state.interval_unit("days").expect("days unit is seeded");
        assert_eq!(days.days_per_unit, Some(1));
        assert!(!days.is_calendar_month);
        assert!(!days.is_usage_based);
        let months = state.interval_unit("months").expect("months unit is seeded");
        assert!(months.is_calendar_month);
        assert!(state.interval_unit("fortnights").is_none());
    }

    #[test]
    fn seeded_state_registries_start_with_the_documented_defaults() {
        let state = FieldServiceState::seeded();
        let priorities = state
            .definitions
            .get("priority")
            .expect("priority registry is seeded");
        assert!(priorities.iter().any(|value| value.value == "normal"));
    }
}
