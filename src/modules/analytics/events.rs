//! analytics - events.

use actix_web::{ http::header };



// ---------------------------------------------------------------------------
// Event names.
// ---------------------------------------------------------------------------
pub const ANALYTICS_REPORT_GENERATED: &str = "analytics.report.generated";

pub const ANALYTICS_REPORT_EXPORTED: &str = "analytics.report.exported";

pub const ANALYTICS_REPORT_RUN_REQUESTED: &str = "analytics.report.run_requested";


pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";

pub(crate) const STORE_ORDER_REFUNDED: &str = "store.order.refunded";

pub(crate) const LICENSE_ACTIVATED: &str = "license.activated";

pub(crate) const LICENSE_SUSPENDED: &str = "license.suspended";

pub(crate) const CLUSTER_HEALTH_CHANGED: &str = "cluster.health.changed";

pub(crate) const TASK_CREATED: &str = "task.created";

pub(crate) const TASK_STATUS_CHANGED: &str = "task.status.changed";

pub(crate) const TASK_COMPLETED: &str = "task.completed";

pub(crate) const PLANNER_POOL_CREATED: &str = "planner.pool.created";

pub(crate) const PLANNER_PLAN_APPROVED: &str = "planner.plan.approved";

pub(crate) const PLANNER_PLAN_ACTIVATED: &str = "planner.plan.activated";

pub(crate) const PLANNER_PLAN_EXPIRED: &str = "planner.plan.expired";

pub(crate) const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
