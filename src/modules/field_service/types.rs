// field_service module — Domain types.
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

use super::constants::{DEFAULT_INTERVAL_DAYS, DEFAULT_MAX_CONCURRENT_JOBS, DEFAULT_OVERDUE_ALERT_DAYS, DEFAULT_PRIORITY};
use super::helpers::created;

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
    pub(crate) fn as_str(&self) -> &'static str {
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

    pub(crate) fn from_str(value: &str) -> Option<Self> {
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
    pub(crate) fn allowed_next(&self) -> &'static [WorkOrderStatus] {
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

    pub(crate) fn is_terminal(&self) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_status_is_open() {
        assert_eq!(WorkOrderStatus::default(), WorkOrderStatus::Open);
    }

    #[test]
    fn status_names_round_trip() {
        for status in [
            WorkOrderStatus::Open,
            WorkOrderStatus::Assigned,
            WorkOrderStatus::InProgress,
            WorkOrderStatus::OnHold,
            WorkOrderStatus::Completed,
            WorkOrderStatus::Verified,
            WorkOrderStatus::Closed,
            WorkOrderStatus::Cancelled,
        ] {
            assert_eq!(WorkOrderStatus::from_str(status.as_str()), Some(status));
        }
        assert_eq!(WorkOrderStatus::from_str("bogus"), None);
    }

    #[test]
    fn status_transitions_match_the_documented_machine() {
        assert_eq!(
            WorkOrderStatus::Open.allowed_next(),
            &[WorkOrderStatus::Assigned, WorkOrderStatus::Cancelled]
        );
        assert_eq!(
            WorkOrderStatus::Assigned.allowed_next(),
            &[
                WorkOrderStatus::InProgress,
                WorkOrderStatus::Open,
                WorkOrderStatus::Cancelled
            ]
        );
        assert_eq!(
            WorkOrderStatus::Completed.allowed_next(),
            &[WorkOrderStatus::Verified]
        );
        assert!(WorkOrderStatus::Closed.allowed_next().is_empty());
        assert!(WorkOrderStatus::Cancelled.allowed_next().is_empty());
    }

    #[test]
    fn only_closed_and_cancelled_are_terminal() {
        assert!(WorkOrderStatus::Closed.is_terminal());
        assert!(WorkOrderStatus::Cancelled.is_terminal());
        assert!(!WorkOrderStatus::Open.is_terminal());
        assert!(!WorkOrderStatus::Completed.is_terminal());
    }
}
