// field_service module — Module constants.
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

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;
pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;
pub(crate) const MAX_LOCATION_LEN: usize = 200;
pub(crate) const MAX_SERIAL_LEN: usize = 128;
pub(crate) const MAX_NOTES_LEN: usize = 5000;
pub(crate) const MAX_TITLE_LEN: usize = 200;
pub(crate) const MAX_SLUG_LEN: usize = 64;
pub(crate) const MAX_LABEL_LEN: usize = 200;
pub(crate) const MAX_UNIT_LEN: usize = 32;
pub(crate) const MAX_CHECKLIST_ITEMS: usize = 100;
pub(crate) const MAX_PARTS_PER_ORDER: usize = 100;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_FIELD_SERVICE_VIEW: &str = "field_service.view";
pub(crate) const PERM_FIELD_SERVICE_MANAGE: &str = "field_service.manage";
pub(crate) const PERM_FIELD_SERVICE_TECHNICIAN: &str = "field_service.technician";

/// Hook event names this module listens to.
pub(crate) const ASSETS_UPDATED: &str = "assets.updated";
pub(crate) const PLANS_CHECK_REQUESTED: &str = "field_service.plans.check_requested";

/// Configurable defaults. Every one of these is a starting value
/// only: the admin can change the settings through
/// `PATCH /settings` and the registries through the definitions
/// endpoints, and seeded registry values are flagged `is_system`
/// so the admin UI can tell defaults apart from custom values.
pub(crate) const DEFAULT_MAX_CONCURRENT_JOBS: i64 = 3;
pub(crate) const DEFAULT_OVERDUE_ALERT_DAYS: i64 = 7;
pub(crate) const DEFAULT_INTERVAL_DAYS: i64 = 90;
pub(crate) const DEFAULT_PRIORITY: &str = "normal";

/// Definition registry kinds accepted by the definitions
/// endpoints. `interval_unit` carries an extra `days_per_unit`
/// (a calendar-month unit uses `is_calendar_month` instead, and
/// a usage-based unit uses `is_usage_based`).
pub(crate) const DEFINITION_KINDS: [&str; 4] = [
    "work_order_type",
    "priority",
    "equipment_status",
    "interval_unit",
];
