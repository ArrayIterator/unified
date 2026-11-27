// Generic Approvals - Public record, request, and view types.

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

use super::constants::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::module::*;

// Types

/// One approver slot on a step: who may satisfy it. `ref_value` is
/// the user id for `user`, the role slug for `role`, and is ignored
/// for `manager` (the requester's manager, resolved via hook).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepApprover {
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub ref_value: String,
}

/// An approval definition: for records of (`module`, `record_type`),
/// which steps apply. At most one ACTIVE definition may exist per
/// (`module`, `record_type`) pair, so a request always resolves to
/// exactly one workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDefinition {
    pub id: Uuid,
    pub module: String,
    pub record_type: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub is_active: bool,
    /// Timeout in hours for requests under this definition. `None`
    /// means "use the settings default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_hours: Option<i64>,
    /// Escalation rule: `notify`, `notify_and_reassign`, or `none`.
    pub escalation_action: String,
    /// Who the escalation notifies: `{kind, ref_value}` in the same
    /// shape as a step approver slot. `None` notifies only the
    /// current step's approvers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalate_to: Option<StepApprover>,
    /// Deadline extension per escalation, in hours. `None` means
    /// "use the settings default".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escalation_extension_hours: Option<i64>,
    pub company_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One ordered step of a definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalStep {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub position: i32,
    pub name: String,
    /// `all` (every slot must approve) or `any` (one approval
    /// completes the step).
    pub mode: String,
    pub approvers: Vec<StepApprover>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

/// One approval request for one record in one module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalRequest {
    pub id: Uuid,
    pub definition_id: Uuid,
    pub module: String,
    pub record_type: String,
    pub record_id: String,
    pub requester_id: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amount_minor: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    pub status: String,
    pub current_step_index: i32,
    /// Decision round: incremented on every resubmit. Only decisions
    /// from the current round count towards step completion.
    pub round: i32,
    pub escalation_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_escalated_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One decision. Append-only: never edited or deleted.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDecision {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub round: i32,
    /// The user who actually decided.
    pub approver_id: String,
    /// When a delegate decided, the original approver they acted for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acting_for: Option<String>,
    /// Which approver slot this decision satisfied.
    pub slot_kind: String,
    pub slot_ref: String,
    /// `approve`, `reject`, or `request_changes`.
    pub decision: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A temporary delegation: `delegate_id` may decide in place of
/// `delegator_id` between `starts_at` and `ends_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalDelegation {
    pub id: Uuid,
    pub delegator_id: String,
    pub delegate_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub is_active: bool,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// One escalation event on a request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalEscalation {
    pub id: Uuid,
    pub request_id: Uuid,
    pub step_index: i32,
    pub reason: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
}

/// Admin-editable configurable defaults. Every number in this module
/// that is not per-definition data falls back to these values, so no
/// behaviour is baked into code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApprovalSettings {
    pub default_timeout_hours: i64,
    pub default_escalation_extension_hours: i64,
    pub max_escalations: u32,
    pub max_steps_per_definition: usize,
    pub max_approvers_per_step: usize,
    pub allow_self_approval: bool,
    pub require_comment_on_approve: bool,
}

impl Default for ApprovalSettings {
    fn default() -> Self {
        Self {
            default_timeout_hours: DEFAULT_TIMEOUT_HOURS,
            default_escalation_extension_hours: DEFAULT_ESCALATION_EXTENSION_HOURS,
            max_escalations: DEFAULT_MAX_ESCALATIONS,
            max_steps_per_definition: DEFAULT_MAX_STEPS_PER_DEFINITION,
            max_approvers_per_step: DEFAULT_MAX_APPROVERS_PER_STEP,
            allow_self_approval: false,
            require_comment_on_approve: false,
        }
    }
}

/// A request with its definition steps (with live resolution) and
/// its decision history, as returned by the detail endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequestDetail {
    #[serde(flatten)]
    pub request: ApprovalRequest,
    pub steps: Vec<ApprovalStepView>,
    pub decisions: Vec<ApprovalDecision>,
}

/// One step as seen from a request: the definition step plus its
/// live resolution state (which slots are satisfied, which manager
/// could not be resolved).
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalStepView {
    #[serde(flatten)]
    pub step: ApprovalStep,
    pub status: String,
    pub satisfied_slots: Vec<String>,
    pub resolution: String,
}

// Input payloads

#[derive(Debug, Deserialize)]
pub struct CreateDefinitionInput {
    pub module: String,
    pub record_type: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub is_active: bool,
    #[serde(default)]
    pub timeout_hours: Option<i64>,
    #[serde(default = "default_escalation_action")]
    pub escalation_action: String,
    #[serde(default)]
    pub escalate_to: Option<StepApprover>,
    #[serde(default)]
    pub escalation_extension_hours: Option<i64>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub steps: Vec<CreateStepInput>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDefinitionInput {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub is_active: Option<bool>,
    #[serde(default)]
    pub timeout_hours: Option<i64>,
    #[serde(default)]
    pub escalation_action: Option<String>,
    #[serde(default)]
    pub escalate_to: Option<StepApprover>,
    #[serde(default)]
    pub escalation_extension_hours: Option<i64>,
    #[serde(default)]
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateStepInput {
    pub name: String,
    #[serde(default = "default_step_mode")]
    pub mode: String,
    #[serde(default)]
    pub approvers: Vec<StepApprover>,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateRequestInput {
    pub module: String,
    pub record_type: String,
    pub record_id: String,
    #[serde(default)]
    pub requester_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub amount_minor: Option<i64>,
    #[serde(default)]
    pub currency: Option<String>,
    #[serde(default)]
    pub company_id: Option<String>,
    #[serde(default)]
    pub definition_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DecideInput {
    pub decision: String,
    #[serde(default)]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReassignInput {
    pub step_index: i32,
    pub approvers: Vec<StepApprover>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDelegationInput {
    pub delegator_id: String,
    pub delegate_id: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsInput {
    #[serde(default)]
    pub default_timeout_hours: Option<i64>,
    #[serde(default)]
    pub default_escalation_extension_hours: Option<i64>,
    #[serde(default)]
    pub max_escalations: Option<u32>,
    #[serde(default)]
    pub max_steps_per_definition: Option<usize>,
    #[serde(default)]
    pub max_approvers_per_step: Option<usize>,
    #[serde(default)]
    pub allow_self_approval: Option<bool>,
    #[serde(default)]
    pub require_comment_on_approve: Option<bool>,
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_step_mode() -> String {
    STEP_MODE_ANY.to_string()
}

pub(crate) fn default_escalation_action() -> String {
    ESCALATION_NOTIFY.to_string()
}
