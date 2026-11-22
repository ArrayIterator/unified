// Manufacturing / MRP module — bills of materials, work orders,
// production runs, work centers, material reservations, cost
// roll-up, and production reports.
//
// Bills of materials (BOMs) are versioned: every edit of the
// component lines or the routing creates a new version and a work
// order pins the version it was created from, so a production run
// always uses the BOM that was approved when the order was placed.
// BOMs are multi-level: a component may itself be a manufactured
// product with its own BOM, and the explosion endpoint resolves
// the full tree with cycle detection and a configurable depth cap.
//
// Work orders move through `draft -> confirmed -> in_progress ->
// done / cancelled`. Transitions are checked against the
// transition map in the module settings (admin-editable); an
// illegal transition is a 409 carrying the list of allowed next
// states.
//
// Interconnection happens only through the hook bus; this module
// never calls another module directly and never duplicates stock
// data:
//
//   - Stock availability is requested with
//     `manufacturing.stock.requested` (`{product_ids}`); an
//     inventory handler answers with per-product
//     `{available, reserved, unit}` levels. When no handler
//     answers, availability is reported as `unknown` and nothing
//     is assumed: reservations are recorded as
//     `pending_confirmation` and no purchase request is emitted.
//   - Reserving components emits
//     `manufacturing.stock.reserve_requested` per component.
//     Releasing emits `manufacturing.stock.release_requested`.
//   - Consuming components emits `manufacturing.components.consumed`
//     per component and `assets.stock.decrement_requested` per
//     line, exactly like the POS stock contract.
//   - Producing finished goods emits
//     `manufacturing.goods.produced` per production run and, on
//     work-order completion, `manufacturing.finished_goods.costed`
//     with the cost roll-up for invoicing.
//   - Short stock emits `manufacturing.purchase.requested`
//     (`{work_order_id, product_id, quantity_short, unit}`) so
//     the procurement side can raise a purchase request.
//   - Completion emits `manufacturing.inspection.requested`
//     (`{work_order_id, target_ref, target_kind, product_id,
//     quantity}` — `target_ref` is the work order id and
//     `target_kind` is `work_order`, the keys the quality side
//     reads) so the quality side can inspect the order; this
//     module listens for the canonical
//     `quality.inspection.completed` and records the outcome on
//     the work order. Before completing, the completion path
//     consults the quality blocks (`quality.blocks.query`, the
//     hook form of quality's `GET /blocks`) and refuses to
//     complete a work order a blocking quality alert covers.
//   - Cost data is requested with `manufacturing.cost.requested`
//     (`{items: [{product_id, quantity, unit}], currency}`);
//     a calculator handler answers with per-product unit costs.
//     Missing cost data is reported as a gap in the response,
//     never fabricated.
//   - This module listens for `manufacturing.stock.confirmed`
//     and `manufacturing.stock.short` and records the
//     confirmations / shortages against the work order.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory BOMs with versions, work centers with capacity and
// scheduling, work orders with checked transitions, reservations,
// production runs, scrap recording against admin-defined reason
// codes, cost roll-up from hook-answered prices plus work-center
// labor rates, and the four reports. What is NOT built here:
//   - Persistence. Durable rows are defined by migration
//     `2026-11-22-000081_manufacturing`; until that migration is
//     executed, records do not survive a restart. Settings,
//     scrap-reason codes, and unit definitions are admin-editable
//     runtime configuration (same pattern as the billing and POS
//     settings); their durable form is a follow-up for the
//     settings store.
//   - Stock application. The stock request / reserve / release
//     events above are a defined contract, but no inventory
//     handler is registered for them in this codebase at the time
//     of writing, so until the inventory side answers them,
//     availability stays `unknown` and stock does not move.
//     Reservations still record what was requested so the
//     inventory side can reconcile them when it lands.
//   - Cost answers. No handler is registered for
//     `manufacturing.cost.requested` at the time of writing, so
//     the cost roll-up reports every component as a gap until
//     the calculator side answers the event. Labor cost is
//     computed from work-center rates stored here.
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check, and no manufacturing route is exposed through
// the API Manager public-access rules (default deny).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the assets and POS modules. Capability
// checks use the `role.check` hook and fail closed (no answer
// means not allowed).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, RwLock,
};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_CODE_LEN: usize = 64;
const MAX_NOTES_LEN: usize = 5000;
const MAX_LABEL_LEN: usize = 120;
const MAX_UNIT_LEN: usize = 32;
/// Hard ceiling on BOM explosion depth, applied on top of the
/// configurable `max_bom_depth` setting so a pathological BOM
/// tree cannot recurse without bound.
const MAX_BOM_DEPTH_HARD_CAP: u32 = 50;
/// Hard ceiling on the number of lines a single BOM version may
/// carry, so one request cannot allocate an unbounded list.
const MAX_BOM_LINES: usize = 500;
/// Hard ceiling on routing steps per BOM version.
const MAX_ROUTING_STEPS: usize = 200;

/// Capability names checked through the `role.check` hook.
const PERM_MFG_VIEW: &str = "manufacturing.view";
const PERM_MFG_MANAGE: &str = "manufacturing.manage";
const PERM_MFG_PRODUCE: &str = "manufacturing.produce";
const PERM_MFG_ADMIN: &str = "manufacturing.admin";

/// Events this module emits.
const EVENT_BOM_CREATED: &str = "manufacturing.bom.created";
const EVENT_BOM_VERSION_CREATED: &str = "manufacturing.bom.version.created";
const EVENT_WORK_ORDER_CREATED: &str = "manufacturing.work_order.created";
const EVENT_WORK_ORDER_STATUS_CHANGED: &str = "manufacturing.work_order.status_changed";
const EVENT_COMPONENTS_CONSUMED: &str = "manufacturing.components.consumed";
const EVENT_GOODS_PRODUCED: &str = "manufacturing.goods.produced";
const EVENT_PURCHASE_REQUESTED: &str = "manufacturing.purchase.requested";
const EVENT_INSPECTION_REQUESTED: &str = "manufacturing.inspection.requested";
const EVENT_FINISHED_GOODS_COSTED: &str = "manufacturing.finished_goods.costed";
const EVENT_STOCK_REQUESTED: &str = "manufacturing.stock.requested";
const EVENT_STOCK_RESERVE_REQUESTED: &str = "manufacturing.stock.reserve_requested";
const EVENT_STOCK_RELEASE_REQUESTED: &str = "manufacturing.stock.release_requested";
const EVENT_STOCK_DECREMENT_REQUESTED: &str = "assets.stock.decrement_requested";
const EVENT_COST_REQUESTED: &str = "manufacturing.cost.requested";
const EVENT_RESERVATION_CREATED: &str = "manufacturing.reservation.created";
const EVENT_RESERVATION_RELEASED: &str = "manufacturing.reservation.released";
const EVENT_PRODUCTION_RUN_COMPLETED: &str = "manufacturing.production_run.completed";
const EVENT_SCRAP_RECORDED: &str = "manufacturing.scrap.recorded";

/// Events this module listens to.
const EVENT_STOCK_CONFIRMED: &str = "manufacturing.stock.confirmed";
const EVENT_STOCK_SHORT: &str = "manufacturing.stock.short";
/// Canonical quality-completion event (emitted by the quality
/// module when an inspection completes). This module used to listen
/// for `manufacturing.inspection.completed`, which quality never
/// emitted; the canonical name is `quality.inspection.completed`.
const EVENT_INSPECTION_COMPLETED: &str = "quality.inspection.completed";
/// Quality block query (hook form of quality's `GET /blocks`):
/// ask the quality module which blocking alerts cover a target
/// before letting that target proceed.
const QUALITY_BLOCKS_QUERY: &str = "quality.blocks.query";
/// Target kind this module uses for work orders in quality
/// payloads (`target_ref` = the work order id).
const QUALITY_TARGET_KIND_WORK_ORDER: &str = "work_order";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One component line of a BOM version: how much of a component
/// product goes into one unit of the manufactured product.
/// `component_product_id` may itself be a manufactured product
/// with its own BOM (multi-level BOMs). `scrap_allowance_percent`
/// is the expected loss for this line, reserved on top of the
/// nominal quantity when a work order reserves stock.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> ManufacturingSettings {
        self.settings
            .read()
            .expect("manufacturing settings lock poisoned")
            .clone()
    }

    fn next_order_number(&self) -> String {
        let seq = self.order_seq.fetch_add(1, Ordering::SeqCst);
        format!("WO-{seq:06}")
    }

    /// Seed the unit definitions and scrap reason codes with
    /// configurable defaults on first use. These are starting
    /// values an admin edits or deletes through the unit and
    /// scrap-reason endpoints; no flow assumes any of them
    /// exists and every use validates against the stored
    /// definitions.
    fn seed_definitions(&self) {
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

// ---------------------------------------------------------------------------
// Request DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateBomRequest {
    pub product_id: String,
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub lines: Vec<BomLineInput>,
    #[serde(default)]
    pub routing: Vec<RoutingStepInput>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BomLineInput {
    pub component_product_id: String,
    pub quantity: f64,
    pub unit: String,
    #[serde(default)]
    pub scrap_allowance_percent: Option<f64>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RoutingStepInput {
    pub sequence: u32,
    pub name: String,
    pub work_center_id: String,
    pub duration_minutes: f64,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBomRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    /// When present, a new BOM version is created from these
    /// lines (and routing when given) and becomes active.
    #[serde(default)]
    pub lines: Option<Vec<BomLineInput>>,
    #[serde(default)]
    pub routing: Option<Vec<RoutingStepInput>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBomVersionRequest {
    pub lines: Vec<BomLineInput>,
    #[serde(default)]
    pub routing: Vec<RoutingStepInput>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkCenterRequest {
    pub name: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub capacity_hours_per_day: Option<f64>,
    #[serde(default)]
    pub cost_per_hour: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkCenterRequest {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub capacity_hours_per_day: Option<f64>,
    #[serde(default)]
    pub cost_per_hour: Option<f64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkOrderRequest {
    pub bom_id: String,
    #[serde(default)]
    pub bom_version_id: Option<String>,
    pub quantity: f64,
    #[serde(default)]
    pub planned_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub planned_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkOrderRequest {
    #[serde(default)]
    pub quantity: Option<f64>,
    #[serde(default)]
    pub planned_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub planned_end: Option<DateTime<Utc>>,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TransitionRequest {
    pub to_status: String,
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ConsumeRequest {
    pub items: Vec<ConsumeItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct ConsumeItemInput {
    pub component_product_id: String,
    pub quantity: f64,
    #[serde(default)]
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProduceRequest {
    pub quantity_produced: f64,
    #[serde(default)]
    pub quantity_scrapped: f64,
    #[serde(default)]
    pub scrap_reason_code_id: Option<String>,
    #[serde(default)]
    pub work_center_id: Option<String>,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default)]
    pub started_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateScrapReasonRequest {
    pub code: String,
    pub label: String,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateScrapReasonRequest {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub active: Option<bool>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateUnitRequest {
    pub code: String,
    pub label: String,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub default_capacity_hours_per_day: Option<f64>,
    #[serde(default)]
    pub max_bom_depth: Option<u32>,
    #[serde(default)]
    pub default_scrap_allowance_percent: Option<f64>,
    #[serde(default)]
    pub enforce_capacity_on_confirm: Option<bool>,
    #[serde(default)]
    pub units: Option<Vec<String>>,
    #[serde(default)]
    pub work_order_transitions: Option<HashMap<String, Vec<String>>>,
    #[serde(default)]
    pub work_order_statuses: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn round_money(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the assets and POS modules.
struct Caller {
    actor: String,
    role: String,
}

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `crate::core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// — when it lands — takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// OPEN (not closed by this guard): the in-memory DashMap stores
/// in this module carry no `tenant_id` on their records yet, so
/// there is no per-record tenant filtering to apply once the
/// context is resolved — that needs a store-shape change (tenant
/// id on every record + a migration column) and is integrator
/// work. What this guard does close is the context requirement
/// itself: every tenant-data endpoint resolves the tenant first
/// through `caller()`, and a request with no tenant context is
/// rejected before any business data is read.
fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is bound for the Diesel persistence layer (see
    // `resolve_tenant`); the in-memory stores below carry no
    // tenant id yet, so per-record filtering is still open.
    let _tenant = resolve_tenant(req)?;
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

fn valid_quantity(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value <= 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number greater than zero"
        )));
    }
    Ok(value)
}

fn valid_non_negative(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite number at least zero"
        )));
    }
    Ok(value)
}

fn valid_percent(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be between 0 and 100"
        )));
    }
    Ok(value)
}

fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let code = currency.trim().to_uppercase();
    if code.len() != 3 || !code.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO 4217 code",
        ));
    }
    Ok(code)
}

fn parse_date_filter(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}

fn in_period(ts: DateTime<Utc>, from: Option<NaiveDate>, to: Option<NaiveDate>) -> bool {
    let date = ts.date_naive();
    if let Some(from) = from {
        if date < from {
            return false;
        }
    }
    if let Some(to) = to {
        if date > to {
            return false;
        }
    }
    true
}

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

fn load_bom(state: &ManufacturingState, id: &str) -> UnifiedResult<Bom> {
    state
        .boms
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BOM not found: {id}")))
}

fn load_bom_version(state: &ManufacturingState, id: &str) -> UnifiedResult<BomVersion> {
    state
        .bom_versions
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("BOM version not found: {id}")))
}

fn load_work_center(state: &ManufacturingState, id: &str) -> UnifiedResult<WorkCenter> {
    state
        .work_centers
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Work center not found: {id}")))
}

fn load_work_order(state: &ManufacturingState, id: &str) -> UnifiedResult<WorkOrder> {
    state
        .work_orders
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Work order not found: {id}")))
}

fn load_scrap_reason(state: &ManufacturingState, id: &str) -> UnifiedResult<ScrapReason> {
    state
        .scrap_reasons
        .get(id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Scrap reason not found: {id}")))
}

fn active_bom_for_product(state: &ManufacturingState, product_id: &str) -> Option<Bom> {
    state
        .boms
        .iter()
        .map(|entry| entry.value().clone())
        .find(|bom| bom.product_id == product_id && bom.status == "active")
}

fn save_work_order(state: &ManufacturingState, order: &WorkOrder) {
    state.work_orders.insert(order.id.clone(), order.clone());
}

fn save_bom(state: &ManufacturingState, bom: &Bom) {
    state.boms.insert(bom.id.clone(), bom.clone());
}

// ---------------------------------------------------------------------------
// BOM validation and explosion
// ---------------------------------------------------------------------------

/// Validate BOM lines against the stored unit definitions and
/// the caller's own limits. Returns validated lines with the
/// caller's scrap allowance filled from the settings default.
fn validate_lines(
    state: &ManufacturingState,
    inputs: &[BomLineInput],
    settings: &ManufacturingSettings,
) -> UnifiedResult<Vec<BomLine>> {
    if inputs.is_empty() {
        return Err(UnifiedError::bad_request(
            "A BOM must have at least one component line",
        ));
    }
    if inputs.len() > MAX_BOM_LINES {
        return Err(UnifiedError::bad_request(format!(
            "A BOM may have at most {MAX_BOM_LINES} lines"
        )));
    }
    let mut lines = Vec::with_capacity(inputs.len());
    let mut seen: HashSet<String> = HashSet::new();
    for input in inputs {
        let component = require_text(&input.component_product_id, "component_product_id", MAX_CODE_LEN)?;
        if !seen.insert(component.clone()) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate component in BOM lines: {component}"
            )));
        }
        let quantity = valid_quantity(input.quantity, "quantity")?;
        let unit = require_text(&input.unit, "unit", MAX_UNIT_LEN)?;
        if !settings.units.iter().any(|u| u == &unit) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown unit \"{unit}\"; units are admin-defined (see GET /api/v1/manufacturing/units)"
            )));
        }
        let allowance = match input.scrap_allowance_percent {
            Some(value) => valid_percent(value, "scrap_allowance_percent")?,
            None => settings.default_scrap_allowance_percent,
        };
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        lines.push(BomLine {
            component_product_id: component,
            quantity,
            unit,
            scrap_allowance_percent: allowance,
            notes,
        });
    }
    Ok(lines)
}

/// Validate routing steps: work centers must exist, sequences
/// must be unique, durations positive.
fn validate_routing(
    state: &ManufacturingState,
    inputs: &[RoutingStepInput],
) -> UnifiedResult<Vec<RoutingStep>> {
    if inputs.len() > MAX_ROUTING_STEPS {
        return Err(UnifiedError::bad_request(format!(
            "A BOM may have at most {MAX_ROUTING_STEPS} routing steps"
        )));
    }
    let mut steps = Vec::with_capacity(inputs.len());
    let mut seen_seq: HashSet<u32> = HashSet::new();
    for input in inputs {
        if !seen_seq.insert(input.sequence) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate routing sequence: {}",
                input.sequence
            )));
        }
        let name = require_text(&input.name, "routing step name", MAX_NAME_LEN)?;
        let center_id = require_text(&input.work_center_id, "work_center_id", MAX_CODE_LEN)?;
        load_work_center(state, &center_id)?;
        let duration = valid_quantity(input.duration_minutes, "duration_minutes")?;
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        steps.push(RoutingStep {
            sequence: input.sequence,
            name,
            work_center_id: center_id,
            duration_minutes: duration,
            description,
        });
    }
    steps.sort_by_key(|step| step.sequence);
    Ok(steps)
}

fn create_version(
    state: &ManufacturingState,
    bom_id: &str,
    version_number: u32,
    lines: Vec<BomLine>,
    routing: Vec<RoutingStep>,
    actor: &str,
) -> BomVersion {
    let version = BomVersion {
        id: new_id(),
        bom_id: bom_id.to_string(),
        version: version_number,
        lines,
        routing,
        created_by: actor.to_string(),
        created_at: now(),
    };
    state
        .bom_versions
        .insert(version.id.clone(), version.clone());
    state.emit(
        EVENT_BOM_VERSION_CREATED,
        json!({
            "bom_id": bom_id,
            "version_id": version.id,
            "version": version_number,
            "created_by": actor,
        }),
    );
    version
}

/// Explode a BOM version into a multi-level tree for `quantity`
/// units of the finished product. Components that have an active
/// BOM of their own are expanded recursively; a component already
/// on the current path is a cycle and is reported as an error
/// naming the product, and the configurable depth cap stops
/// runaway trees.
fn explode_version(
    state: &ManufacturingState,
    version: &BomVersion,
    quantity: f64,
    settings: &ManufacturingSettings,
) -> UnifiedResult<Vec<ExplosionNode>> {
    let max_depth = settings.max_bom_depth.min(MAX_BOM_DEPTH_HARD_CAP).max(1);
    let mut path: Vec<String> = Vec::new();
    let mut nodes = Vec::new();
    for line in &version.lines {
        nodes.push(explode_line(state, line, quantity, 0, max_depth, &mut path)?);
    }
    Ok(nodes)
}

fn explode_line(
    state: &ManufacturingState,
    line: &BomLine,
    parent_quantity: f64,
    depth: u32,
    max_depth: u32,
    path: &mut Vec<String>,
) -> UnifiedResult<ExplosionNode> {
    if path.contains(&line.component_product_id) {
        let mut cycle = path.clone();
        cycle.push(line.component_product_id.clone());
        return Err(UnifiedError::conflict(format!(
            "BOM cycle detected: {}",
            cycle.join(" -> ")
        )));
    }
    let quantity = parent_quantity * line.quantity;
    let mut node = ExplosionNode {
        product_id: line.component_product_id.clone(),
        quantity,
        unit: line.unit.clone(),
        depth,
        bom_id: None,
        bom_version: None,
        children: Vec::new(),
    };
    if depth < max_depth {
        if let Some(child_bom) = active_bom_for_product(state, &line.component_product_id) {
            if let Some(active_id) = &child_bom.active_version_id {
                if let Ok(child_version) = load_bom_version(state, active_id) {
                    node.bom_id = Some(child_bom.id.clone());
                    node.bom_version = Some(child_version.version);
                    path.push(line.component_product_id.clone());
                    for child_line in &child_version.lines {
                        node.children.push(explode_line(
                            state,
                            child_line,
                            quantity,
                            depth + 1,
                            max_depth,
                            path,
                        )?);
                    }
                    path.pop();
                }
            }
        }
    }
    Ok(node)
}

/// Flatten an explosion tree into total quantities per product
/// (leaf totals, keyed by product id with its unit).
fn flatten_explosion(nodes: &[ExplosionNode], totals: &mut HashMap<String, (f64, String)>) {
    for node in nodes {
        let entry = totals
            .entry(node.product_id.clone())
            .or_insert((0.0, node.unit.clone()));
        entry.0 += node.quantity;
        flatten_explosion(&node.children, totals);
    }
}

// ---------------------------------------------------------------------------
// Stock and cost hooks (request / response over the bus)
// ---------------------------------------------------------------------------

/// One product's stock level as answered by the inventory side.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StockLevel {
    #[serde(default)]
    pub available: f64,
    #[serde(default)]
    pub reserved: f64,
    #[serde(default)]
    pub unit: Option<String>,
}

/// Ask the inventory side for stock levels. Returns the levels
/// plus whether any handler answered at all; when nothing
/// answers, the caller must treat availability as unknown.
fn query_stock(
    state: &ManufacturingState,
    product_ids: &[String],
) -> (HashMap<String, StockLevel>, bool) {
    let Some(bus) = state.bus() else {
        return (HashMap::new(), false);
    };
    let context = bus.emit(
        EVENT_STOCK_REQUESTED,
        json!({ "product_ids": product_ids }),
    );
    let mut levels: HashMap<String, StockLevel> = HashMap::new();
    let mut answered = false;
    if let Some(stock) = context.get("stock").and_then(|v| v.as_object()) {
        answered = true;
        for (product_id, value) in stock {
            if let Ok(level) = serde_json::from_value::<StockLevel>(value.clone()) {
                levels.insert(product_id.clone(), level);
            }
        }
    }
    for result in &context.results {
        if let Some(stock) = result.get("stock").and_then(|v| v.as_object()) {
            answered = true;
            for (product_id, value) in stock {
                if let Ok(level) = serde_json::from_value::<StockLevel>(value.clone()) {
                    levels.insert(product_id.clone(), level);
                }
            }
        }
        if let Some(product_id) = result.get("product_id").and_then(|v| v.as_str()) {
            if let Ok(level) = serde_json::from_value::<StockLevel>(result.clone()) {
                answered = true;
                levels.insert(product_id.to_string(), level);
            }
        }
    }
    (levels, answered)
}

/// Ask the calculator side for per-product unit costs. Returns
/// the answered costs plus whether any handler answered.
fn query_unit_costs(
    state: &ManufacturingState,
    items: &[(String, f64, String)],
    currency: &str,
) -> (HashMap<String, f64>, bool) {
    let Some(bus) = state.bus() else {
        return (HashMap::new(), false);
    };
    let payload_items: Vec<Value> = items
        .iter()
        .map(|(product_id, quantity, unit)| {
            json!({ "product_id": product_id, "quantity": quantity, "unit": unit })
        })
        .collect();
    let context = bus.emit(
        EVENT_COST_REQUESTED,
        json!({ "items": payload_items, "currency": currency }),
    );
    let mut costs: HashMap<String, f64> = HashMap::new();
    let mut answered = false;
    let read_costs = |value: &Value, costs: &mut HashMap<String, f64>, answered: &mut bool| {
        if let Some(map) = value.get("unit_costs").and_then(|v| v.as_object()) {
            *answered = true;
            for (product_id, cost) in map {
                if let Some(number) = cost.as_f64() {
                    costs.insert(product_id.clone(), number);
                }
            }
        }
        if let Some(product_id) = value.get("product_id").and_then(|v| v.as_str()) {
            if let Some(number) = value.get("unit_cost").and_then(|v| v.as_f64()) {
                *answered = true;
                costs.insert(product_id.to_string(), number);
            }
        }
    };
    read_costs(&context.data.clone(), &mut costs, &mut answered);
    for result in &context.results {
        read_costs(result, &mut costs, &mut answered);
    }
    (costs, answered)
}

/// Cost roll-up for a work order: component cost from hook
/// answers plus labor from work-center rates. Anything without
/// a price is listed in `gaps` and excluded from the totals;
/// the totals therefore cover only priced inputs and are
/// labelled as partial whenever gaps exist.
fn compute_cost(state: &ManufacturingState, order: &WorkOrder) -> Value {
    let settings = state.settings();
    let items: Vec<(String, f64, String)> = order
        .requirements
        .iter()
        .map(|req| {
            (
                req.component_product_id.clone(),
                req.quantity_required,
                req.unit.clone(),
            )
        })
        .collect();
    let (unit_costs, cost_answered) = query_unit_costs(state, &items, &settings.currency);
    let mut component_lines: Vec<Value> = Vec::new();
    let mut gaps: Vec<Value> = Vec::new();
    let mut component_total = 0.0;
    for req in &order.requirements {
        match unit_costs.get(&req.component_product_id) {
            Some(unit_cost) => {
                let line_total = round_money(req.quantity_required * unit_cost);
                component_total += line_total;
                component_lines.push(json!({
                    "component_product_id": req.component_product_id,
                    "quantity": req.quantity_required,
                    "unit": req.unit,
                    "unit_cost": round_money(*unit_cost),
                    "line_total": line_total,
                    "currency": settings.currency,
                }));
            }
            None => {
                gaps.push(json!({
                    "kind": "missing_component_cost",
                    "component_product_id": req.component_product_id,
                    "quantity": req.quantity_required,
                    "unit": req.unit,
                    "reason": if cost_answered {
                        "No cost handler answered for this product"
                    } else {
                        "No cost handler is registered for manufacturing.cost.requested"
                    },
                }));
            }
        }
    }
    let version = load_bom_version(state, &order.bom_version_id).ok();
    let mut labor_lines: Vec<Value> = Vec::new();
    let mut labor_total = 0.0;
    if let Some(version) = version {
        for step in &version.routing {
            let center = load_work_center(state, &step.work_center_id).ok();
            let hours = step.duration_minutes / 60.0 * order.quantity_planned;
            match center.and_then(|c| c.cost_per_hour) {
                Some(rate) => {
                    let line_total = round_money(hours * rate);
                    labor_total += line_total;
                    labor_lines.push(json!({
                        "work_center_id": step.work_center_id,
                        "step": step.name,
                        "hours": (hours * 100.0).round() / 100.0,
                        "rate_per_hour": round_money(rate),
                        "line_total": line_total,
                        "currency": settings.currency,
                    }));
                }
                None => {
                    gaps.push(json!({
                        "kind": "missing_labor_rate",
                        "work_center_id": step.work_center_id,
                        "step": step.name,
                        "hours": (hours * 100.0).round() / 100.0,
                        "reason": "Work center has no cost_per_hour configured",
                    }));
                }
            }
        }
    }
    let total = round_money(component_total + labor_total);
    let produced = order.quantity_produced;
    let unit_cost_value: Value = if produced > 0.0 {
        json!(round_money(total / produced))
    } else {
        Value::Null
    };
    json!({
        "work_order_id": order.id,
        "number": order.number,
        "currency": settings.currency,
        "quantity_planned": order.quantity_planned,
        "quantity_produced": produced,
        "component_cost": round_money(component_total),
        "labor_cost": round_money(labor_total),
        "total_cost": total,
        "unit_cost": unit_cost_value,
        "partial": !gaps.is_empty(),
        "components": component_lines,
        "labor": labor_lines,
        "gaps": gaps,
    })
}

/// Daily load of a work center over a date range: routing steps
/// of confirmed and in-progress work orders are laid out in
/// sequence from each order's planned start (or now when none is
/// set), and each step's minutes land on the day it starts. Days
/// whose load exceeds capacity are flagged `overloaded`.
fn work_center_schedule(
    state: &ManufacturingState,
    center_id: &str,
    from: NaiveDate,
    to: NaiveDate,
) -> Vec<Value> {
    let mut load_minutes: HashMap<NaiveDate, f64> = HashMap::new();
    for entry in state.work_orders.iter() {
        let order = entry.value();
        if order.status != "confirmed" && order.status != "in_progress" {
            continue;
        }
        let Ok(version) = load_bom_version(state, &order.bom_version_id) else {
            continue;
        };
        let mut cursor = order.planned_start.unwrap_or_else(now);
        for step in &version.routing {
            if step.work_center_id != center_id {
                cursor += chrono::Duration::minutes(step.duration_minutes as i64);
                continue;
            }
            let day = cursor.date_naive();
            if day >= from && day <= to {
                let minutes = step.duration_minutes * order.quantity_planned;
                *load_minutes.entry(day).or_insert(0.0) += minutes;
            }
            cursor += chrono::Duration::minutes(step.duration_minutes as i64);
        }
    }
    let center = load_work_center(state, center_id).ok();
    let capacity_minutes = center
        .as_ref()
        .map(|c| c.capacity_hours_per_day * 60.0)
        .unwrap_or(0.0);
    let mut days: Vec<Value> = Vec::new();
    let mut day = from;
    while day <= to {
        let minutes = load_minutes.get(&day).copied().unwrap_or(0.0);
        days.push(json!({
            "date": day.to_string(),
            "scheduled_minutes": minutes,
            "capacity_minutes": capacity_minutes,
            "load_percent": if capacity_minutes > 0.0 {
                (minutes / capacity_minutes * 100.0 * 100.0).round() / 100.0
            } else {
                0.0
            },
            "overloaded": capacity_minutes > 0.0 && minutes > capacity_minutes,
        }));
        match day.succ_opt() {
            Some(next) => day = next,
            None => break,
        }
    }
    days
}

/// Overload warnings for confirming a work order: days in its
/// planned window where any routing work center would exceed
/// capacity once this order's steps are added.
fn capacity_warnings(state: &ManufacturingState, order: &WorkOrder) -> Vec<Value> {
    let mut warnings: Vec<Value> = Vec::new();
    let Ok(version) = load_bom_version(state, &order.bom_version_id) else {
        return warnings;
    };
    let mut cursor = order.planned_start.unwrap_or_else(now);
    for step in &version.routing {
        let day = cursor.date_naive();
        let minutes = step.duration_minutes * order.quantity_planned;
        let existing = work_center_schedule(state, &step.work_center_id, day, day);
        let used = existing
            .first()
            .and_then(|d| d.get("scheduled_minutes"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        let capacity = existing
            .first()
            .and_then(|d| d.get("capacity_minutes"))
            .and_then(|v| v.as_f64())
            .unwrap_or(0.0);
        if capacity > 0.0 && used + minutes > capacity {
            warnings.push(json!({
                "work_center_id": step.work_center_id,
                "step": step.name,
                "date": day.to_string(),
                "scheduled_minutes": used + minutes,
                "capacity_minutes": capacity,
                "message": "Routing load exceeds work center capacity on this day",
            }));
        }
        cursor += chrono::Duration::minutes(step.duration_minutes as i64);
    }
    warnings
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ManufacturingModule {
    state: ManufacturingState,
}

impl ManufacturingModule {
    pub fn new() -> Self {
        Self {
            state: ManufacturingState::default(),
        }
    }
}

impl Default for ManufacturingModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ManufacturingModule {
    fn id(&self) -> &'static str {
        "manufacturing"
    }

    fn name(&self) -> &'static str {
        "Manufacturing"
    }

    fn description(&self) -> &'static str {
        "Manufacturing resource planning: versioned multi-level bills of materials, work centers with capacity and scheduling, work orders with checked state transitions, material reservations, production runs with scrap recording, cost roll-up with honest gaps, and production reports. Stock, procurement, quality, and billing are wired through the hook bus only."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["assets", "calculator", "billing"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["assets", "calculator", "billing"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        self.state.seed_definitions();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_stock_confirmed(state.clone(), ctx));
        bus.register(EVENT_STOCK_CONFIRMED, "manufacturing", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_stock_short(state.clone(), ctx));
        bus.register(EVENT_STOCK_SHORT, "manufacturing", handler);
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_inspection_completed(state.clone(), ctx));
        bus.register(EVENT_INSPECTION_COMPLETED, "manufacturing", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/manufacturing")
                .app_data(state)
                // Bills of materials.
                .route("/boms", web::get().to(list_boms))
                .route("/boms", web::post().to(create_bom))
                .route("/boms/{id}", web::get().to(get_bom))
                .route("/boms/{id}", web::patch().to(update_bom))
                .route("/boms/{id}", web::delete().to(delete_bom))
                .route("/boms/{id}/versions", web::get().to(list_bom_versions))
                .route("/boms/{id}/versions", web::post().to(create_bom_version))
                .route(
                    "/boms/{id}/versions/{version_id}",
                    web::get().to(get_bom_version),
                )
                .route(
                    "/boms/{id}/versions/{version_id}/activate",
                    web::post().to(activate_bom_version),
                )
                .route("/boms/{id}/explode", web::get().to(explode_bom))
                // Work centers.
                .route("/work-centers", web::get().to(list_work_centers))
                .route("/work-centers", web::post().to(create_work_center))
                .route("/work-centers/{id}", web::get().to(get_work_center))
                .route("/work-centers/{id}", web::patch().to(update_work_center))
                .route("/work-centers/{id}", web::delete().to(delete_work_center))
                .route(
                    "/work-centers/{id}/schedule",
                    web::get().to(get_work_center_schedule),
                )
                // Work orders.
                .route("/work-orders", web::get().to(list_work_orders))
                .route("/work-orders", web::post().to(create_work_order))
                .route("/work-orders/{id}", web::get().to(get_work_order))
                .route("/work-orders/{id}", web::patch().to(update_work_order))
                .route(
                    "/work-orders/{id}/transition",
                    web::post().to(transition_work_order),
                )
                .route(
                    "/work-orders/{id}/check-stock",
                    web::post().to(check_work_order_stock),
                )
                .route(
                    "/work-orders/{id}/reserve",
                    web::post().to(reserve_work_order),
                )
                .route(
                    "/work-orders/{id}/release-reservations",
                    web::post().to(release_work_order_reservations),
                )
                .route(
                    "/work-orders/{id}/consume",
                    web::post().to(consume_work_order),
                )
                .route(
                    "/work-orders/{id}/produce",
                    web::post().to(produce_work_order),
                )
                .route(
                    "/work-orders/{id}/complete",
                    web::post().to(complete_work_order),
                )
                .route("/work-orders/{id}/cost", web::get().to(get_work_order_cost))
                .route(
                    "/work-orders/{id}/reservations",
                    web::get().to(list_work_order_reservations),
                )
                .route(
                    "/work-orders/{id}/runs",
                    web::get().to(list_work_order_runs),
                )
                // Production runs.
                .route("/production-runs", web::get().to(list_production_runs))
                .route("/production-runs/{id}", web::get().to(get_production_run))
                // Reservations.
                .route("/reservations", web::get().to(list_reservations))
                // Scrap reasons (admin definitions).
                .route("/scrap-reasons", web::get().to(list_scrap_reasons))
                .route("/scrap-reasons", web::post().to(create_scrap_reason))
                .route(
                    "/scrap-reasons/{id}",
                    web::patch().to(update_scrap_reason),
                )
                .route(
                    "/scrap-reasons/{id}",
                    web::delete().to(delete_scrap_reason),
                )
                // Units of measure (admin definitions).
                .route("/units", web::get().to(list_units))
                .route("/units", web::post().to(create_unit))
                .route("/units/{id}", web::delete().to(delete_unit))
                // Settings.
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                // Reports.
                .route(
                    "/reports/work-orders-by-status",
                    web::get().to(report_work_orders_by_status),
                )
                .route(
                    "/reports/production-output",
                    web::get().to(report_production_output),
                )
                .route(
                    "/reports/component-usage",
                    web::get().to(report_component_usage),
                )
                .route("/reports/scrap-rate", web::get().to(report_scrap_rate)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms", owner: "manufacturing", description: "List bills of materials (paginated, filter by status, product_id, search by name/code)." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/boms", owner: "manufacturing", description: "Create a BOM with component lines and routing; creates version 1." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}", owner: "manufacturing", description: "Get one BOM with its active version." },
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/boms/{id}", owner: "manufacturing", description: "Update a BOM; new lines or routing create a new version and activate it." },
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/boms/{id}", owner: "manufacturing", description: "Delete a BOM with no work orders." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}/versions", owner: "manufacturing", description: "List the versions of a BOM (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/boms/{id}/versions", owner: "manufacturing", description: "Create a new BOM version from lines and routing." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}/versions/{version_id}", owner: "manufacturing", description: "Get one BOM version with its lines and routing." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/boms/{id}/versions/{version_id}/activate", owner: "manufacturing", description: "Make a version the active version for new work orders." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/boms/{id}/explode", owner: "manufacturing", description: "Multi-level BOM explosion for a quantity, with cycle detection and a configurable depth cap." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-centers", owner: "manufacturing", description: "List work centers (paginated, filter by status, search by name/code)." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-centers", owner: "manufacturing", description: "Create a work center with capacity and optional labor rate." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-centers/{id}", owner: "manufacturing", description: "Get one work center." },
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/work-centers/{id}", owner: "manufacturing", description: "Update a work center (capacity, labor rate, status)." },
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/work-centers/{id}", owner: "manufacturing", description: "Delete a work center not referenced by any BOM routing." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-centers/{id}/schedule", owner: "manufacturing", description: "Daily scheduled load versus capacity for a work center over a date range (from/to filters)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders", owner: "manufacturing", description: "List work orders (paginated, filter by status, product_id, bom_id)." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders", owner: "manufacturing", description: "Create a work order from a BOM and quantity; pins the active (or chosen) BOM version." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}", owner: "manufacturing", description: "Get one work order with its component requirements." },
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/work-orders/{id}", owner: "manufacturing", description: "Update a draft work order (quantity, planned dates, notes); recomputes requirements." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/transition", owner: "manufacturing", description: "Move a work order to a new status; illegal transitions are 409 with the allowed list." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/check-stock", owner: "manufacturing", description: "Check component availability through the stock hook; shortfalls emit purchase requests." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/reserve", owner: "manufacturing", description: "Reserve components for a work order; emits stock reserve requests per component." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/release-reservations", owner: "manufacturing", description: "Release a work order's reservations; emits stock release requests." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/consume", owner: "manufacturing", description: "Record component consumption on an in-progress work order; emits components.consumed and stock decrement requests." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/produce", owner: "manufacturing", description: "Record a production run (produced and scrapped quantities, scrap reason required when scrapping); emits goods.produced." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/work-orders/{id}/complete", owner: "manufacturing", description: "Complete a work order; emits inspection, finished-goods cost, and status events." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}/cost", owner: "manufacturing", description: "Cost roll-up for a work order; missing prices are reported as gaps, never fabricated." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}/reservations", owner: "manufacturing", description: "List a work order's material reservations (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/work-orders/{id}/runs", owner: "manufacturing", description: "List a work order's production runs (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/production-runs", owner: "manufacturing", description: "List production runs (paginated, filter by work_order_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/production-runs/{id}", owner: "manufacturing", description: "Get one production run." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reservations", owner: "manufacturing", description: "List material reservations (paginated, filter by work_order_id, component_product_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/scrap-reasons", owner: "manufacturing", description: "List the admin-defined scrap reason codes (paginated, filter by active)." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/scrap-reasons", owner: "manufacturing", description: "Add a scrap reason code (admin)." },
            RouteInfo { method: "PATCH", path: "/api/v1/manufacturing/scrap-reasons/{id}", owner: "manufacturing", description: "Update a scrap reason code (admin)." },
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/scrap-reasons/{id}", owner: "manufacturing", description: "Delete a scrap reason code not used by any production run (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/units", owner: "manufacturing", description: "List the admin-defined units of measure (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/manufacturing/units", owner: "manufacturing", description: "Add a unit of measure (admin)." },
            RouteInfo { method: "DELETE", path: "/api/v1/manufacturing/units/{id}", owner: "manufacturing", description: "Delete a unit of measure not used by any BOM line (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/settings", owner: "manufacturing", description: "Get the manufacturing settings, including the work-order transition map." },
            RouteInfo { method: "PUT", path: "/api/v1/manufacturing/settings", owner: "manufacturing", description: "Update the manufacturing settings (admin)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/work-orders-by-status", owner: "manufacturing", description: "Report: work-order counts and planned quantities grouped by status." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/production-output", owner: "manufacturing", description: "Report: produced and scrapped quantities per period (from/to filters, group_by day/week/month)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/component-usage", owner: "manufacturing", description: "Report: consumed component quantities per product over a period (from/to filters)." },
            RouteInfo { method: "GET", path: "/api/v1/manufacturing/reports/scrap-rate", owner: "manufacturing", description: "Report: scrap rate overall and per product over a period (from/to filters)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ManufacturingModule::new()));
}

// ---------------------------------------------------------------------------
// Hook listeners
// ---------------------------------------------------------------------------

/// Record a stock confirmation from the inventory side and mark
/// the matching reservations confirmed.
fn on_stock_confirmed(state: ManufacturingState, ctx: &mut HookContext) {
    let reference = ctx
        .get("reference_id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let work_order_id = ctx
        .get("work_order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reservation_id = ctx
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let component_product_id = ctx
        .get("component_product_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let quantity = ctx.get("quantity").and_then(|v| v.as_f64());
    let status = ctx
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("confirmed")
        .to_string();
    let notice = StockNotice {
        id: new_id(),
        kind: "confirmed".to_string(),
        work_order_id: work_order_id.clone(),
        reservation_id: reservation_id.clone(),
        component_product_id: component_product_id.clone(),
        quantity,
        message: ctx.get("message").and_then(|v| v.as_str()).map(str::to_string),
        received_at: now(),
    };
    state.stock_notices.insert(notice.id.clone(), notice);
    if status == "confirmed" {
        if let Some(reservation_id) = reservation_id {
            if let Some(mut reservation) = state.reservations.get_mut(&reservation_id) {
                reservation.status = "reserved".to_string();
                reservation.confirmation_reference = if reference.is_empty() {
                    None
                } else {
                    Some(reference)
                };
                reservation.stock_unknown = false;
                reservation.updated_at = now();
            }
        } else if let Some(work_order_id) = work_order_id {
            for mut entry in state.reservations.iter_mut() {
                let reservation = entry.value_mut();
                if reservation.work_order_id == work_order_id
                    && reservation.status == "pending_confirmation"
                {
                    reservation.status = "reserved".to_string();
                    reservation.stock_unknown = false;
                    reservation.updated_at = now();
                }
            }
        }
    }
}

/// Record a stock shortage notice from the inventory side and
/// mark the matching reservations short.
fn on_stock_short(state: ManufacturingState, ctx: &mut HookContext) {
    let work_order_id = ctx
        .get("work_order_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let reservation_id = ctx
        .get("reservation_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let component_product_id = ctx
        .get("component_product_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let quantity = ctx
        .get("quantity_short")
        .or_else(|| ctx.get("short_quantity"))
        .and_then(|v| v.as_f64());
    let notice = StockNotice {
        id: new_id(),
        kind: "short".to_string(),
        work_order_id: work_order_id.clone(),
        reservation_id: reservation_id.clone(),
        component_product_id: component_product_id.clone(),
        quantity,
        message: ctx.get("message").and_then(|v| v.as_str()).map(str::to_string),
        received_at: now(),
    };
    state.stock_notices.insert(notice.id.clone(), notice);
    if let Some(reservation_id) = reservation_id {
        if let Some(mut reservation) = state.reservations.get_mut(&reservation_id) {
            reservation.status = "short".to_string();
            reservation.stock_unknown = false;
            reservation.updated_at = now();
        }
    }
    if let (Some(work_order_id), Some(product_id), Some(short)) =
        (work_order_id, component_product_id, quantity)
    {
        if let Some(mut order) = state.work_orders.get_mut(&work_order_id) {
            for req in order.requirements.iter_mut() {
                if req.component_product_id == product_id {
                    req.quantity_short = short;
                }
            }
            order.updated_at = now();
        }
    }
}

/// Record a quality inspection outcome on the work order.
///
/// Listens for the canonical `quality.inspection.completed`
/// (payload: `{inspection_id, point_id, target_ref, target_kind,
/// result, inspector_id, blocked, alert_id?}` where `target_ref`
/// is the work order id and `target_kind` is `work_order`).
/// Inspections for other target kinds are ignored — they belong
/// to another module's record. A `work_order_id` key is still
/// accepted as a fallback for the target, for emitters that send
/// the old shape. When the completion carries `blocked: true`
/// the block is named in the inspection notes, so the work order
/// shows why it must not proceed until the alert is resolved.
fn on_inspection_completed(state: ManufacturingState, ctx: &mut HookContext) {
    if let Some(target_kind) = ctx.get("target_kind").and_then(|v| v.as_str()) {
        if target_kind != QUALITY_TARGET_KIND_WORK_ORDER {
            return;
        }
    }
    let Some(work_order_id) = ctx
        .get("target_ref")
        .and_then(|v| v.as_str())
        .or_else(|| ctx.get("work_order_id").and_then(|v| v.as_str()))
    else {
        return;
    };
    let outcome = ctx
        .get("result")
        .or_else(|| ctx.get("outcome"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let blocked = ctx
        .get("blocked")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let alert_id = ctx.get("alert_id").and_then(|v| v.as_str());
    let notes = ctx.get("notes").and_then(|v| v.as_str()).map(str::to_string);
    if let Some(mut order) = state.work_orders.get_mut(work_order_id) {
        order.inspection_status = Some(outcome);
        order.inspection_notes = match (notes, blocked, alert_id) {
            (Some(notes), true, Some(alert_id)) => Some(format!(
                "{notes} Blocked by quality alert {alert_id} until the alert is resolved."
            )),
            (Some(notes), true, None) => Some(format!(
                "{notes} Blocked by a quality alert until the alert is resolved."
            )),
            (Some(notes), false, _) => Some(notes),
            (None, true, Some(alert_id)) => Some(format!(
                "Blocked by quality alert {alert_id} until the alert is resolved."
            )),
            (None, true, None) => {
                Some("Blocked by a quality alert until the alert is resolved.".to_string())
            }
            (None, false, _) => order.inspection_notes.clone(),
        };
        order.updated_at = now();
    }
}

/// Ask the quality module which blocking alerts cover a target
/// (`quality.blocks.query`, the hook form of quality's
/// `GET /blocks`). Returns the blocking alerts as JSON values
/// (each carrying at least `id`, `target_ref`, `target_kind`,
/// `message`). An empty list means either no block exists or the
/// quality module is not registered — in the latter case there is
/// no owner of blocks at all, so there is nothing to refuse on.
fn quality_blocks_for(
    state: &ManufacturingState,
    target_ref: &str,
    target_kind: &str,
) -> Vec<Value> {
    let Some(bus) = state.bus() else {
        return Vec::new();
    };
    let context = bus.emit(
        QUALITY_BLOCKS_QUERY,
        json!({ "target_ref": target_ref, "target_kind": target_kind }),
    );
    let mut blocks: Vec<Value> = Vec::new();
    let mut read = |value: &Value| {
        if let Some(list) = value.get("blocks").and_then(|v| v.as_array()) {
            blocks.extend(list.iter().cloned());
        }
    };
    read(&context.data);
    for result in &context.results {
        read(result);
    }
    blocks
}

// ---------------------------------------------------------------------------
// BOM handlers
// ---------------------------------------------------------------------------

async fn list_boms(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Bom> = state
            .boms
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|bom| {
                if let Some(status) = &status_filter {
                    if bom.status != *status {
                        return false;
                    }
                }
                if let Some(product_id) = &product_filter {
                    if bom.product_id != *product_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", bom.name, bom.code, bom.product_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "code", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "code" => a.code.to_lowercase().cmp(&b.code.to_lowercase()),
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

async fn create_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateBomRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Bom> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let product_id = require_text(&input.product_id, "product_id", MAX_CODE_LEN)?;
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = match &input.code {
            Some(raw) => require_text(raw, "code", MAX_CODE_LEN)?.to_uppercase(),
            None => format!("BOM-{}", &new_id()[..8].to_uppercase()),
        };
        if state.boms.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "BOM code already exists: {code}"
            )));
        }
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        let lines = validate_lines(&state, &input.lines, &settings)?;
        let routing = validate_routing(&state, &input.routing)?;
        let bom = Bom {
            id: new_id(),
            product_id,
            name,
            code,
            description,
            status: "active".to_string(),
            active_version_id: None,
            latest_version: 1,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        let version = create_version(&state, &bom.id, 1, lines, routing, &caller.actor);
        let mut bom = bom;
        bom.active_version_id = Some(version.id.clone());
        save_bom(&state, &bom);
        state.emit(
            EVENT_BOM_CREATED,
            json!({
                "bom_id": bom.id,
                "product_id": bom.product_id,
                "code": bom.code,
                "version_id": version.id,
                "version": 1,
                "created_by": caller.actor,
            }),
        );
        Ok(bom)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

fn bom_with_version(state: &ManufacturingState, bom: Bom) -> Value {
    let version = bom
        .active_version_id
        .as_ref()
        .and_then(|id| load_bom_version(state, id).ok());
    json!({ "bom": bom, "active_version": version })
}

async fn get_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let bom = load_bom(&state, &path.into_inner())?;
        Ok(bom_with_version(&state, bom))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBomRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let id = path.into_inner();
        let mut bom = load_bom(&state, &id)?;
        let settings = state.settings();
        if let Some(name) = &input.name {
            bom.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            bom.description = optional_text(&Some(description.clone()), "description", MAX_NOTES_LEN)?;
        }
        if let Some(status) = &input.status {
            let status = status.trim().to_lowercase();
            if !["active", "draft", "archived"].contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(
                    "status must be active, draft, or archived",
                ));
            }
            bom.status = status;
        }
        // New lines (or routing) always create a new version.
        if let Some(lines_input) = &input.lines {
            let lines = validate_lines(&state, lines_input, &settings)?;
            let routing_input = input.routing.clone().unwrap_or_default();
            let routing = validate_routing(&state, &routing_input)?;
            let next = bom.latest_version + 1;
            let version = create_version(&state, &bom.id, next, lines, routing, &caller.actor);
            bom.latest_version = next;
            bom.active_version_id = Some(version.id.clone());
        } else if let Some(routing_input) = &input.routing {
            let current = bom
                .active_version_id
                .as_ref()
                .and_then(|vid| load_bom_version(&state, vid).ok())
                .ok_or_else(|| {
                    UnifiedError::conflict("BOM has no active version to base routing on")
                })?;
            let routing = validate_routing(&state, routing_input)?;
            let next = bom.latest_version + 1;
            let version = create_version(
                &state,
                &bom.id,
                next,
                current.lines.clone(),
                routing,
                &caller.actor,
            );
            bom.latest_version = next;
            bom.active_version_id = Some(version.id.clone());
        }
        bom.updated_at = now();
        save_bom(&state, &bom);
        Ok(bom_with_version(&state, bom))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let id = path.into_inner();
        let bom = load_bom(&state, &id)?;
        if state
            .work_orders
            .iter()
            .any(|entry| entry.value().bom_id == bom.id)
        {
            return Err(UnifiedError::conflict(
                "BOM cannot be deleted: work orders reference it",
            ));
        }
        state.boms.remove(&bom.id);
        state
            .bom_versions
            .retain(|_, version| version.bom_id != bom.id);
        Ok(json!({ "deleted": true, "bom_id": bom.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_bom_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let bom = load_bom(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<BomVersion> = state
            .bom_versions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|version| version.bom_id == bom.id)
            .collect();
        items.sort_by_key(|version| std::cmp::Reverse(version.version));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_bom_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateBomVersionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BomVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut bom = load_bom(&state, &path.into_inner())?;
        let settings = state.settings();
        let lines = validate_lines(&state, &input.lines, &settings)?;
        let routing = validate_routing(&state, &input.routing)?;
        let next = bom.latest_version + 1;
        let version = create_version(&state, &bom.id, next, lines, routing, &caller.actor);
        bom.latest_version = next;
        bom.active_version_id = Some(version.id.clone());
        bom.updated_at = now();
        save_bom(&state, &bom);
        Ok(version)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_bom_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BomVersion> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let (bom_id, version_id) = path.into_inner();
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom_id {
            return Err(UnifiedError::not_found(format!(
                "BOM version not found: {version_id}"
            )));
        }
        Ok(version)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn activate_bom_version(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let (bom_id, version_id) = path.into_inner();
        let mut bom = load_bom(&state, &bom_id)?;
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom.id {
            return Err(UnifiedError::not_found(format!(
                "BOM version not found: {version_id}"
            )));
        }
        bom.active_version_id = Some(version.id.clone());
        bom.updated_at = now();
        save_bom(&state, &bom);
        Ok(bom_with_version(&state, bom))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn explode_bom(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let bom = load_bom(&state, &path.into_inner())?;
        let quantity = params
            .filters
            .get("quantity")
            .and_then(|raw| raw.parse::<f64>().ok())
            .unwrap_or(1.0);
        let quantity = valid_quantity(quantity, "quantity")?;
        let version_id = params
            .filters
            .get("version_id")
            .cloned()
            .or(bom.active_version_id.clone())
            .ok_or_else(|| UnifiedError::conflict("BOM has no active version"))?;
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom.id {
            return Err(UnifiedError::not_found(format!(
                "BOM version not found: {version_id}"
            )));
        }
        let settings = state.settings();
        let tree = explode_version(&state, &version, quantity, &settings)?;
        let mut totals: HashMap<String, (f64, String)> = HashMap::new();
        flatten_explosion(&tree, &mut totals);
        let mut flat: Vec<Value> = totals
            .into_iter()
            .map(|(product_id, (total_quantity, unit))| {
                json!({ "product_id": product_id, "quantity": total_quantity, "unit": unit })
            })
            .collect();
        flat.sort_by(|a, b| {
            a.get("product_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("product_id").and_then(|v| v.as_str()))
        });
        Ok(json!({
            "bom_id": bom.id,
            "product_id": bom.product_id,
            "bom_version": version.version,
            "quantity": quantity,
            "tree": tree,
            "totals": flat,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Work center handlers
// ---------------------------------------------------------------------------

async fn list_work_centers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WorkCenter> = state
            .work_centers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|center| {
                if let Some(status) = &status_filter {
                    if center.status != *status {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", center.name, center.code).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["name", "code", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                "code" => a.code.to_lowercase().cmp(&b.code.to_lowercase()),
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

async fn create_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateWorkCenterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkCenter> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let code = match &input.code {
            Some(raw) => require_text(raw, "code", MAX_CODE_LEN)?.to_uppercase(),
            None => format!("WC-{}", &new_id()[..8].to_uppercase()),
        };
        if state
            .work_centers
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Work center code already exists: {code}"
            )));
        }
        let capacity = match input.capacity_hours_per_day {
            Some(value) => valid_quantity(value, "capacity_hours_per_day")?,
            None => settings.default_capacity_hours_per_day,
        };
        let cost_per_hour = match input.cost_per_hour {
            Some(value) => Some(valid_non_negative(value, "cost_per_hour")?),
            None => None,
        };
        let currency = match &input.currency {
            Some(raw) => Some(valid_currency(raw)?),
            None => None,
        };
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        let center = WorkCenter {
            id: new_id(),
            name,
            code,
            capacity_hours_per_day: capacity,
            cost_per_hour,
            currency,
            status: "active".to_string(),
            description,
            created_at: now(),
            updated_at: now(),
        };
        state
            .work_centers
            .insert(center.id.clone(), center.clone());
        Ok(center)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkCenter> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        load_work_center(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkCenterRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkCenter> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut center = load_work_center(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            center.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(capacity) = input.capacity_hours_per_day {
            center.capacity_hours_per_day = valid_quantity(capacity, "capacity_hours_per_day")?;
        }
        if let Some(cost) = input.cost_per_hour {
            center.cost_per_hour = Some(valid_non_negative(cost, "cost_per_hour")?);
        }
        if let Some(currency) = &input.currency {
            center.currency = Some(valid_currency(currency)?);
        }
        if let Some(status) = &input.status {
            let status = status.trim().to_lowercase();
            if !["active", "inactive"].contains(&status.as_str()) {
                return Err(UnifiedError::bad_request(
                    "status must be active or inactive",
                ));
            }
            center.status = status;
        }
        if let Some(description) = &input.description {
            center.description =
                optional_text(&Some(description.clone()), "description", MAX_NOTES_LEN)?;
        }
        center.updated_at = now();
        state
            .work_centers
            .insert(center.id.clone(), center.clone());
        Ok(center)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_work_center(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let center = load_work_center(&state, &path.into_inner())?;
        if state.bom_versions.iter().any(|entry| {
            entry
                .value()
                .routing
                .iter()
                .any(|step| step.work_center_id == center.id)
        }) {
            return Err(UnifiedError::conflict(
                "Work center cannot be deleted: BOM routings reference it",
            ));
        }
        state.work_centers.remove(&center.id);
        Ok(json!({ "deleted": true, "work_center_id": center.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_work_center_schedule(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let center = load_work_center(&state, &path.into_inner())?;
        let today = now().date_naive();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw))
            .unwrap_or(today);
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw))
            .unwrap_or(from);
        if to < from {
            return Err(UnifiedError::bad_request(
                "to must not be earlier than from",
            ));
        }
        if (to - from).num_days() > 366 {
            return Err(UnifiedError::bad_request(
                "Schedule range must be at most 366 days",
            ));
        }
        let days = work_center_schedule(&state, &center.id, from, to);
        Ok(json!({
            "work_center_id": center.id,
            "code": center.code,
            "capacity_hours_per_day": center.capacity_hours_per_day,
            "from": from.to_string(),
            "to": to.to_string(),
            "days": days,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Work order handlers
// ---------------------------------------------------------------------------

/// Build the component requirements for a work order from its
/// pinned BOM version and planned quantity (one level; the scrap
/// allowance is added on top of the nominal quantity).
fn build_requirements(version: &BomVersion, quantity: f64) -> Vec<WorkOrderRequirement> {
    version
        .lines
        .iter()
        .map(|line| {
            let allowance = 1.0 + line.scrap_allowance_percent / 100.0;
            WorkOrderRequirement {
                component_product_id: line.component_product_id.clone(),
                quantity_required: quantity * line.quantity * allowance,
                unit: line.unit.clone(),
                quantity_reserved: 0.0,
                quantity_consumed: 0.0,
                quantity_short: 0.0,
            }
        })
        .collect()
}

async fn list_work_orders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let product_filter = params.filters.get("product_id").cloned();
        let bom_filter = params.filters.get("bom_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WorkOrder> = state
            .work_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|order| {
                if let Some(status) = &status_filter {
                    if order.status != *status {
                        return false;
                    }
                }
                if let Some(product_id) = &product_filter {
                    if order.product_id != *product_id {
                        return false;
                    }
                }
                if let Some(bom_id) = &bom_filter {
                    if order.bom_id != *bom_id {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", order.number, order.product_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["number", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "number" => a.number.cmp(&b.number),
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

async fn create_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let bom = load_bom(&state, &input.bom_id)?;
        if bom.status == "archived" {
            return Err(UnifiedError::conflict(
                "Archived BOMs cannot start new work orders",
            ));
        }
        let version_id = input
            .bom_version_id
            .clone()
            .or(bom.active_version_id.clone())
            .ok_or_else(|| UnifiedError::conflict("BOM has no active version"))?;
        let version = load_bom_version(&state, &version_id)?;
        if version.bom_id != bom.id {
            return Err(UnifiedError::bad_request(
                "bom_version_id does not belong to bom_id",
            ));
        }
        let quantity = valid_quantity(input.quantity, "quantity")?;
        if let (Some(start), Some(end)) = (input.planned_start, input.planned_end) {
            if end < start {
                return Err(UnifiedError::bad_request(
                    "planned_end must not be earlier than planned_start",
                ));
            }
        }
        let company_id = optional_text(&input.company_id, "company_id", MAX_CODE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let order = WorkOrder {
            id: new_id(),
            number: state.next_order_number(),
            bom_id: bom.id.clone(),
            bom_version_id: version.id.clone(),
            bom_version: version.version,
            product_id: bom.product_id.clone(),
            quantity_planned: quantity,
            quantity_produced: 0.0,
            quantity_scrapped: 0.0,
            status: "draft".to_string(),
            requirements: build_requirements(&version, quantity),
            planned_start: input.planned_start,
            planned_end: input.planned_end,
            actual_start: None,
            actual_end: None,
            company_id,
            notes,
            inspection_status: None,
            inspection_notes: None,
            created_by: caller.actor.clone(),
            created_at: now(),
            updated_at: now(),
        };
        save_work_order(&state, &order);
        state.emit(
            EVENT_WORK_ORDER_CREATED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "bom_id": order.bom_id,
                "bom_version_id": order.bom_version_id,
                "product_id": order.product_id,
                "quantity_planned": order.quantity_planned,
                "created_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        load_work_order(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only draft work orders can be edited (current status: {})",
                order.status
            )));
        }
        if let Some(quantity) = input.quantity {
            order.quantity_planned = valid_quantity(quantity, "quantity")?;
            let version = load_bom_version(&state, &order.bom_version_id)?;
            order.requirements = build_requirements(&version, order.quantity_planned);
        }
        if let Some(start) = input.planned_start {
            order.planned_start = Some(start);
        }
        if let Some(end) = input.planned_end {
            order.planned_end = Some(end);
        }
        if let (Some(start), Some(end)) = (order.planned_start, order.planned_end) {
            if end < start {
                return Err(UnifiedError::bad_request(
                    "planned_end must not be earlier than planned_start",
                ));
            }
        }
        if let Some(notes) = &input.notes {
            order.notes = optional_text(&Some(notes.clone()), "notes", MAX_NOTES_LEN)?;
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        Ok(order)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Checked transition: the target must appear in the settings
/// transition map for the current status, otherwise 409 with the
/// allowed list. Confirming also runs the capacity check.
async fn transition_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TransitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let input = body.into_inner();
        let mut order = load_work_order(&state, &path.into_inner())?;
        let target = input.to_status.trim().to_lowercase();
        let settings = state.settings();
        let allowed = settings
            .work_order_transitions
            .get(&order.status)
            .cloned()
            .unwrap_or_default();
        if !allowed.contains(&target) {
            return Err(UnifiedError::conflict(format!(
                "Illegal transition from \"{}\" to \"{}\"",
                order.status, target
            ))
            .with_metadata("allowed", json!(allowed))
            .with_metadata("current_status", json!(order.status)));
        }
        if target == "in_progress" && order.actual_start.is_none() {
            order.actual_start = Some(now());
        }
        if target == "done" {
            if order.quantity_produced <= 0.0 {
                return Err(UnifiedError::conflict(
                    "A work order with no produced quantity cannot be marked done; record a production run first",
                ));
            }
            order.actual_end = Some(now());
        }
        if target == "cancelled" {
            release_reservations(&state, &order.id, &caller.actor);
        }
        let mut warnings: Vec<Value> = Vec::new();
        if target == "confirmed" {
            warnings = capacity_warnings(&state, &order);
            if settings.enforce_capacity_on_confirm && !warnings.is_empty() {
                return Err(UnifiedError::conflict(
                    "Work order cannot be confirmed: routing load exceeds work center capacity",
                )
                .with_metadata("capacity_warnings", json!(warnings)));
            }
        }
        let previous = order.status.clone();
        order.status = target.clone();
        if let Some(notes) = &input.notes {
            order.notes = optional_text(&Some(notes.clone()), "notes", MAX_NOTES_LEN)?;
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        state.emit(
            EVENT_WORK_ORDER_STATUS_CHANGED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "from_status": previous,
                "to_status": target,
                "changed_by": caller.actor,
            }),
        );
        Ok(json!({ "work_order": order, "capacity_warnings": warnings }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Release every open reservation of a work order (used on
/// cancel and by the release endpoint). Emits a release request
/// per reservation.
fn release_reservations(state: &ManufacturingState, work_order_id: &str, actor: &str) {
    let mut released: Vec<String> = Vec::new();
    for mut entry in state.reservations.iter_mut() {
        let reservation = entry.value_mut();
        if reservation.work_order_id == work_order_id
            && (reservation.status == "reserved"
                || reservation.status == "pending_confirmation"
                || reservation.status == "short")
        {
            reservation.status = "released".to_string();
            reservation.updated_at = now();
            released.push(reservation.id.clone());
            state.emit(
                EVENT_STOCK_RELEASE_REQUESTED,
                json!({
                    "reservation_id": reservation.id,
                    "work_order_id": work_order_id,
                    "component_product_id": reservation.component_product_id,
                    "quantity": reservation.quantity_reserved,
                    "unit": reservation.unit,
                    "released_by": actor,
                }),
            );
        }
    }
    if !released.is_empty() {
        state.emit(
            EVENT_RESERVATION_RELEASED,
            json!({
                "work_order_id": work_order_id,
                "reservation_ids": released,
                "released_by": actor,
            }),
        );
    }
}

/// Shared stock check: asks the inventory side, compares with
/// the order's requirements, emits a purchase request per
/// shortfall, and reports the per-component picture.
fn check_stock_for_order(
    state: &ManufacturingState,
    order: &WorkOrder,
    actor: &str,
) -> Value {
    let product_ids: Vec<String> = order
        .requirements
        .iter()
        .map(|req| req.component_product_id.clone())
        .collect();
    let (levels, answered) = query_stock(state, &product_ids);
    let mut rows: Vec<Value> = Vec::new();
    let mut shortfalls: Vec<Value> = Vec::new();
    for req in &order.requirements {
        let available = levels.get(&req.component_product_id).map(|l| l.available);
        let remaining = req.quantity_required - req.quantity_consumed - req.quantity_reserved;
        let short = match available {
            Some(available) => (remaining - available).max(0.0),
            None => 0.0,
        };
        if short > 0.0 {
            shortfalls.push(json!({
                "component_product_id": req.component_product_id,
                "quantity_short": short,
                "unit": req.unit,
            }));
            state.emit(
                EVENT_PURCHASE_REQUESTED,
                json!({
                    "work_order_id": order.id,
                    "product_id": req.component_product_id,
                    "quantity_short": short,
                    "unit": req.unit,
                    "requested_by": actor,
                }),
            );
        }
        rows.push(json!({
            "component_product_id": req.component_product_id,
            "unit": req.unit,
            "quantity_required": req.quantity_required,
            "quantity_reserved": req.quantity_reserved,
            "quantity_consumed": req.quantity_consumed,
            "quantity_remaining": remaining.max(0.0),
            "available": available,
            "quantity_short": short,
            "stock_status": if answered { "known" } else { "unknown" },
        }));
    }
    json!({
        "work_order_id": order.id,
        "number": order.number,
        "stock_answered": answered,
        "stock_status": if answered { "known" } else { "unknown" },
        "components": rows,
        "shortfalls": shortfalls,
    })
}

async fn check_work_order_stock(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        Ok(check_stock_for_order(&state, &order, &caller.actor))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reserve_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "confirmed" && order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only confirmed or in-progress work orders can reserve stock (current status: {})",
                order.status
            )));
        }
        let product_ids: Vec<String> = order
            .requirements
            .iter()
            .map(|req| req.component_product_id.clone())
            .collect();
        let (levels, answered) = query_stock(&state, &product_ids);
        let mut created: Vec<MaterialReservation> = Vec::new();
        let mut shortfalls: Vec<Value> = Vec::new();
        for req in order.requirements.iter_mut() {
            let remaining = req.quantity_required - req.quantity_reserved;
            if remaining <= 0.0 {
                continue;
            }
            // Do not double-reserve a component that already has
            // an open reservation on this order.
            let already_open = state.reservations.iter().any(|entry| {
                let r = entry.value();
                r.work_order_id == order.id
                    && r.component_product_id == req.component_product_id
                    && matches!(
                        r.status.as_str(),
                        "pending_confirmation" | "reserved" | "partially_consumed" | "short"
                    )
            });
            if already_open {
                continue;
            }
            let available = levels.get(&req.component_product_id).map(|l| l.available);
            let mut status = if answered { "reserved" } else { "pending_confirmation" };
            if let Some(available) = available {
                if available < remaining {
                    status = "short";
                    shortfalls.push(json!({
                        "component_product_id": req.component_product_id,
                        "quantity_short": remaining - available,
                        "unit": req.unit,
                    }));
                    state.emit(
                        EVENT_PURCHASE_REQUESTED,
                        json!({
                            "work_order_id": order.id,
                            "product_id": req.component_product_id,
                            "quantity_short": remaining - available,
                            "unit": req.unit,
                            "requested_by": caller.actor,
                        }),
                    );
                }
            }
            let reservation = MaterialReservation {
                id: new_id(),
                work_order_id: order.id.clone(),
                component_product_id: req.component_product_id.clone(),
                quantity_reserved: remaining,
                quantity_consumed: 0.0,
                unit: req.unit.clone(),
                status: status.to_string(),
                stock_unknown: !answered,
                confirmation_reference: None,
                created_by: caller.actor.clone(),
                created_at: now(),
                updated_at: now(),
            };
            if status == "reserved" {
                req.quantity_reserved += remaining;
            }
            state.emit(
                EVENT_STOCK_RESERVE_REQUESTED,
                json!({
                    "reservation_id": reservation.id,
                    "work_order_id": order.id,
                    "component_product_id": reservation.component_product_id,
                    "quantity": reservation.quantity_reserved,
                    "unit": reservation.unit,
                    "requested_by": caller.actor,
                }),
            );
            state
                .reservations
                .insert(reservation.id.clone(), reservation.clone());
            created.push(reservation);
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        state.emit(
            EVENT_RESERVATION_CREATED,
            json!({
                "work_order_id": order.id,
                "reservation_ids": created.iter().map(|r| r.id.clone()).collect::<Vec<_>>(),
                "created_by": caller.actor,
            }),
        );
        Ok(json!({
            "work_order_id": order.id,
            "stock_answered": answered,
            "stock_status": if answered { "known" } else { "unknown" },
            "reservations": created,
            "shortfalls": shortfalls,
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn release_work_order_reservations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        release_reservations(&state, &order.id, &caller.actor);
        for req in order.requirements.iter_mut() {
            req.quantity_reserved = 0.0;
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        Ok(json!({ "work_order_id": order.id, "released": true }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn consume_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ConsumeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let input = body.into_inner();
        if input.items.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one consumption item is required",
            ));
        }
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only in-progress work orders can consume components (current status: {})",
                order.status
            )));
        }
        let mut consumed_rows: Vec<Value> = Vec::new();
        for item in &input.items {
            let component = require_text(
                &item.component_product_id,
                "component_product_id",
                MAX_CODE_LEN,
            )?;
            let quantity = valid_quantity(item.quantity, "quantity")?;
            let position = order
                .requirements
                .iter()
                .position(|req| req.component_product_id == component)
                .ok_or_else(|| {
                    UnifiedError::bad_request(format!(
                        "Component is not part of this work order: {component}"
                    ))
                })?;
            let req = &mut order.requirements[position];
            let remaining = req.quantity_required - req.quantity_consumed;
            if quantity > remaining {
                return Err(UnifiedError::conflict(format!(
                    "Cannot consume {quantity} of {component}: only {remaining} remains on this work order"
                )));
            }
            if let Some(unit) = &item.unit {
                let unit = require_text(unit, "unit", MAX_UNIT_LEN)?;
                if unit != req.unit {
                    return Err(UnifiedError::bad_request(format!(
                        "Unit mismatch for {component}: work order uses {}",
                        req.unit
                    )));
                }
            }
            req.quantity_consumed += quantity;
            // Draw down the matching reservations first.
            let mut left = quantity;
            let mut reservation_ids: Vec<String> = state
                .reservations
                .iter()
                .filter(|entry| {
                    let r = entry.value();
                    r.work_order_id == order.id
                        && r.component_product_id == component
                        && (r.status == "reserved" || r.status == "partially_consumed")
                })
                .map(|entry| entry.key().clone())
                .collect();
            reservation_ids.sort();
            for reservation_id in reservation_ids {
                if left <= 0.0 {
                    break;
                }
                if let Some(mut reservation) = state.reservations.get_mut(&reservation_id) {
                    let open = reservation.quantity_reserved - reservation.quantity_consumed;
                    let take = open.min(left);
                    if take > 0.0 {
                        reservation.quantity_consumed += take;
                        reservation.status = if reservation.quantity_consumed
                            >= reservation.quantity_reserved
                        {
                            "consumed".to_string()
                        } else {
                            "partially_consumed".to_string()
                        };
                        reservation.updated_at = now();
                        left -= take;
                    }
                }
            }
            state.emit(
                EVENT_COMPONENTS_CONSUMED,
                json!({
                    "work_order_id": order.id,
                    "number": order.number,
                    "component_product_id": component,
                    "quantity": quantity,
                    "unit": req.unit,
                    "consumed_by": caller.actor,
                }),
            );
            state.emit(
                EVENT_STOCK_DECREMENT_REQUESTED,
                json!({
                    "asset_id": component,
                    "quantity": quantity,
                    "reason": "manufacturing_consumption",
                    "work_order_id": order.id,
                    "product_id": order.product_id,
                }),
            );
            consumed_rows.push(json!({
                "component_product_id": component,
                "quantity": quantity,
                "unit": req.unit,
            }));
        }
        order.updated_at = now();
        save_work_order(&state, &order);
        Ok(json!({ "work_order": order, "consumed": consumed_rows }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn produce_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ProduceRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductionRun> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_PRODUCE)?;
        let input = body.into_inner();
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only in-progress work orders can record production (current status: {})",
                order.status
            )));
        }
        let produced = valid_quantity(input.quantity_produced, "quantity_produced")?;
        let scrapped = valid_non_negative(input.quantity_scrapped, "quantity_scrapped")?;
        let scrap_reason_id = match (scrapped > 0.0, &input.scrap_reason_code_id) {
            (true, Some(raw)) => {
                let reason = load_scrap_reason(&state, raw.trim())?;
                if !reason.active {
                    return Err(UnifiedError::bad_request(
                        "Scrap reason code is inactive",
                    ));
                }
                Some(reason.id)
            }
            (true, None) => {
                return Err(UnifiedError::bad_request(
                    "quantity_scrapped above zero requires scrap_reason_code_id",
                ));
            }
            (false, Some(_)) => {
                return Err(UnifiedError::bad_request(
                    "scrap_reason_code_id requires quantity_scrapped above zero",
                ));
            }
            (false, None) => None,
        };
        if let Some(center_id) = &input.work_center_id {
            load_work_center(&state, center_id.trim())?;
        }
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        if produced + scrapped > order.quantity_planned - order.quantity_produced - order.quantity_scrapped {
            return Err(UnifiedError::conflict(format!(
                "Produced plus scrapped exceeds the remaining planned quantity ({})",
                order.quantity_planned - order.quantity_produced - order.quantity_scrapped
            )));
        }
        let started = input.started_at.unwrap_or_else(now);
        let run = ProductionRun {
            id: new_id(),
            work_order_id: order.id.clone(),
            quantity_produced: produced,
            quantity_scrapped: scrapped,
            scrap_reason_code_id: scrap_reason_id.clone(),
            work_center_id: input.work_center_id.clone().map(|v| v.trim().to_string()),
            operator_id: caller.actor.clone(),
            status: "completed".to_string(),
            notes,
            started_at: started,
            completed_at: now(),
            created_at: now(),
        };
        order.quantity_produced += produced;
        order.quantity_scrapped += scrapped;
        order.updated_at = now();
        save_work_order(&state, &order);
        state
            .production_runs
            .insert(run.id.clone(), run.clone());
        state.emit(
            EVENT_GOODS_PRODUCED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "production_run_id": run.id,
                "product_id": order.product_id,
                "quantity": produced,
                "quantity_scrapped": scrapped,
                "operator_id": caller.actor,
            }),
        );
        if scrapped > 0.0 {
            state.emit(
                EVENT_SCRAP_RECORDED,
                json!({
                    "work_order_id": order.id,
                    "production_run_id": run.id,
                    "product_id": order.product_id,
                    "quantity_scrapped": scrapped,
                    "scrap_reason_code_id": scrap_reason_id,
                    "recorded_by": caller.actor,
                }),
            );
        }
        state.emit(
            EVENT_PRODUCTION_RUN_COMPLETED,
            json!({
                "production_run_id": run.id,
                "work_order_id": order.id,
                "quantity_produced": produced,
                "quantity_scrapped": scrapped,
            }),
        );
        Ok(run)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn complete_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status != "in_progress" {
            return Err(UnifiedError::conflict(format!(
                "Only in-progress work orders can be completed (current status: {})",
                order.status
            )));
        }
        if order.quantity_produced <= 0.0 {
            return Err(UnifiedError::conflict(
                "A work order with no produced quantity cannot be completed; record a production run first",
            ));
        }
        // Quality blocking is advisory but binding here: a work
        // order a blocking quality alert covers must not complete
        // until the alert is resolved. The consultation is the
        // hook form of quality's `GET /blocks`; the refusal names
        // the blocking alert so the caller knows what to resolve.
        let blocks = quality_blocks_for(&state, &order.id, QUALITY_TARGET_KIND_WORK_ORDER);
        if let Some(block) = blocks.first() {
            let alert_id = block
                .get("id")
                .and_then(|value| value.as_str())
                .unwrap_or("unknown");
            let message = block
                .get("message")
                .and_then(|value| value.as_str())
                .unwrap_or("blocking quality alert");
            return Err(UnifiedError::conflict(format!(
                "Work order {} is blocked by quality alert {alert_id}: {message}",
                order.id
            ))
            .with_metadata("quality_block", json!(block)));
        }
        let settings = state.settings();
        let allowed = settings
            .work_order_transitions
            .get(&order.status)
            .cloned()
            .unwrap_or_default();
        if !allowed.contains(&"done".to_string()) {
            return Err(UnifiedError::conflict("Transition to done is not allowed by the configured transition map")
                .with_metadata("allowed", json!(allowed)));
        }
        let cost = compute_cost(&state, &order);
        order.status = "done".to_string();
        order.actual_end = Some(now());
        order.updated_at = now();
        save_work_order(&state, &order);
        state.emit(
            EVENT_WORK_ORDER_STATUS_CHANGED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "from_status": "in_progress",
                "to_status": "done",
                "changed_by": caller.actor,
            }),
        );
        state.emit(
            EVENT_INSPECTION_REQUESTED,
            json!({
                "work_order_id": order.id,
                "target_ref": order.id,
                "target_kind": QUALITY_TARGET_KIND_WORK_ORDER,
                "number": order.number,
                "product_id": order.product_id,
                "quantity": order.quantity_produced,
                "requested_by": caller.actor,
            }),
        );
        state.emit(
            EVENT_FINISHED_GOODS_COSTED,
            json!({
                "work_order_id": order.id,
                "number": order.number,
                "product_id": order.product_id,
                "quantity_produced": order.quantity_produced,
                "cost": cost,
            }),
        );
        Ok(json!({ "work_order": order, "cost": cost }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_work_order_cost(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        Ok(compute_cost(&state, &order))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_work_order_reservations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<MaterialReservation> = state
            .reservations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reservation| reservation.work_order_id == order.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_work_order_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<ProductionRun> = state
            .production_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| run.work_order_id == order.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Production run and reservation handlers
// ---------------------------------------------------------------------------

async fn list_production_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let order_filter = params.filters.get("work_order_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<ProductionRun> = state
            .production_runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                if let Some(order_id) = &order_filter {
                    if run.work_order_id != *order_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if run.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_production_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProductionRun> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        state
            .production_runs
            .get(&path.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Production run not found"))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_reservations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let order_filter = params.filters.get("work_order_id").cloned();
        let component_filter = params.filters.get("component_product_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<MaterialReservation> = state
            .reservations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reservation| {
                if let Some(order_id) = &order_filter {
                    if reservation.work_order_id != *order_id {
                        return false;
                    }
                }
                if let Some(component) = &component_filter {
                    if reservation.component_product_id != *component {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if reservation.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Scrap reason handlers (admin definitions)
// ---------------------------------------------------------------------------

async fn list_scrap_reasons(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        state.seed_definitions();
        let params = query.into_inner();
        let active_filter = params.filters.get("active").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ScrapReason> = state
            .scrap_reasons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|reason| {
                if let Some(active) = &active_filter {
                    let want = active == "true";
                    if reason.active != want {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", reason.code, reason.label).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_scrap_reason(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateScrapReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScrapReason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let code = require_text(&input.code, "code", MAX_CODE_LEN)?.to_lowercase();
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if state
            .scrap_reasons
            .iter()
            .any(|entry| entry.value().code == code)
        {
            return Err(UnifiedError::conflict(format!(
                "Scrap reason code already exists: {code}"
            )));
        }
        let description = optional_text(&input.description, "description", MAX_NOTES_LEN)?;
        let reason = ScrapReason {
            id: new_id(),
            code,
            label,
            active: true,
            description,
            created_at: now(),
            updated_at: now(),
        };
        state
            .scrap_reasons
            .insert(reason.id.clone(), reason.clone());
        Ok(reason)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn update_scrap_reason(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateScrapReasonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScrapReason> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let mut reason = load_scrap_reason(&state, &path.into_inner())?;
        if let Some(label) = &input.label {
            reason.label = require_text(label, "label", MAX_LABEL_LEN)?;
        }
        if let Some(active) = input.active {
            reason.active = active;
        }
        if let Some(description) = &input.description {
            reason.description =
                optional_text(&Some(description.clone()), "description", MAX_NOTES_LEN)?;
        }
        reason.updated_at = now();
        state
            .scrap_reasons
            .insert(reason.id.clone(), reason.clone());
        Ok(reason)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_scrap_reason(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let reason = load_scrap_reason(&state, &path.into_inner())?;
        if state.production_runs.iter().any(|entry| {
            entry.value().scrap_reason_code_id.as_deref() == Some(reason.id.as_str())
        }) {
            return Err(UnifiedError::conflict(
                "Scrap reason cannot be deleted: production runs reference it",
            ));
        }
        state.scrap_reasons.remove(&reason.id);
        Ok(json!({ "deleted": true, "scrap_reason_id": reason.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Unit handlers (admin definitions)
// ---------------------------------------------------------------------------

async fn list_units(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        state.seed_definitions();
        let params = query.into_inner();
        let mut items: Vec<UnitDefinition> = state
            .units
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| a.code.cmp(&b.code));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_unit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<CreateUnitRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<UnitDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let code = require_text(&input.code, "code", MAX_UNIT_LEN)?.to_lowercase();
        let label = require_text(&input.label, "label", MAX_LABEL_LEN)?;
        if state.units.iter().any(|entry| entry.value().code == code) {
            return Err(UnifiedError::conflict(format!(
                "Unit already exists: {code}"
            )));
        }
        let unit = UnitDefinition {
            id: new_id(),
            code: code.clone(),
            label,
            active: true,
            created_at: now(),
        };
        state.units.insert(unit.id.clone(), unit.clone());
        let mut settings = state.settings();
        if !settings.units.contains(&code) {
            settings.units.push(code);
            *state
                .settings
                .write()
                .expect("manufacturing settings lock poisoned") = settings;
        }
        Ok(unit)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_unit(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let id = path.into_inner();
        let unit = state
            .units
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Unit not found: {id}")))?;
        if state.bom_versions.iter().any(|entry| {
            entry
                .value()
                .lines
                .iter()
                .any(|line| line.unit == unit.code)
        }) {
            return Err(UnifiedError::conflict(
                "Unit cannot be deleted: BOM lines reference it",
            ));
        }
        state.units.remove(&unit.id);
        let mut settings = state.settings();
        settings.units.retain(|u| u != &unit.code);
        *state
            .settings
            .write()
            .expect("manufacturing settings lock poisoned") = settings;
        Ok(json!({ "deleted": true, "unit_id": unit.id }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ManufacturingSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ManufacturingSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_ADMIN)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(currency) = &input.currency {
            settings.currency = valid_currency(currency)?;
        }
        if let Some(capacity) = input.default_capacity_hours_per_day {
            settings.default_capacity_hours_per_day =
                valid_quantity(capacity, "default_capacity_hours_per_day")?;
        }
        if let Some(depth) = input.max_bom_depth {
            if depth == 0 || depth > MAX_BOM_DEPTH_HARD_CAP {
                return Err(UnifiedError::bad_request(format!(
                    "max_bom_depth must be between 1 and {MAX_BOM_DEPTH_HARD_CAP}"
                )));
            }
            settings.max_bom_depth = depth;
        }
        if let Some(allowance) = input.default_scrap_allowance_percent {
            settings.default_scrap_allowance_percent =
                valid_percent(allowance, "default_scrap_allowance_percent")?;
        }
        if let Some(enforce) = input.enforce_capacity_on_confirm {
            settings.enforce_capacity_on_confirm = enforce;
        }
        if let Some(units) = &input.units {
            if units.is_empty() {
                return Err(UnifiedError::bad_request(
                    "units must contain at least one unit",
                ));
            }
            let mut clean: Vec<String> = Vec::new();
            for unit in units {
                let unit = require_text(unit, "unit", MAX_UNIT_LEN)?.to_lowercase();
                if !clean.contains(&unit) {
                    clean.push(unit);
                }
            }
            settings.units = clean;
        }
        if let Some(transitions) = &input.work_order_transitions {
            for (from, targets) in transitions {
                let from = from.trim().to_lowercase();
                if !settings.work_order_statuses.contains(&from) {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown work order status in transition map: {from}"
                    )));
                }
                for target in targets {
                    let target = target.trim().to_lowercase();
                    if !settings.work_order_statuses.contains(&target) {
                        return Err(UnifiedError::bad_request(format!(
                            "Unknown work order status in transition map: {target}"
                        )));
                    }
                }
            }
            settings.work_order_transitions = transitions.clone();
        }
        if let Some(statuses) = &input.work_order_statuses {
            if statuses.is_empty() {
                return Err(UnifiedError::bad_request(
                    "work_order_statuses must contain at least one status",
                ));
            }
            let mut clean: Vec<String> = Vec::new();
            for status in statuses {
                let status = require_text(status, "work order status", MAX_CODE_LEN)?
                    .to_lowercase();
                if !clean.contains(&status) {
                    clean.push(status);
                }
            }
            settings.work_order_statuses = clean;
        }
        *state
            .settings
            .write()
            .expect("manufacturing settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Reports
// ---------------------------------------------------------------------------

async fn report_work_orders_by_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        let mut groups: HashMap<String, (u64, f64, f64, f64)> = HashMap::new();
        for entry in state.work_orders.iter() {
            let order = entry.value();
            if !in_period(order.created_at, from, to) {
                continue;
            }
            let slot = groups.entry(order.status.clone()).or_insert((0, 0.0, 0.0, 0.0));
            slot.0 += 1;
            slot.1 += order.quantity_planned;
            slot.2 += order.quantity_produced;
            slot.3 += order.quantity_scrapped;
        }
        let mut rows: Vec<Value> = groups
            .into_iter()
            .map(|(status, (count, planned, produced, scrapped))| {
                json!({
                    "status": status,
                    "work_order_count": count,
                    "quantity_planned": planned,
                    "quantity_produced": produced,
                    "quantity_scrapped": scrapped,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.get("status")
                .and_then(|v| v.as_str())
                .cmp(&b.get("status").and_then(|v| v.as_str()))
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn period_key(date: NaiveDate, group_by: &str) -> String {
    match group_by {
        "week" => {
            let weekday = date.weekday().num_days_from_monday();
            let monday = date - chrono::Duration::days(i64::from(weekday));
            format!("{}-W{}", monday.iso_week().year(), monday.iso_week().week())
        }
        "month" => date.format("%Y-%m").to_string(),
        _ => date.to_string(),
    }
}

async fn report_production_output(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        let group_by = params
            .filters
            .get("group_by")
            .map(|raw| raw.trim().to_lowercase())
            .unwrap_or_else(|| "day".to_string());
        if !["day", "week", "month"].contains(&group_by.as_str()) {
            return Err(UnifiedError::bad_request(
                "group_by must be day, week, or month",
            ));
        }
        // (period, product) -> (produced, scrapped, runs)
        let mut groups: HashMap<(String, String), (f64, f64, u64)> = HashMap::new();
        for entry in state.production_runs.iter() {
            let run = entry.value();
            if !in_period(run.completed_at, from, to) {
                continue;
            }
            let order = match state.work_orders.get(&run.work_order_id) {
                Some(order) => order.value().clone(),
                None => continue,
            };
            let key = (
                period_key(run.completed_at.date_naive(), &group_by),
                order.product_id.clone(),
            );
            let slot = groups.entry(key).or_insert((0.0, 0.0, 0));
            slot.0 += run.quantity_produced;
            slot.1 += run.quantity_scrapped;
            slot.2 += 1;
        }
        let mut rows: Vec<Value> = groups
            .into_iter()
            .map(|((period, product_id), (produced, scrapped, runs))| {
                json!({
                    "period": period,
                    "product_id": product_id,
                    "quantity_produced": produced,
                    "quantity_scrapped": scrapped,
                    "production_runs": runs,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            let by_period = a
                .get("period")
                .and_then(|v| v.as_str())
                .cmp(&b.get("period").and_then(|v| v.as_str()));
            if by_period == std::cmp::Ordering::Equal {
                a.get("product_id")
                    .and_then(|v| v.as_str())
                    .cmp(&b.get("product_id").and_then(|v| v.as_str()))
            } else {
                by_period
            }
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn report_component_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        let mut totals: HashMap<String, (f64, String, u64)> = HashMap::new();
        for entry in state.work_orders.iter() {
            let order = entry.value();
            if !in_period(order.updated_at, from, to) {
                continue;
            }
            for req in &order.requirements {
                if req.quantity_consumed <= 0.0 {
                    continue;
                }
                let slot = totals
                    .entry(req.component_product_id.clone())
                    .or_insert((0.0, req.unit.clone(), 0));
                slot.0 += req.quantity_consumed;
                slot.2 += 1;
            }
        }
        let mut rows: Vec<Value> = totals
            .into_iter()
            .map(|(product_id, (consumed, unit, orders))| {
                json!({
                    "component_product_id": product_id,
                    "unit": unit,
                    "quantity_consumed": consumed,
                    "work_orders": orders,
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.get("component_product_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("component_product_id").and_then(|v| v.as_str()))
        });
        Ok(paginate(rows, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn report_scrap_rate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ManufacturingState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MFG_VIEW)?;
        let params = query.into_inner();
        let from = params
            .filters
            .get("from")
            .and_then(|raw| parse_date_filter(raw));
        let to = params
            .filters
            .get("to")
            .and_then(|raw| parse_date_filter(raw));
        // Per product: (produced, scrapped).
        let mut per_product: HashMap<String, (f64, f64)> = HashMap::new();
        let mut total_produced = 0.0;
        let mut total_scrapped = 0.0;
        for entry in state.production_runs.iter() {
            let run = entry.value();
            if !in_period(run.completed_at, from, to) {
                continue;
            }
            let order = match state.work_orders.get(&run.work_order_id) {
                Some(order) => order.value().clone(),
                None => continue,
            };
            let slot = per_product
                .entry(order.product_id.clone())
                .or_insert((0.0, 0.0));
            slot.0 += run.quantity_produced;
            slot.1 += run.quantity_scrapped;
            total_produced += run.quantity_produced;
            total_scrapped += run.quantity_scrapped;
        }
        let overall_total = total_produced + total_scrapped;
        let overall_rate = if overall_total > 0.0 {
            total_scrapped / overall_total * 100.0
        } else {
            0.0
        };
        let mut rows: Vec<Value> = per_product
            .into_iter()
            .map(|(product_id, (produced, scrapped))| {
                let total = produced + scrapped;
                json!({
                    "product_id": product_id,
                    "quantity_produced": produced,
                    "quantity_scrapped": scrapped,
                    "scrap_rate_percent": if total > 0.0 {
                        (scrapped / total * 100.0 * 100.0).round() / 100.0
                    } else {
                        0.0
                    },
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            a.get("product_id")
                .and_then(|v| v.as_str())
                .cmp(&b.get("product_id").and_then(|v| v.as_str()))
        });
        Ok(json!({
            "from": from.map(|d| d.to_string()),
            "to": to.map(|d| d.to_string()),
            "quantity_produced": total_produced,
            "quantity_scrapped": total_scrapped,
            "scrap_rate_percent": (overall_rate * 100.0).round() / 100.0,
            "by_product": paginate(rows, &params).to_json(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
