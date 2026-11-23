//! manufacturing - types.

use unified_core::hooks::HookBus;
use chrono::{ DateTime, Utc };
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use super::{constants::*, inputs::*, helpers::*, store::*, validation::*, stock::*, module::*, hooks::*, handlers_boms::*, handlers_work_centers::*, handlers_work_orders::*, handlers_runs::*, handlers_scrap::*, handlers_units::*, handlers_settings::*, reports::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------
/// One component line of a BOM version: how much of a component
/// product goes into one unit of the manufactured product.
/// `component_product_id` may itself be a manufactured product
/// with its own BOM (multi-level BOMs). `scrap_allowance_percent`
/// is the expected loss for this line, reserved on top of the
/// nominal quantity when a work order reserves stock.
pub struct BomLine {
    pub component_product_id: String,
    pub quantity: f64,
    pub unit: String,
    #[serde(default)]
    pub scrap_allowance_percent: f64,
    #[serde(default)]
    pub notes: Option<String>,
}


/// One ordered routing step of a BOM version: an operation
/// performed at a work center, with its planned duration in
/// minutes. Steps run in `sequence` order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingStep {
    pub sequence: u32,
    pub name: String,
    pub work_center_id: String,
    pub duration_minutes: f64,
    #[serde(default)]
    pub description: Option<String>,
}


/// One immutable version of a BOM. Editing a BOM's lines or
/// routing always creates a new version; versions are never
/// mutated in place, and a work order pins the version it was
/// created from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BomVersion {
    pub id: String,
    pub bom_id: String,
    pub version: u32,
    pub lines: Vec<BomLine>,
    #[serde(default)]
    pub routing: Vec<RoutingStep>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}


/// A bill of materials: the recipe for one manufactured product.
/// `active_version_id` is the version new work orders pin when
/// the caller does not pick a version explicitly. Status is
/// `active`, `draft`, or `archived`; archived BOMs cannot start
/// new work orders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bom {
    pub id: String,
    pub product_id: String,
    pub name: String,
    pub code: String,
    #[serde(default)]
    pub description: Option<String>,
    pub status: String,
    #[serde(default)]
    pub active_version_id: Option<String>,
    #[serde(default)]
    pub latest_version: u32,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// A work center: a machine, line, or team where operations run.
/// `capacity_hours_per_day` bounds the scheduling load report;
/// `cost_per_hour` is the labor / machine rate used by the cost
/// roll-up and may be absent, in which case labor for that center
/// is reported as a cost gap rather than guessed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkCenter {
    pub id: String,
    pub name: String,
    pub code: String,
    pub capacity_hours_per_day: f64,
    #[serde(default)]
    pub cost_per_hour: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    pub status: String,
    #[serde(default)]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One component requirement of a work order, computed from the
/// pinned BOM version at creation time (exploded one level; the
/// full multi-level tree is available from the explode endpoint).
/// Quantities are for the whole order, including the scrap
/// allowance. `quantity_reserved`, `quantity_consumed`, and
/// `quantity_short` track progress against the requirement.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOrderRequirement {
    pub component_product_id: String,
    pub quantity_required: f64,
    pub unit: String,
    #[serde(default)]
    pub quantity_reserved: f64,
    #[serde(default)]
    pub quantity_consumed: f64,
    #[serde(default)]
    pub quantity_short: f64,
}


/// A work order: a production job for `quantity_planned` units of
/// `product_id`, built from a pinned BOM version. Statuses:
/// `draft`, `confirmed`, `in_progress`, `done`, `cancelled`.
/// `inspection_status` is empty until the quality side answers
/// `quality.inspection.completed` (`pass` / `fail` /
/// `conditional`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOrder {
    pub id: String,
    /// Human-readable order number, for example `WO-000001`.
    pub number: String,
    pub bom_id: String,
    pub bom_version_id: String,
    pub bom_version: u32,
    pub product_id: String,
    pub quantity_planned: f64,
    #[serde(default)]
    pub quantity_produced: f64,
    #[serde(default)]
    pub quantity_scrapped: f64,
    pub status: String,
    #[serde(default)]
    pub requirements: Vec<WorkOrderRequirement>,
    #[serde(default)]
    pub planned_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub planned_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub actual_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub actual_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub inspection_status: Option<String>,
    #[serde(default)]
    pub inspection_notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// One production run against a work order: a shift's output.
/// `quantity_scrapped` must be explained by an active scrap
/// reason code. A run is `completed` once recorded; runs are
/// append-only and never edited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProductionRun {
    pub id: String,
    pub work_order_id: String,
    pub quantity_produced: f64,
    #[serde(default)]
    pub quantity_scrapped: f64,
    #[serde(default)]
    pub scrap_reason_code_id: Option<String>,
    #[serde(default)]
    pub work_center_id: Option<String>,
    pub operator_id: String,
    pub status: String,
    #[serde(default)]
    pub notes: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}


/// A reservation of component stock for a work order. Stock is
/// never held here: this record states what was requested, and
/// the inventory side confirms or rejects it through
/// `manufacturing.stock.confirmed` / `manufacturing.stock.short`.
/// Statuses: `pending_confirmation`, `reserved`, `partially_consumed`,
/// `consumed`, `released`, `short`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaterialReservation {
    pub id: String,
    pub work_order_id: String,
    pub component_product_id: String,
    pub quantity_reserved: f64,
    #[serde(default)]
    pub quantity_consumed: f64,
    pub unit: String,
    pub status: String,
    #[serde(default)]
    pub stock_unknown: bool,
    #[serde(default)]
    pub confirmation_reference: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// An admin-editable scrap reason code. Scrap cannot be recorded
/// without one of these, and inactive codes are rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapReason {
    pub id: String,
    pub code: String,
    pub label: String,
    pub active: bool,
    #[serde(default)]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}


/// An admin-editable unit of measure definition. Component
/// quantities must use one of these units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnitDefinition {
    pub id: String,
    pub code: String,
    pub label: String,
    pub active: bool,
    pub created_at: DateTime<Utc>,
}


/// A stock confirmation / shortage notice received from the
/// inventory side, kept for audit on the work order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StockNotice {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub work_order_id: Option<String>,
    #[serde(default)]
    pub reservation_id: Option<String>,
    #[serde(default)]
    pub component_product_id: Option<String>,
    #[serde(default)]
    pub quantity: Option<f64>,
    #[serde(default)]
    pub message: Option<String>,
    pub received_at: DateTime<Utc>,
}


/// One node of a multi-level BOM explosion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExplosionNode {
    pub product_id: String,
    pub quantity: f64,
    pub unit: String,
    pub depth: u32,
    #[serde(default)]
    pub bom_id: Option<String>,
    #[serde(default)]
    pub bom_version: Option<u32>,
    #[serde(default)]
    pub children: Vec<ExplosionNode>,
}


/// Settings for the manufacturing module. Every value is a
/// configurable default: an admin changes it through the settings
/// endpoints, and the module never treats a default as a fixed
/// rule. The transition map is the state machine for work orders.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManufacturingSettings {
    /// ISO 4217 currency used by the cost roll-up.
    pub currency: String,
    /// Default capacity in hours per day for new work centers.
    pub default_capacity_hours_per_day: f64,
    /// Maximum depth of a multi-level BOM explosion.
    pub max_bom_depth: u32,
    /// Default scrap allowance percent applied when a BOM line
    /// does not state its own.
    pub default_scrap_allowance_percent: f64,
    /// When true, confirming a work order whose routing load
    /// exceeds a work center's capacity on a day is rejected
    /// with 409; when false (default) the overload is reported
    /// as a warning in the response instead.
    pub enforce_capacity_on_confirm: bool,
    /// Units of measure an admin may use on BOM lines.
    pub units: Vec<String>,
    /// Allowed work-order transitions: current status ->
    /// list of next statuses.
    pub work_order_transitions: HashMap<String, Vec<String>>,
    /// Statuses a work order may be created in (first key is
    /// the default when the caller omits `status`).
    pub work_order_statuses: Vec<String>,
}


impl Default for ManufacturingSettings {
    fn default() -> Self {
        let mut transitions: HashMap<String, Vec<String>> = HashMap::new();
        transitions.insert(
            "draft".to_string(),
            vec!["confirmed".to_string(), "cancelled".to_string()],
        );
        transitions.insert(
            "confirmed".to_string(),
            vec!["in_progress".to_string(), "cancelled".to_string()],
        );
        transitions.insert(
            "in_progress".to_string(),
            vec!["done".to_string(), "cancelled".to_string()],
        );
        transitions.insert("done".to_string(), Vec::new());
        transitions.insert("cancelled".to_string(), Vec::new());
        Self {
            currency: "USD".to_string(),
            default_capacity_hours_per_day: 8.0,
            max_bom_depth: 10,
            default_scrap_allowance_percent: 0.0,
            enforce_capacity_on_confirm: false,
            units: vec![
                "piece".to_string(),
                "kg".to_string(),
                "g".to_string(),
                "litre".to_string(),
                "metre".to_string(),
                "hour".to_string(),
                "box".to_string(),
                "pack".to_string(),
            ],
            work_order_transitions: transitions,
            work_order_statuses: vec![
                "draft".to_string(),
                "confirmed".to_string(),
                "in_progress".to_string(),
                "done".to_string(),
                "cancelled".to_string(),
            ],
        }
    }
}


/// In-memory state. DashMaps stay authoritative until migration
/// `2026-11-22-000081_manufacturing` is executed (see the module
/// docs); the migration mirrors these shapes.
#[derive(Clone)]
pub struct ManufacturingState {
    pub boms: Arc<DashMap<String, Bom>>,
    pub bom_versions: Arc<DashMap<String, BomVersion>>,
    pub work_centers: Arc<DashMap<String, WorkCenter>>,
    pub work_orders: Arc<DashMap<String, WorkOrder>>,
    pub production_runs: Arc<DashMap<String, ProductionRun>>,
    pub reservations: Arc<DashMap<String, MaterialReservation>>,
    pub scrap_reasons: Arc<DashMap<String, ScrapReason>>,
    pub units: Arc<DashMap<String, UnitDefinition>>,
    pub stock_notices: Arc<DashMap<String, StockNotice>>,
    pub settings: Arc<RwLock<ManufacturingSettings>>,
    pub order_seq: Arc<AtomicU64>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}


impl Default for ManufacturingState {
    fn default() -> Self {
        Self {
            boms: Arc::new(DashMap::new()),
            bom_versions: Arc::new(DashMap::new()),
            work_centers: Arc::new(DashMap::new()),
            work_orders: Arc::new(DashMap::new()),
            production_runs: Arc::new(DashMap::new()),
            reservations: Arc::new(DashMap::new()),
            scrap_reasons: Arc::new(DashMap::new()),
            units: Arc::new(DashMap::new()),
            stock_notices: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ManufacturingSettings::default())),
            order_seq: Arc::new(AtomicU64::new(1)),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}


impl ManufacturingState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> ManufacturingSettings {
        self.settings
            .read()
            .expect("manufacturing settings lock poisoned")
            .clone()
    }

    pub(crate) fn next_order_number(&self) -> String {
        let seq = self.order_seq.fetch_add(1, Ordering::SeqCst);
        format!("WO-{seq:06}")
    }

    /// Seed the unit definitions and scrap reason codes with
    /// configurable defaults on first use. These are starting
    /// values an admin edits or deletes through the unit and
    /// scrap-reason endpoints; no flow assumes any of them
    /// exists and every use validates against the stored
    /// definitions.
    pub(crate) fn seed_definitions(&self) {
        if self.units.is_empty() {
            let created = now();
            for code in self.settings().units {
                let unit = UnitDefinition {
                    id: new_id(),
                    code: code.clone(),
                    label: code.clone(),
                    active: true,
                    created_at: created,
                };
                self.units.insert(unit.id.clone(), unit);
            }
        }
        if self.scrap_reasons.is_empty() {
            let created = now();
            let defaults: [(&str, &str); 5] = [
                ("defective_material", "Defective material"),
                ("machine_breakdown", "Machine breakdown"),
                ("operator_error", "Operator error"),
                ("setup_waste", "Setup / trial waste"),
                ("damaged_in_handling", "Damaged in handling"),
            ];
            for (code, label) in defaults {
                let reason = ScrapReason {
                    id: new_id(),
                    code: code.to_string(),
                    label: label.to_string(),
                    active: true,
                    description: None,
                    created_at: created,
                    updated_at: created,
                };
                self.scrap_reasons.insert(reason.id.clone(), reason);
            }
        }
    }
}
