// Field Service / Equipment Maintenance module — equipment
// registry, recurring maintenance plans, work orders with a
// validated status machine, technician dispatch, parts
// consumption requests, time logging, customer sign-off, and
// completion reports.
//
// Routes live under `/api/v1/field-service`:
//
//   Equipment types (admin-editable definitions):
//     GET/POST /equipment-types — list / create a type.
//     GET/PATCH/DELETE /equipment-types/{id}.
//   Equipment:
//     GET/POST /equipment — list / register equipment.
//     GET/PATCH/DELETE /equipment/{id}.
//     POST /equipment/{id}/report-failure — open a corrective work
//          order for a failure and request an incident.
//   Maintenance templates (admin-editable):
//     GET/POST /maintenance-templates.
//     GET/PATCH/DELETE /maintenance-templates/{id}.
//     POST /maintenance-templates/{id}/apply — create a plan from
//          a template for one equipment item.
//   Maintenance plans:
//     GET/POST /plans — list / create a plan.
//     GET  /plans/due — plans due within ?days (default 7).
//     GET  /plans/overdue — overdue plans.
//     POST /plans/check-due — run the due/overdue check now
//          (alerts via `notification.requested`).
//     GET/PATCH/DELETE /plans/{id}.
//     POST /plans/{id}/recompute — recompute the next-due date.
//   Work orders:
//     GET/POST /work-orders — list / create a work order.
//     GET/PATCH/DELETE /work-orders/{id}.
//     POST /work-orders/{id}/status — validated status transition.
//     POST /work-orders/{id}/assign — assign a technician.
//     POST /work-orders/{id}/cancel — cancel the work order.
//     GET/POST /work-orders/{id}/items — checklist items.
//     PATCH/DELETE /work-orders/{id}/items/{item_id}.
//     POST /work-orders/{id}/items/{item_id}/complete.
//     GET/POST /work-orders/{id}/parts — required / used parts.
//     PATCH/DELETE /work-orders/{id}/parts/{part_id}.
//     POST /work-orders/{id}/parts/{part_id}/consume — request
//          stock consumption (never decrements stock directly).
//     POST /work-orders/{id}/time-log — log time through the
//          timetracker module (event request).
//     POST /work-orders/{id}/raise-incident — request an incident
//          for a failure found on the job.
//     POST /work-orders/{id}/sign-off — customer sign-off.
//     GET  /work-orders/{id}/sign-off — the sign-off record.
//     GET  /work-orders/{id}/report — completion report data.
//   Dispatch:
//     GET  /dispatch/board — per-technician ordered job lists.
//     GET  /dispatch/assignments — list dispatch assignments.
//     POST /dispatch/assignments/{id}/release — release a technician.
//     POST /dispatch/assignments/{id}/complete — mark done.
//   Definitions and settings:
//     GET  /definitions — every admin-editable value registry.
//     POST /definitions/{kind} — add a value to a registry.
//     DELETE /definitions/{kind}/{value} — remove a custom value.
//     GET/PATCH /settings — module settings.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory equipment, plan, work-order, checklist, part,
// dispatch-assignment, and sign-off management; the validated
// work-order status machine; interval-based next-due computation
// (days / weeks / calendar months, plus usage-based plans driven
// by the equipment meter reading); overdue detection; a dispatch
// board that orders jobs by priority and due date; customer
// sign-off records. What is NOT built here: asset records
// themselves — equipment only stores an `asset_id` reference and
// never duplicates an asset row (warranty data is a local copy
// refreshed from `assets.updated` events when that payload
// carries a warranty date); user records — technicians are user
// id references and this module cannot verify that a given user
// holds a technician capability (the roles module answers
// `role.check` for roles, not for users, so the check happens
// when the technician acts, through `can_act_on_order`); stock
// decrements — consuming a part only emits
// `assets.stock.adjust_requested` and records the part as
// `consumption_requested`, because the assets module has no
// listener for that event yet (the integrator must add one, or
// stock stays unchanged); time entries — logging time only emits
// `timetracker.time.log_requested` because the timetracker module
// exposes no log-request listener yet (the answer is inspected
// and reported honestly as `handler_answered`); incident
// creation — raising an incident only emits
// `incidents.create_requested` because the incidents module
// listens to no such event yet (again reported as
// `handler_answered`); survey delivery — a follow-up survey is
// only sent when the caller supplies an existing surveys
// distribution id, through `surveys.send_requested`, which the
// surveys module does listen to; without one the follow-up is
// recorded as pending and the event `field_service.survey.requested`
// is emitted for a future listener. The dispatch board ordering
// is a priority / due-date heuristic, documented on the endpoint
// itself — it is not a route optimiser and never claims travel
// times or distances. Actual email / push delivery belongs to
// the notifications module (this module only emits
// `notification.requested`).
//
// There are no public (unauthenticated) routes in this module:
// every route requires the `X-User-Id` / `X-Role` headers and a
// capability check. Field service data is internal and is never
// exposed through the API Manager public-access rules.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `field_service.equipment.created`,
//     `field_service.equipment.updated`,
//     `field_service.equipment.deleted`,
//     `field_service.equipment.failure_reported`
//   - `field_service.equipment_type.created`,
//     `field_service.equipment_type.updated`,
//     `field_service.equipment_type.deleted`
//   - `field_service.template.created`,
//     `field_service.template.updated`,
//     `field_service.template.deleted`
//   - `field_service.plan.created`, `field_service.plan.updated`,
//     `field_service.plan.deleted`,
//     `field_service.plan.overdue`,
//     `field_service.plan.next_due_reset`,
//     `field_service.maintenance.completed`
//   - `field_service.work_order.created`,
//     `field_service.work_order.updated`,
//     `field_service.work_order.status_changed`,
//     `field_service.work_order.assigned`,
//     `field_service.work_order.cancelled`,
//     `field_service.work_order.completed`,
//     `field_service.work_order.verified`
//   - `field_service.work_order.item.created`,
//     `field_service.work_order.item.updated`,
//     `field_service.work_order.item.deleted`,
//     `field_service.work_order.item.completed`
//   - `field_service.work_order.part.created`,
//     `field_service.work_order.part.updated`,
//     `field_service.work_order.part.deleted`,
//     `field_service.work_order.part.consumed`
//   - `field_service.work_order.time_logged`
//   - `field_service.dispatch.assigned`,
//     `field_service.dispatch.released`,
//     `field_service.dispatch.completed`
//   - `field_service.signoff.created`
//   - `field_service.survey.requested`
//   - `assets.stock.adjust_requested` (part consumption — no
//     assets listener yet, see the honesty note)
//   - `assets.maintenance.completed` (preventive completion — no
//     assets listener yet, see the honesty note)
//   - `timetracker.time.log_requested` (time logging — no
//     timetracker listener yet, see the honesty note)
//   - `incidents.create_requested` (failure / incident raising —
//     no incidents listener yet, see the honesty note)
//   - `surveys.send_requested` (post-job satisfaction survey,
//     only with a caller-supplied distribution id; the surveys
//     module listens to this event)
//   - `notification.requested` (dispatch assignment, overdue and
//     due-soon plans) with the notifications module's payload
//     shape: `{"kind", "user_id", "title", "body",
//     "module": "field_service", "link", "priority", "channels"}`.
// Events this module listens to:
//   - `assets.updated` — refreshes the local warranty copy of
//     equipment rows that reference the asset, when the payload
//     carries `asset_id` and `warranty_end`.
//   - `field_service.plans.check_requested` — the event the
//     scheduler emits when a `field_service.plans.check` job
//     fires; runs the due/overdue check and emits
//     `notification.requested` for every plan inside the alert
//     window. The scheduler job kind itself is not registered
//     yet, so today the check only runs on demand or through
//     this hook.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_LOCATION_LEN: usize = 200;
const MAX_SERIAL_LEN: usize = 128;
const MAX_NOTES_LEN: usize = 5000;
const MAX_TITLE_LEN: usize = 200;
const MAX_SLUG_LEN: usize = 64;
const MAX_LABEL_LEN: usize = 200;
const MAX_UNIT_LEN: usize = 32;
const MAX_CHECKLIST_ITEMS: usize = 100;
const MAX_PARTS_PER_ORDER: usize = 100;

/// Capability names checked through the `role.check` hook.
const PERM_FIELD_SERVICE_VIEW: &str = "field_service.view";
const PERM_FIELD_SERVICE_MANAGE: &str = "field_service.manage";
const PERM_FIELD_SERVICE_TECHNICIAN: &str = "field_service.technician";

/// Hook event names this module listens to.
const ASSETS_UPDATED: &str = "assets.updated";
const PLANS_CHECK_REQUESTED: &str = "field_service.plans.check_requested";

/// Configurable defaults. Every one of these is a starting value
/// only: the admin can change the settings through
/// `PATCH /settings` and the registries through the definitions
/// endpoints, and seeded registry values are flagged `is_system`
/// so the admin UI can tell defaults apart from custom values.
const DEFAULT_MAX_CONCURRENT_JOBS: i64 = 3;
const DEFAULT_OVERDUE_ALERT_DAYS: i64 = 7;
const DEFAULT_INTERVAL_DAYS: i64 = 90;
const DEFAULT_PRIORITY: &str = "normal";

/// Definition registry kinds accepted by the definitions
/// endpoints. `interval_unit` carries an extra `days_per_unit`
/// (a calendar-month unit uses `is_calendar_month` instead, and
/// a usage-based unit uses `is_usage_based`).
const DEFINITION_KINDS: [&str; 4] = [
    "work_order_type",
    "priority",
    "equipment_status",
    "interval_unit",
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One value of an admin-editable registry (work order types,
/// priorities, equipment statuses). Seeded values carry
/// `is_system: true`; custom values carry `false` and can be
/// deleted by the admin when nothing uses them.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefinitionValue {
    pub value: String,
    pub label: String,
    pub is_system: bool,
}

/// One interval unit definition. A unit is either day-based
/// (`days_per_unit`, for example `weeks` = 7), calendar-month
/// based (`is_calendar_month`, months are added on the calendar
/// so month lengths are honoured), or usage-based
/// (`is_usage_based`, the interval counts meter hours instead of
/// time). Admins can add custom day-based units (for example a
/// two-week unit) through the definitions endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IntervalUnitDef {
    pub unit: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub days_per_unit: Option<i64>,
    #[serde(default)]
    pub is_calendar_month: bool,
    #[serde(default)]
    pub is_usage_based: bool,
    pub is_system: bool,
}

/// One equipment type. Types are data, not code: the admin
/// creates and edits them, and each type carries its own
/// configurable default maintenance interval so a new plan can
/// start from the type default. `is_system` marks the seeded
/// defaults.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EquipmentType {
    pub id: Uuid,
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Default interval (in `default_interval_unit`) applied when
    /// a plan is created from this type without an explicit
    /// interval.
    pub default_interval_value: i64,
    pub default_interval_unit: String,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One equipment item. `asset_id` is a reference to the asset
/// record owned by the assets module — this module never stores
/// asset rows of its own. `warranty_end` is a local copy of the
/// warranty date, refreshed from `assets.updated` events when
/// that payload carries one; the assets module remains the source
/// of truth. `usage_reading` is the current meter reading used by
/// usage-based plans; `usage_unit` names the meter unit (for
/// example "hours" or "cycles") and is free text so meter kinds
/// stay configurable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Equipment {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warranty_end: Option<NaiveDate>,
    /// Equipment status, validated against the `equipment_status`
    /// registry: `active`, `out_of_service`, `retired`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_reading: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_unit: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One recurring maintenance plan for one equipment item.
/// Time-based plans use `interval_value` x `interval_unit` after
/// `last_service_at` (or after creation when never serviced);
/// usage-based plans (`interval_unit` is a usage-based unit) use
/// `usage_at_last_service` + `interval_value` meter units, and
/// `next_due_usage` is the reading at which service falls due.
/// `next_due_at` is `None` for usage-based plans and
/// `next_due_usage` is `None` for time-based plans.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenancePlan {
    pub id: Uuid,
    pub equipment_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub interval_value: i64,
    pub interval_unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_service_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_at_last_service: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_due_usage: Option<f64>,
    pub is_active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One admin-editable schedule template. Applying a template to
/// an equipment item creates a plan (see
/// `POST /maintenance-templates/{id}/apply`) and seeds the new
/// work order's checklist from `checklist_template` when a work
/// order is created from that plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceTemplate {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// When set, the template applies to equipment of this type;
    /// when `None` it applies to any equipment type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equipment_type_id: Option<Uuid>,
    pub interval_value: i64,
    pub interval_unit: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checklist_template: Vec<String>,
    /// Default priority for work orders created from this
    /// template, validated against the `priority` registry.
    pub default_priority: String,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Work order status. The machine is:
///
///   open -> assigned -> in_progress -> on_hold -> completed ->
///   verified -> closed
///
/// with `assigned -> open` (unassign) and `cancelled` reachable
/// from `open`, `assigned`, `in_progress`, and `on_hold`.
/// `closed` and `cancelled` are terminal. Customer sign-off moves
/// `completed -> verified`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkOrderStatus {
    Open,
    Assigned,
    InProgress,
    OnHold,
    Completed,
    Verified,
    Closed,
    Cancelled,
}

impl Default for WorkOrderStatus {
    fn default() -> Self {
        Self::Open
    }
}

impl WorkOrderStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Assigned => "assigned",
            Self::InProgress => "in_progress",
            Self::OnHold => "on_hold",
            Self::Completed => "completed",
            Self::Verified => "verified",
            Self::Closed => "closed",
            Self::Cancelled => "cancelled",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        match value {
            "open" => Some(Self::Open),
            "assigned" => Some(Self::Assigned),
            "in_progress" => Some(Self::InProgress),
            "on_hold" => Some(Self::OnHold),
            "completed" => Some(Self::Completed),
            "verified" => Some(Self::Verified),
            "closed" => Some(Self::Closed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// Allowed next statuses from this status.
    fn allowed_next(&self) -> &'static [WorkOrderStatus] {
        match self {
            Self::Open => &[Self::Assigned, Self::Cancelled],
            Self::Assigned => &[Self::InProgress, Self::Open, Self::Cancelled],
            Self::InProgress => &[Self::OnHold, Self::Completed, Self::Cancelled],
            Self::OnHold => &[Self::InProgress, Self::Cancelled],
            Self::Completed => &[Self::Verified],
            Self::Verified => &[Self::Closed],
            Self::Closed => &[],
            Self::Cancelled => &[],
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Closed | Self::Cancelled)
    }
}

/// One work order. `equipment_id` is the serviced equipment;
/// `plan_id` links the preventive plan this order fulfils, when
/// any; `project_id` is a reference to a project record owned by
/// the projects module (never a duplicated project row).
/// `assignee_id` is the technician user id. `order_type` and
/// `priority` are validated against the admin-editable
/// definitions registries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOrder {
    pub id: Uuid,
    pub equipment_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub order_type: String,
    pub priority: String,
    pub status: WorkOrderStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One checklist item of a work order. Items are ordered by
/// `position`; completing an item stamps who did it and when.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOrderItem {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub title: String,
    pub position: i64,
    pub is_done: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One part line of a work order. `stock_id` references a stock
/// item owned by the assets module. `status` is `required`
/// (planned), `reserved`, `consumption_requested` (a stock
/// adjustment was requested from assets — stock itself is never
/// decremented here), or `returned`. The quantity is a count of
/// whole units.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkOrderPart {
    pub id: Uuid,
    pub work_order_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: i64,
    pub unit: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One dispatch assignment: a technician assigned to a work
/// order, with a sequence number that orders the technician's
/// jobs on the dispatch board. `status` is `active`, `completed`,
/// or `released`. Only one active assignment exists per work
/// order; reassigning releases the previous one.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchAssignment {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub technician_id: String,
    pub sequence: i64,
    pub status: String,
    pub assigned_by: String,
    pub assigned_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One customer sign-off. `signature_ref` is a storage reference
/// (opaque key handed to the storage module) to the captured
/// signature image — no signature bytes are stored here.
/// `follow_up_status` is `none`, `requested` (a survey send was
/// requested through the surveys module), or
/// `pending_no_distribution` (no surveys distribution was
/// supplied, so no survey could be sent yet).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signoff {
    pub id: Uuid,
    pub work_order_id: Uuid,
    pub customer_name: String,
    pub signature_ref: String,
    pub signed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub survey_distribution_id: Option<Uuid>,
    pub follow_up_status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Module settings. All values are configurable defaults: the
/// admin changes them through `PATCH /settings`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldServiceSettings {
    /// How many active jobs one technician may hold before the
    /// dispatch board reports them unavailable.
    pub max_concurrent_jobs_per_technician: i64,
    /// Alert window, in days, for the due-soon check.
    pub overdue_alert_days: i64,
    /// Default plan interval, in days, when a type or template
    /// supplies no interval of its own.
    pub default_interval_days: i64,
    /// Default priority for new work orders.
    pub default_priority: String,
}

impl Default for FieldServiceSettings {
    fn default() -> Self {
        Self {
            max_concurrent_jobs_per_technician: DEFAULT_MAX_CONCURRENT_JOBS,
            overdue_alert_days: DEFAULT_OVERDUE_ALERT_DAYS,
            default_interval_days: DEFAULT_INTERVAL_DAYS,
            default_priority: DEFAULT_PRIORITY.to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateEquipmentTypeRequest {
    pub slug: String,
    pub name: String,
    pub description: Option<String>,
    pub default_interval_value: Option<i64>,
    pub default_interval_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEquipmentTypeRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub default_interval_value: Option<i64>,
    pub default_interval_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEquipmentRequest {
    pub asset_id: Option<Uuid>,
    pub equipment_type_id: Uuid,
    pub name: String,
    pub serial_number: Option<String>,
    pub location: Option<String>,
    pub install_date: Option<NaiveDate>,
    pub warranty_end: Option<NaiveDate>,
    pub status: Option<String>,
    pub usage_reading: Option<f64>,
    pub usage_unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEquipmentRequest {
    pub equipment_type_id: Option<Uuid>,
    pub name: Option<String>,
    pub serial_number: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub install_date: Option<Option<NaiveDate>>,
    pub warranty_end: Option<Option<NaiveDate>>,
    pub status: Option<String>,
    pub usage_reading: Option<Option<f64>>,
    pub usage_unit: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct ReportFailureRequest {
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
    pub priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    pub name: String,
    pub description: Option<String>,
    pub equipment_type_id: Option<Uuid>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub checklist_template: Option<Vec<String>>,
    pub default_priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub equipment_type_id: Option<Option<Uuid>>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub checklist_template: Option<Vec<String>>,
    pub default_priority: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ApplyTemplateRequest {
    pub equipment_id: Uuid,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePlanRequest {
    pub equipment_id: Uuid,
    pub template_id: Option<Uuid>,
    pub name: Option<String>,
    pub description: Option<String>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub last_service_at: Option<DateTime<Utc>>,
    pub usage_at_last_service: Option<f64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePlanRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub interval_value: Option<i64>,
    pub interval_unit: Option<String>,
    pub last_service_at: Option<Option<DateTime<Utc>>>,
    pub usage_at_last_service: Option<Option<f64>>,
    pub is_active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreateWorkOrderRequest {
    pub equipment_id: Uuid,
    pub plan_id: Option<Uuid>,
    pub project_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub order_type: Option<String>,
    pub priority: Option<String>,
    pub assignee_id: Option<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub due_at: Option<DateTime<Utc>>,
    pub checklist_items: Option<Vec<String>>,
    pub required_parts: Option<Vec<CreatePartLineRequest>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartLineRequest {
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkOrderRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub priority: Option<String>,
    pub project_id: Option<Option<String>>,
    pub plan_id: Option<Option<Uuid>>,
    pub scheduled_at: Option<Option<DateTime<Utc>>>,
    pub due_at: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub struct ChangeStatusRequest {
    pub status: String,
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AssignRequest {
    pub technician_id: String,
    pub sequence: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CancelRequest {
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateItemRequest {
    pub title: String,
    pub position: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateItemRequest {
    pub title: Option<String>,
    pub position: Option<i64>,
    pub notes: Option<Option<String>>,
    pub is_done: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePartRequest {
    pub stock_id: Option<Uuid>,
    pub part_name: String,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePartRequest {
    pub part_name: Option<String>,
    pub quantity: Option<i64>,
    pub unit: Option<String>,
    pub status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TimeLogRequest {
    pub technician_id: String,
    pub hours: f64,
    pub description: Option<String>,
    pub logged_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct RaiseIncidentRequest {
    pub title: String,
    pub description: Option<String>,
    pub severity: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct SignOffRequest {
    pub customer_name: String,
    pub signature_ref: String,
    pub notes: Option<String>,
    pub survey_distribution_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct AddDefinitionRequest {
    pub value: String,
    pub label: Option<String>,
    /// Required only for `interval_unit`: days per unit for a
    /// day-based custom unit. Calendar-month and usage-based
    /// units cannot be added by the admin.
    pub days_per_unit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub max_concurrent_jobs_per_technician: Option<i64>,
    pub overdue_alert_days: Option<i64>,
    pub default_interval_days: Option<i64>,
    pub default_priority: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

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

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit an event and report whether any listener answered.
    /// Used for cross-module requests (time logging, incidents,
    /// stock adjustments) so handlers can report honestly when
    /// no listener exists yet.
    fn emit_request(&self, event: &str, data: Value) -> bool {
        match self.bus() {
            Some(bus) => !bus.emit(event, data).results.is_empty(),
            None => false,
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    fn notify(
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
    fn definition_exists(&self, kind: &str, value: &str) -> bool {
        self.definitions
            .get(kind)
            .map(|entry| entry.value().iter().any(|item| item.value == value))
            .unwrap_or(false)
    }

    fn interval_unit(&self, unit: &str) -> Option<IntervalUnitDef> {
        self.interval_units
            .get(unit)
            .map(|entry| entry.value().clone())
    }

    fn settings(&self) -> FieldServiceSettings {
        self.settings.read().expect("settings lock poisoned").clone()
    }

    /// Active (not released / completed) dispatch assignments of
    /// one technician.
    fn active_assignments_of(&self, technician_id: &str) -> Vec<DispatchAssignment> {
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
    fn active_assignment_of_order(&self, work_order_id: Uuid) -> Option<DispatchAssignment> {
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

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the ai module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the ai module's capability check).
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

/// A caller may act on a work order (status changes, time logs,
/// parts, sign-off) when they hold `field_service.manage`, or
/// when they hold `field_service.technician` and are the assigned
/// technician. Anyone else is forbidden.
fn can_act_on_order(ctx: &AppContext, caller: &Caller, order: &WorkOrder) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_FIELD_SERVICE_TECHNICIAN)
        && order.assignee_id.as_deref() == Some(caller.actor.as_str())
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Only a manager or the assigned technician may act on this work order",
    ))
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
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
        Some(raw) => Some(require_text(raw, field, max)).transpose(),
        None => Ok(None),
    }
}

/// A registry slug: lowercase letters, digits, `_`, and `-` only.
fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SLUG_LEN
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
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

/// Add `months` calendar months to a date, clamping the day to the
/// last day of the target month (so 31 January + 1 month becomes
/// 28/29 February, never an invalid date).
fn add_months(date: NaiveDate, months: i64) -> NaiveDate {
    let total_months = date.year() * 12 + (date.month() as i32 - 1) + months as i32;
    let year = total_months.div_euclid(12);
    let month = (total_months.rem_euclid(12) + 1) as u32;
    let last_day = days_in_month(year, month);
    let day = date.day().min(last_day);
    NaiveDate::from_ymd_opt(year, month, day).unwrap_or(date)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Compute the next due date / usage of a plan from its interval
/// and its last service. Time-based plans anchor on
/// `last_service_at` (or `created_at` when never serviced);
/// usage-based plans anchor on `usage_at_last_service` (or 0).
/// Returns `(next_due_at, next_due_usage)`.
fn compute_next_due(
    state: &FieldServiceState,
    plan: &MaintenancePlan,
) -> UnifiedResult<(Option<DateTime<Utc>>, Option<f64>)> {
    let unit = state.interval_unit(&plan.interval_unit).ok_or_else(|| {
        UnifiedError::bad_request(format!("Unknown interval unit: {}", plan.interval_unit))
    })?;
    if unit.is_usage_based {
        let base = plan.usage_at_last_service.unwrap_or(0.0);
        return Ok((None, Some(base + plan.interval_value as f64)));
    }
    let base = plan.last_service_at.unwrap_or(plan.created_at);
    let base_date = base.date_naive();
    let due_date = if unit.is_calendar_month {
        add_months(base_date, plan.interval_value)
    } else {
        let days = unit.days_per_unit.unwrap_or(1) * plan.interval_value;
        base_date + chrono::Duration::days(days)
    };
    let due = due_date
        .and_hms_opt(base.time().hour(), base.time().minute(), base.time().second())
        .map(|naive| DateTime::<Utc>::from_naive_utc_and_offset(naive, Utc))
        .unwrap_or(base);
    Ok((Some(due), None))
}

/// True when a plan is past due right now: for time-based plans
/// the due date has passed; for usage-based plans the equipment
/// meter reading has reached the due reading.
fn plan_is_overdue(state: &FieldServiceState, plan: &MaintenancePlan) -> bool {
    if !plan.is_active {
        return false;
    }
    let now = Utc::now();
    if let Some(due_at) = plan.next_due_at {
        return due_at < now;
    }
    if let Some(due_usage) = plan.next_due_usage {
        if let Some(equipment) = state
            .equipment
            .get(&plan.equipment_id)
            .map(|entry| entry.value().clone())
        {
            if let Some(reading) = equipment.usage_reading {
                return reading >= due_usage;
            }
        }
    }
    false
}

/// Days (negative when overdue) until a time-based plan falls due;
/// `None` for usage-based plans.
fn days_until_due(plan: &MaintenancePlan) -> Option<i64> {
    plan.next_due_at
        .map(|due_at| (due_at.date_naive() - Utc::now().date_naive()).num_days())
}

/// Priority rank for dispatch ordering: lower sorts first.
fn priority_rank(priority: &str) -> i64 {
    match priority {
        "urgent" => 0,
        "high" => 1,
        "normal" => 2,
        "low" => 3,
        _ => 4,
    }
}

fn load_equipment_type(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<EquipmentType> {
    let id = parse_uuid(raw_id, "equipment type")?;
    state
        .equipment_types
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Equipment type not found: {id}")))
}

fn load_equipment(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<Equipment> {
    let id = parse_uuid(raw_id, "equipment")?;
    state
        .equipment
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Equipment not found: {id}")))
}

fn load_template(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<MaintenanceTemplate> {
    let id = parse_uuid(raw_id, "maintenance template")?;
    state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Maintenance template not found: {id}")))
}

fn load_plan(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<MaintenancePlan> {
    let id = parse_uuid(raw_id, "maintenance plan")?;
    state
        .plans
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Maintenance plan not found: {id}")))
}

fn load_work_order(state: &FieldServiceState, raw_id: &str) -> UnifiedResult<WorkOrder> {
    let id = parse_uuid(raw_id, "work order")?;
    state
        .work_orders
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Work order not found: {id}")))
}

fn load_item(
    state: &FieldServiceState,
    work_order_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WorkOrderItem> {
    let id = parse_uuid(raw_id, "checklist item")?;
    state
        .items
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|item| item.work_order_id == work_order_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Checklist item not found: {id}")))
}

fn load_part(
    state: &FieldServiceState,
    work_order_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<WorkOrderPart> {
    let id = parse_uuid(raw_id, "part")?;
    state
        .parts
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|part| part.work_order_id == work_order_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Part not found: {id}")))
}

fn load_dispatch_assignment(
    state: &FieldServiceState,
    raw_id: &str,
) -> UnifiedResult<DispatchAssignment> {
    let id = parse_uuid(raw_id, "dispatch assignment")?;
    state
        .dispatch_assignments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Dispatch assignment not found: {id}")))
}

fn items_of(state: &FieldServiceState, work_order_id: Uuid) -> Vec<WorkOrderItem> {
    let mut rows: Vec<WorkOrderItem> = state
        .items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.work_order_id == work_order_id)
        .collect();
    rows.sort_by_key(|item| (item.position, item.created_at));
    rows
}

fn parts_of(state: &FieldServiceState, work_order_id: Uuid) -> Vec<WorkOrderPart> {
    let mut rows: Vec<WorkOrderPart> = state
        .parts
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|part| part.work_order_id == work_order_id)
        .collect();
    rows.sort_by_key(|part| part.created_at);
    rows
}

/// One due/overdue check result, for the check-due report.
#[derive(Debug, Clone, Serialize)]
struct DueAlert {
    plan_id: Uuid,
    plan_name: String,
    equipment_id: Uuid,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    days_until_due: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_due_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    next_due_usage: Option<f64>,
}

/// Run the due/overdue check: every active plan that is overdue
/// or falls due within the alert window gets a
/// `notification.requested` to the plan creator plus a
/// `field_service.plan.overdue` event. Returns the alerts raised.
fn run_due_check(state: &FieldServiceState) -> Vec<DueAlert> {
    let window = state.settings().overdue_alert_days;
    let mut alerts: Vec<DueAlert> = Vec::new();
    for entry in state.plans.iter() {
        let plan = entry.value().clone();
        if !plan.is_active {
            continue;
        }
        let overdue = plan_is_overdue(state, &plan);
        let due_soon = match days_until_due(&plan) {
            Some(days) => days >= 0 && days <= window,
            None => false,
        };
        if !overdue && !due_soon {
            continue;
        }
        let kind = if overdue { "overdue" } else { "due_soon" };
        let title = if overdue {
            format!("Maintenance overdue: {}", plan.name)
        } else {
            format!("Maintenance due soon: {}", plan.name)
        };
        let body = match (plan.next_due_at, plan.next_due_usage) {
            (Some(due_at), _) => format!(
                "Plan \"{}\" for equipment {} falls due on {}.",
                plan.name, plan.equipment_id, due_at
            ),
            (None, Some(due_usage)) => format!(
                "Plan \"{}\" for equipment {} falls due at meter reading {}.",
                plan.name, plan.equipment_id, due_usage
            ),
            _ => format!("Plan \"{}\" for equipment {} is due.", plan.name, plan.equipment_id),
        };
        let link = format!("/api/v1/field-service/plans/{}", plan.id);
        state.notify(
            &format!("field_service.plan.{kind}"),
            &plan.created_by,
            &title,
            &body,
            Some(link),
            if overdue { "high" } else { "normal" },
        );
        state.emit(
            "field_service.plan.overdue",
            json!({
                "plan_id": plan.id.to_string(),
                "equipment_id": plan.equipment_id.to_string(),
                "kind": kind,
                "days_until_due": days_until_due(&plan),
            }),
        );
        alerts.push(DueAlert {
            plan_id: plan.id,
            plan_name: plan.name.clone(),
            equipment_id: plan.equipment_id,
            kind: kind.to_string(),
            days_until_due: days_until_due(&plan),
            next_due_at: plan.next_due_at,
            next_due_usage: plan.next_due_usage,
        });
    }
    alerts.sort_by_key(|alert| alert.days_until_due.unwrap_or(i64::MAX));
    alerts
}

fn on_assets_updated(state: FieldServiceState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some(asset_id) = data
        .get("asset_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok())
    else {
        return;
    };
    let Some(warranty_end) = data
        .get("warranty_end")
        .and_then(|value| value.as_str())
        .and_then(|raw| NaiveDate::parse_from_str(raw, "%Y-%m-%d").ok())
    else {
        return;
    };
    let mut updated = 0;
    for mut entry in state.equipment.iter_mut() {
        if entry.value().asset_id == Some(asset_id) {
            entry.value_mut().warranty_end = Some(warranty_end);
            entry.value_mut().updated_at = Utc::now();
            updated += 1;
        }
    }
    ctx.add_result(json!({ "synced_equipment": updated, "asset_id": asset_id.to_string() }));
}

fn on_plans_check_requested(state: FieldServiceState, ctx: &mut HookContext) {
    let alerts = run_due_check(&state);
    ctx.add_result(json!({
        "checked": true,
        "alerts": alerts.len(),
        "alerts_detail": alerts,
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Field Service / Equipment Maintenance module.
pub struct FieldServiceModule {
    state: FieldServiceState,
}

impl FieldServiceModule {
    pub fn new() -> Self {
        Self {
            state: FieldServiceState::seeded(),
        }
    }
}

impl Default for FieldServiceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for FieldServiceModule {
    fn id(&self) -> &'static str {
        "field_service"
    }

    fn name(&self) -> &'static str {
        "Field Service & Equipment Maintenance"
    }

    fn description(&self) -> &'static str {
        "Field service and equipment maintenance: equipment registry linked to assets, recurring maintenance plans with next-due computation and overdue detection, work orders with a validated status machine, checklist items and required parts, technician dispatch board, time logging through the timetracker module, parts consumption requests to assets, customer sign-off with completion reports, and post-job satisfaction follow-up through the surveys module."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["assets", "projects", "incidents", "timetracker"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["assets", "projects", "incidents", "timetracker"]
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
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            ASSETS_UPDATED,
            "field_service",
            Arc::new(move |ctx: &mut HookContext| on_assets_updated(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            PLANS_CHECK_REQUESTED,
            "field_service",
            Arc::new(move |ctx: &mut HookContext| on_plans_check_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/field-service")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("/plans/due", web::get().to(list_due_plans))
                .route("/plans/overdue", web::get().to(list_overdue_plans))
                .route("/plans/check-due", web::post().to(check_due_plans))
                .route("/dispatch/board", web::get().to(dispatch_board))
                .route("/equipment-types", web::get().to(list_equipment_types))
                .route("/equipment-types", web::post().to(create_equipment_type))
                .route("/equipment-types/{id}", web::get().to(get_equipment_type))
                .route("/equipment-types/{id}", web::patch().to(update_equipment_type))
                .route("/equipment-types/{id}", web::delete().to(delete_equipment_type))
                .route("/equipment", web::get().to(list_equipment))
                .route("/equipment", web::post().to(create_equipment))
                .route("/equipment/{id}", web::get().to(get_equipment))
                .route("/equipment/{id}", web::patch().to(update_equipment))
                .route("/equipment/{id}", web::delete().to(delete_equipment))
                .route("/equipment/{id}/report-failure", web::post().to(report_failure))
                .route("/maintenance-templates", web::get().to(list_templates))
                .route("/maintenance-templates", web::post().to(create_template))
                .route("/maintenance-templates/{id}", web::get().to(get_template))
                .route("/maintenance-templates/{id}", web::patch().to(update_template))
                .route("/maintenance-templates/{id}", web::delete().to(delete_template))
                .route("/maintenance-templates/{id}/apply", web::post().to(apply_template))
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::patch().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/plans/{id}/recompute", web::post().to(recompute_plan))
                .route("/work-orders", web::get().to(list_work_orders))
                .route("/work-orders", web::post().to(create_work_order))
                .route("/work-orders/{id}", web::get().to(get_work_order))
                .route("/work-orders/{id}", web::patch().to(update_work_order))
                .route("/work-orders/{id}", web::delete().to(delete_work_order))
                .route("/work-orders/{id}/status", web::post().to(change_work_order_status))
                .route("/work-orders/{id}/assign", web::post().to(assign_work_order))
                .route("/work-orders/{id}/cancel", web::post().to(cancel_work_order))
                .route("/work-orders/{id}/items", web::get().to(list_work_order_items))
                .route("/work-orders/{id}/items", web::post().to(add_work_order_item))
                .route("/work-orders/{id}/items/{item_id}", web::patch().to(update_work_order_item))
                .route("/work-orders/{id}/items/{item_id}", web::delete().to(delete_work_order_item))
                .route("/work-orders/{id}/items/{item_id}/complete", web::post().to(complete_work_order_item))
                .route("/work-orders/{id}/parts", web::get().to(list_work_order_parts))
                .route("/work-orders/{id}/parts", web::post().to(add_work_order_part))
                .route("/work-orders/{id}/parts/{part_id}", web::patch().to(update_work_order_part))
                .route("/work-orders/{id}/parts/{part_id}", web::delete().to(delete_work_order_part))
                .route("/work-orders/{id}/parts/{part_id}/consume", web::post().to(consume_work_order_part))
                .route("/work-orders/{id}/time-log", web::post().to(log_work_order_time))
                .route("/work-orders/{id}/raise-incident", web::post().to(raise_incident))
                .route("/work-orders/{id}/sign-off", web::post().to(sign_off_work_order))
                .route("/work-orders/{id}/sign-off", web::get().to(get_work_order_signoff))
                .route("/work-orders/{id}/report", web::get().to(work_order_report))
                .route("/dispatch/assignments", web::get().to(list_dispatch_assignments))
                .route("/dispatch/assignments/{id}/release", web::post().to(release_dispatch_assignment))
                .route("/dispatch/assignments/{id}/complete", web::post().to(complete_dispatch_assignment))
                .route("/definitions", web::get().to(list_definitions))
                .route("/definitions/{kind}", web::post().to(add_definition))
                .route("/definitions/{kind}/{value}", web::delete().to(delete_definition))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment-types", owner: "field_service", description: "List equipment types (paginated, filter by slug, search)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/equipment-types", owner: "field_service", description: "Create an equipment type (admin-editable definition)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment-types/{id}", owner: "field_service", description: "Get one equipment type." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/equipment-types/{id}", owner: "field_service", description: "Update an equipment type." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/equipment-types/{id}", owner: "field_service", description: "Delete an equipment type with no equipment using it." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment", owner: "field_service", description: "List equipment (paginated, filter by type, status, location, asset_id, search)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/equipment", owner: "field_service", description: "Register equipment (asset_id is a reference to the assets module)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/equipment/{id}", owner: "field_service", description: "Get one equipment item." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/equipment/{id}", owner: "field_service", description: "Update equipment (retired equipment cannot be edited)." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/equipment/{id}", owner: "field_service", description: "Delete equipment with no active plans or open work orders." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/equipment/{id}/report-failure", owner: "field_service", description: "Report a failure: opens a corrective work order and requests an incident." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/maintenance-templates", owner: "field_service", description: "List maintenance templates (paginated, filter by equipment_type_id)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/maintenance-templates", owner: "field_service", description: "Create a maintenance template (admin-editable)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/maintenance-templates/{id}", owner: "field_service", description: "Get one maintenance template." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/maintenance-templates/{id}", owner: "field_service", description: "Update a maintenance template." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/maintenance-templates/{id}", owner: "field_service", description: "Delete a maintenance template with no plans using it." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/maintenance-templates/{id}/apply", owner: "field_service", description: "Create a maintenance plan from a template for one equipment item." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans", owner: "field_service", description: "List maintenance plans (paginated, filter by equipment_id, is_active, overdue)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/plans", owner: "field_service", description: "Create a maintenance plan (next-due date is computed from the interval)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans/due", owner: "field_service", description: "Plans due within ?days (default 7), including overdue ones." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans/overdue", owner: "field_service", description: "Overdue maintenance plans." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/plans/check-due", owner: "field_service", description: "Run the due/overdue check now and notify plan owners." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/plans/{id}", owner: "field_service", description: "Get one maintenance plan." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/plans/{id}", owner: "field_service", description: "Update a maintenance plan (next-due date is recomputed)." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/plans/{id}", owner: "field_service", description: "Delete a maintenance plan with no open work orders." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/plans/{id}/recompute", owner: "field_service", description: "Recompute a plan's next-due date from its interval and last service." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders", owner: "field_service", description: "List work orders (paginated, filter by equipment_id, plan_id, project_id, status, type, priority, assignee_id, search)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders", owner: "field_service", description: "Create a work order (checklist and required parts can be seeded inline)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}", owner: "field_service", description: "Get one work order with its checklist items and parts." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/work-orders/{id}", owner: "field_service", description: "Update a work order (closed/cancelled orders cannot be edited)." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/work-orders/{id}", owner: "field_service", description: "Delete an open or cancelled work order." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/status", owner: "field_service", description: "Validated status transition (open -> assigned -> in_progress -> on_hold -> completed -> verified -> closed, with cancel)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/assign", owner: "field_service", description: "Assign a technician (creates a dispatch assignment and notifies them)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/cancel", owner: "field_service", description: "Cancel a work order (releases the active dispatch assignment)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/items", owner: "field_service", description: "Checklist items of a work order (paginated, ordered by position)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/items", owner: "field_service", description: "Add a checklist item." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/work-orders/{id}/items/{item_id}", owner: "field_service", description: "Update a checklist item." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/work-orders/{id}/items/{item_id}", owner: "field_service", description: "Delete a checklist item." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/items/{item_id}/complete", owner: "field_service", description: "Mark a checklist item done." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/parts", owner: "field_service", description: "Parts of a work order (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/parts", owner: "field_service", description: "Add a required part to a work order." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/work-orders/{id}/parts/{part_id}", owner: "field_service", description: "Update a part line." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/work-orders/{id}/parts/{part_id}", owner: "field_service", description: "Delete a part line that has not been consumed." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/parts/{part_id}/consume", owner: "field_service", description: "Request stock consumption for a part (emits assets.stock.adjust_requested; stock is never decremented here)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/time-log", owner: "field_service", description: "Log time on a work order through the timetracker module (event request)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/raise-incident", owner: "field_service", description: "Request an incident for a failure found on the job (event request)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/work-orders/{id}/sign-off", owner: "field_service", description: "Customer sign-off (completed -> verified; optional satisfaction survey follow-up)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/sign-off", owner: "field_service", description: "Get the sign-off record of a work order." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/work-orders/{id}/report", owner: "field_service", description: "Completion report data: work order, checklist, parts, dispatch, and sign-off." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/dispatch/board", owner: "field_service", description: "Dispatch board: per-technician ordered job lists with availability (priority/due-date heuristic, not a route optimiser)." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/dispatch/assignments", owner: "field_service", description: "List dispatch assignments (paginated, filter by technician_id, work_order_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/dispatch/assignments/{id}/release", owner: "field_service", description: "Release a technician from a dispatch assignment." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/dispatch/assignments/{id}/complete", owner: "field_service", description: "Mark a dispatch assignment completed." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/definitions", owner: "field_service", description: "Every admin-editable value registry (work order types, priorities, equipment statuses, interval units)." },
            RouteInfo { method: "POST", path: "/api/v1/field-service/definitions/{kind}", owner: "field_service", description: "Add a value to a registry (interval_unit needs days_per_unit)." },
            RouteInfo { method: "DELETE", path: "/api/v1/field-service/definitions/{kind}/{value}", owner: "field_service", description: "Remove a custom registry value that nothing uses." },
            RouteInfo { method: "GET", path: "/api/v1/field-service/settings", owner: "field_service", description: "Get the module settings." },
            RouteInfo { method: "PATCH", path: "/api/v1/field-service/settings", owner: "field_service", description: "Update the module settings." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(FieldServiceModule::new()));
}

// ---------------------------------------------------------------------------
// Equipment type handlers
// ---------------------------------------------------------------------------

async fn list_equipment_types(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn create_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateEquipmentTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EquipmentType> = (|| {
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

async fn get_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EquipmentType> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        load_equipment_type(&state, &path.into_inner())
    })();
    match result {
        Ok(equipment_type) => ok(equipment_type),
        Err(error) => err(error, debug),
    }
}

async fn update_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEquipmentTypeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<EquipmentType> = (|| {
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

async fn delete_equipment_type(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Equipment handlers
// ---------------------------------------------------------------------------

async fn list_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn create_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateEquipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Equipment> = (|| {
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

async fn get_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Equipment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        load_equipment(&state, &path.into_inner())
    })();
    match result {
        Ok(equipment) => ok(equipment),
        Err(error) => err(error, debug),
    }
}

async fn update_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEquipmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Equipment> = (|| {
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

async fn delete_equipment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
async fn report_failure(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportFailureRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Maintenance template handlers
// ---------------------------------------------------------------------------

async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

fn validate_checklist(items: &Option<Vec<String>>) -> UnifiedResult<Vec<String>> {
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

fn validate_interval(
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

async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceTemplate> = (|| {
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

async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        load_template(&state, &path.into_inner())
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}

async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenanceTemplate> = (|| {
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

async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
async fn apply_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ApplyTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
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

// ---------------------------------------------------------------------------
// Maintenance plan handlers
// ---------------------------------------------------------------------------

async fn list_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let input = body.into_inner();
        let equipment = load_equipment(&state, &input.equipment_id.to_string())?;
        let template = match input.template_id {
            Some(id) => Some(load_template(&state, &id.to_string())?),
            None => None,
        };
        let (interval_value, interval_unit) = match (&input.interval_value, &input.interval_unit) {
            (None, None) => match &template {
                Some(template) => (template.interval_value, template.interval_unit.clone()),
                None => match state.equipment_types.get(&equipment.equipment_type_id) {
                    Some(entry) => (
                        entry.value().default_interval_value,
                        entry.value().default_interval_unit.clone(),
                    ),
                    None => (state.settings().default_interval_days, "days".to_string()),
                },
            },
            _ => validate_interval(&state, input.interval_value, input.interval_unit.clone())?,
        };
        // When only one side of the interval is given, the other
        // falls back to the template / type default.
        let (interval_value, interval_unit) = if input.interval_value.is_some() ^ input.interval_unit.is_some() {
            let fallback = match &template {
                Some(template) => (template.interval_value, template.interval_unit.clone()),
                None => (state.settings().default_interval_days, "days".to_string()),
            };
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

async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
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

async fn delete_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn recompute_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MaintenancePlan> = (|| {
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

async fn list_due_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn list_overdue_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn check_due_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

// ---------------------------------------------------------------------------
// Work order handlers
// ---------------------------------------------------------------------------

/// Shared work-order creation used by the create endpoint and by
/// failure reports: validates references, seeds checklist items
/// from the plan template when present, and stores required parts.
fn insert_work_order(
    state: &FieldServiceState,
    caller: &Caller,
    input: CreateWorkOrderRequest,
) -> UnifiedResult<WorkOrder> {
    let equipment = load_equipment(state, &input.equipment_id.to_string())?;
    let plan = match input.plan_id {
        Some(id) => Some(load_plan(state, &id.to_string())?),
        None => None,
    };
    if let Some(plan) = &plan {
        if plan.equipment_id != equipment.id {
            return Err(UnifiedError::bad_request(
                "Maintenance plan does not belong to this equipment",
            ));
        }
    }
    let order_type = match &input.order_type {
        Some(raw) => {
            let order_type = raw.trim().to_string();
            if !state.definition_exists("work_order_type", &order_type) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown work order type: {order_type}"
                )));
            }
            order_type
        }
        None => "preventive".to_string(),
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
    let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
    let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
    let project_id = match &input.project_id {
        Some(raw) => Some(require_text(raw, "project_id", MAX_NAME_LEN)?),
        None => None,
    };
    let assignee_id = match &input.assignee_id {
        Some(raw) => {
            let assignee = raw.trim().to_string();
            if assignee.is_empty() {
                return Err(UnifiedError::bad_request("assignee_id must not be empty"));
            }
            Some(assignee)
        }
        None => None,
    };
    if let (Some(scheduled), Some(due)) = (input.scheduled_at, input.due_at) {
        if due < scheduled {
            return Err(UnifiedError::bad_request(
                "due_at must not be earlier than scheduled_at",
            ));
        }
    }
    // Validate the seed data before inserting anything, so a bad
    // checklist title or part line cannot leave an orphan order.
    let mut checklist: Vec<String> = Vec::new();
    if let Some(items) = &input.checklist_items {
        if items.len() > MAX_CHECKLIST_ITEMS {
            return Err(UnifiedError::bad_request(format!(
                "checklist_items must contain at most {MAX_CHECKLIST_ITEMS} items"
            )));
        }
        for item in items {
            checklist.push(require_text(item, "checklist item", MAX_TITLE_LEN)?);
        }
    } else if let Some(plan) = &plan {
        if let Some(template_id) = plan.template_id {
            if let Some(template) = state
                .templates
                .get(&template_id)
                .map(|entry| entry.value().clone())
            {
                checklist = template.checklist_template.clone();
            }
        }
    }
    let mut part_lines: Vec<(Option<Uuid>, String, i64, String)> = Vec::new();
    if let Some(parts) = &input.required_parts {
        if parts.len() > MAX_PARTS_PER_ORDER {
            return Err(UnifiedError::bad_request(format!(
                "required_parts must contain at most {MAX_PARTS_PER_ORDER} entries"
            )));
        }
        for part in parts {
            let part_name = require_text(&part.part_name, "part_name", MAX_NAME_LEN)?;
            let quantity = part.quantity.unwrap_or(1);
            if quantity < 1 || quantity > 100_000 {
                return Err(UnifiedError::bad_request(
                    "quantity must be between 1 and 100000",
                ));
            }
            let unit = match &part.unit {
                Some(raw) => require_text(raw, "unit", MAX_UNIT_LEN)?,
                None => "unit".to_string(),
            };
            part_lines.push((part.stock_id, part_name, quantity, unit));
        }
    }
    let now = Utc::now();
    let work_order = WorkOrder {
        id: Uuid::now_v7(),
        equipment_id: equipment.id,
        plan_id: input.plan_id,
        project_id,
        title,
        description,
        order_type,
        priority,
        status: if assignee_id.is_some() {
            WorkOrderStatus::Assigned
        } else {
            WorkOrderStatus::Open
        },
        assignee_id: assignee_id.clone(),
        scheduled_at: input.scheduled_at,
        due_at: input.due_at,
        completed_at: None,
        verified_at: None,
        closed_at: None,
        cancelled_at: None,
        created_by: caller.actor.clone(),
        created_at: now,
        updated_at: now,
    };
    state.work_orders.insert(work_order.id, work_order.clone());
    // Seed the checklist: explicit items win; otherwise the plan
    // template's checklist seeds the order.
    for (index, title) in checklist.into_iter().enumerate() {
        let item = WorkOrderItem {
            id: Uuid::now_v7(),
            work_order_id: work_order.id,
            title,
            position: index as i64,
            is_done: false,
            done_by: None,
            done_at: None,
            notes: None,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item);
    }
    for (stock_id, part_name, quantity, unit) in part_lines {
        let record = WorkOrderPart {
            id: Uuid::now_v7(),
            work_order_id: work_order.id,
            stock_id,
            part_name,
            quantity,
            unit,
            status: "required".to_string(),
            consumed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.parts.insert(record.id, record);
    }
    state.emit(
        "field_service.work_order.created",
        json!({
            "work_order_id": work_order.id.to_string(),
            "equipment_id": equipment.id.to_string(),
            "plan_id": work_order.plan_id.map(|id| id.to_string()),
            "project_id": work_order.project_id,
            "order_type": work_order.order_type,
            "created_by": caller.actor,
        }),
    );
    if let Some(technician) = &assignee_id {
        create_dispatch_assignment(state, &work_order, technician, None, &caller.actor);
    }
    Ok(work_order)
}

/// Create (or replace) the active dispatch assignment for a work
/// order. A previous active assignment is released first, so at
/// most one active assignment exists per work order.
fn create_dispatch_assignment(
    state: &FieldServiceState,
    order: &WorkOrder,
    technician_id: &str,
    sequence: Option<i64>,
    assigned_by: &str,
) -> DispatchAssignment {
    if let Some(previous) = state.active_assignment_of_order(order.id) {
        if let Some(mut entry) = state.dispatch_assignments.get_mut(&previous.id) {
            entry.value_mut().status = "released".to_string();
            entry.value_mut().released_at = Some(Utc::now());
            entry.value_mut().updated_at = Utc::now();
        }
    }
    let sequence = match sequence {
        Some(value) if value >= 0 => value,
        _ => state.active_assignments_of(technician_id).len() as i64 + 1,
    };
    let now = Utc::now();
    let assignment = DispatchAssignment {
        id: Uuid::now_v7(),
        work_order_id: order.id,
        technician_id: technician_id.to_string(),
        sequence,
        status: "active".to_string(),
        assigned_by: assigned_by.to_string(),
        assigned_at: now,
        started_at: None,
        completed_at: None,
        released_at: None,
        notes: None,
        created_at: now,
        updated_at: now,
    };
    state.dispatch_assignments.insert(assignment.id, assignment.clone());
    state.notify(
        "field_service.dispatch.assigned",
        technician_id,
        &format!("New work order assigned: {}", order.title),
        &format!(
            "Work order \"{}\" ({}) was assigned to you. Priority: {}.",
            order.title, order.id, order.priority
        ),
        Some(format!("/api/v1/field-service/work-orders/{}", order.id)),
        if order.priority == "urgent" { "high" } else { "normal" },
    );
    state.emit(
        "field_service.dispatch.assigned",
        json!({
            "dispatch_assignment_id": assignment.id.to_string(),
            "work_order_id": order.id.to_string(),
            "technician_id": technician_id,
            "sequence": sequence,
            "assigned_by": assigned_by,
        }),
    );
    assignment
}

async fn list_work_orders(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let params = query.into_inner();
        let equipment_filter = params.filters.get("equipment_id").cloned();
        let plan_filter = params.filters.get("plan_id").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("order_type").cloned();
        let priority_filter = params.filters.get("priority").cloned();
        let assignee_filter = params.filters.get("assignee_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<WorkOrder> = state
            .work_orders
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|order| {
                if let Some(equipment_id) = &equipment_filter {
                    if order.equipment_id.to_string() != *equipment_id {
                        return false;
                    }
                }
                if let Some(plan_id) = &plan_filter {
                    if order.plan_id.map(|id| id.to_string()) != Some(plan_id.clone()) {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if order.project_id.as_deref() != Some(project_id.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if order.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(order_type) = &type_filter {
                    if &order.order_type != order_type {
                        return false;
                    }
                }
                if let Some(priority) = &priority_filter {
                    if &order.priority != priority {
                        return false;
                    }
                }
                if let Some(assignee) = &assignee_filter {
                    if order.assignee_id.as_deref() != Some(assignee.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        order.title,
                        order.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["created_at", "due_at", "priority", "status"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "due_at" => a.due_at.cmp(&b.due_at),
                "priority" => priority_rank(&a.priority).cmp(&priority_rank(&b.priority)),
                "status" => a.status.as_str().cmp(b.status.as_str()),
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
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<CreateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        insert_work_order(&state, &caller, body.into_inner())
    })();
    match result {
        Ok(order) => created(order),
        Err(error) => err(error, debug),
    }
}

async fn get_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        Ok(json!({
            "work_order": order,
            "items": items_of(&state, order.id),
            "parts": parts_of(&state, order.id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkOrderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be updated",
            ));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            order.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            order.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(priority) = input.priority {
            let priority = priority.trim().to_string();
            if !state.definition_exists("priority", &priority) {
                return Err(UnifiedError::bad_request(format!("Unknown priority: {priority}")));
            }
            order.priority = priority;
        }
        if let Some(project_id) = input.project_id {
            order.project_id = match project_id {
                Some(raw) => Some(require_text(&raw, "project_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(plan_id) = input.plan_id {
            match plan_id {
                Some(id) => {
                    let plan = load_plan(&state, &id.to_string())?;
                    if plan.equipment_id != order.equipment_id {
                        return Err(UnifiedError::bad_request(
                            "Maintenance plan does not belong to this equipment",
                        ));
                    }
                    order.plan_id = Some(id);
                }
                None => order.plan_id = None,
            }
        }
        if let Some(scheduled_at) = input.scheduled_at {
            order.scheduled_at = scheduled_at;
        }
        if let Some(due_at) = input.due_at {
            order.due_at = due_at;
        }
        if let (Some(scheduled), Some(due)) = (order.scheduled_at, order.due_at) {
            if due < scheduled {
                return Err(UnifiedError::bad_request(
                    "due_at must not be earlier than scheduled_at",
                ));
            }
        }
        order.updated_at = Utc::now();
        state.work_orders.insert(order.id, order.clone());
        state.emit(
            "field_service.work_order.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

async fn delete_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let order = load_work_order(&state, &path.into_inner())?;
        if !matches!(order.status, WorkOrderStatus::Open | WorkOrderStatus::Cancelled) {
            return Err(UnifiedError::conflict(
                "Only an open or cancelled work order can be deleted",
            ));
        }
        for item in items_of(&state, order.id) {
            state.items.remove(&item.id);
        }
        for part in parts_of(&state, order.id) {
            state.parts.remove(&part.id);
        }
        if let Some(assignment) = state.active_assignment_of_order(order.id) {
            if let Some(mut entry) = state.dispatch_assignments.get_mut(&assignment.id) {
                entry.value_mut().status = "released".to_string();
                entry.value_mut().released_at = Some(Utc::now());
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.work_orders.remove(&order.id);
        state.emit(
            "field_service.work_order.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "deleted": true,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "work_order_id": order.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Apply a validated status transition, stamping the lifecycle
/// timestamps. Shared by the status endpoint, the cancel endpoint,
/// and sign-off. Completing a preventive order linked to a plan
/// resets that plan's next-due date and emits the maintenance
/// completion events.
fn apply_work_order_transition(
    state: &FieldServiceState,
    order: &mut WorkOrder,
    next: WorkOrderStatus,
    actor: &str,
) -> UnifiedResult<()> {
    if !order.status.allowed_next().contains(&next) {
        return Err(UnifiedError::conflict(format!(
            "Invalid work order status transition from \"{}\" to \"{}\"",
            order.status.as_str(),
            next.as_str()
        )));
    }
    let now = Utc::now();
    order.status = next;
    order.updated_at = now;
    match next {
        WorkOrderStatus::Completed => {
            order.completed_at = Some(now);
        }
        WorkOrderStatus::Verified => {
            order.verified_at = Some(now);
        }
        WorkOrderStatus::Closed => {
            order.closed_at = Some(now);
        }
        WorkOrderStatus::Cancelled => {
            order.cancelled_at = Some(now);
        }
        WorkOrderStatus::Open => {
            order.assignee_id = None;
        }
        _ => {}
    }
    state.work_orders.insert(order.id, order.clone());
    state.emit(
        "field_service.work_order.status_changed",
        json!({
            "work_order_id": order.id.to_string(),
            "status": next.as_str(),
            "changed_by": actor,
        }),
    );
    if next == WorkOrderStatus::Completed {
        state.emit(
            "field_service.work_order.completed",
            json!({
                "work_order_id": order.id.to_string(),
                "equipment_id": order.equipment_id.to_string(),
                "plan_id": order.plan_id.map(|id| id.to_string()),
                "completed_by": actor,
            }),
        );
        // A completed preventive order resets its plan's next-due
        // date: the service is done, so the interval restarts now.
        if order.order_type == "preventive" {
            if let Some(plan_id) = order.plan_id {
                if let Some(mut plan) = state.plans.get(&plan_id).map(|entry| entry.value().clone()) {
                    let equipment_reading = state
                        .equipment
                        .get(&order.equipment_id)
                        .and_then(|entry| entry.value().usage_reading);
                    plan.last_service_at = Some(now);
                    plan.usage_at_last_service = equipment_reading;
                    if let Ok((next_due_at, next_due_usage)) = compute_next_due(state, &plan) {
                        plan.next_due_at = next_due_at;
                        plan.next_due_usage = next_due_usage;
                    }
                    plan.updated_at = now;
                    state.plans.insert(plan.id, plan.clone());
                    state.emit(
                        "field_service.plan.next_due_reset",
                        json!({
                            "plan_id": plan.id.to_string(),
                            "equipment_id": order.equipment_id.to_string(),
                            "work_order_id": order.id.to_string(),
                            "next_due_at": plan.next_due_at.map(|due| due.to_rfc3339()),
                            "next_due_usage": plan.next_due_usage,
                        }),
                    );
                    state.emit(
                        "field_service.maintenance.completed",
                        json!({
                            "plan_id": plan.id.to_string(),
                            "equipment_id": order.equipment_id.to_string(),
                            "work_order_id": order.id.to_string(),
                            "completed_at": now.to_rfc3339(),
                        }),
                    );
                    if let Some(equipment) = state
                        .equipment
                        .get(&order.equipment_id)
                        .map(|entry| entry.value().clone())
                    {
                        if let Some(asset_id) = equipment.asset_id {
                            state.emit(
                                "assets.maintenance.completed",
                                json!({
                                    "asset_id": asset_id.to_string(),
                                    "equipment_id": equipment.id.to_string(),
                                    "plan_id": plan.id.to_string(),
                                    "work_order_id": order.id.to_string(),
                                    "completed_at": now.to_rfc3339(),
                                }),
                            );
                        }
                    }
                }
            }
        }
    }
    if next == WorkOrderStatus::Verified {
        state.emit(
            "field_service.work_order.verified",
            json!({
                "work_order_id": order.id.to_string(),
                "verified_by": actor,
            }),
        );
    }
    Ok(())
}

async fn change_work_order_status(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ChangeStatusRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        let input = body.into_inner();
        let next = WorkOrderStatus::from_str(input.status.trim()).ok_or_else(|| {
            UnifiedError::bad_request(format!("Unknown work order status: {}", input.status))
        })?;
        if let Some(note) = &input.note {
            let _ = require_text(note, "note", MAX_NOTES_LEN)?;
        }
        if next == WorkOrderStatus::Cancelled {
            return Err(UnifiedError::bad_request(
                "Use POST /cancel to cancel a work order",
            ));
        }
        if next == WorkOrderStatus::Verified {
            return Err(UnifiedError::bad_request(
                "Verification happens through customer sign-off (POST /sign-off)",
            ));
        }
        if next == WorkOrderStatus::Assigned {
            return Err(UnifiedError::bad_request(
                "Use POST /assign to assign a technician",
            ));
        }
        apply_work_order_transition(&state, &mut order, next, &caller.actor)?;
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

async fn assign_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AssignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        if !matches!(
            order.status,
            WorkOrderStatus::Open | WorkOrderStatus::Assigned | WorkOrderStatus::OnHold
        ) {
            return Err(UnifiedError::conflict(format!(
                "A work order in status \"{}\" cannot be assigned",
                order.status.as_str()
            )));
        }
        let input = body.into_inner();
        let technician_id = input.technician_id.trim().to_string();
        if technician_id.is_empty() {
            return Err(UnifiedError::bad_request("technician_id must not be empty"));
        }
        if let Some(sequence) = input.sequence {
            if sequence < 0 || sequence > 10_000 {
                return Err(UnifiedError::bad_request(
                    "sequence must be between 0 and 10000",
                ));
            }
        }
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        order.assignee_id = Some(technician_id.clone());
        if order.status == WorkOrderStatus::Open {
            order.status = WorkOrderStatus::Assigned;
        }
        order.updated_at = Utc::now();
        state.work_orders.insert(order.id, order.clone());
        let assignment =
            create_dispatch_assignment(&state, &order, &technician_id, input.sequence, &caller.actor);
        if let Some(notes) = notes {
            if let Some(mut entry) = state.dispatch_assignments.get_mut(&assignment.id) {
                entry.value_mut().notes = Some(notes);
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "field_service.work_order.assigned",
            json!({
                "work_order_id": order.id.to_string(),
                "technician_id": technician_id,
                "assigned_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

async fn cancel_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CancelRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrder> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(reason) = &input.reason {
            let _ = require_text(reason, "reason", MAX_NOTES_LEN)?;
        }
        apply_work_order_transition(&state, &mut order, WorkOrderStatus::Cancelled, &caller.actor)?;
        if let Some(assignment) = state.active_assignment_of_order(order.id) {
            if let Some(mut entry) = state.dispatch_assignments.get_mut(&assignment.id) {
                entry.value_mut().status = "released".to_string();
                entry.value_mut().released_at = Some(Utc::now());
                entry.value_mut().updated_at = Utc::now();
            }
        }
        state.emit(
            "field_service.work_order.cancelled",
            json!({
                "work_order_id": order.id.to_string(),
                "reason": input.reason,
                "cancelled_by": caller.actor,
            }),
        );
        Ok(order)
    })();
    match result {
        Ok(order) => ok(order),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Work order checklist item handlers
// ---------------------------------------------------------------------------

async fn list_work_order_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        Ok(paginate(items_of(&state, order.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderItem> = (|| {
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot gain checklist items",
            ));
        }
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let existing = items_of(&state, order.id);
        if existing.len() >= MAX_CHECKLIST_ITEMS {
            return Err(UnifiedError::bad_request(format!(
                "A work order holds at most {MAX_CHECKLIST_ITEMS} checklist items"
            )));
        }
        let position = input
            .position
            .unwrap_or_else(|| existing.iter().map(|item| item.position).max().unwrap_or(-1) + 1);
        if position < 0 || position > 10_000 {
            return Err(UnifiedError::bad_request(
                "position must be between 0 and 10000",
            ));
        }
        let now = Utc::now();
        let item = WorkOrderItem {
            id: Uuid::now_v7(),
            work_order_id: order.id,
            title,
            position,
            is_done: false,
            done_by: None,
            done_at: None,
            notes,
            created_at: now,
            updated_at: now,
        };
        state.items.insert(item.id, item.clone());
        state.emit(
            "field_service.work_order.item.created",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

async fn update_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateItemRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderItem> = (|| {
        let caller = caller(&req)?;
        let (order_id, item_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let mut item = load_item(&state, order.id, &item_id)?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            item.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(position) = input.position {
            if position < 0 || position > 10_000 {
                return Err(UnifiedError::bad_request(
                    "position must be between 0 and 10000",
                ));
            }
            item.position = position;
        }
        if let Some(notes) = input.notes {
            item.notes = match notes {
                Some(raw) => Some(require_text(&raw, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        if let Some(is_done) = input.is_done {
            if is_done && !item.is_done {
                item.is_done = true;
                item.done_by = Some(caller.actor.clone());
                item.done_at = Some(Utc::now());
            } else if !is_done {
                item.is_done = false;
                item.done_by = None;
                item.done_at = None;
            }
        }
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        state.emit(
            "field_service.work_order.item.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

async fn delete_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let (order_id, item_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let item = load_item(&state, order.id, &item_id)?;
        state.items.remove(&item.id);
        state.emit(
            "field_service.work_order.item.deleted",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "item_id": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn complete_work_order_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderItem> = (|| {
        let caller = caller(&req)?;
        let (order_id, item_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let mut item = load_item(&state, order.id, &item_id)?;
        if item.is_done {
            return Err(UnifiedError::conflict("Checklist item is already done"));
        }
        item.is_done = true;
        item.done_by = Some(caller.actor.clone());
        item.done_at = Some(Utc::now());
        item.updated_at = Utc::now();
        state.items.insert(item.id, item.clone());
        state.emit(
            "field_service.work_order.item.completed",
            json!({
                "work_order_id": order.id.to_string(),
                "item_id": item.id.to_string(),
                "done_by": caller.actor,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Work order part handlers
// ---------------------------------------------------------------------------

async fn list_work_order_parts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let params = query.into_inner();
        Ok(paginate(parts_of(&state, order.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn validate_part_line(input: &CreatePartRequest) -> UnifiedResult<(String, i64, String)> {
    let part_name = require_text(&input.part_name, "part_name", MAX_NAME_LEN)?;
    let quantity = input.quantity.unwrap_or(1);
    if quantity < 1 || quantity > 100_000 {
        return Err(UnifiedError::bad_request(
            "quantity must be between 1 and 100000",
        ));
    }
    let unit = match &input.unit {
        Some(raw) => require_text(raw, "unit", MAX_UNIT_LEN)?,
        None => "unit".to_string(),
    };
    Ok((part_name, quantity, unit))
}

async fn add_work_order_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreatePartRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderPart> = (|| {
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot gain parts",
            ));
        }
        if parts_of(&state, order.id).len() >= MAX_PARTS_PER_ORDER {
            return Err(UnifiedError::bad_request(format!(
                "A work order holds at most {MAX_PARTS_PER_ORDER} parts"
            )));
        }
        let input = body.into_inner();
        let (part_name, quantity, unit) = validate_part_line(&input)?;
        let now = Utc::now();
        let part = WorkOrderPart {
            id: Uuid::now_v7(),
            work_order_id: order.id,
            stock_id: input.stock_id,
            part_name,
            quantity,
            unit,
            status: "required".to_string(),
            consumed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.parts.insert(part.id, part.clone());
        state.emit(
            "field_service.work_order.part.created",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(part)
    })();
    match result {
        Ok(part) => created(part),
        Err(error) => err(error, debug),
    }
}

async fn update_work_order_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdatePartRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WorkOrderPart> = (|| {
        let caller = caller(&req)?;
        let (order_id, part_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let mut part = load_part(&state, order.id, &part_id)?;
        if part.status == "consumption_requested" {
            return Err(UnifiedError::conflict(
                "A part whose consumption was requested cannot be edited",
            ));
        }
        let input = body.into_inner();
        if let Some(part_name) = input.part_name {
            part.part_name = require_text(&part_name, "part_name", MAX_NAME_LEN)?;
        }
        if let Some(quantity) = input.quantity {
            if quantity < 1 || quantity > 100_000 {
                return Err(UnifiedError::bad_request(
                    "quantity must be between 1 and 100000",
                ));
            }
            part.quantity = quantity;
        }
        if let Some(unit) = input.unit {
            part.unit = require_text(&unit, "unit", MAX_UNIT_LEN)?;
        }
        if let Some(status) = input.status {
            match status.trim() {
                "required" | "reserved" | "returned" => part.status = status.trim().to_string(),
                "consumption_requested" => {
                    return Err(UnifiedError::bad_request(
                        "Use POST /consume to request consumption of a part",
                    ));
                }
                other => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unknown part status: {other}"
                    )));
                }
            }
        }
        part.updated_at = Utc::now();
        state.parts.insert(part.id, part.clone());
        state.emit(
            "field_service.work_order.part.updated",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(part)
    })();
    match result {
        Ok(part) => ok(part),
        Err(error) => err(error, debug),
    }
}

async fn delete_work_order_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let (order_id, part_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot be edited",
            ));
        }
        let part = load_part(&state, order.id, &part_id)?;
        if part.status == "consumption_requested" {
            return Err(UnifiedError::conflict(
                "A part whose consumption was requested cannot be deleted",
            ));
        }
        state.parts.remove(&part.id);
        state.emit(
            "field_service.work_order.part.deleted",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "part_id": part.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Request stock consumption for a part. This module never touches
/// stock levels itself: it emits `assets.stock.adjust_requested`
/// with a negative delta and records the part as
/// `consumption_requested`. The assets module has no listener for
/// that event yet, so the answer is reported honestly as
/// `stock_handler_answered`.
async fn consume_work_order_part(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let (order_id, part_id) = path.into_inner();
        let order = load_work_order(&state, &order_id)?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "A closed or cancelled work order cannot consume parts",
            ));
        }
        let mut part = load_part(&state, order.id, &part_id)?;
        if part.status == "consumption_requested" {
            return Err(UnifiedError::conflict(
                "Consumption was already requested for this part",
            ));
        }
        let stock_answered = match part.stock_id {
            Some(stock_id) => state.emit_request(
                "assets.stock.adjust_requested",
                json!({
                    "stock_id": stock_id.to_string(),
                    "asset_id": null,
                    "delta": -part.quantity,
                    "reason": format!(
                        "Consumed for field service work order {} (part \"{}\")",
                        order.id, part.part_name
                    ),
                    "requested_by": caller.actor,
                    "work_order_id": order.id.to_string(),
                    "part_id": part.id.to_string(),
                }),
            ),
            None => false,
        };
        part.status = "consumption_requested".to_string();
        part.consumed_at = Some(Utc::now());
        part.updated_at = Utc::now();
        state.parts.insert(part.id, part.clone());
        state.emit(
            "field_service.work_order.part.consumed",
            json!({
                "work_order_id": order.id.to_string(),
                "part_id": part.id.to_string(),
                "stock_id": part.stock_id.map(|id| id.to_string()),
                "quantity": part.quantity,
                "stock_handler_answered": stock_answered,
                "consumed_by": caller.actor,
            }),
        );
        Ok(json!({
            "part": part,
            "stock_adjustment_requested": part.stock_id.is_some(),
            "stock_handler_answered": stock_answered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Time logging, incident raising, sign-off, report
// ---------------------------------------------------------------------------

/// Log time on a work order through the timetracker module. The
/// timetracker module exposes no log-request listener yet, so the
/// request is an event (`timetracker.time.log_requested`) and the
/// answer is reported honestly as `handler_answered`.
async fn log_work_order_time(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TimeLogRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "Time cannot be logged on a closed or cancelled work order",
            ));
        }
        let input = body.into_inner();
        let technician_id = input.technician_id.trim().to_string();
        if technician_id.is_empty() {
            return Err(UnifiedError::bad_request("technician_id must not be empty"));
        }
        if !input.hours.is_finite() || input.hours <= 0.0 || input.hours > 24.0 {
            return Err(UnifiedError::bad_request(
                "hours must be a finite number between 0 and 24",
            ));
        }
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let answered = state.emit_request(
            "timetracker.time.log_requested",
            json!({
                "work_order_id": order.id.to_string(),
                "equipment_id": order.equipment_id.to_string(),
                "project_id": order.project_id,
                "user_id": technician_id,
                "technician_id": technician_id,
                "hours": input.hours,
                "description": description,
                "logged_at": input.logged_at.map(|at| at.to_rfc3339()),
                "source": "field_service",
                "logged_by": caller.actor,
            }),
        );
        state.emit(
            "field_service.work_order.time_logged",
            json!({
                "work_order_id": order.id.to_string(),
                "technician_id": technician_id,
                "hours": input.hours,
                "handler_answered": answered,
                "logged_by": caller.actor,
            }),
        );
        Ok(json!({
            "work_order_id": order.id.to_string(),
            "technician_id": technician_id,
            "hours": input.hours,
            "status": "requested",
            "handler_answered": answered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Request an incident for a failure found on the job. The
/// incidents module listens to no create-request event yet, so the
/// request is an event (`incidents.create_requested`) and the
/// answer is reported honestly as `handler_answered`.
async fn raise_incident(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RaiseIncidentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status.is_terminal() {
            return Err(UnifiedError::conflict(
                "An incident cannot be raised from a closed or cancelled work order",
            ));
        }
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
        let answered = state.emit_request(
            "incidents.create_requested",
            json!({
                "title": title,
                "description": description.clone().unwrap_or_else(|| format!(
                    "Failure found during field service work order {}",
                    order.id
                )),
                "severity": severity,
                "source": "field_service",
                "work_order_id": order.id.to_string(),
                "equipment_id": order.equipment_id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(json!({
            "work_order_id": order.id.to_string(),
            "title": title,
            "severity": severity,
            "requested": true,
            "handler_answered": answered,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Customer sign-off: records the customer name, the storage
/// reference of the captured signature, and the timestamp, then
/// moves the work order from `completed` to `verified`. When a
/// surveys distribution id is supplied, a satisfaction survey
/// send is requested through the surveys module; without one the
/// follow-up is recorded as pending.
async fn sign_off_work_order(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SignOffRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Signoff> = (|| {
        let caller = caller(&req)?;
        let mut order = load_work_order(&state, &path.into_inner())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if order.status != WorkOrderStatus::Completed {
            return Err(UnifiedError::conflict(format!(
                "Only a completed work order can be signed off (current status: \"{}\")",
                order.status.as_str()
            )));
        }
        if state
            .signoffs
            .iter()
            .any(|entry| entry.value().work_order_id == order.id)
        {
            return Err(UnifiedError::conflict(
                "This work order already has a sign-off",
            ));
        }
        let input = body.into_inner();
        let customer_name = require_text(&input.customer_name, "customer_name", MAX_NAME_LEN)?;
        let signature_ref = require_text(&input.signature_ref, "signature_ref", MAX_NAME_LEN)?;
        let notes = optional_text(&input.notes, "notes", MAX_NOTES_LEN)?;
        let now = Utc::now();
        let follow_up_status = match input.survey_distribution_id {
            Some(distribution_id) => {
                state.emit_request(
                    "surveys.send_requested",
                    json!({
                        "distribution_id": distribution_id.to_string(),
                        "work_order_id": order.id.to_string(),
                        "source": "field_service",
                    }),
                );
                "requested".to_string()
            }
            None => {
                state.emit(
                    "field_service.survey.requested",
                    json!({
                        "work_order_id": order.id.to_string(),
                        "customer_name": customer_name,
                        "reason": "no survey distribution supplied",
                    }),
                );
                "pending_no_distribution".to_string()
            }
        };
        let signoff = Signoff {
            id: Uuid::now_v7(),
            work_order_id: order.id,
            customer_name,
            signature_ref,
            signed_at: now,
            notes,
            survey_distribution_id: input.survey_distribution_id,
            follow_up_status,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.signoffs.insert(signoff.id, signoff.clone());
        apply_work_order_transition(&state, &mut order, WorkOrderStatus::Verified, &caller.actor)?;
        state.emit(
            "field_service.signoff.created",
            json!({
                "signoff_id": signoff.id.to_string(),
                "work_order_id": order.id.to_string(),
                "customer_name": signoff.customer_name,
                "follow_up_status": signoff.follow_up_status,
                "created_by": caller.actor,
            }),
        );
        Ok(signoff)
    })();
    match result {
        Ok(signoff) => created(signoff),
        Err(error) => err(error, debug),
    }
}

async fn get_work_order_signoff(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Signoff> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        state
            .signoffs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|signoff| signoff.work_order_id == order.id)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "No sign-off exists for work order: {}",
                    order.id
                ))
            })
    })();
    match result {
        Ok(signoff) => ok(signoff),
        Err(error) => err(error, debug),
    }
}

/// Completion report data for a work order: the order itself, its
/// equipment, checklist completion, parts, dispatch history, and
/// sign-off. Time totals are not summed here because time entries
/// live in the timetracker module.
async fn work_order_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let order = load_work_order(&state, &path.into_inner())?;
        let equipment = state
            .equipment
            .get(&order.equipment_id)
            .map(|entry| entry.value().clone());
        let items = items_of(&state, order.id);
        let done_items = items.iter().filter(|item| item.is_done).count();
        let parts = parts_of(&state, order.id);
        let mut assignments: Vec<DispatchAssignment> = state
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.work_order_id == order.id)
            .collect();
        assignments.sort_by_key(|row| row.assigned_at);
        let signoff = state
            .signoffs
            .iter()
            .map(|entry| entry.value().clone())
            .find(|signoff| signoff.work_order_id == order.id);
        Ok(json!({
            "work_order": order,
            "equipment": equipment,
            "checklist": {
                "total": items.len(),
                "done": done_items,
                "items": items,
            },
            "parts": parts,
            "dispatch_assignments": assignments,
            "signoff": signoff,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Dispatch handlers
// ---------------------------------------------------------------------------

/// One technician's board entry: their active jobs in dispatch
/// order, their availability, and the ordering rule. The order is
/// a heuristic — priority first, then due date, then creation
/// time — and is documented as such in the response: it is not a
/// route optimiser and carries no travel times or distances.
#[derive(Debug, Clone, Serialize)]
struct TechnicianBoard {
    technician_id: String,
    active_jobs: usize,
    max_concurrent_jobs: i64,
    available: bool,
    jobs: Vec<Value>,
}

async fn dispatch_board(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let params = query.into_inner();
        let technician_filter = params.filters.get("technician_id").cloned();
        let max_jobs = state.settings().max_concurrent_jobs_per_technician;
        // Technicians are the users holding active dispatch
        // assignments (plus an explicitly requested technician,
        // who may currently hold no jobs).
        let mut technician_ids: Vec<String> = state
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.status == "active")
            .map(|row| row.technician_id)
            .collect();
        technician_ids.sort();
        technician_ids.dedup();
        if let Some(requested) = &technician_filter {
            if !technician_ids.contains(requested) {
                technician_ids.push(requested.clone());
            }
        }
        let mut board: Vec<TechnicianBoard> = Vec::new();
        for technician_id in technician_ids {
            if let Some(filter) = &technician_filter {
                if &technician_id != filter {
                    continue;
                }
            }
            let assignments = state.active_assignments_of(&technician_id);
            let mut jobs: Vec<(i64, WorkOrder)> = Vec::new();
            for assignment in &assignments {
                if let Some(order) = state
                    .work_orders
                    .get(&assignment.work_order_id)
                    .map(|entry| entry.value().clone())
                {
                    jobs.push((assignment.sequence, order));
                }
            }
            // Dispatch order heuristic: assignment sequence first
            // (the dispatcher sets it), then priority, then due
            // date, then creation time. This is an ordering hint,
            // not a route optimiser.
            jobs.sort_by(|a, b| {
                a.0.cmp(&b.0)
                    .then(priority_rank(&a.1.priority).cmp(&priority_rank(&b.1.priority)))
                    .then(a.1.due_at.cmp(&b.1.due_at))
                    .then(a.1.created_at.cmp(&b.1.created_at))
            });
            let job_values: Vec<Value> = jobs
                .into_iter()
                .enumerate()
                .map(|(index, (sequence, order))| {
                    json!({
                        "sequence": sequence,
                        "board_position": index + 1,
                        "work_order": order,
                    })
                })
                .collect();
            let active_jobs = assignments.len();
            board.push(TechnicianBoard {
                technician_id,
                active_jobs,
                max_concurrent_jobs: max_jobs,
                available: (active_jobs as i64) < max_jobs,
                jobs: job_values,
            });
        }
        Ok(json!({
            "technicians": board,
            "max_concurrent_jobs_per_technician": max_jobs,
            "ordering": "assignment sequence, then priority (urgent > high > normal > low), then due date, then creation time. This is a dispatch ordering heuristic, not a route optimiser: no travel times or distances are computed.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_dispatch_assignments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        let params = query.into_inner();
        let technician_filter = params.filters.get("technician_id").cloned();
        let order_filter = params.filters.get("work_order_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<DispatchAssignment> = state
            .dispatch_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(technician) = &technician_filter {
                    if &row.technician_id != technician {
                        return false;
                    }
                }
                if let Some(order_id) = &order_filter {
                    if row.work_order_id.to_string() != *order_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| {
            a.sequence
                .cmp(&b.sequence)
                .then(a.assigned_at.cmp(&b.assigned_at))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn release_dispatch_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DispatchAssignment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_MANAGE)?;
        let mut assignment = load_dispatch_assignment(&state, &path.into_inner())?;
        if assignment.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active dispatch assignment can be released (current status: \"{}\")",
                assignment.status
            )));
        }
        assignment.status = "released".to_string();
        assignment.released_at = Some(Utc::now());
        assignment.updated_at = Utc::now();
        state.dispatch_assignments.insert(assignment.id, assignment.clone());
        if let Some(mut order) = state
            .work_orders
            .get(&assignment.work_order_id)
            .map(|entry| entry.value().clone())
        {
            if order.status == WorkOrderStatus::Assigned
                && order.assignee_id.as_deref() == Some(assignment.technician_id.as_str())
            {
                order.status = WorkOrderStatus::Open;
                order.assignee_id = None;
                order.updated_at = Utc::now();
                state.work_orders.insert(order.id, order.clone());
            }
        }
        state.emit(
            "field_service.dispatch.released",
            json!({
                "dispatch_assignment_id": assignment.id.to_string(),
                "work_order_id": assignment.work_order_id.to_string(),
                "technician_id": assignment.technician_id,
                "released_by": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}

async fn complete_dispatch_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DispatchAssignment> = (|| {
        let caller = caller(&req)?;
        let mut assignment = load_dispatch_assignment(&state, &path.into_inner())?;
        let order = load_work_order(&state, &assignment.work_order_id.to_string())?;
        can_act_on_order(&ctx, &caller, &order)?;
        if assignment.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Only an active dispatch assignment can be completed (current status: \"{}\")",
                assignment.status
            )));
        }
        assignment.status = "completed".to_string();
        assignment.completed_at = Some(Utc::now());
        assignment.updated_at = Utc::now();
        state.dispatch_assignments.insert(assignment.id, assignment.clone());
        state.emit(
            "field_service.dispatch.completed",
            json!({
                "dispatch_assignment_id": assignment.id.to_string(),
                "work_order_id": assignment.work_order_id.to_string(),
                "technician_id": assignment.technician_id,
                "completed_by": caller.actor,
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Definition and settings handlers
// ---------------------------------------------------------------------------

async fn list_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn add_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddDefinitionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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

async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
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
            "priority" => state
                .work_orders
                .iter()
                .any(|entry| entry.value().priority == value),
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

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldServiceSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_FIELD_SERVICE_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<FieldServiceState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<FieldServiceSettings> = (|| {
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
