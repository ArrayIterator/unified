// Generic Approvals - Event names, permission strings, limits, and other constants.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::module::*;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_NAME_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_MODULE_LEN: usize = 128;

pub(crate) const MAX_RECORD_TYPE_LEN: usize = 128;

pub(crate) const MAX_RECORD_ID_LEN: usize = 256;

pub(crate) const MAX_USER_ID_LEN: usize = 128;

pub(crate) const MAX_ROLE_LEN: usize = 128;

pub(crate) const MAX_SUMMARY_LEN: usize = 1000;

pub(crate) const MAX_COMMENT_LEN: usize = 5000;

pub(crate) const MAX_CURRENCY_LEN: usize = 8;

pub(crate) const MAX_COMPANY_ID_LEN: usize = 128;

/// Maximum steps on one definition (configurable via settings).
pub(crate) const DEFAULT_MAX_STEPS_PER_DEFINITION: usize = 20;

/// Maximum approver slots on one step (configurable via settings).
pub(crate) const DEFAULT_MAX_APPROVERS_PER_STEP: usize = 10;

/// Default request timeout in hours (configurable via settings).
pub(crate) const DEFAULT_TIMEOUT_HOURS: i64 = 72;

/// Default deadline extension per escalation, in hours
/// (configurable via settings).
pub(crate) const DEFAULT_ESCALATION_EXTENSION_HOURS: i64 = 24;

/// Default maximum escalations before a request expires
/// (configurable via settings).
pub(crate) const DEFAULT_MAX_ESCALATIONS: u32 = 3;

/// Longest timeout a definition may set, in hours (30 days).
pub(crate) const MAX_TIMEOUT_HOURS: i64 = 720;

/// Longest escalation extension, in hours (30 days).
pub(crate) const MAX_ESCALATION_EXTENSION_HOURS: i64 = 720;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_APPROVALS_VIEW: &str = "approvals.view";

pub(crate) const PERM_APPROVALS_MANAGE: &str = "approvals.manage";

pub(crate) const PERM_APPROVALS_DECIDE: &str = "approvals.decide";

/// Hook event names.
pub(crate) const APPROVAL_REQUESTED: &str = "approval.requested";

pub(crate) const APPROVAL_DECIDED: &str = "approval.decided";

pub(crate) const APPROVAL_CREATED: &str = "approval.created";

pub(crate) const APPROVAL_RESUBMITTED: &str = "approval.resubmitted";

pub(crate) const APPROVAL_CANCELLED: &str = "approval.cancelled";

pub(crate) const APPROVAL_ESCALATED: &str = "approval.escalated";

pub(crate) const APPROVAL_EXPIRED: &str = "approval.expired";

pub(crate) const NOTIFICATION_REQUESTED: &str = "notification.requested";

pub(crate) const ROLE_CHECK: &str = "role.check";

pub(crate) const USER_MANAGER_QUERY: &str = "user.manager.query";

/// Request statuses. This is the state-machine vocabulary (see the
/// lifecycle note in the header): the values are fixed so every
/// module that reads `approval.decided` can rely on them, while
/// everything else about a workflow - steps, approvers, timeouts,
/// escalation rules - is data.
pub(crate) const STATUS_REQUESTED: &str = "requested";

pub(crate) const STATUS_IN_PROGRESS: &str = "in_progress";

pub(crate) const STATUS_APPROVED: &str = "approved";

pub(crate) const STATUS_REJECTED: &str = "rejected";

pub(crate) const STATUS_CHANGES_REQUESTED: &str = "changes_requested";

pub(crate) const STATUS_EXPIRED: &str = "expired";

pub(crate) const STATUS_CANCELLED: &str = "cancelled";

/// All request statuses, in lifecycle order.
pub(crate) const REQUEST_STATUSES: [&str; 7] = [
    STATUS_REQUESTED,
    STATUS_IN_PROGRESS,
    STATUS_APPROVED,
    STATUS_REJECTED,
    STATUS_CHANGES_REQUESTED,
    STATUS_EXPIRED,
    STATUS_CANCELLED,
];

/// Statuses a request may sit in while it is still open.
pub(crate) const OPEN_STATUSES: [&str; 3] = [STATUS_REQUESTED, STATUS_IN_PROGRESS, STATUS_CHANGES_REQUESTED];

/// Statuses a resubmit may start from.
pub(crate) const RESUBMITTABLE_STATUSES: [&str; 2] = [STATUS_CHANGES_REQUESTED, STATUS_EXPIRED];

/// Step completion modes (data on the step, validated at save time).
pub(crate) const STEP_MODE_ALL: &str = "all";

pub(crate) const STEP_MODE_ANY: &str = "any";

pub(crate) const STEP_MODES: [&str; 2] = [STEP_MODE_ALL, STEP_MODE_ANY];

/// Approver slot kinds (data on the step, validated at save time).
pub(crate) const APPROVER_KIND_USER: &str = "user";

pub(crate) const APPROVER_KIND_ROLE: &str = "role";

pub(crate) const APPROVER_KIND_MANAGER: &str = "manager";

pub(crate) const APPROVER_KINDS: [&str; 3] = [APPROVER_KIND_USER, APPROVER_KIND_ROLE, APPROVER_KIND_MANAGER];

/// Decision values. `request_changes` is stored snake_case and
/// reported as `changes_requested` in the `approval.decided` payload.
pub(crate) const DECISION_APPROVE: &str = "approve";

pub(crate) const DECISION_REJECT: &str = "reject";

pub(crate) const DECISION_REQUEST_CHANGES: &str = "request_changes";

pub(crate) const DECISIONS: [&str; 3] = [DECISION_APPROVE, DECISION_REJECT, DECISION_REQUEST_CHANGES];

/// Escalation actions a definition may carry (data on the
/// definition, validated at save time).
pub(crate) const ESCALATION_NOTIFY: &str = "notify";

pub(crate) const ESCALATION_NOTIFY_AND_REASSIGN: &str = "notify_and_reassign";

pub(crate) const ESCALATION_NONE: &str = "none";

pub(crate) const ESCALATION_ACTIONS: [&str; 3] = [
    ESCALATION_NOTIFY,
    ESCALATION_NOTIFY_AND_REASSIGN,
    ESCALATION_NONE,
];
