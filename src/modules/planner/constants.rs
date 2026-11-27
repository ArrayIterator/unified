//! Planner - Constants: limits, capability names, event names, statuses.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{helpers::*, types::*, state::*, handlers_pools::*, forecast::*, handlers_plans::*, events::*, module::*};

// Event names.

pub const PLANNER_POOL_CREATED: &str = "planner.pool.created";

pub const PLANNER_PLAN_APPROVED: &str = "planner.plan.approved";

pub const PLANNER_PLAN_ACTIVATED: &str = "planner.plan.activated";

pub const PLANNER_PLAN_EXPIRED: &str = "planner.plan.expired";

pub const PLANNER_OVERCOMMIT: &str = "planner.overcommit";

pub const PLANNER_FORECAST_GENERATED: &str = "planner.forecast.generated";

/// Provider sync event consumed to refresh pool sync information.
pub(crate) const PROVIDER_SYNC_COMPLETED: &str = "provider.sync.completed";

/// Capability names checked through the `role.check` hook, following
/// the recruitment and saas_packages modules (`<module>.view` /
/// `<module>.manage`).
///
/// Every planner route requires the `X-User-Id` and `X-Role` headers
/// (see `caller` in helpers.rs). Reads require `planner.view`;
/// mutations - creating, updating or deleting pools and plans,
/// recording usage, and the approve / activate / expire transitions -
/// require `planner.manage`. The check fails closed: when no
/// `role.check` handler answers, the request is refused.
pub(crate) const PERM_PLANNER_VIEW: &str = "planner.view";

pub(crate) const PERM_PLANNER_MANAGE: &str = "planner.manage";
