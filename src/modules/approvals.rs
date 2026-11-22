// Generic Approvals module — data-driven approval workflows for any
// record in any module, without duplicating another module's data.
//
// Routes live under `/api/v1/approvals`:
//
//   Definitions (admin: what gets approved, and by whom):
//     GET/POST /definitions — list definitions / create a definition.
//     GET/PATCH/DELETE /definitions/{id}.
//     POST /definitions/{id}/steps — append a step to a definition.
//     DELETE /definitions/{id}/steps/{step_id} — remove a step.
//   Requests (one approval flow for one record):
//     GET/POST /requests — list requests / create a request.
//     GET /requests/{id} — one request with its steps and decisions.
//     POST /requests/{id}/decide — approve / reject / request-changes.
//     POST /requests/{id}/resubmit — return a changes-requested or
//       expired request to in_progress (new decision round).
//     POST /requests/{id}/cancel — cancel an open request.
//     POST /requests/{id}/reassign — replace the approvers of one
//       pending step (admin; used for escalation handover).
//     GET /requests/{id}/history — append-only decision history.
//     POST /requests/sweep-expiry — expire overdue requests and
//       escalate them (admin; see the expiry note below).
//   Dashboards:
//     GET /my-pending — requests waiting on the caller (as approver
//       or as an active delegate), paginated.
//     GET /my-requests — requests the caller asked for, paginated.
//     GET /overdue — requests past their deadline, paginated.
//     GET /by-record?module=&record_type=&record_id= — requests and
//       decisions for one record.
//   Delegations:
//     GET/POST /delegations — list / create a delegation.
//     DELETE /delegations/{id} — revoke a delegation.
//   Settings (admin-editable configurable defaults):
//     GET/PATCH /settings.
//
//
// HOOK CONTRACT — the core of this module. Other modules never call
// this module directly; everything crosses the hook bus.
//
//   `approval.requested` (emitted BY any module, consumed HERE).
//   Payload:
//     {
//       "module": "invoices",            // owning module id (required)
//       "record_type": "invoice",        // record kind in that module (required)
//       "record_id": "9f...",            // record id as text (required)
//       "requester_id": "user-42",        // who asks (required)
//       "summary": "Invoice #1042 — 1,250,000 IDR",  // (required)
//       "amount_minor": 125000000,       // optional, integer minor units
//       "currency": "IDR",               // optional, ISO 4217 code
//       "company_id": "acme"             // optional tenant scope
//     }
//   The handler finds the active definition for
//   (`module`, `record_type`); when none exists it answers
//   `{"accepted": false, "error": "No active approval definition ..."}`
//   and creates nothing. When one exists it creates the request and
//   answers `{"accepted": true, "request_id": "...",
//   "status": "in_progress"}` via `add_result` and the `result` key.
//   The requesting module keeps the `request_id` and uses it to
//   correlate the `approval.decided` answer.
//
//   `approval.decided` (emitted BY this module, consumed by the
//   requesting module). Payload:
//     {
//       "request_id": "9f...",
//       "decision": "approved" | "rejected" | "changes_requested",
//       "decided_by": "user-7",          // last deciding approver
//       "module": "invoices",
//       "record_type": "invoice",
//       "record_id": "9f...",
//       "requester_id": "user-42"
//     }
//   Emitted exactly once per request, when it reaches a terminal
//   decision. The requesting module applies the outcome to its own
//   record; this module never writes to another module's tables.
//
//   `approval.status.query` (emitted BY any module, answered HERE).
//   Payload: either `{"request_id": "..."}` or
//   `{"module": "...", "record_type": "...", "record_id": "..."}`.
//   Answer (via `add_result` and the `result` key):
//     {"found": true, "request_id": "...", "status": "in_progress",
//      "current_step_index": 0, "expires_at": "2026-11-25T...Z"}
//   or `{"found": false}` when no request matches. The latest
//   request for the record wins.
//
//   Other events emitted by this module:
//     - `approval.created` — `{"request_id", "module", "record_type",
//       "record_id", "requester_id", "definition_id"}`.
//     - `approval.resubmitted` — `{"request_id", "round"}`.
//     - `approval.cancelled` — `{"request_id", "cancelled_by"}`.
//     - `approval.escalated` — `{"request_id", "step_index",
//       "escalation_count", "action"}`.
//     - `approval.expired` — `{"request_id", "step_index"}` (only
//       when the escalation rule gives up and the request expires).
//     - `notification.requested` — to the current step's approvers
//       when a request is created, and to the escalation target plus
//       the current approvers when a request escalates, using the
//       notifications module's payload shape (`to_user_id`,
//       `title`, `body`). Delivery is the notifications module's job.
//
//   Events this module emits to resolve people (never duplicated
//   user records — approvers are always resolved through other
//   modules):
//     - `user.manager.query` — `{"user_id": "..."}`; the HR / users
//       module answers with `{"manager_id": "..."}` (read from the
//       `manager_id` key). When nobody answers, a `manager`
//       approver slot stays unresolved: the step cannot be decided
//       by that slot and the request lists
//       `"resolution": "unresolved"` for it, so an administrator can
//       reassign the step instead of the request stalling silently.
//
//
// DATA MODEL — everything is data, never baked into code:
//
//   An approval definition names a (`module`, `record_type`) pair and
//   owns ordered steps. A step has a `mode` (`all` — every approver
//   slot must approve; `any` — one approval completes the step) and a
//   list of approver slots. A slot is `{kind, ref}` where kind is
//   `user` (ref is the user id), `role` (ref is the role slug; any
//   caller whose `X-Role` matches satisfies the slot), or `manager`
//   (the requester's manager, resolved through the hook above; the
//   ref is ignored). Steps run in `position` order; a request
//   advances one step at a time.
//
//   A request is polymorphic: (`module`, `record_type`, `record_id`)
//   are plain text. There is deliberately NO foreign key into
//   another module's tables — the owning module owns its records.
//
//   Delegation: an approver delegates to another user for a date
//   range. A delegate deciding records `acting_for` with the original
//   approver's id, so the history always shows who really decided
//   and on whose behalf.
//
//   Decisions are append-only history: `approve`, `reject`, or
//   `request_changes`, each with a comment (required for `reject` and
//   `request_changes`, optional for `approve` unless the settings
//   require it), a timestamp, and the acting identity. Decisions are
//   never edited or deleted — there is no update/delete endpoint for
//   them, and the SQL migration adds a trigger that rejects
//   UPDATE/DELETE on the decisions table.
//
//
// REQUEST LIFECYCLE — the state machine:
//
//   requested -> in_progress -> approved | rejected |
//   changes_requested -> (resubmit) -> in_progress
//   in_progress -> expired (only when escalation gives up; a
//   resubmit from expired is allowed) ; any open state -> cancelled.
//
//   A new request starts `requested` and moves to `in_progress`
//   immediately when its definition has at least one active step; a
//   definition with no active steps leaves the request `requested`
//   until a step is added. A `reject` anywhere ends the request as
//   `rejected`. `request_changes` ends the round as
//   `changes_requested`; a resubmit starts a new decision round
//   (decisions keep their `round` number, so earlier rounds stay
//   visible in history and only the current round counts towards
//   step completion). Completing the last step ends the request as
//   `approved`.
//
//   Expiry: every request carries `expires_at`, computed from its
//   definition's `timeout_hours` (or the settings default when the
//   definition leaves it unset). Expiry NEVER silently auto-approves.
//   The sweep (`POST /requests/sweep-expiry`, or any scheduler job
//   that calls it) finds overdue `in_progress` requests and applies
//   the definition's escalation rule: `notify` (notify the escalation
//   target and the current approvers, extend the deadline by
//   `escalation_extension_hours`), `notify_and_reassign` (same, and
//   the caller is expected to reassign the step through
//   `POST /requests/{id}/reassign` — the escalation record names the
//   target), or `none` (the request expires immediately). After
//   `max_escalations` escalations (settings default 3) the request
//   expires. An expired request can be resubmitted.
//
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory definition / step / request / decision / delegation /
// escalation management in DashMaps (the in-memory store is
// authoritative; the SQL migration mirrors the tables for the
// persistence layer, exactly like the other modules that landed
// before the database layer was wired in); the full hook contract
// above; step completion for `all`/`any` modes; delegation with
// acting-for recording; append-only decisions; expiry sweep with
// escalation and a hard cap; per-request step reassignment;
// dashboards (my pending / my requests / overdue / per-record
// history); admin-editable settings for every configurable default.
//
// What is NOT built here:
// - No persistence beyond the in-memory store: the migration
//   creates the tables, but no Diesel queries run against them yet.
//   A restart loses in-memory state until the persistence layer is
//   wired (same standing gap as the sibling modules).
// - No real user directory: user ids and role slugs are opaque
//   strings. `role` slots are satisfied by the caller's `X-Role`
//   header, and `manager` slots depend on the HR / users module
//   answering `user.manager.query` (see above). Nothing here
//   duplicates user records.
// - The escalation target is notified, not granted decision rights
//   by the escalation itself; handing the step to someone else is
//   the administrator's explicit `reassign` call.
// - `notification.requested` is only emitted: delivery belongs to
//   the notifications module.
//
// Public-access gate: every route requires `X-User-Id` and `X-Role`
// headers and a capability check through the `role.check` hook,
// which fails closed (no answer means not allowed). Reads need
// `approvals.view`; writes need `approvals.manage`; recording a
// decision needs `approvals.decide` (an approver does not need
// `approvals.manage`). There is no token-authenticated public route.
// Like every other module, the API Manager's `ApiAccessRule` gate
// still applies on top (default deny).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs against,
// exactly like the events and workflows modules.
//
// Security: capability checks on every endpoint; all input validated
// (lengths, UUIDs, enums, amounts); no raw SQL (Diesel only for the
// future persistence layer — this file issues no SQL at all);
// secrets are never stored (there are no secrets in this module);
// output is serialized through serde, never string-built HTML.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_MODULE_LEN: usize = 128;
const MAX_RECORD_TYPE_LEN: usize = 128;
const MAX_RECORD_ID_LEN: usize = 256;
const MAX_USER_ID_LEN: usize = 128;
const MAX_ROLE_LEN: usize = 128;
const MAX_SUMMARY_LEN: usize = 1000;
const MAX_COMMENT_LEN: usize = 5000;
const MAX_CURRENCY_LEN: usize = 8;
const MAX_COMPANY_ID_LEN: usize = 128;
/// Maximum steps on one definition (configurable via settings).
const DEFAULT_MAX_STEPS_PER_DEFINITION: usize = 20;
/// Maximum approver slots on one step (configurable via settings).
const DEFAULT_MAX_APPROVERS_PER_STEP: usize = 10;
/// Default request timeout in hours (configurable via settings).
const DEFAULT_TIMEOUT_HOURS: i64 = 72;
/// Default deadline extension per escalation, in hours
/// (configurable via settings).
const DEFAULT_ESCALATION_EXTENSION_HOURS: i64 = 24;
/// Default maximum escalations before a request expires
/// (configurable via settings).
const DEFAULT_MAX_ESCALATIONS: u32 = 3;
/// Longest timeout a definition may set, in hours (30 days).
const MAX_TIMEOUT_HOURS: i64 = 720;
/// Longest escalation extension, in hours (30 days).
const MAX_ESCALATION_EXTENSION_HOURS: i64 = 720;

/// Capability names checked through the `role.check` hook.
const PERM_APPROVALS_VIEW: &str = "approvals.view";
const PERM_APPROVALS_MANAGE: &str = "approvals.manage";
const PERM_APPROVALS_DECIDE: &str = "approvals.decide";

/// Hook event names.
const APPROVAL_REQUESTED: &str = "approval.requested";
const APPROVAL_DECIDED: &str = "approval.decided";
const APPROVAL_STATUS_QUERY: &str = "approval.status.query";
const APPROVAL_CREATED: &str = "approval.created";
const APPROVAL_RESUBMITTED: &str = "approval.resubmitted";
const APPROVAL_CANCELLED: &str = "approval.cancelled";
const APPROVAL_ESCALATED: &str = "approval.escalated";
const APPROVAL_EXPIRED: &str = "approval.expired";
const NOTIFICATION_REQUESTED: &str = "notification.requested";
const ROLE_CHECK: &str = "role.check";
const USER_MANAGER_QUERY: &str = "user.manager.query";

/// Request statuses. This is the state-machine vocabulary (see the
/// lifecycle note in the header): the values are fixed so every
/// module that reads `approval.decided` can rely on them, while
/// everything else about a workflow — steps, approvers, timeouts,
/// escalation rules — is data.
const STATUS_REQUESTED: &str = "requested";
const STATUS_IN_PROGRESS: &str = "in_progress";
const STATUS_APPROVED: &str = "approved";
const STATUS_REJECTED: &str = "rejected";
const STATUS_CHANGES_REQUESTED: &str = "changes_requested";
const STATUS_EXPIRED: &str = "expired";
const STATUS_CANCELLED: &str = "cancelled";
/// All request statuses, in lifecycle order.
const REQUEST_STATUSES: [&str; 7] = [
    STATUS_REQUESTED,
    STATUS_IN_PROGRESS,
    STATUS_APPROVED,
    STATUS_REJECTED,
    STATUS_CHANGES_REQUESTED,
    STATUS_EXPIRED,
    STATUS_CANCELLED,
];
/// Statuses a request may sit in while it is still open.
const OPEN_STATUSES: [&str; 3] = [STATUS_REQUESTED, STATUS_IN_PROGRESS, STATUS_CHANGES_REQUESTED];
/// Statuses a resubmit may start from.
const RESUBMITTABLE_STATUSES: [&str; 2] = [STATUS_CHANGES_REQUESTED, STATUS_EXPIRED];

/// Step completion modes (data on the step, validated at save time).
const STEP_MODE_ALL: &str = "all";
const STEP_MODE_ANY: &str = "any";
const STEP_MODES: [&str; 2] = [STEP_MODE_ALL, STEP_MODE_ANY];

/// Approver slot kinds (data on the step, validated at save time).
const APPROVER_KIND_USER: &str = "user";
const APPROVER_KIND_ROLE: &str = "role";
const APPROVER_KIND_MANAGER: &str = "manager";
const APPROVER_KINDS: [&str; 3] = [APPROVER_KIND_USER, APPROVER_KIND_ROLE, APPROVER_KIND_MANAGER];

/// Decision values. `request_changes` is stored snake_case and
/// reported as `changes_requested` in the `approval.decided` payload.
const DECISION_APPROVE: &str = "approve";
const DECISION_REJECT: &str = "reject";
const DECISION_REQUEST_CHANGES: &str = "request_changes";
const DECISIONS: [&str; 3] = [DECISION_APPROVE, DECISION_REJECT, DECISION_REQUEST_CHANGES];

/// Escalation actions a definition may carry (data on the
/// definition, validated at save time).
const ESCALATION_NOTIFY: &str = "notify";
const ESCALATION_NOTIFY_AND_REASSIGN: &str = "notify_and_reassign";
const ESCALATION_NONE: &str = "none";
const ESCALATION_ACTIONS: [&str; 3] = [
    ESCALATION_NOTIFY,
    ESCALATION_NOTIFY_AND_REASSIGN,
    ESCALATION_NONE,
];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Input payloads
// ---------------------------------------------------------------------------

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

fn default_true() -> bool {
    true
}

fn default_step_mode() -> String {
    STEP_MODE_ANY.to_string()
}

fn default_escalation_action() -> String {
    ESCALATION_NOTIFY.to_string()
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ApprovalsState {
    pub definitions: Arc<DashMap<Uuid, ApprovalDefinition>>,
    pub steps: Arc<DashMap<Uuid, ApprovalStep>>,
    pub requests: Arc<DashMap<Uuid, ApprovalRequest>>,
    pub decisions: Arc<DashMap<Uuid, ApprovalDecision>>,
    pub delegations: Arc<DashMap<Uuid, ApprovalDelegation>>,
    pub escalations: Arc<DashMap<Uuid, ApprovalEscalation>>,
    /// Per-request override of one step's approvers, set by the
    /// reassign endpoint: (request_id, step_index) -> approvers.
    pub step_overrides: Arc<DashMap<(Uuid, i32), Vec<StepApprover>>>,
    pub settings: Arc<RwLock<ApprovalSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ApprovalsState {
    fn default() -> Self {
        Self {
            definitions: Arc::new(DashMap::new()),
            steps: Arc::new(DashMap::new()),
            requests: Arc::new(DashMap::new()),
            decisions: Arc::new(DashMap::new()),
            delegations: Arc::new(DashMap::new()),
            escalations: Arc::new(DashMap::new()),
            step_overrides: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ApprovalSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ApprovalsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> ApprovalSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events and workflows
/// modules.
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
        ROLE_CHECK,
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
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                Ok(None)
            } else if trimmed.len() > max {
                Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )))
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
    }
}

/// A module id / record type: lowercase letters, digits, dots,
/// underscores, and hyphens, never leading / trailing with a dot, no
/// empty segments (for example `invoices`, `purchase.order`).
fn valid_record_key(value: &str, max: usize) -> bool {
    if value.is_empty() || value.len() > max {
        return false;
    }
    if value.starts_with('.') || value.ends_with('.') || value.contains("..") {
        return false;
    }
    value.chars().all(|c| {
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-'
    })
}

fn validate_approver(slot: &StepApprover) -> UnifiedResult<()> {
    if !APPROVER_KINDS.contains(&slot.kind.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Unknown approver kind: {} (expected one of {})",
            slot.kind,
            APPROVER_KINDS.join(", ")
        )));
    }
    match slot.kind.as_str() {
        APPROVER_KIND_USER => {
            require_text(&slot.ref_value, "approver ref (user id)", MAX_USER_ID_LEN)?;
        }
        APPROVER_KIND_ROLE => {
            require_text(&slot.ref_value, "approver ref (role slug)", MAX_ROLE_LEN)?;
        }
        APPROVER_KIND_MANAGER => {
            // The ref is ignored for a manager slot; anything stored
            // there is a caller mistake, not data.
            if !slot.ref_value.trim().is_empty() && slot.ref_value.trim() != "manager" {
                return Err(UnifiedError::bad_request(
                    "A manager approver slot takes no ref value",
                ));
            }
        }
        _ => {
            // The kind was validated against APPROVER_KINDS above,
            // so this arm is unreachable in practice — but a panic
            // here would take down the request if the list and this
            // match ever drift apart, so fail with a proper error
            // instead of `unreachable!()`.
            return Err(UnifiedError::bad_request(format!(
                "Unknown approver kind: {} (expected one of {})",
                slot.kind,
                APPROVER_KINDS.join(", ")
            )));
        }
    }
    Ok(())
}

fn validate_approvers(slots: &[StepApprover], max: usize) -> UnifiedResult<()> {
    if slots.is_empty() {
        return Err(UnifiedError::bad_request(
            "A step needs at least one approver",
        ));
    }
    if slots.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "A step holds at most {max} approvers"
        )));
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for slot in slots {
        validate_approver(slot)?;
        if !seen.insert((slot.kind.clone(), slot.ref_value.clone())) {
            return Err(UnifiedError::bad_request(format!(
                "Duplicate approver slot: {}:{}",
                slot.kind, slot.ref_value
            )));
        }
    }
    Ok(())
}

fn validate_timeout(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        None => Ok(None),
        Some(hours) => {
            if hours < 1 || hours > MAX_TIMEOUT_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be between 1 and {MAX_TIMEOUT_HOURS} hours"
                )));
            }
            Ok(Some(hours))
        }
    }
}

fn validate_extension(value: Option<i64>, field: &str) -> UnifiedResult<Option<i64>> {
    match value {
        None => Ok(None),
        Some(hours) => {
            if hours < 1 || hours > MAX_ESCALATION_EXTENSION_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be between 1 and {MAX_ESCALATION_EXTENSION_HOURS} hours"
                )));
            }
            Ok(Some(hours))
        }
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

// ---------------------------------------------------------------------------
// Lookups
// ---------------------------------------------------------------------------

fn load_definition(state: &ApprovalsState, raw_id: &str) -> UnifiedResult<ApprovalDefinition> {
    let id = parse_uuid(raw_id, "definition")?;
    state
        .definitions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Approval definition not found: {id}")))
}

fn load_request(state: &ApprovalsState, raw_id: &str) -> UnifiedResult<ApprovalRequest> {
    let id = parse_uuid(raw_id, "request")?;
    state
        .requests
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Approval request not found: {id}")))
}

fn active_definition_for(
    state: &ApprovalsState,
    module: &str,
    record_type: &str,
) -> Option<ApprovalDefinition> {
    let mut matches: Vec<ApprovalDefinition> = state
        .definitions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|definition| {
            definition.is_active && definition.module == module && definition.record_type == record_type
        })
        .collect();
    // Newest wins when more than one is active (creation refuses a
    // second active definition, so this is only a safety net).
    matches.sort_by_key(|definition| definition.created_at);
    matches.pop()
}

fn steps_for_definition(state: &ApprovalsState, definition_id: Uuid) -> Vec<ApprovalStep> {
    let mut items: Vec<ApprovalStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.definition_id == definition_id && step.is_active)
        .collect();
    items.sort_by_key(|step| step.position);
    items
}

fn decisions_for_request(state: &ApprovalsState, request_id: Uuid) -> Vec<ApprovalDecision> {
    let mut items: Vec<ApprovalDecision> = state
        .decisions
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|decision| decision.request_id == request_id)
        .collect();
    items.sort_by_key(|decision| decision.created_at);
    items
}

fn escalations_for_request(state: &ApprovalsState, request_id: Uuid) -> Vec<ApprovalEscalation> {
    let mut items: Vec<ApprovalEscalation> = state
        .escalations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|escalation| escalation.request_id == request_id)
        .collect();
    items.sort_by_key(|escalation| escalation.created_at);
    items
}

/// The approvers that apply to one step of one request: the
/// reassignment override wins over the definition step.
fn approvers_for_step(
    state: &ApprovalsState,
    request_id: Uuid,
    step_index: i32,
    step: &ApprovalStep,
) -> Vec<StepApprover> {
    state
        .step_overrides
        .get(&(request_id, step_index))
        .map(|entry| entry.value().clone())
        .unwrap_or_else(|| step.approvers.clone())
}

// ---------------------------------------------------------------------------
// Approver resolution
// ---------------------------------------------------------------------------

/// Ask the HR / users module for the requester's manager. Returns
/// `None` when nobody answers or the answer carries no manager id —
/// the slot stays unresolved instead of failing the whole request.
fn resolve_manager(state: &ApprovalsState, requester_id: &str) -> Option<String> {
    let bus = state.bus()?;
    let context = bus.emit(
        USER_MANAGER_QUERY,
        json!({ "user_id": requester_id }),
    );
    context
        .get("manager_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// An active delegation that lets `delegate_id` act for
/// `delegator_id` right now, if any.
fn active_delegation_for(
    state: &ApprovalsState,
    delegator_id: &str,
    delegate_id: &str,
    now: DateTime<Utc>,
) -> Option<ApprovalDelegation> {
    state
        .delegations
        .iter()
        .map(|entry| entry.value().clone())
        .find(|delegation| {
            delegation.is_active
                && delegation.delegator_id == delegator_id
                && delegation.delegate_id == delegate_id
                && delegation.starts_at <= now
                && now <= delegation.ends_at
        })
}

/// One slot of the current step, resolved against a caller: does the
/// caller satisfy this slot, directly or as a delegate? `acting_for`
/// is `Some(original approver)` when the caller decides as a
/// delegate, `None` when they decide as themselves.
struct SlotMatch {
    slot: StepApprover,
    acting_for: Option<String>,
}

fn match_slot(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    slot: &StepApprover,
    actor: &str,
    role: &str,
    now: DateTime<Utc>,
) -> Option<SlotMatch> {
    match slot.kind.as_str() {
        APPROVER_KIND_USER => {
            if actor == slot.ref_value {
                Some(SlotMatch { slot: slot.clone(), acting_for: None })
            } else if active_delegation_for(state, &slot.ref_value, actor, now).is_some() {
                Some(SlotMatch {
                    slot: slot.clone(),
                    acting_for: Some(slot.ref_value.clone()),
                })
            } else {
                None
            }
        }
        APPROVER_KIND_ROLE => {
            if role == slot.ref_value {
                Some(SlotMatch { slot: slot.clone(), acting_for: None })
            } else {
                None
            }
        }
        APPROVER_KIND_MANAGER => {
            let manager = resolve_manager(state, &request.requester_id)?;
            if actor == manager {
                Some(SlotMatch { slot: slot.clone(), acting_for: None })
            } else if active_delegation_for(state, &manager, actor, now).is_some() {
                Some(SlotMatch {
                    slot: slot.clone(),
                    acting_for: Some(manager),
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Every slot of the current step the caller may decide for, plus
/// whether the step has any slot that cannot be resolved right now
/// (an unanswered manager query).
fn matchable_slots(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    slots: &[StepApprover],
    actor: &str,
    role: &str,
    now: DateTime<Utc>,
) -> (Vec<SlotMatch>, bool) {
    let mut matched = Vec::new();
    let mut unresolved = false;
    for slot in slots {
        if slot.kind == APPROVER_KIND_MANAGER && resolve_manager(state, &request.requester_id).is_none() {
            unresolved = true;
        }
        if let Some(found) = match_slot(state, request, slot, actor, role, now) {
            matched.push(found);
        }
    }
    (matched, unresolved)
}

/// Is the caller waiting on this request — an approver of its current
/// step, or a delegate of one? Used by the `my-pending` dashboard.
fn caller_is_pending_approver(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    actor: &str,
    role: &str,
    now: DateTime<Utc>,
) -> bool {
    if request.status != STATUS_IN_PROGRESS {
        return false;
    }
    let Some(step) = current_step(state, request) else {
        return false;
    };
    let slots = approvers_for_step(state, request.id, request.current_step_index, &step);
    let (matched, _) = matchable_slots(state, request, &slots, actor, role, now);
    !matched.is_empty()
}

fn current_step(state: &ApprovalsState, request: &ApprovalRequest) -> Option<ApprovalStep> {
    let steps = steps_for_definition(state, request.definition_id);
    steps
        .into_iter()
        .nth(request.current_step_index as usize)
}

/// Step completion for the current round: which slots of the step
/// already hold an `approve` decision, and whether the step is done.
/// A slot is satisfied by a decision whose `slot_kind` / `slot_ref`
/// match it; a manager slot is satisfied by a decision recorded
/// against the resolved manager id.
fn satisfied_slots(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    step: &ApprovalStep,
) -> Vec<String> {
    let slots = approvers_for_step(state, request.id, request.current_step_index, step);
    let decisions = decisions_for_request(state, request.id);
    let mut satisfied: Vec<String> = Vec::new();
    for slot in &slots {
        let key = format!("{}:{}", slot.kind, slot.ref_value);
        let done = decisions.iter().any(|decision| {
            decision.step_index == request.current_step_index
                && decision.round == request.round
                && decision.decision == DECISION_APPROVE
                && slot_matches_decision(state, request, slot, decision)
        });
        if done {
            satisfied.push(key);
        }
    }
    satisfied
}

fn slot_matches_decision(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    slot: &StepApprover,
    decision: &ApprovalDecision,
) -> bool {
    if decision.slot_kind != slot.kind {
        return false;
    }
    match slot.kind.as_str() {
        APPROVER_KIND_USER => {
            decision.slot_ref == slot.ref_value
                || decision
                    .acting_for
                    .as_deref()
                    .map(|acting| acting == slot.ref_value)
                    .unwrap_or(false)
        }
        APPROVER_KIND_ROLE => decision.slot_ref == slot.ref_value,
        APPROVER_KIND_MANAGER => {
            // The decision records the resolved manager id in
            // `slot_ref`; it matches when it is the requester's
            // current manager, or was recorded for this request.
            match resolve_manager(state, &request.requester_id) {
                Some(manager) => decision.slot_ref == manager,
                None => decision.slot_ref == slot.ref_value || !decision.slot_ref.is_empty(),
            }
        }
        _ => false,
    }
}

fn step_is_complete(
    state: &ApprovalsState,
    request: &ApprovalRequest,
    step: &ApprovalStep,
) -> bool {
    let slots = approvers_for_step(state, request.id, request.current_step_index, step);
    let satisfied = satisfied_slots(state, request, step);
    match step.mode.as_str() {
        STEP_MODE_ALL => satisfied.len() == slots.len() && !slots.is_empty(),
        _ => !satisfied.is_empty(),
    }
}

/// Build the detail view of a request: steps with live status plus
/// the full decision history.
fn request_detail(state: &ApprovalsState, request: &ApprovalRequest) -> ApprovalRequestDetail {
    let steps = steps_for_definition(state, request.definition_id);
    let decisions = decisions_for_request(state, request.id);
    let mut views: Vec<ApprovalStepView> = Vec::new();
    for (index, step) in steps.iter().enumerate() {
        let index = index as i32;
        let slots = approvers_for_step(state, request.id, index, step);
        let satisfied = satisfied_slots(state, request, step);
        let (_, unresolved) = {
            // Resolution is only meaningful for the current step;
            // earlier / later steps report their stored shape.
            let mut unres = false;
            if index == request.current_step_index {
                for slot in &slots {
                    if slot.kind == APPROVER_KIND_MANAGER
                        && resolve_manager(state, &request.requester_id).is_none()
                    {
                        unres = true;
                    }
                }
            }
            (Vec::<SlotMatch>::new(), unres)
        };
        let status = if index < request.current_step_index {
            "approved".to_string()
        } else if index > request.current_step_index {
            "pending".to_string()
        } else if request.status == STATUS_APPROVED
            || request.status == STATUS_REJECTED
            || request.status == STATUS_CHANGES_REQUESTED
            || request.status == STATUS_EXPIRED
            || request.status == STATUS_CANCELLED
        {
            request.status.clone()
        } else if decisions.iter().any(|decision| {
            decision.step_index == index
                && decision.round == request.round
                && decision.decision == DECISION_REJECT
        }) {
            STATUS_REJECTED.to_string()
        } else if decisions.iter().any(|decision| {
            decision.step_index == index
                && decision.round == request.round
                && decision.decision == DECISION_REQUEST_CHANGES
        }) {
            STATUS_CHANGES_REQUESTED.to_string()
        } else {
            STATUS_IN_PROGRESS.to_string()
        };
        views.push(ApprovalStepView {
            step: step.clone(),
            status,
            satisfied_slots: satisfied,
            resolution: if unresolved {
                "unresolved".to_string()
            } else {
                "resolved".to_string()
            },
        });
    }
    ApprovalRequestDetail {
        request: request.clone(),
        steps: views,
        decisions,
    }
}

/// Notify one user through the bus (`notification.requested`).
/// Delivery belongs to the notifications module; a missing consumer
/// only means nobody delivers, never an error here.
fn notify_user(state: &ApprovalsState, user_id: &str, title: &str, body: &str) {
    state.emit(
        NOTIFICATION_REQUESTED,
        json!({ "to_user_id": user_id, "title": title, "body": body }),
    );
}

/// Notify every approver slot of a step that can be addressed
/// directly (user slots by id; role and manager slots are named in
/// the body because they have no single address).
fn notify_step(state: &ApprovalsState, request: &ApprovalRequest, step: &ApprovalStep, title: &str) {
    let slots = approvers_for_step(state, request.id, request.current_step_index, step);
    let mut named: Vec<String> = Vec::new();
    for slot in &slots {
        match slot.kind.as_str() {
            APPROVER_KIND_USER => {
                notify_user(
                    state,
                    &slot.ref_value,
                    title,
                    &format!(
                        "Approval requested: {} (record {} {}).",
                        request.summary, request.record_type, request.record_id
                    ),
                );
            }
            APPROVER_KIND_ROLE => named.push(format!("role {}", slot.ref_value)),
            APPROVER_KIND_MANAGER => named.push("the requester's manager".to_string()),
            _ => {}
        }
    }
    if !named.is_empty() {
        notify_user(
            state,
            &request.requester_id,
            title,
            &format!(
                "Approval requested: {} — waiting on {}.",
                request.summary,
                named.join(", ")
            ),
        );
    }
}

/// The deadline for a new request under a definition.
fn deadline_for(definition: &ApprovalDefinition, settings: &ApprovalSettings, now: DateTime<Utc>) -> DateTime<Utc> {
    let hours = definition
        .timeout_hours
        .unwrap_or(settings.default_timeout_hours);
    now + Duration::hours(hours)
}

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

async fn list_definitions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let module_filter = params.filters.get("module").cloned();
        let record_type_filter = params.filters.get("record_type").cloned();
        let active_filter = params.filters.get("is_active").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ApprovalDefinition> = state
            .definitions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|definition| {
                if let Some(module) = &module_filter {
                    if definition.module != *module {
                        return false;
                    }
                }
                if let Some(record_type) = &record_type_filter {
                    if definition.record_type != *record_type {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let expected = match active.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if definition.is_active != expected {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if definition.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {} {}",
                        definition.name,
                        definition.module,
                        definition.record_type,
                        definition.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|definition| definition.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<CreateDefinitionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let module = require_text(&input.module, "module", MAX_MODULE_LEN)?;
        if !valid_record_key(&module, MAX_MODULE_LEN) {
            return Err(UnifiedError::bad_request(
                "module must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let record_type = require_text(&input.record_type, "record_type", MAX_RECORD_TYPE_LEN)?;
        if !valid_record_key(&record_type, MAX_RECORD_TYPE_LEN) {
            return Err(UnifiedError::bad_request(
                "record_type must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let timeout_hours = validate_timeout(input.timeout_hours, "timeout_hours")?;
        let escalation_extension_hours =
            validate_extension(input.escalation_extension_hours, "escalation_extension_hours")?;
        if !ESCALATION_ACTIONS.contains(&input.escalation_action.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown escalation action: {} (expected one of {})",
                input.escalation_action,
                ESCALATION_ACTIONS.join(", ")
            )));
        }
        if let Some(target) = &input.escalate_to {
            validate_approver(target)?;
        }
        let company_id = optional_text(&input.company_id, "company_id", MAX_COMPANY_ID_LEN)?;
        if input.steps.len() > settings.max_steps_per_definition {
            return Err(UnifiedError::bad_request(format!(
                "A definition holds at most {} steps",
                settings.max_steps_per_definition
            )));
        }
        for step in &input.steps {
            require_text(&step.name, "step name", MAX_NAME_LEN)?;
            if !STEP_MODES.contains(&step.mode.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown step mode: {} (expected one of {})",
                    step.mode,
                    STEP_MODES.join(", ")
                )));
            }
            validate_approvers(&step.approvers, settings.max_approvers_per_step)?;
        }
        if input.is_active
            && active_definition_for(&state, &module, &record_type).is_some()
        {
            return Err(UnifiedError::conflict(format!(
                "An active approval definition already exists for {module} / {record_type}"
            )));
        }
        let now = Utc::now();
        let definition = ApprovalDefinition {
            id: Uuid::new_v4(),
            module,
            record_type,
            name,
            description,
            is_active: input.is_active,
            timeout_hours,
            escalation_action: input.escalation_action,
            escalate_to: input.escalate_to,
            escalation_extension_hours,
            company_id,
            created_by: caller.actor,
            created_at: now,
            updated_at: now,
        };
        state.definitions.insert(definition.id, definition.clone());
        let mut position = 0;
        for step in input.steps {
            position += 1;
            let stored = ApprovalStep {
                id: Uuid::new_v4(),
                definition_id: definition.id,
                position,
                name: step.name.trim().to_string(),
                mode: step.mode,
                approvers: step.approvers,
                is_active: step.is_active,
                created_at: now,
            };
            state.steps.insert(stored.id, stored);
        }
        Ok(definition)
    })();
    match result {
        Ok(definition) => created(definition),
        Err(error) => err(error, debug),
    }
}

async fn get_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let definition = load_definition(&state, &path.into_inner())?;
        let steps = steps_for_definition(&state, definition.id);
        Ok(json!({ "definition": definition, "steps": steps }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDefinitionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let mut definition = load_definition(&state, &path.into_inner())?;
        if let Some(name) = &input.name {
            definition.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if input.description.is_some() {
            definition.description =
                optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(active) = input.is_active {
            if active
                && !definition.is_active
                && active_definition_for(&state, &definition.module, &definition.record_type)
                    .is_some()
            {
                return Err(UnifiedError::conflict(format!(
                    "An active approval definition already exists for {} / {}",
                    definition.module, definition.record_type
                )));
            }
            definition.is_active = active;
        }
        if input.timeout_hours.is_some() {
            definition.timeout_hours = validate_timeout(input.timeout_hours, "timeout_hours")?;
        }
        if let Some(action) = &input.escalation_action {
            if !ESCALATION_ACTIONS.contains(&action.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown escalation action: {action} (expected one of {})",
                    ESCALATION_ACTIONS.join(", ")
                )));
            }
            definition.escalation_action = action.clone();
        }
        if let Some(target) = &input.escalate_to {
            validate_approver(target)?;
            definition.escalate_to = Some(target.clone());
        }
        if input.escalation_extension_hours.is_some() {
            definition.escalation_extension_hours = validate_extension(
                input.escalation_extension_hours,
                "escalation_extension_hours",
            )?;
        }
        if input.company_id.is_some() {
            definition.company_id = optional_text(&input.company_id, "company_id", MAX_COMPANY_ID_LEN)?;
        }
        definition.updated_at = Utc::now();
        state.definitions.insert(definition.id, definition.clone());
        Ok(definition)
    })();
    match result {
        Ok(definition) => ok(definition),
        Err(error) => err(error, debug),
    }
}

async fn delete_definition(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let definition = load_definition(&state, &path.into_inner())?;
        let open = state.requests.iter().any(|entry| {
            entry.value().definition_id == definition.id
                && OPEN_STATUSES.contains(&entry.value().status.as_str())
        });
        if open {
            return Err(UnifiedError::conflict(
                "Cannot delete a definition with open approval requests; disable it instead",
            ));
        }
        let step_ids: Vec<Uuid> = state
            .steps
            .iter()
            .filter(|entry| entry.value().definition_id == definition.id)
            .map(|entry| *entry.key())
            .collect();
        for step_id in step_ids {
            state.steps.remove(&step_id);
        }
        state.definitions.remove(&definition.id);
        Ok(json!({ "deleted": true, "id": definition.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_definition_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateStepInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalStep> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        let definition = load_definition(&state, &path.into_inner())?;
        let name = require_text(&input.name, "step name", MAX_NAME_LEN)?;
        if !STEP_MODES.contains(&input.mode.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown step mode: {} (expected one of {})",
                input.mode,
                STEP_MODES.join(", ")
            )));
        }
        validate_approvers(&input.approvers, settings.max_approvers_per_step)?;
        let existing = steps_for_definition(&state, definition.id);
        if existing.len() >= settings.max_steps_per_definition {
            return Err(UnifiedError::bad_request(format!(
                "A definition holds at most {} steps",
                settings.max_steps_per_definition
            )));
        }
        let position = existing
            .iter()
            .map(|step| step.position)
            .max()
            .unwrap_or(0)
            + 1;
        let step = ApprovalStep {
            id: Uuid::new_v4(),
            definition_id: definition.id,
            position,
            name,
            mode: input.mode,
            approvers: input.approvers,
            is_active: input.is_active,
            created_at: Utc::now(),
        };
        state.steps.insert(step.id, step.clone());
        Ok(step)
    })();
    match result {
        Ok(step) => created(step),
        Err(error) => err(error, debug),
    }
}

async fn delete_definition_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let (definition_id, step_id) = path.into_inner();
        let definition = load_definition(&state, &definition_id)?;
        let step_uuid = parse_uuid(&step_id, "step")?;
        let step = state
            .steps
            .get(&step_uuid)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Approval step not found: {step_id}")))?;
        if step.definition_id != definition.id {
            return Err(UnifiedError::not_found(format!(
                "Approval step not found: {step_id}"
            )));
        }
        let in_use = state.requests.iter().any(|entry| {
            entry.value().definition_id == definition.id
                && OPEN_STATUSES.contains(&entry.value().status.as_str())
                && entry.value().current_step_index
                    >= steps_for_definition(&state, definition.id)
                        .iter()
                        .position(|candidate| candidate.id == step.id)
                        .map(|index| index as i32)
                        .unwrap_or(i32::MAX)
        });
        if in_use {
            return Err(UnifiedError::conflict(
                "Cannot remove a step that open requests have reached or passed; disable the definition instead",
            ));
        }
        state.steps.remove(&step.id);
        Ok(json!({ "deleted": true, "id": step.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Requests — creation
// ---------------------------------------------------------------------------

/// Shared request creation used by the HTTP endpoint and by the
/// `approval.requested` hook handler.
fn create_request_record(
    state: &ApprovalsState,
    definition: &ApprovalDefinition,
    module: String,
    record_type: String,
    record_id: String,
    requester_id: String,
    summary: String,
    amount_minor: Option<i64>,
    currency: Option<String>,
    company_id: Option<String>,
) -> ApprovalRequest {
    let settings = state.settings();
    let now = Utc::now();
    let steps = steps_for_definition(state, definition.id);
    let status = if steps.is_empty() {
        STATUS_REQUESTED.to_string()
    } else {
        STATUS_IN_PROGRESS.to_string()
    };
    let request = ApprovalRequest {
        id: Uuid::new_v4(),
        definition_id: definition.id,
        module,
        record_type,
        record_id,
        requester_id,
        summary,
        amount_minor,
        currency,
        status,
        current_step_index: 0,
        round: 1,
        escalation_count: 0,
        last_escalated_at: None,
        expires_at: deadline_for(definition, &settings, now),
        decided_at: None,
        decided_by: None,
        company_id,
        created_at: now,
        updated_at: now,
    };
    state.requests.insert(request.id, request.clone());
    state.emit(
        APPROVAL_CREATED,
        json!({
            "request_id": request.id.to_string(),
            "module": request.module,
            "record_type": request.record_type,
            "record_id": request.record_id,
            "requester_id": request.requester_id,
            "definition_id": request.definition_id.to_string(),
        }),
    );
    if let Some(step) = steps.first() {
        notify_step(state, &request, step, "Approval requested");
    }
    request
}

async fn list_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let module_filter = params.filters.get("module").cloned();
        let record_type_filter = params.filters.get("record_type").cloned();
        let record_id_filter = params.filters.get("record_id").cloned();
        let requester_filter = params.filters.get("requester_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let definition_filter = params.filters.get("definition_id").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if let Some(module) = &module_filter {
                    if request.module != *module {
                        return false;
                    }
                }
                if let Some(record_type) = &record_type_filter {
                    if request.record_type != *record_type {
                        return false;
                    }
                }
                if let Some(record_id) = &record_id_filter {
                    if request.record_id != *record_id {
                        return false;
                    }
                }
                if let Some(requester) = &requester_filter {
                    if request.requester_id != *requester {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if request.status != *status {
                        return false;
                    }
                }
                if let Some(definition_id) = &definition_filter {
                    if request.definition_id.to_string() != *definition_id {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if request.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {} {}", request.summary, request.record_id, request.requester_id)
                        .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|request| request.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<CreateRequestInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let module = require_text(&input.module, "module", MAX_MODULE_LEN)?;
        if !valid_record_key(&module, MAX_MODULE_LEN) {
            return Err(UnifiedError::bad_request(
                "module must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let record_type = require_text(&input.record_type, "record_type", MAX_RECORD_TYPE_LEN)?;
        if !valid_record_key(&record_type, MAX_RECORD_TYPE_LEN) {
            return Err(UnifiedError::bad_request(
                "record_type must be lowercase letters, digits, dots, underscores, or hyphens",
            ));
        }
        let record_id = require_text(&input.record_id, "record_id", MAX_RECORD_ID_LEN)?;
        let requester_id = match &input.requester_id {
            Some(raw) => require_text(raw, "requester_id", MAX_USER_ID_LEN)?,
            None => caller.actor.clone(),
        };
        let summary = require_text(&input.summary, "summary", MAX_SUMMARY_LEN)?;
        if let Some(amount) = input.amount_minor {
            if amount < 0 {
                return Err(UnifiedError::bad_request("amount_minor must not be negative"));
            }
        }
        let currency = optional_text(&input.currency, "currency", MAX_CURRENCY_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_COMPANY_ID_LEN)?;
        let definition = match &input.definition_id {
            Some(raw) => {
                let definition = load_definition(&state, raw)?;
                if definition.module != module || definition.record_type != record_type {
                    return Err(UnifiedError::bad_request(
                        "definition_id does not match the given module / record_type",
                    ));
                }
                if !definition.is_active {
                    return Err(UnifiedError::bad_request(
                        "The given approval definition is not active",
                    ));
                }
                definition
            }
            None => active_definition_for(&state, &module, &record_type).ok_or_else(|| {
                UnifiedError::bad_request(format!(
                    "No active approval definition exists for {module} / {record_type}"
                ))
            })?,
        };
        Ok(create_request_record(
            &state,
            &definition,
            module,
            record_type,
            record_id,
            requester_id,
            summary,
            input.amount_minor,
            currency,
            company_id,
        ))
    })();
    match result {
        Ok(request) => created(request),
        Err(error) => err(error, debug),
    }
}

async fn get_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let request = load_request(&state, &path.into_inner())?;
        Ok(request_detail(&state, &request))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn request_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let request = load_request(&state, &path.into_inner())?;
        let params = query.into_inner();
        let decisions = decisions_for_request(&state, request.id);
        let escalations = escalations_for_request(&state, request.id);
        Ok(json!({
            "request_id": request.id.to_string(),
            "decisions": paginate(decisions, &params).to_json(),
            "escalations": escalations,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Requests — decisions and lifecycle
// ---------------------------------------------------------------------------

/// Apply a terminal decision to a request: persist the new status,
/// stamp who decided, and answer the requesting module on the bus.
/// `approval.decided` is emitted exactly once per request.
fn finish_request(
    state: &ApprovalsState,
    request: &mut ApprovalRequest,
    status: &str,
    decision: &str,
    decided_by: &str,
) {
    let now = Utc::now();
    request.status = status.to_string();
    request.decided_at = Some(now);
    request.decided_by = Some(decided_by.to_string());
    request.updated_at = now;
    state.requests.insert(request.id, request.clone());
    state.emit(
        APPROVAL_DECIDED,
        json!({
            "request_id": request.id.to_string(),
            "decision": decision,
            "decided_by": decided_by,
            "module": request.module,
            "record_type": request.record_type,
            "record_id": request.record_id,
            "requester_id": request.requester_id,
        }),
    );
}

async fn decide_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_DECIDE)?;
        let input = body.into_inner();
        if !DECISIONS.contains(&input.decision.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown decision: {} (expected one of {})",
                input.decision,
                DECISIONS.join(", ")
            )));
        }
        let comment = optional_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
        if (input.decision == DECISION_REJECT || input.decision == DECISION_REQUEST_CHANGES)
            && comment.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "A comment is required when rejecting or requesting changes",
            ));
        }
        let settings = state.settings();
        if input.decision == DECISION_APPROVE
            && settings.require_comment_on_approve
            && comment.as_deref().unwrap_or("").is_empty()
        {
            return Err(UnifiedError::bad_request(
                "A comment is required when approving (see the module settings)",
            ));
        }
        let mut request = load_request(&state, &path.into_inner())?;
        if request.status != STATUS_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Only an in_progress request can be decided (current status: {})",
                request.status
            )));
        }
        if !settings.allow_self_approval && caller.actor == request.requester_id {
            return Err(UnifiedError::forbidden(
                "The requester cannot approve their own request",
            ));
        }
        let now = Utc::now();
        let Some(step) = current_step(&state, &request) else {
            return Err(UnifiedError::conflict(
                "The request has no active step to decide",
            ));
        };
        let slots = approvers_for_step(&state, request.id, request.current_step_index, &step);
        let (matched, unresolved) =
            matchable_slots(&state, &request, &slots, &caller.actor, &caller.role, now);
        let Some(found) = matched.first() else {
            if unresolved {
                return Err(UnifiedError::forbidden(
                    "The current step waits on the requester's manager, which could not be resolved; ask an administrator to reassign the step",
                ));
            }
            return Err(UnifiedError::forbidden(
                "The caller is not an approver of the current step",
            ));
        };
        // A slot that is already satisfied cannot be decided twice in
        // the same round: the decision would be meaningless.
        let slot_key = format!("{}:{}", found.slot.kind, found.slot.ref_value);
        let already_satisfied = satisfied_slots(&state, &request, &step).contains(&slot_key);
        if input.decision == DECISION_APPROVE && already_satisfied {
            return Err(UnifiedError::conflict(
                "This approver slot already approved the current step in this round",
            ));
        }
        let slot_ref = if found.slot.kind == APPROVER_KIND_MANAGER {
            resolve_manager(&state, &request.requester_id)
                .unwrap_or_else(|| found.slot.ref_value.clone())
        } else {
            found.slot.ref_value.clone()
        };
        let decision = ApprovalDecision {
            id: Uuid::new_v4(),
            request_id: request.id,
            step_index: request.current_step_index,
            round: request.round,
            approver_id: caller.actor.clone(),
            acting_for: found.acting_for.clone(),
            slot_kind: found.slot.kind.clone(),
            slot_ref,
            decision: input.decision.clone(),
            comment,
            created_at: now,
        };
        state.decisions.insert(decision.id, decision.clone());
        match input.decision.as_str() {
            DECISION_REJECT => {
                finish_request(
                    &state,
                    &mut request,
                    STATUS_REJECTED,
                    "rejected",
                    &caller.actor,
                );
            }
            DECISION_REQUEST_CHANGES => {
                finish_request(
                    &state,
                    &mut request,
                    STATUS_CHANGES_REQUESTED,
                    "changes_requested",
                    &caller.actor,
                );
            }
            _ => {
                if step_is_complete(&state, &request, &step) {
                    let steps = steps_for_definition(&state, request.definition_id);
                    if (request.current_step_index as usize) + 1 >= steps.len() {
                        finish_request(
                            &state,
                            &mut request,
                            STATUS_APPROVED,
                            "approved",
                            &caller.actor,
                        );
                    } else {
                        request.current_step_index += 1;
                        request.updated_at = now;
                        state.requests.insert(request.id, request.clone());
                        if let Some(next) = current_step(&state, &request) {
                            notify_step(&state, &request, &next, "Approval requested");
                        }
                    }
                } else {
                    request.updated_at = now;
                    state.requests.insert(request.id, request.clone());
                }
            }
        }
        let fresh = load_request(&state, &request.id.to_string())?;
        Ok(request_detail(&state, &fresh))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn resubmit_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let mut request = load_request(&state, &path.into_inner())?;
        if !RESUBMITTABLE_STATUSES.contains(&request.status.as_str()) {
            return Err(UnifiedError::conflict(format!(
                "Only a changes_requested or expired request can be resubmitted (current status: {})",
                request.status
            )));
        }
        let definition = load_definition(&state, &request.definition_id.to_string())?;
        let settings = state.settings();
        let now = Utc::now();
        request.status = STATUS_IN_PROGRESS.to_string();
        request.round += 1;
        request.escalation_count = 0;
        request.last_escalated_at = None;
        request.expires_at = deadline_for(&definition, &settings, now);
        request.decided_at = None;
        request.decided_by = None;
        request.updated_at = now;
        state.requests.insert(request.id, request.clone());
        state.emit(
            APPROVAL_RESUBMITTED,
            json!({ "request_id": request.id.to_string(), "round": request.round }),
        );
        if let Some(step) = current_step(&state, &request) {
            notify_step(&state, &request, &step, "Approval resubmitted");
        }
        Ok(request_detail(&state, &request))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn cancel_request(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let mut request = load_request(&state, &path.into_inner())?;
        if !OPEN_STATUSES.contains(&request.status.as_str()) {
            return Err(UnifiedError::conflict(format!(
                "Only an open request can be cancelled (current status: {})",
                request.status
            )));
        }
        request.status = STATUS_CANCELLED.to_string();
        request.decided_at = Some(Utc::now());
        request.decided_by = Some(caller.actor.clone());
        request.updated_at = Utc::now();
        state.requests.insert(request.id, request.clone());
        state.emit(
            APPROVAL_CANCELLED,
            json!({
                "request_id": request.id.to_string(),
                "cancelled_by": caller.actor,
            }),
        );
        Ok(request_detail(&state, &request))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reassign_request_step(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReassignInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalRequestDetail> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let settings = state.settings();
        validate_approvers(&input.approvers, settings.max_approvers_per_step)?;
        let request = load_request(&state, &path.into_inner())?;
        if request.status != STATUS_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Only an in_progress request can be reassigned (current status: {})",
                request.status
            )));
        }
        let steps = steps_for_definition(&state, request.definition_id);
        if input.step_index < 0 || (input.step_index as usize) >= steps.len() {
            return Err(UnifiedError::bad_request(format!(
                "step_index {} is out of range for this request",
                input.step_index
            )));
        }
        state
            .step_overrides
            .insert((request.id, input.step_index), input.approvers);
        let fresh = load_request(&state, &request.id.to_string())?;
        Ok(request_detail(&state, &fresh))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Expire overdue requests and escalate them. Expiry never silently
/// auto-approves: each overdue request is notified + escalated, and
/// only expires once its definition's rule gives up (action `none`,
/// or the escalation cap is reached).
async fn sweep_expiry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let settings = state.settings();
        let now = Utc::now();
        let mut overdue: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| request.status == STATUS_IN_PROGRESS && request.expires_at < now)
            .collect();
        overdue.sort_by_key(|request| request.expires_at);
        let mut escalated: Vec<String> = Vec::new();
        let mut expired: Vec<String> = Vec::new();
        for mut request in overdue {
            let definition = match state
                .definitions
                .get(&request.definition_id)
                .map(|entry| entry.value().clone())
            {
                Some(definition) => definition,
                None => continue,
            };
            let give_up = definition.escalation_action == ESCALATION_NONE
                || request.escalation_count >= settings.max_escalations;
            if give_up {
                request.status = STATUS_EXPIRED.to_string();
                request.updated_at = now;
                state.requests.insert(request.id, request.clone());
                state.escalations.insert(
                    Uuid::new_v4(),
                    ApprovalEscalation {
                        id: Uuid::new_v4(),
                        request_id: request.id,
                        step_index: request.current_step_index,
                        reason: "deadline reached and the escalation rule gave up".to_string(),
                        action: ESCALATION_NONE.to_string(),
                        created_at: now,
                    },
                );
                state.emit(
                    APPROVAL_EXPIRED,
                    json!({
                        "request_id": request.id.to_string(),
                        "step_index": request.current_step_index,
                    }),
                );
                notify_user(
                    &state,
                    &request.requester_id,
                    "Approval request expired",
                    &format!("Approval expired: {} (record {} {}).", request.summary, request.record_type, request.record_id),
                );
                expired.push(request.id.to_string());
                continue;
            }
            request.escalation_count += 1;
            request.last_escalated_at = Some(now);
            let extension = definition
                .escalation_extension_hours
                .unwrap_or(settings.default_escalation_extension_hours);
            request.expires_at = now + Duration::hours(extension);
            request.updated_at = now;
            state.requests.insert(request.id, request.clone());
            state.escalations.insert(
                Uuid::new_v4(),
                ApprovalEscalation {
                    id: Uuid::new_v4(),
                    request_id: request.id,
                    step_index: request.current_step_index,
                    reason: "deadline reached".to_string(),
                    action: definition.escalation_action.clone(),
                    created_at: now,
                },
            );
            state.emit(
                APPROVAL_ESCALATED,
                json!({
                    "request_id": request.id.to_string(),
                    "step_index": request.current_step_index,
                    "escalation_count": request.escalation_count,
                    "action": definition.escalation_action,
                }),
            );
            if let Some(target) = &definition.escalate_to {
                match target.kind.as_str() {
                    APPROVER_KIND_USER => {
                        notify_user(
                            &state,
                            &target.ref_value,
                            "Approval request escalated",
                            &format!(
                                "Escalated approval: {} (record {} {}), deadline extended by {}h.",
                                request.summary, request.record_type, request.record_id, extension
                            ),
                        );
                    }
                    APPROVER_KIND_MANAGER => {
                        if let Some(manager) = resolve_manager(&state, &request.requester_id) {
                            notify_user(
                                &state,
                                &manager,
                                "Approval request escalated",
                                &format!(
                                    "Escalated approval: {} (record {} {}), deadline extended by {}h.",
                                    request.summary, request.record_type, request.record_id, extension
                                ),
                            );
                        }
                    }
                    _ => {}
                }
            }
            if let Some(step) = current_step(&state, &request) {
                notify_step(&state, &request, &step, "Approval request escalated");
            }
            escalated.push(request.id.to_string());
        }
        Ok(json!({
            "swept_at": now.to_rfc3339(),
            "escalated": escalated,
            "expired": expired,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Dashboards
// ---------------------------------------------------------------------------

async fn my_pending(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let now = Utc::now();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if let Some(status) = &status_filter {
                    if request.status != *status {
                        return false;
                    }
                }
                caller_is_pending_approver(&state, request, &caller.actor, &caller.role, now)
            })
            .collect();
        items.sort_by_key(|request| request.expires_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn my_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                if request.requester_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if request.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|request| request.created_at);
        items.reverse();
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn overdue_requests(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let now = Utc::now();
        let mut items: Vec<ApprovalRequest> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| request.status == STATUS_IN_PROGRESS && request.expires_at < now)
            .collect();
        items.sort_by_key(|request| request.expires_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn by_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let module = params
            .filters
            .get("module")
            .cloned()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter module is required"))?;
        let record_type = params
            .filters
            .get("record_type")
            .cloned()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter record_type is required"))?;
        let record_id = params
            .filters
            .get("record_id")
            .cloned()
            .ok_or_else(|| UnifiedError::bad_request("Query parameter record_id is required"))?;
        let mut items: Vec<ApprovalRequestDetail> = state
            .requests
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|request| {
                request.module == module
                    && request.record_type == record_type
                    && request.record_id == record_id
            })
            .map(|request| request_detail(&state, &request))
            .collect();
        items.sort_by_key(|detail| detail.request.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Delegations
// ---------------------------------------------------------------------------

async fn list_delegations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        let params = query.into_inner();
        let delegator_filter = params.filters.get("delegator_id").cloned();
        let delegate_filter = params.filters.get("delegate_id").cloned();
        let active_filter = params.filters.get("is_active").cloned();
        let mut items: Vec<ApprovalDelegation> = state
            .delegations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|delegation| {
                if let Some(delegator) = &delegator_filter {
                    if delegation.delegator_id != *delegator {
                        return false;
                    }
                }
                if let Some(delegate) = &delegate_filter {
                    if delegation.delegate_id != *delegate {
                        return false;
                    }
                }
                if let Some(active) = &active_filter {
                    let expected = match active.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if delegation.is_active != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|delegation| delegation.starts_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_delegation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<CreateDelegationInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalDelegation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let delegator_id = require_text(&input.delegator_id, "delegator_id", MAX_USER_ID_LEN)?;
        let delegate_id = require_text(&input.delegate_id, "delegate_id", MAX_USER_ID_LEN)?;
        if delegator_id == delegate_id {
            return Err(UnifiedError::bad_request(
                "An approver cannot delegate to themselves",
            ));
        }
        if input.ends_at <= input.starts_at {
            return Err(UnifiedError::bad_request(
                "ends_at must be after starts_at",
            ));
        }
        if input.ends_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "ends_at must be in the future",
            ));
        }
        let delegation = ApprovalDelegation {
            id: Uuid::new_v4(),
            delegator_id,
            delegate_id,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            is_active: true,
            created_by: caller.actor,
            created_at: Utc::now(),
        };
        state.delegations.insert(delegation.id, delegation.clone());
        Ok(delegation)
    })();
    match result {
        Ok(delegation) => created(delegation),
        Err(error) => err(error, debug),
    }
}

async fn delete_delegation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let id = parse_uuid(&path.into_inner(), "delegation")?;
        let delegation = state
            .delegations
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Approval delegation not found: {id}")))?;
        // Revoking is a soft delete: the row stays so past decisions
        // that recorded `acting_for` keep their meaning.
        let mut revoked = delegation.clone();
        revoked.is_active = false;
        state.delegations.insert(revoked.id, revoked);
        Ok(json!({ "revoked": true, "id": delegation.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ApprovalsState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApprovalSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVALS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(hours) = input.default_timeout_hours {
            if hours < 1 || hours > MAX_TIMEOUT_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "default_timeout_hours must be between 1 and {MAX_TIMEOUT_HOURS}"
                )));
            }
            settings.default_timeout_hours = hours;
        }
        if let Some(hours) = input.default_escalation_extension_hours {
            if hours < 1 || hours > MAX_ESCALATION_EXTENSION_HOURS {
                return Err(UnifiedError::bad_request(format!(
                    "default_escalation_extension_hours must be between 1 and {MAX_ESCALATION_EXTENSION_HOURS}"
                )));
            }
            settings.default_escalation_extension_hours = hours;
        }
        if let Some(max) = input.max_escalations {
            if max < 1 || max > 100 {
                return Err(UnifiedError::bad_request(
                    "max_escalations must be between 1 and 100",
                ));
            }
            settings.max_escalations = max;
        }
        if let Some(max) = input.max_steps_per_definition {
            if max < 1 || max > 100 {
                return Err(UnifiedError::bad_request(
                    "max_steps_per_definition must be between 1 and 100",
                ));
            }
            settings.max_steps_per_definition = max;
        }
        if let Some(max) = input.max_approvers_per_step {
            if max < 1 || max > 50 {
                return Err(UnifiedError::bad_request(
                    "max_approvers_per_step must be between 1 and 50",
                ));
            }
            settings.max_approvers_per_step = max;
        }
        if let Some(allow) = input.allow_self_approval {
            settings.allow_self_approval = allow;
        }
        if let Some(require) = input.require_comment_on_approve {
            settings.require_comment_on_approve = require;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

/// `approval.requested` — another module asks for approval of one of
/// its records. Creates the request under the active definition for
/// (`module`, `record_type`) and answers with the `request_id`, or
/// answers `accepted: false` when no definition applies.
fn on_approval_requested(state: Arc<ApprovalsState>, ctx: &mut HookContext) {
    let module = ctx
        .data
        .get("module")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let record_type = ctx
        .data
        .get("record_type")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let record_id = ctx
        .data
        .get("record_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let requester_id = ctx
        .data
        .get("requester_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let summary = ctx
        .data
        .get("summary")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let amount_minor = ctx
        .data
        .get("amount_minor")
        .and_then(|value| value.as_i64());
    let currency = ctx
        .data
        .get("currency")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let company_id = ctx
        .data
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    if module.is_empty() || record_type.is_empty() || record_id.is_empty() || requester_id.is_empty() || summary.is_empty() {
        ctx.add_result(json!({
            "accepted": false,
            "error": "approval.requested requires module, record_type, record_id, requester_id, and summary",
        }));
        return;
    }
    if !valid_record_key(&module, MAX_MODULE_LEN) || !valid_record_key(&record_type, MAX_RECORD_TYPE_LEN) {
        ctx.add_result(json!({
            "accepted": false,
            "error": "module and record_type must be lowercase keys (letters, digits, dots, underscores, hyphens)",
        }));
        return;
    }
    if summary.len() > MAX_SUMMARY_LEN || record_id.len() > MAX_RECORD_ID_LEN {
        ctx.add_result(json!({
            "accepted": false,
            "error": "summary or record_id is too long",
        }));
        return;
    }
    if let Some(amount) = amount_minor {
        if amount < 0 {
            ctx.add_result(json!({
                "accepted": false,
                "error": "amount_minor must not be negative",
            }));
            return;
        }
    }
    let Some(definition) = active_definition_for(&state, &module, &record_type) else {
        ctx.add_result(json!({
            "accepted": false,
            "error": format!("No active approval definition exists for {module} / {record_type}"),
        }));
        return;
    };
    let request = create_request_record(
        &state,
        &definition,
        module,
        record_type,
        record_id,
        requester_id,
        summary,
        amount_minor,
        currency,
        company_id,
    );
    let answer = json!({
        "accepted": true,
        "request_id": request.id.to_string(),
        "status": request.status,
    });
    ctx.set("result", answer.clone());
    ctx.add_result(answer);
}

/// `approval.status.query` — a module asks for the status of a
/// request, by `request_id` or by (`module`, `record_type`,
/// `record_id`). Answers with the latest matching request, or
/// `{"found": false}`.
fn on_approval_status_query(state: Arc<ApprovalsState>, ctx: &mut HookContext) {
    let found: Option<ApprovalRequest> = match ctx
        .data
        .get("request_id")
        .and_then(|value| value.as_str())
    {
        Some(raw) => Uuid::parse_str(raw)
            .ok()
            .and_then(|id| state.requests.get(&id).map(|entry| entry.value().clone())),
        None => {
            let module = ctx
                .data
                .get("module")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let record_type = ctx
                .data
                .get("record_type")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            let record_id = ctx
                .data
                .get("record_id")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if module.is_empty() || record_type.is_empty() || record_id.is_empty() {
                None
            } else {
                let mut matches: Vec<ApprovalRequest> = state
                    .requests
                    .iter()
                    .map(|entry| entry.value().clone())
                    .filter(|request| {
                        request.module == module
                            && request.record_type == record_type
                            && request.record_id == record_id
                    })
                    .collect();
                matches.sort_by_key(|request| request.created_at);
                matches.pop()
            }
        }
    };
    let answer = match found {
        Some(request) => json!({
            "found": true,
            "request_id": request.id.to_string(),
            "status": request.status,
            "current_step_index": request.current_step_index,
            "round": request.round,
            "expires_at": request.expires_at.to_rfc3339(),
            "module": request.module,
            "record_type": request.record_type,
            "record_id": request.record_id,
        }),
        None => json!({ "found": false }),
    };
    ctx.set("result", answer.clone());
    ctx.add_result(answer);
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ApprovalsModule {
    state: ApprovalsState,
}

impl ApprovalsModule {
    pub fn new() -> Self {
        Self {
            state: ApprovalsState::default(),
        }
    }
}

impl Default for ApprovalsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ApprovalsModule {
    fn id(&self) -> &'static str {
        "approvals"
    }

    fn name(&self) -> &'static str {
        "Approvals"
    }

    fn version(&self) -> &'static str {
        "1.0.0"
    }

    fn description(&self) -> &'static str {
        "Data-driven approval workflows for any record in any module: sequential and parallel steps, user / role / manager approvers, delegation, append-only decisions, expiry with escalation, and a hook contract (approval.requested / approval.decided / approval.status.query)."
    }

    fn requires(&self) -> &[&'static str] {
        &["roles", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["roles", "notifications"]
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
        let state = Arc::new(self.state.clone());
        bus.register(
            APPROVAL_REQUESTED,
            "approvals",
            Arc::new(move |ctx: &mut HookContext| {
                on_approval_requested(Arc::clone(&state), ctx)
            }),
        );
        let state = Arc::new(self.state.clone());
        bus.register(
            APPROVAL_STATUS_QUERY,
            "approvals",
            Arc::new(move |ctx: &mut HookContext| {
                on_approval_status_query(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/approvals")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("/definitions", web::get().to(list_definitions))
                .route("/definitions", web::post().to(create_definition))
                .route("/definitions/{id}", web::get().to(get_definition))
                .route("/definitions/{id}", web::patch().to(update_definition))
                .route("/definitions/{id}", web::delete().to(delete_definition))
                .route("/definitions/{id}/steps", web::post().to(add_definition_step))
                .route(
                    "/definitions/{id}/steps/{step_id}",
                    web::delete().to(delete_definition_step),
                )
                .route("/requests", web::get().to(list_requests))
                .route("/requests", web::post().to(create_request))
                .route("/requests/sweep-expiry", web::post().to(sweep_expiry))
                .route("/requests/{id}", web::get().to(get_request))
                .route("/requests/{id}/decide", web::post().to(decide_request))
                .route("/requests/{id}/resubmit", web::post().to(resubmit_request))
                .route("/requests/{id}/cancel", web::post().to(cancel_request))
                .route("/requests/{id}/reassign", web::post().to(reassign_request_step))
                .route("/requests/{id}/history", web::get().to(request_history))
                .route("/my-pending", web::get().to(my_pending))
                .route("/my-requests", web::get().to(my_requests))
                .route("/overdue", web::get().to(overdue_requests))
                .route("/by-record", web::get().to(by_record))
                .route("/delegations", web::get().to(list_delegations))
                .route("/delegations", web::post().to(create_delegation))
                .route("/delegations/{id}", web::delete().to(delete_delegation))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::patch().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/approvals/definitions", owner: "approvals", description: "List approval definitions (paginated, filter by module, record_type, is_active, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/definitions", owner: "approvals", description: "Create an approval definition with optional steps (one active definition per module / record_type)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/definitions/{id}", owner: "approvals", description: "Get one definition with its steps." },
            RouteInfo { method: "PATCH", path: "/api/v1/approvals/definitions/{id}", owner: "approvals", description: "Update a definition (name, activity, timeout, escalation rule)." },
            RouteInfo { method: "DELETE", path: "/api/v1/approvals/definitions/{id}", owner: "approvals", description: "Delete a definition with its steps (refused while open requests exist)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/definitions/{id}/steps", owner: "approvals", description: "Append a step (mode all/any, user / role / manager approvers) to a definition." },
            RouteInfo { method: "DELETE", path: "/api/v1/approvals/definitions/{id}/steps/{step_id}", owner: "approvals", description: "Remove a step (refused when open requests have reached it)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/requests", owner: "approvals", description: "List approval requests (paginated, filter by module, record_type, record_id, requester_id, status, definition_id, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests", owner: "approvals", description: "Create an approval request for a record under the active definition." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/requests/{id}", owner: "approvals", description: "Get one request with its steps (live status) and decision history." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/decide", owner: "approvals", description: "Decide the current step: approve / reject / request_changes (comment required for reject / request_changes)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/resubmit", owner: "approvals", description: "Resubmit a changes_requested or expired request (new decision round)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/cancel", owner: "approvals", description: "Cancel an open request." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/{id}/reassign", owner: "approvals", description: "Replace the approvers of one pending step (admin; escalation handover)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/requests/{id}/history", owner: "approvals", description: "Append-only decision history plus escalations for one request (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/requests/sweep-expiry", owner: "approvals", description: "Expire overdue requests and escalate them (never auto-approves; gives up after the escalation cap)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/my-pending", owner: "approvals", description: "Requests waiting on the caller as approver or active delegate (paginated, soonest deadline first)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/my-requests", owner: "approvals", description: "Requests the caller asked for (paginated, filter by status)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/overdue", owner: "approvals", description: "In-progress requests past their deadline (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/by-record", owner: "approvals", description: "Requests and decisions for one record (?module=&record_type=&record_id=, paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/delegations", owner: "approvals", description: "List delegations (paginated, filter by delegator_id, delegate_id, is_active)." },
            RouteInfo { method: "POST", path: "/api/v1/approvals/delegations", owner: "approvals", description: "Create a temporary delegation (delegate decides as acting-for the original approver)." },
            RouteInfo { method: "DELETE", path: "/api/v1/approvals/delegations/{id}", owner: "approvals", description: "Revoke a delegation (soft revoke; past acting-for decisions keep their meaning)." },
            RouteInfo { method: "GET", path: "/api/v1/approvals/settings", owner: "approvals", description: "Read the admin-editable approval defaults (timeouts, escalation cap, limits, self-approval)." },
            RouteInfo { method: "PATCH", path: "/api/v1/approvals/settings", owner: "approvals", description: "Update the admin-editable approval defaults." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ApprovalsModule::new()));
}
