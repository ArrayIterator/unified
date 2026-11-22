// Workflow Automation / Autopilot module — event-driven workflows
// ("if this happens, do that"), scheduled workflows, webhook
// triggers, and manual runs, with condition groups, ordered
// actions, per-run logs, dry-run testing, retries, and loop /
// recursion guards.
//
// Routes live under `/api/v1/workflows`:
//
//   Workflows:
//     GET/POST / — list workflows / create a workflow.
//     GET/PATCH/DELETE /{id}.
//     POST /{id}/enable, /{id}/disable — activate / deactivate.
//     POST /{id}/duplicate — copy (the copy starts disabled).
//     POST /{id}/run — manual run ({payload, dry_run}).
//     POST /{id}/test — dry-run evaluation: conditions + planned
//       actions, no side effects, no run row.
//   Runs:
//     GET  /{id}/runs — paginated, filter by status, trigger_kind.
//     GET  /runs/{run_id} — one run with its steps.
//     POST /runs/{run_id}/retry — re-run a failed run with a
//       caller-supplied payload (see the honesty note: the original
//       payload is not retained, only a redacted summary).
//   Triggers:
//     GET  /triggers — active bus-event subscriptions
//       (event name -> workflow ids).
//     POST /hooks/{token} — webhook trigger. The token is the
//       secret (see the public-access note below); the body is the
//       trigger payload.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory workflow, run, and run-step management; trigger
// validation (bus-event names, 5-field cron + timezone, webhook
// tokens); condition groups nested up to 3 levels with field
// validation at save time; ordered action execution with template
// substitution from the trigger payload; per-run logs with
// per-step success / failure / skipped / deferred status,
// attempts, and duration; dry-run / test mode that evaluates
// conditions and lists planned actions without side effects;
// retries (max 3 attempts, exponential backoff) for outbound
// webhook steps only — emit-based actions are attempted once,
// because re-emitting them could duplicate a notification, email,
// task, or ticket; loop / recursion guards (max workflow-call
// depth 3, max 60 runs per workflow per minute with excess runs
// dropped and logged, self-trigger rejection at save time and at
// runtime); SSRF-safe outbound webhooks (per-workflow host
// allowlist, DNS resolution checked against private / reserved
// ranges, 10s timeout, redirects disabled).
//
// What is NOT built here:
// - No real delivery of anything: notification / email / task /
//   ticket actions only emit hook-bus events and the owning module
//   does the work. `notification.requested` is consumed by the
//   notifications module; `mail.send_requested` by the mail
//   module; `ticket.create_requested` by the tickets module
//   (which really creates the ticket and answers with the ticket
//   or `{"error": ...}`). `task.create_requested`,
//   `{tasks|tickets}.update_requested`,
//   `{tasks|tickets}.assign_requested`, and
//   `{tasks|tickets}.tag_requested` have NO consumer yet: the
//   tasks module does not listen to the bus at all, and the
//   tickets module only listens to `ticket.create_requested`.
//   When no handler is registered for an action's event the step
//   is recorded as `skipped` (not failed) with the reason in the
//   step detail, so a workflow never pretends an undelivered
//   action happened. The follow-up is for the tasks / tickets
//   modules to add `*_requested` listeners mirroring
//   `tickets.rs` `on_ticket_create_requested`.
// - The `update_record` action never writes to a table: it only
//   emits `{module}.update_requested` for an allowlisted
//   (module, field) pair. Arbitrary table writes are refused.
// - Scheduled triggers queue through `scheduler.enqueue` with
//   kind `workflows.run`, exactly like the events module queues
//   reminders — and `workflows.run` is NOT in the scheduler's
//   `KNOWN_KINDS` list yet, so today the scheduler answers
//   `{"queued": false, "error": "Unknown job kind: ..."}` and the
//   workflow records `scheduler_status: "declined"` instead of
//   pretending it was scheduled. The follow-up is to add
//   `"workflows.run"` to `KNOWN_KINDS` in `scheduler.rs` and arm
//   the dispatch so the job emits `workflows.run_requested` with
//   `{"workflow_id", "trigger": "schedule"}` (see the integrator
//   snippet in the module report). Until then a scheduled
//   workflow is stored and stays usable through manual runs.
// - The trigger payload is never stored in full: only a redacted
//   summary (top-level keys with value types, strings truncated
//   to 128 characters, secret-named keys dropped). That is why a
//   retry needs a caller-supplied payload.
// - DNS-based SSRF checks resolve the host once before the call,
//   but the HTTP client resolves it again when connecting, so a
//   DNS-rebinding (TOCTOU) attack between the check and the
//   connect is not fully closed; the allowlist is the primary
//   defence and the check is the second layer.
//
// Public-access gate: the webhook trigger endpoint
// `POST /api/v1/workflows/hooks/{token}` carries no `X-User-Id` /
// `X-Role` headers, so it has no capability check — the token in
// the URL path IS the credential (64 hex characters, stored only
// as a SHA-256 hash, compared in constant time). Like the events
// module's public routes it relies on the API Manager's
// `ApiAccessRule` gate: an administrator must create the matching
// rule for `POST /api/v1/workflows/hooks/*` or the API Manager
// denies the call (default deny). Rotate a leaked token with
// `{"rotate_webhook_token": true}` on PATCH /{id}; the old token
// stops working immediately. Every other route requires
// `workflows.view` (reads) or `workflows.manage` (writes).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).
//
// Events emitted on the hook bus:
//   - `workflows.created`, `workflows.updated`, `workflows.deleted`
//   - `workflows.run.started`, `workflows.run.finished`,
//     `workflows.run.failed`
//   - `notification.requested` (send_notification action) with the
//     notifications module's payload shape.
//   - `mail.send_requested` (send_email action) with the mail
//     module's payload shape (`to`, `subject`, `body_text` /
//     `body_html` / `template_id`, `variables`).
//   - `task.create_requested` (create_task action; no consumer
//     yet — see the honesty note).
//   - `ticket.create_requested` (create_ticket action; consumed
//     by the tickets module).
//   - `{tasks|tickets}.update_requested`,
//     `{tasks|tickets}.assign_requested`,
//     `{tasks|tickets}.tag_requested` (update_record / assign_user
//     / add_tag actions; no consumer yet).
//   - `scheduler.enqueue` with `{"kind": "workflows.run",
//     "payload": {"workflow_id", "trigger": "schedule"}}` when a
//     workflow with a schedule trigger is created or enabled.
// Events this module listens to:
//   - Nothing by default. Each workflow with an `event` trigger
//     subscribes its own handler for that bus event name at
//     runtime (on create / enable / trigger change) and
//     unsubscribes on disable / delete / trigger change. A
//     handler is registered per workflow, so several workflows
//     can listen to the same event independently. Subscribing is
//     how a trigger "listens": there is no static list of event
//     names in this file — any hook-bus event name is a valid
//     trigger, for example `invoice.overdue`, `ticket.created`,
//     or `order.paid`.
//   - `workflows.run_requested` — the event the scheduler emits
//     when a `workflows.run` job fires, following its naming rule
//     (`backup.run` -> `backup.run_requested`); runs the named
//     scheduled workflow.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_EVENT_NAME_LEN: usize = 128;
const MAX_TIMEZONE_LEN: usize = 64;
const MAX_CRON_LEN: usize = 64;
const MAX_HOST_LEN: usize = 253;
const MAX_URL_LEN: usize = 2048;
const MAX_FIELD_PATH_LEN: usize = 256;
const MAX_TAG_LEN: usize = 64;
const MAX_USER_ID_LEN: usize = 128;
const MAX_TITLE_LEN: usize = 500;
const MAX_BODY_LEN: usize = 20_000;
const MAX_EMAIL_LEN: usize = 254;
/// Maximum number of actions on one workflow.
const MAX_ACTIONS: usize = 50;
/// Maximum nesting depth of condition groups (a group inside a
/// group inside a group is the deepest allowed shape).
const MAX_CONDITION_DEPTH: usize = 3;
/// Maximum number of leaf conditions inside one group.
const MAX_CONDITIONS_PER_GROUP: usize = 50;
/// Maximum field-path segments (`payload.a.b.c`).
const MAX_FIELD_SEGMENTS: usize = 8;
/// Maximum values in an `in` operator list.
const MAX_IN_VALUES: usize = 100;
/// Maximum webhook hosts on one workflow's allowlist.
const MAX_ALLOWED_HOSTS: usize = 20;
/// Maximum custom headers on one webhook action.
const MAX_WEBHOOK_HEADERS: usize = 20;
/// Maximum serialized bytes of a webhook action body.
const MAX_WEBHOOK_BODY_BYTES: usize = 65_536;
/// Maximum serialized bytes of one condition value.
const MAX_CONDITION_VALUE_BYTES: usize = 4096;
/// Loop / recursion guards.
const MAX_RUNS_PER_MINUTE: usize = 60;
const MAX_WORKFLOW_CALL_DEPTH: u8 = 3;
/// Retries for outbound webhook steps: 3 attempts total, with
/// exponential backoff (1s, then 2s, then 4s between attempts).
const MAX_WEBHOOK_ATTEMPTS: u32 = 3;
const WEBHOOK_BACKOFF_SECS: [u64; 2] = [1, 2];
/// Outbound webhook timeout in seconds.
const WEBHOOK_TIMEOUT_SECS: u64 = 10;
/// Rate-limit window in seconds.
const RATE_WINDOW_SECS: i64 = 60;
/// Payload summary: strings longer than this are truncated.
const SUMMARY_STRING_LEN: usize = 128;

/// Capability names checked through the `role.check` hook.
const PERM_WORKFLOWS_VIEW: &str = "workflows.view";
const PERM_WORKFLOWS_MANAGE: &str = "workflows.manage";

/// Hook event names.
const SCHEDULER_ENQUEUE: &str = "scheduler.enqueue";
const JOB_KIND_WORKFLOWS_RUN: &str = "workflows.run";
const WORKFLOWS_RUN_REQUESTED: &str = "workflows.run_requested";
const NOTIFICATION_REQUESTED: &str = "notification.requested";
const MAIL_SEND_REQUESTED: &str = "mail.send_requested";
const TASK_CREATE_REQUESTED: &str = "task.create_requested";
const TICKET_CREATE_REQUESTED: &str = "ticket.create_requested";

/// Field-name segments that mark a value as a secret: such values
/// are never stored in a run's payload summary, never echoed in a
/// condition evaluation, and never written to a step detail.
const SECRET_SEGMENTS: [&str; 10] = [
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "auth",
    "private_key",
    "credit_card",
];

/// (module, field) pairs the `update_record` action may write.
/// Anything else is rejected at save time — never an arbitrary
/// table write. The owning module must consume the
/// `{module}.update_requested` event; today no module does (see
/// the honesty note), so the step records `skipped` until one
/// does.
const UPDATE_ALLOWLIST: [(&str, &[&str]); 3] = [
    ("tasks", &["status", "priority", "assignee_id"]),
    ("tickets", &["status", "priority", "assignee_id"]),
    ("projects", &["status"]),
];

/// Modules the `assign_user` / `add_tag` actions may target.
const TARGET_MODULES: [&str; 2] = ["tasks", "tickets"];

/// Accepted priority values for task / ticket actions.
const PRIORITIES: [&str; 6] = ["low", "medium", "normal", "high", "urgent", "critical"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// What starts a workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Event,
    Schedule,
    Webhook,
    Manual,
}

impl TriggerKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Event => "event",
            Self::Schedule => "schedule",
            Self::Webhook => "webhook",
            Self::Manual => "manual",
        }
    }
}

/// One workflow's trigger. Only the fields for the chosen kind are
/// used: `event` for `event`, `cron` + `timezone` for `schedule`,
/// nothing stored for `webhook` (the token lives hashed on the
/// workflow) or `manual`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTrigger {
    pub kind: TriggerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cron: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
}

/// One leaf condition: compare one field of the trigger payload.
/// `field` must start with `payload.` (for example
/// `payload.order.total` or `payload.ticket.priority`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionLeaf {
    pub field: String,
    pub operator: String,
    pub value: Value,
}

/// A condition group: its children (leaves or nested groups) are
/// combined with `and` / `or`. Nesting is at most 3 levels deep.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConditionGroup {
    pub operator: String,
    pub conditions: Vec<ConditionNode>,
}

/// One node of a condition tree: either a nested group or a leaf.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ConditionNode {
    Group(ConditionGroup),
    Leaf(ConditionLeaf),
}

/// One action of a workflow. `config` is validated per kind at
/// save time; see `validate_action`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowAction {
    pub kind: String,
    #[serde(default)]
    pub config: Value,
}

/// One workflow definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workflow {
    pub id: Uuid,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub trigger: WorkflowTrigger,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conditions: Option<ConditionGroup>,
    #[serde(default)]
    pub actions: Vec<WorkflowAction>,
    pub is_active: bool,
    pub owner_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    /// SHA-256 hex of the webhook token; the token itself is
    /// returned once at creation / rotation and never stored or
    /// serialized. `None` unless the trigger kind is `webhook`.
    #[serde(skip_serializing, default)]
    pub webhook_token_hash: Option<String>,
    /// Host allowlist for the `call_webhook` action: exact hosts
    /// (`hooks.example.com`) or wildcards (`*.example.com`). An
    /// empty list means no outbound webhook call is allowed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub webhook_allowed_hosts: Vec<String>,
    /// Run counters (all runs / finished / failed / dropped).
    pub run_count: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub dropped_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_run_status: Option<String>,
    /// Outcome of the last `scheduler.enqueue` attempt for a
    /// schedule trigger: `not_scheduled`, `queued`, or `declined`.
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of a run: `running` -> `finished` / `failed`;
/// `dropped` when the per-minute rate limit was hit (the run never
/// started). A run whose conditions did not match still finishes
/// (`finished` with `condition_matched: false`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Running,
    Finished,
    Failed,
    Dropped,
}

impl RunStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Dropped => "dropped",
        }
    }
}

/// One execution of a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub workflow_id: Uuid,
    /// `event`, `schedule`, `webhook`, or `manual`.
    pub trigger_kind: String,
    /// Event name / cron expression / `manual`, for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_name: Option<String>,
    pub status: RunStatus,
    /// Redacted summary of the trigger payload — never secrets,
    /// never the full payload (see `summarize_payload`).
    #[serde(default)]
    pub payload_summary: Value,
    pub dry_run: bool,
    /// Workflow-call depth of this run (0 for a direct run).
    pub depth: u8,
    pub condition_matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skipped_reason: Option<String>,
    /// Run this retry was started from, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retried_from_run_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One executed step of a run. `status` is `pending` (recorded,
/// then updated), `success`, `failed`, `skipped` (conditions not
/// met for a later step, dry-run plan, or no consumer for the
/// action's event), or `deferred` (a webhook step in a run that
/// started on the sync bus path without an async runtime).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRunStep {
    pub id: Uuid,
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub step_index: usize,
    pub action_kind: String,
    pub status: String,
    /// Attempts made (1 for emit-based actions, up to 3 for
    /// outbound webhooks).
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default)]
    pub detail: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One evaluated leaf condition, as reported by a dry run / test.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LeafEvaluation {
    pub field: String,
    pub operator: String,
    pub value: Value,
    /// The payload value found at the field path (`null` when the
    /// field is missing; `[redacted]` for secret-named fields).
    pub actual: Value,
    pub matched: bool,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateWorkflowRequest {
    pub name: String,
    pub description: Option<String>,
    pub trigger: WorkflowTrigger,
    #[serde(default)]
    pub conditions: Option<ConditionGroup>,
    #[serde(default)]
    pub actions: Vec<WorkflowAction>,
    #[serde(default = "default_active")]
    pub is_active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    #[serde(default)]
    pub webhook_allowed_hosts: Vec<String>,
}

fn default_active() -> bool {
    true
}

#[derive(Debug, Deserialize)]
pub struct UpdateWorkflowRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub trigger: Option<WorkflowTrigger>,
    pub conditions: Option<Option<ConditionGroup>>,
    pub actions: Option<Vec<WorkflowAction>>,
    pub is_active: Option<bool>,
    pub company_id: Option<Option<String>>,
    pub webhook_allowed_hosts: Option<Vec<String>>,
    /// When true and the trigger kind is `webhook`, a fresh token
    /// is generated (returned once in the response) and the old
    /// token stops working immediately.
    #[serde(default)]
    pub rotate_webhook_token: bool,
}

#[derive(Debug, Deserialize)]
pub struct RunWorkflowRequest {
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Deserialize)]
pub struct TestWorkflowRequest {
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct RetryRunRequest {
    #[serde(default)]
    pub payload: Value,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for workflows. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000059_workflows`; until that migration is executed,
/// workflows do not survive a restart.
#[derive(Clone)]
pub struct WorkflowsState {
    pub workflows: Arc<DashMap<Uuid, Workflow>>,
    pub runs: Arc<DashMap<Uuid, WorkflowRun>>,
    pub steps: Arc<DashMap<Uuid, WorkflowRunStep>>,
    /// SHA-256 hex of a webhook token -> workflow id.
    pub webhook_tokens: Arc<DashMap<String, Uuid>>,
    /// Workflow id -> (bus event name, handler id) pairs for its
    /// event trigger subscriptions.
    pub subscriptions: Arc<DashMap<Uuid, Vec<(String, u64)>>>,
    /// Workflow id -> start times of recent runs, for the
    /// per-minute rate limit (pruned to the last 60 seconds).
    pub run_starts: Arc<DashMap<Uuid, Vec<DateTime<Utc>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for WorkflowsState {
    fn default() -> Self {
        Self {
            workflows: Arc::new(DashMap::new()),
            runs: Arc::new(DashMap::new()),
            steps: Arc::new(DashMap::new()),
            webhook_tokens: Arc::new(DashMap::new()),
            subscriptions: Arc::new(DashMap::new()),
            run_starts: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl WorkflowsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
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
/// (same fail-closed shape as the events module's capability check).
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

fn load_workflow(state: &WorkflowsState, raw_id: &str) -> UnifiedResult<Workflow> {
    let id = parse_uuid(raw_id, "workflow")?;
    state
        .workflows
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Workflow not found: {id}")))
}

fn load_run(state: &WorkflowsState, raw_id: &str) -> UnifiedResult<WorkflowRun> {
    let id = parse_uuid(raw_id, "run")?;
    state
        .runs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Workflow run not found: {id}")))
}

fn steps_for_run(state: &WorkflowsState, run_id: Uuid) -> Vec<WorkflowRunStep> {
    let mut items: Vec<WorkflowRunStep> = state
        .steps
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|step| step.run_id == run_id)
        .collect();
    items.sort_by_key(|step| step.step_index);
    items
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no conversion is done (all stored times are UTC).
fn valid_timezone(timezone: &str) -> bool {
    if timezone.is_empty() || timezone.len() > MAX_TIMEZONE_LEN {
        return false;
    }
    if timezone == "UTC" {
        return true;
    }
    timezone.contains('/')
        && timezone.split('/').all(|segment| {
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
        })
}

/// A bus event name: lowercase letters, digits, dots, underscores,
/// and hyphens, never leading / trailing with a dot, no empty
/// segments (for example `invoice.overdue`, `ticket.created`).
fn valid_event_name(event: &str) -> bool {
    if event.is_empty() || event.len() > MAX_EVENT_NAME_LEN {
        return false;
    }
    if event.starts_with('.') || event.ends_with('.') || event.contains("..") {
        return false;
    }
    event
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '_' || c == '-')
}

/// A 5-field cron expression where each field is `*`, `*/N`, a
/// plain number, or a comma-separated list of numbers. Same shape
/// rule as the scheduler module's `valid_schedule` (an interval in
/// seconds is not accepted here: a schedule trigger is always a
/// cron expression).
fn valid_cron(cron: &str) -> bool {
    let cron = cron.trim();
    if cron.is_empty() || cron.len() > MAX_CRON_LEN {
        return false;
    }
    let fields: Vec<&str> = cron.split_whitespace().collect();
    if fields.len() != 5 {
        return false;
    }
    fields.iter().all(|field| {
        if *field == "*" {
            return true;
        }
        if let Some(step) = field.strip_prefix("*/") {
            return step.parse::<u32>().is_ok_and(|n| n > 0);
        }
        field
            .split(',')
            .all(|part| !part.is_empty() && part.parse::<u32>().is_ok())
    })
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// A webhook host allowlist entry: a bare hostname
/// (`hooks.example.com`) or a wildcard (`*.example.com`).
fn valid_allowed_host(host: &str) -> bool {
    if host.is_empty() || host.len() > MAX_HOST_LEN {
        return false;
    }
    let host = host.strip_prefix("*.").unwrap_or(host);
    if host.starts_with('.') || host.ends_with('.') || host.contains("..") {
        return false;
    }
    host.split('.').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !segment.starts_with('-')
            && !segment.ends_with('-')
    })
}

fn host_allowed(host: &str, allowlist: &[String]) -> bool {
    let host = host.to_lowercase();
    allowlist.iter().any(|entry| {
        let entry = entry.to_lowercase();
        if let Some(suffix) = entry.strip_prefix("*.") {
            host != suffix && host.ends_with(&format!(".{suffix}"))
        } else {
            host == entry
        }
    })
}

fn is_secret_segment(segment: &str) -> bool {
    let lower = segment.to_lowercase();
    SECRET_SEGMENTS
        .iter()
        .any(|secret| lower.contains(secret))
}

/// Generate a fresh webhook token: 32 random bytes, hex-encoded
/// (64 characters). The raw token is returned once to the caller;
/// only its SHA-256 hash is stored.
fn generate_webhook_token() -> (String, String) {
    let bytes: [u8; 32] = rand::random();
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    let hash = token_hash(&token);
    (token, hash)
}

fn token_hash(token: &str) -> String {
    let digest = Sha256::digest(token.as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Constant-time comparison of two hash strings, so token lookup
/// does not leak the expected value through timing.
fn hashes_equal(a: &str, b: &str) -> bool {
    let a = a.as_bytes();
    let b = b.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

// ---------------------------------------------------------------------------
// Validation (save time)
// ---------------------------------------------------------------------------

fn validate_trigger(trigger: &WorkflowTrigger) -> UnifiedResult<()> {
    match trigger.kind {
        TriggerKind::Event => {
            let event = trigger.event.as_deref().unwrap_or("");
            if !valid_event_name(event) {
                return Err(UnifiedError::bad_request(
                    "trigger.event must be a valid bus event name such as \"invoice.overdue\"",
                ));
            }
        }
        TriggerKind::Schedule => {
            let cron = trigger.cron.as_deref().unwrap_or("");
            if !valid_cron(cron) {
                return Err(UnifiedError::bad_request(
                    "trigger.cron must be a 5-field cron expression such as \"0 9 * * *\"",
                ));
            }
            let timezone = trigger.timezone.as_deref().unwrap_or("UTC");
            if !valid_timezone(timezone) {
                return Err(UnifiedError::bad_request(
                    "trigger.timezone must be a valid IANA timezone name such as \"Asia/Jakarta\"",
                ));
            }
        }
        TriggerKind::Webhook | TriggerKind::Manual => {}
    }
    Ok(())
}

/// Validate a condition field path: `payload.` plus 1..=8
/// segments of letters, digits, or underscores.
fn validate_field_path(field: &str) -> UnifiedResult<Vec<String>> {
    if field.len() > MAX_FIELD_PATH_LEN {
        return Err(UnifiedError::bad_request(
            "condition field must be at most 256 characters",
        ));
    }
    let Some(rest) = field.strip_prefix("payload.") else {
        return Err(UnifiedError::bad_request(
            "condition field must start with \"payload.\" (for example \"payload.order.total\")",
        ));
    };
    if rest.is_empty() {
        return Err(UnifiedError::bad_request(
            "condition field must name a field under \"payload.\"",
        ));
    }
    let segments: Vec<String> = rest.split('.').map(str::to_string).collect();
    if segments.len() > MAX_FIELD_SEGMENTS {
        return Err(UnifiedError::bad_request(
            "condition field must have at most 8 segments",
        ));
    }
    for segment in &segments {
        if segment.is_empty()
            || !segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            return Err(UnifiedError::bad_request(format!(
                "condition field has an invalid segment: \"{segment}\""
            )));
        }
    }
    Ok(segments)
}

fn validate_condition_leaf(leaf: &ConditionLeaf) -> UnifiedResult<()> {
    validate_field_path(&leaf.field)?;
    match leaf.operator.as_str() {
        "equals" | "not_equals" | "contains" | "greater_than" | "less_than" | "in" => {}
        other => {
            return Err(UnifiedError::bad_request(format!(
                "unknown condition operator \"{other}\" (expected equals, not_equals, contains, greater_than, less_than, or in)"
            )));
        }
    }
    let serialized = serde_json::to_string(&leaf.value)
        .unwrap_or_default();
    if serialized.len() > MAX_CONDITION_VALUE_BYTES {
        return Err(UnifiedError::bad_request(
            "condition value must be at most 4096 bytes when serialized",
        ));
    }
    if leaf.operator == "in" {
        let values = leaf.value.as_array().ok_or_else(|| {
            UnifiedError::bad_request("condition operator \"in\" requires an array value")
        })?;
        if values.is_empty() || values.len() > MAX_IN_VALUES {
            return Err(UnifiedError::bad_request(
                "condition operator \"in\" requires between 1 and 100 values",
            ));
        }
    }
    if leaf.operator == "contains" && !leaf.value.is_string() && !leaf.value.is_array() {
        return Err(UnifiedError::bad_request(
            "condition operator \"contains\" requires a string or array value",
        ));
    }
    if (leaf.operator == "greater_than" || leaf.operator == "less_than")
        && !value_is_numeric(&leaf.value)
    {
        return Err(UnifiedError::bad_request(format!(
            "condition operator \"{}\" requires a numeric value",
            leaf.operator
        )));
    }
    Ok(())
}

fn value_is_numeric(value: &Value) -> bool {
    if value.is_number() {
        return true;
    }
    value
        .as_str()
        .is_some_and(|raw| raw.trim().parse::<f64>().is_ok())
}

fn validate_condition_group(group: &ConditionGroup, depth: usize) -> UnifiedResult<()> {
    if depth > MAX_CONDITION_DEPTH {
        return Err(UnifiedError::bad_request(
            "conditions nest at most 3 levels deep",
        ));
    }
    if group.operator != "and" && group.operator != "or" {
        return Err(UnifiedError::bad_request(
            "condition group operator must be \"and\" or \"or\"",
        ));
    }
    if group.conditions.is_empty() || group.conditions.len() > MAX_CONDITIONS_PER_GROUP {
        return Err(UnifiedError::bad_request(
            "a condition group must hold between 1 and 50 conditions",
        ));
    }
    for node in &group.conditions {
        match node {
            ConditionNode::Group(nested) => validate_condition_group(nested, depth + 1)?,
            ConditionNode::Leaf(leaf) => validate_condition_leaf(leaf)?,
        }
    }
    Ok(())
}

fn validate_conditions(conditions: &Option<ConditionGroup>) -> UnifiedResult<()> {
    if let Some(group) = conditions {
        validate_condition_group(group, 1)?;
    }
    Ok(())
}

fn config_text(config: &Value, field: &str, max: usize, required: bool) -> UnifiedResult<Option<String>> {
    match config.get(field) {
        None | Some(Value::Null) => {
            if required {
                Err(UnifiedError::bad_request(format!(
                    "action config requires \"{field}\""
                )))
            } else {
                Ok(None)
            }
        }
        Some(Value::String(raw)) => Ok(Some(require_text(raw, field, max)?)),
        Some(_) => Err(UnifiedError::bad_request(format!(
            "action config \"{field}\" must be a string"
        ))),
    }
}

fn validate_action(action: &WorkflowAction, own_id: Option<Uuid>) -> UnifiedResult<()> {
    let config = &action.config;
    if !config.is_null() && !config.is_object() {
        return Err(UnifiedError::bad_request(
            "action config must be a JSON object",
        ));
    }
    match action.kind.as_str() {
        "send_notification" => {
            config_text(config, "title", MAX_TITLE_LEN, true)?;
            config_text(config, "body", MAX_BODY_LEN, true)?;
            if let Some(kind) = config_text(config, "kind", 128, false)? {
                if kind.trim().is_empty() {
                    return Err(UnifiedError::bad_request(
                        "action config \"kind\" must not be empty",
                    ));
                }
            }
            if let Some(priority) = config.get("priority").and_then(Value::as_str) {
                if !["low", "normal", "high", "urgent"].contains(&priority) {
                    return Err(UnifiedError::bad_request(
                        "action config \"priority\" must be low, normal, high, or urgent",
                    ));
                }
            }
            let has_user = config.get("user_id").and_then(Value::as_str).is_some_and(|v| !v.trim().is_empty())
                || config.get("user_ids").and_then(Value::as_array).is_some_and(|v| !v.is_empty());
            if !has_user {
                return Err(UnifiedError::bad_request(
                    "send_notification requires \"user_id\" or a non-empty \"user_ids\" list",
                ));
            }
        }
        "send_email" => {
            let to = config_text(config, "to", MAX_EMAIL_LEN, true)?.unwrap_or_default();
            if !valid_email(&to) {
                return Err(UnifiedError::bad_request(format!("Invalid email address: {to}")));
            }
            let subject = config_text(config, "subject", MAX_TITLE_LEN, false)?;
            let template_id = config_text(config, "template_id", 128, false)?;
            if subject.is_none() && template_id.is_none() {
                return Err(UnifiedError::bad_request(
                    "send_email requires \"subject\" or \"template_id\"",
                ));
            }
            config_text(config, "body_text", MAX_BODY_LEN, false)?;
            config_text(config, "body_html", MAX_BODY_LEN, false)?;
        }
        "create_task" => {
            config_text(config, "title", MAX_NAME_LEN, true)?;
            config_text(config, "description", MAX_DESCRIPTION_LEN, false)?;
            if let Some(priority) = config.get("priority").and_then(Value::as_str) {
                if !PRIORITIES.contains(&priority) {
                    return Err(UnifiedError::bad_request(format!(
                        "action config \"priority\" must be one of: {}",
                        PRIORITIES.join(", ")
                    )));
                }
            }
            if let Some(assignee) = config.get("assignee_id").and_then(Value::as_str) {
                require_text(assignee, "assignee_id", MAX_USER_ID_LEN)?;
            }
        }
        "create_ticket" => {
            config_text(config, "title", MAX_NAME_LEN, true)?;
            config_text(config, "body", MAX_DESCRIPTION_LEN, false)?;
            if let Some(priority) = config.get("priority").and_then(Value::as_str) {
                if !PRIORITIES.contains(&priority) {
                    return Err(UnifiedError::bad_request(format!(
                        "action config \"priority\" must be one of: {}",
                        PRIORITIES.join(", ")
                    )));
                }
            }
            for field in ["category", "product", "department_id"] {
                config_text(config, field, 128, false)?;
            }
        }
        "update_record" => {
            let module = config_text(config, "target_module", 64, true)?.unwrap_or_default();
            let field = config_text(config, "field", 64, true)?.unwrap_or_default();
            let allowed = UPDATE_ALLOWLIST
                .iter()
                .find(|(name, _)| *name == module)
                .map(|(_, fields)| *fields);
            let Some(fields) = allowed else {
                return Err(UnifiedError::bad_request(format!(
                    "update_record target_module \"{module}\" is not allowlisted (allowed: tasks, tickets, projects)"
                )));
            };
            if !fields.contains(&field.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "update_record field \"{field}\" is not allowlisted for module \"{module}\" (allowed: {})",
                    fields.join(", ")
                )));
            }
            let target_id = config_text(config, "target_id", 128, true)?.unwrap_or_default();
            parse_uuid(&target_id, "update_record target")?;
            if config.get("value").is_none() || config.get("value") == Some(&Value::Null) {
                return Err(UnifiedError::bad_request(
                    "update_record requires a \"value\"",
                ));
            }
            if let Some(value) = config.get("value").and_then(Value::as_str) {
                if value.len() > 256 {
                    return Err(UnifiedError::bad_request(
                        "update_record \"value\" must be at most 256 characters",
                    ));
                }
            }
        }
        "call_webhook" => {
            let url = config_text(config, "url", MAX_URL_LEN, true)?.unwrap_or_default();
            validate_webhook_url_shape(&url)?;
            let method = config
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("POST");
            if !["GET", "POST", "PUT", "PATCH", "DELETE"].contains(&method) {
                return Err(UnifiedError::bad_request(
                    "call_webhook \"method\" must be GET, POST, PUT, PATCH, or DELETE",
                ));
            }
            if let Some(headers) = config.get("headers") {
                let map = headers.as_object().ok_or_else(|| {
                    UnifiedError::bad_request("call_webhook \"headers\" must be an object")
                })?;
                if map.len() > MAX_WEBHOOK_HEADERS {
                    return Err(UnifiedError::bad_request(
                        "call_webhook allows at most 20 headers",
                    ));
                }
                for (name, value) in map {
                    validate_header_name(name)?;
                    let raw = value.as_str().ok_or_else(|| {
                        UnifiedError::bad_request("call_webhook header values must be strings")
                    })?;
                    if raw.len() > 4096 {
                        return Err(UnifiedError::bad_request(
                            "call_webhook header values must be at most 4096 characters",
                        ));
                    }
                }
            }
            if let Some(body) = config.get("body") {
                if serde_json::to_string(body).unwrap_or_default().len() > MAX_WEBHOOK_BODY_BYTES {
                    return Err(UnifiedError::bad_request(
                        "call_webhook \"body\" must be at most 65536 bytes when serialized",
                    ));
                }
            }
        }
        "assign_user" => {
            let module = config_text(config, "target_module", 64, true)?.unwrap_or_default();
            if !TARGET_MODULES.contains(&module.as_str()) {
                return Err(UnifiedError::bad_request(
                    "assign_user target_module must be \"tasks\" or \"tickets\"",
                ));
            }
            let target_id = config_text(config, "target_id", 128, true)?.unwrap_or_default();
            parse_uuid(&target_id, "assign_user target")?;
            config_text(config, "user_id", MAX_USER_ID_LEN, true)?;
        }
        "add_tag" => {
            let module = config_text(config, "target_module", 64, true)?.unwrap_or_default();
            if !TARGET_MODULES.contains(&module.as_str()) {
                return Err(UnifiedError::bad_request(
                    "add_tag target_module must be \"tasks\" or \"tickets\"",
                ));
            }
            let target_id = config_text(config, "target_id", 128, true)?.unwrap_or_default();
            parse_uuid(&target_id, "add_tag target")?;
            config_text(config, "tag", MAX_TAG_LEN, true)?;
        }
        "run_workflow" => {
            let target = config_text(config, "workflow_id", 128, true)?.unwrap_or_default();
            let target_id = parse_uuid(&target, "run_workflow target")?;
            if Some(target_id) == own_id {
                return Err(UnifiedError::bad_request(
                    "a workflow cannot run itself (self-trigger is rejected)",
                ));
            }
        }
        other => {
            return Err(UnifiedError::bad_request(format!(
                "unknown action kind \"{other}\" (expected send_notification, send_email, create_task, create_ticket, update_record, call_webhook, assign_user, add_tag, or run_workflow)"
            )));
        }
    }
    Ok(())
}

/// Shape checks for a webhook URL that need no network: http(s)
/// only, no embedded credentials, host present. The SSRF checks
/// (allowlist + DNS resolution) run at execution time.
fn validate_webhook_url_shape(url: &str) -> UnifiedResult<()> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(UnifiedError::bad_request(
            "call_webhook \"url\" must start with http:// or https://",
        ));
    }
    if url.contains('@') {
        // Reject userinfo (`http://user:pass@host/...`) outright:
        // credentials never belong in a workflow action URL.
        let after_scheme = url.split("://").nth(1).unwrap_or("");
        let authority = after_scheme.split('/').next().unwrap_or("");
        if authority.contains('@') {
            return Err(UnifiedError::bad_request(
                "call_webhook \"url\" must not contain credentials",
            ));
        }
    }
    Ok(())
}

fn validate_header_name(name: &str) -> UnifiedResult<()> {
    let lower = name.to_lowercase();
    if ["host", "content-length", "connection", "transfer-encoding"].contains(&lower.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "call_webhook header \"{name}\" is not allowed"
        )));
    }
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!(
            "call_webhook header name \"{name}\" is invalid"
        )));
    }
    Ok(())
}

fn validate_actions(actions: &[WorkflowAction], own_id: Option<Uuid>) -> UnifiedResult<()> {
    if actions.len() > MAX_ACTIONS {
        return Err(UnifiedError::bad_request(format!(
            "a workflow holds at most {MAX_ACTIONS} actions"
        )));
    }
    for action in actions {
        validate_action(action, own_id)?;
    }
    Ok(())
}

fn validate_allowed_hosts(hosts: &[String]) -> UnifiedResult<()> {
    if hosts.len() > MAX_ALLOWED_HOSTS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_ALLOWED_HOSTS} webhook hosts are allowed per workflow"
        )));
    }
    let mut seen = HashSet::new();
    for host in hosts {
        let lower = host.trim().to_lowercase();
        if !valid_allowed_host(&lower) {
            return Err(UnifiedError::bad_request(format!(
                "invalid webhook host in allowlist: \"{host}\""
            )));
        }
        if !seen.insert(lower) {
            return Err(UnifiedError::bad_request(format!(
                "duplicate webhook host in allowlist: \"{host}\""
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Payload summary, conditions, templates
// ---------------------------------------------------------------------------

/// Redacted summary of a trigger payload for a run row: only
/// top-level keys, values replaced by their type (strings truncated
/// to 128 characters), secret-named keys dropped entirely. The full
/// payload is never stored — that is why a retry needs a
/// caller-supplied payload.
fn summarize_payload(payload: &Value) -> Value {
    match payload {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, value) in map {
                if is_secret_segment(key) {
                    continue;
                }
                out.insert(key.clone(), summarize_value(value));
            }
            Value::Object(out)
        }
        other => summarize_value(other),
    }
}

fn summarize_value(value: &Value) -> Value {
    match value {
        Value::String(raw) => {
            let mut text = raw.clone();
            if text.len() > SUMMARY_STRING_LEN {
                text.truncate(SUMMARY_STRING_LEN);
                text.push_str("...");
            }
            json!({ "type": "string", "value": text })
        }
        Value::Number(number) => json!({ "type": "number", "value": number }),
        Value::Bool(flag) => json!({ "type": "boolean", "value": flag }),
        Value::Null => json!({ "type": "null" }),
        Value::Array(items) => json!({ "type": "array", "length": items.len() }),
        Value::Object(map) => {
            let mut keys: Vec<String> = map
                .keys()
                .filter(|key| !is_secret_segment(key))
                .cloned()
                .collect();
            keys.sort();
            keys.truncate(20);
            json!({ "type": "object", "keys": keys })
        }
    }
}

/// Resolve a `payload.a.b.c` field path against the trigger
/// payload. Returns `None` when any segment is missing.
fn resolve_field<'a>(payload: &'a Value, segments: &[String]) -> Option<&'a Value> {
    let mut current = payload;
    for segment in segments {
        current = current.get(segment)?;
    }
    Some(current)
}

fn values_equal(left: &Value, right: &Value) -> bool {
    if let (Some(a), Some(b)) = (left.as_f64(), right.as_f64()) {
        return a == b;
    }
    left == right
}

fn value_as_number(value: &Value) -> Option<f64> {
    if let Some(number) = value.as_f64() {
        return Some(number);
    }
    value
        .as_str()
        .and_then(|raw| raw.trim().parse::<f64>().ok())
}

/// Evaluate one leaf against the trigger payload. A missing field
/// matches nothing (not even `not_equals`): a condition can only
/// pass on data that is actually present.
fn eval_leaf(leaf: &ConditionLeaf, payload: &Value) -> LeafEvaluation {
    let segments = validate_field_path(&leaf.field).unwrap_or_default();
    let secret = segments.iter().any(|segment| is_secret_segment(segment));
    let actual = segments
        .is_empty()
        .then_some(None)
        .unwrap_or_else(|| resolve_field(payload, &segments));
    let matched = match actual {
        None => false,
        Some(actual) => match leaf.operator.as_str() {
            "equals" => values_equal(actual, &leaf.value),
            "not_equals" => !values_equal(actual, &leaf.value),
            "contains" => {
                if let (Some(haystack), Some(needle)) =
                    (actual.as_str(), leaf.value.as_str())
                {
                    haystack.contains(needle)
                } else if let Some(items) = actual.as_array() {
                    items.iter().any(|item| values_equal(item, &leaf.value))
                } else {
                    false
                }
            }
            "greater_than" => matches!(
                (value_as_number(actual), value_as_number(&leaf.value)),
                (Some(a), Some(b)) if a > b
            ),
            "less_than" => matches!(
                (value_as_number(actual), value_as_number(&leaf.value)),
                (Some(a), Some(b)) if a < b
            ),
            "in" => leaf
                .value
                .as_array()
                .is_some_and(|values| values.iter().any(|item| values_equal(actual, item))),
            _ => false,
        },
    };
    LeafEvaluation {
        field: leaf.field.clone(),
        operator: leaf.operator.clone(),
        value: leaf.value.clone(),
        actual: if secret {
            Value::String("[redacted]".to_string())
        } else {
            actual.cloned().unwrap_or(Value::Null)
        },
        matched,
    }
}

/// Evaluate a condition group, collecting every leaf evaluation
/// (used by dry runs and tests so the caller sees exactly which
/// leaves passed).
fn eval_group(group: &ConditionGroup, payload: &Value) -> (bool, Vec<LeafEvaluation>) {
    let mut leaves = Vec::new();
    let mut child_results = Vec::new();
    for node in &group.conditions {
        match node {
            ConditionNode::Group(nested) => {
                let (matched, mut nested_leaves) = eval_group(nested, payload);
                leaves.append(&mut nested_leaves);
                child_results.push(matched);
            }
            ConditionNode::Leaf(leaf) => {
                let evaluation = eval_leaf(leaf, payload);
                child_results.push(evaluation.matched);
                leaves.push(evaluation);
            }
        }
    }
    let matched = match group.operator.as_str() {
        "and" => child_results.iter().all(|value| *value),
        "or" => child_results.iter().any(|value| *value),
        _ => false,
    };
    (matched, leaves)
}

/// Render `{payload.a.b}` placeholders inside a template string.
/// `{workflow.id}` and `{workflow.name}` resolve against the
/// workflow. Unknown or missing fields render as an empty string;
/// secret-named fields render as `[redacted]`.
fn render_template(template: &str, payload: &Value, workflow: &Workflow) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        let (before, after_open) = rest.split_at(start);
        out.push_str(before);
        let Some(end) = after_open.find('}') else {
            out.push_str(after_open);
            break;
        };
        let placeholder = &after_open[1..end];
        let replacement = if placeholder == "workflow.id" {
            workflow.id.to_string()
        } else if placeholder == "workflow.name" {
            workflow.name.clone()
        } else if let Some(path) = placeholder.strip_prefix("payload.") {
            let segments: Vec<String> = path.split('.').map(str::to_string).collect();
            if segments.iter().any(|segment| is_secret_segment(segment)) {
                "[redacted]".to_string()
            } else {
                match resolve_field(payload, &segments) {
                    Some(Value::String(raw)) => raw.clone(),
                    Some(Value::Number(number)) => number.to_string(),
                    Some(Value::Bool(flag)) => flag.to_string(),
                    Some(Value::Null) | None => String::new(),
                    Some(other) => serde_json::to_string(other).unwrap_or_default(),
                }
            }
        } else {
            format!("{{{placeholder}}}")
        };
        out.push_str(&replacement);
        rest = &after_open[end + 1..];
    }
    out
}

/// Render templates inside a config value (strings only; objects
/// and arrays are walked recursively).
fn render_config_value(value: &Value, payload: &Value, workflow: &Workflow) -> Value {
    match value {
        Value::String(raw) => Value::String(render_template(raw, payload, workflow)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| render_config_value(item, payload, workflow))
                .collect(),
        ),
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (key, item) in map {
                out.insert(
                    key.clone(),
                    render_config_value(item, payload, workflow),
                );
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Outbound webhook (SSRF-safe)
// ---------------------------------------------------------------------------

/// True when an IP address is safe to connect to from a workflow:
/// public only. Private, loopback, link-local, multicast,
/// unspecified, documentation, benchmarking, and carrier-grade NAT
/// ranges are all rejected.
fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ipv4) => {
            let octets = ipv4.octets();
            if ipv4.is_private()
                || ipv4.is_loopback()
                || ipv4.is_link_local()
                || ipv4.is_multicast()
                || ipv4.is_unspecified()
                || ipv4.is_broadcast()
            {
                return false;
            }
            // 0.0.0.0/8 ("this network"), 100.64.0.0/10 (CGNAT),
            // 192.0.0.0/24 (IETF assignments), documentation
            // ranges, 198.18.0.0/15 (benchmarking), 240.0.0.0/4.
            if octets[0] == 0
                || (octets[0] == 100 && (64..=127).contains(&octets[1]))
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 0)
                || (octets[0] == 192 && octets[1] == 0 && octets[2] == 2)
                || (octets[0] == 198 && octets[1] == 51 && octets[2] == 100)
                || (octets[0] == 203 && octets[1] == 0 && octets[2] == 113)
                || (octets[0] == 198 && (18..=19).contains(&octets[1]))
                || octets[0] >= 240
            {
                return false;
            }
            true
        }
        IpAddr::V6(ipv6) => {
            if ipv6.is_loopback()
                || ipv6.is_multicast()
                || ipv6.is_unspecified()
                || ipv6.is_unique_local()
            {
                return false;
            }
            let segments = ipv6.segments();
            // fe80::/10 link-local, ::/128 handled above,
            // 2001:db8::/32 documentation.
            if (0xfe80..=0xfebf).contains(&segments[0]) {
                return false;
            }
            if segments[0] == 0x2001 && segments[1] == 0x0db8 {
                return false;
            }
            // ::ffff:a.b.c.d mapped IPv4: judge the mapped address.
            if let Some(mapped) = ipv6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            true
        }
    }
}

/// Resolve a hostname and reject it unless every resolved address
/// is public. DNS is resolved again by the HTTP client when it
/// connects, so this check cannot fully close a rebinding window
/// (see the honesty note); the host allowlist is the primary
/// defence.
async fn resolve_host_is_public(host: &str, port: u16) -> UnifiedResult<()> {
    let addresses = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| UnifiedError::bad_request(format!("Cannot resolve webhook host: {host}")))?;
    let mut saw_any = false;
    for address in addresses {
        saw_any = true;
        if !is_public_ip(address.ip()) {
            return Err(UnifiedError::bad_request(format!(
                "Webhook host \"{host}\" resolves to a non-public address ({})",
                address.ip()
            )));
        }
    }
    if !saw_any {
        return Err(UnifiedError::bad_request(format!(
            "Webhook host \"{host}\" did not resolve"
        )));
    }
    Ok(())
}

/// Outcome of one executed action step.
struct StepOutcome {
    status: &'static str,
    attempts: u32,
    error: Option<String>,
    detail: Value,
}

fn step_success(detail: Value) -> StepOutcome {
    StepOutcome {
        status: "success",
        attempts: 1,
        error: None,
        detail,
    }
}

fn step_failed(error: String, detail: Value) -> StepOutcome {
    StepOutcome {
        status: "failed",
        attempts: 1,
        error: Some(error),
        detail,
    }
}

fn step_skipped(reason: &str, detail: Value) -> StepOutcome {
    StepOutcome {
        status: "skipped",
        attempts: 0,
        error: Some(reason.to_string()),
        detail,
    }
}

/// Emit an event and interpret the answers: no registered handler
/// means the action was not delivered (`skipped`); a result
/// carrying an `error` key means the consumer refused it
/// (`failed`); anything else is `success`. The created record's id
/// is captured from the first result when the consumer returns one.
fn emit_action(state: &WorkflowsState, event: &str, payload: Value) -> StepOutcome {
    let Some(bus) = state.bus() else {
        return step_skipped(
            "no hook bus is attached; the action was not delivered",
            json!({ "event": event }),
        );
    };
    let handler_count = bus
        .registrations()
        .iter()
        .filter(|(name, _)| name == event)
        .count();
    if handler_count == 0 {
        return step_skipped(
            &format!("no handler is registered for \"{event}\"; the action was not delivered"),
            json!({ "event": event, "handlers": 0 }),
        );
    }
    let context = bus.emit(event, payload);
    let mut result_id: Option<String> = None;
    for result in &context.results {
        if let Some(error) = result.get("error").and_then(Value::as_str) {
            return step_failed(
                format!("\"{event}\" was refused: {error}"),
                json!({ "event": event, "handlers": handler_count }),
            );
        }
        if result_id.is_none() {
            result_id = result.get("id").and_then(Value::as_str).map(str::to_string);
        }
    }
    step_success(json!({ "event": event, "handlers": handler_count, "result_id": result_id }))
}

/// Execute one `call_webhook` action: allowlist + SSRF checks,
/// then the HTTP call with up to 3 attempts and exponential
/// backoff. Only 2xx counts as success.
async fn execute_webhook_action(
    workflow: &Workflow,
    config: &Value,
    payload: &Value,
) -> StepOutcome {
    let rendered = render_config_value(config, payload, workflow);
    let url_raw = rendered
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let method = rendered
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("POST")
        .to_string();
    let parsed = match url::Url::parse(&url_raw) {
        Ok(url) => url,
        Err(_) => {
            return step_failed(
                format!("Invalid webhook URL: {url_raw}"),
                json!({ "method": method }),
            )
        }
    };
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return step_failed(
            "webhook URL must use http or https".to_string(),
            json!({ "method": method }),
        );
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return step_failed(
            "webhook URL must not contain credentials".to_string(),
            json!({ "method": method }),
        );
    }
    let Some(host) = parsed.host_str().map(str::to_string) else {
        return step_failed(
            "webhook URL has no host".to_string(),
            json!({ "method": method }),
        );
    };
    if !host_allowed(&host, &workflow.webhook_allowed_hosts) {
        return step_failed(
            format!("webhook host \"{host}\" is not on this workflow's allowlist"),
            json!({ "method": method, "host": host }),
        );
    }
    let port = parsed.port_or_known_default().unwrap_or(443);
    if let Err(error) = resolve_host_is_public(&host, port).await {
        return step_failed(error.message, json!({ "method": method, "host": host }));
    }
    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(WEBHOOK_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent("Unified-Workflows/1.0")
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            return step_failed(
                format!("cannot build webhook client: {error}"),
                json!({ "method": method, "host": host }),
            )
        }
    };
    let mut headers = reqwest::header::HeaderMap::new();
    if let Some(map) = rendered.get("headers").and_then(Value::as_object) {
        for (name, value) in map {
            let Ok(header_name) = reqwest::header::HeaderName::from_bytes(name.as_bytes()) else {
                continue;
            };
            let Ok(header_value) =
                reqwest::header::HeaderValue::from_str(value.as_str().unwrap_or(""))
            else {
                continue;
            };
            headers.insert(header_name, header_value);
        }
    }
    let body_text: Option<String> = match rendered.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(raw)) => Some(raw.clone()),
        Some(other) => Some(serde_json::to_string(other).unwrap_or_default()),
    };
    let mut last_error = String::from("webhook call failed");
    let mut attempts = 0u32;
    while attempts < MAX_WEBHOOK_ATTEMPTS {
        attempts += 1;
        let request = client
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .unwrap_or(reqwest::Method::POST),
                url_raw.clone(),
            )
            .headers(headers.clone());
        let request = match &body_text {
            Some(body) if method != "GET" => request.body(body.clone()),
            _ => request,
        };
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                if response.status().is_success() {
                    return StepOutcome {
                        status: "success",
                        attempts,
                        error: None,
                        detail: json!({ "method": method, "host": host, "status": status }),
                    };
                }
                last_error = format!("webhook answered with HTTP {status}");
            }
            Err(error) => {
                last_error = format!("webhook call failed: {error}");
            }
        }
        if attempts < MAX_WEBHOOK_ATTEMPTS {
            let backoff = WEBHOOK_BACKOFF_SECS[(attempts - 1) as usize];
            tokio::time::sleep(std::time::Duration::from_secs(backoff)).await;
        }
    }
    StepOutcome {
        status: "failed",
        attempts,
        error: Some(last_error),
        detail: json!({ "method": method, "host": host }),
    }
}

// ---------------------------------------------------------------------------
// Execution engine
// ---------------------------------------------------------------------------

/// Execute one action. Emit-based actions run synchronously; the
/// webhook action needs the network and runs async. `depth` is the
/// workflow-call depth of the run this action belongs to.
async fn execute_action(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    action: &WorkflowAction,
    payload: &Value,
    depth: u8,
) -> StepOutcome {
    let config = render_config_value(&action.config, payload, workflow);
    match action.kind.as_str() {
        "send_notification" => {
            let kind = config
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("workflow.notification");
            let title = config
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("");
            let body = config
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("");
            let link = config
                .get("link")
                .and_then(Value::as_str)
                .map(str::to_string);
            let priority = config
                .get("priority")
                .and_then(Value::as_str)
                .unwrap_or("normal");
            let mut event_payload = json!({
                "kind": kind,
                "title": title,
                "body": body,
                "module": "workflows",
                "category": "workflows",
                "priority": priority,
                "link": link,
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(user_id) = config.get("user_id").and_then(Value::as_str) {
                event_payload["user_id"] = Value::String(user_id.to_string());
            }
            if let Some(user_ids) = config.get("user_ids") {
                event_payload["user_ids"] = user_ids.clone();
            }
            if let Some(channels) = config.get("channels") {
                event_payload["channels"] = channels.clone();
            }
            // The notifications module delivers; this only asks.
            emit_action(state, NOTIFICATION_REQUESTED, event_payload)
        }
        "send_email" => {
            let mut event_payload = json!({
                "to": config.get("to"),
                "subject": config.get("subject"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["body_text", "body_html", "template_id", "variables"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, MAIL_SEND_REQUESTED, event_payload)
        }
        "create_task" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["description", "assignee_id", "priority", "due_at"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TASK_CREATE_REQUESTED, event_payload)
        }
        "create_ticket" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "body": config.get("body"),
                "user_id": workflow.owner_id,
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(company_id) = &workflow.company_id {
                event_payload["company_id"] = Value::String(company_id.clone());
            }
            for field in ["priority", "category", "product", "department_id"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TICKET_CREATE_REQUESTED, event_payload)
        }
        "update_record" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            let event = format!("{module}.update_requested");
            emit_action(
                state,
                &event,
                json!({
                    "id": config.get("target_id"),
                    "field": config.get("field"),
                    "value": config.get("value"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
            )
        }
        "assign_user" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            let event = format!("{module}.assign_requested");
            emit_action(
                state,
                &event,
                json!({
                    "id": config.get("target_id"),
                    "user_id": config.get("user_id"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
            )
        }
        "add_tag" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            let event = format!("{module}.tag_requested");
            emit_action(
                state,
                &event,
                json!({
                    "id": config.get("target_id"),
                    "tag": config.get("tag"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
            )
        }
        "run_workflow" => {
            let target_raw = config
                .get("workflow_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let Ok(target_id) = Uuid::parse_str(target_raw) else {
                return step_failed(
                    format!("Invalid run_workflow target id: {target_raw}"),
                    json!({}),
                );
            };
            if target_id == workflow.id {
                return step_failed(
                    "a workflow cannot run itself".to_string(),
                    json!({ "workflow_id": target_id.to_string() }),
                );
            }
            if depth >= MAX_WORKFLOW_CALL_DEPTH {
                return step_failed(
                    format!(
                        "max workflow-call depth ({MAX_WORKFLOW_CALL_DEPTH}) reached; refusing to run workflow {target_id}"
                    ),
                    json!({ "workflow_id": target_id.to_string(), "depth": depth }),
                );
            }
            let Some(child) = state
                .workflows
                .get(&target_id)
                .map(|entry| entry.value().clone())
            else {
                return step_failed(
                    format!("run_workflow target not found: {target_id}"),
                    json!({ "workflow_id": target_id.to_string() }),
                );
            };
            if !child.is_active {
                return step_failed(
                    format!("run_workflow target is disabled: {target_id}"),
                    json!({ "workflow_id": target_id.to_string() }),
                );
            }
            let child_payload = payload.clone();
            match begin_run(
                state,
                &child,
                "event",
                Some("workflows.run_workflow"),
                &child_payload,
                false,
                depth + 1,
            ) {
                RunStart::Dropped(dropped) => step_failed(
                    format!(
                        "run_workflow target hit its rate limit (run {} dropped)",
                        dropped.id
                    ),
                    json!({ "workflow_id": target_id.to_string(), "run_id": dropped.id.to_string() }),
                ),
                RunStart::Started(child_run) => {
                    let finished =
                        finish_run_async(state, child_run, child, child_payload, depth + 1).await;
                    if finished.status == RunStatus::Finished {
                        step_success(
                            json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                        )
                    } else {
                        step_failed(
                            format!(
                                "run_workflow target run {} ended as {}",
                                finished.id,
                                finished.status.as_str()
                            ),
                            json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                        )
                    }
                }
            }
        }
        "call_webhook" => execute_webhook_action(workflow, &config, payload).await,
        other => step_failed(
            format!("unknown action kind \"{other}\""),
            json!({}),
        ),
    }
}

/// The sync path for runs started from a bus handler without an
/// async runtime: emit-based actions execute; a webhook step is
/// recorded as `deferred` (the caller can retry the run from
/// `POST /{id}/run`, where the async runtime is available).
fn execute_action_sync(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    action: &WorkflowAction,
    payload: &Value,
    depth: u8,
) -> StepOutcome {
    if action.kind == "call_webhook" {
        return StepOutcome {
            status: "deferred",
            attempts: 0,
            error: Some(
                "outbound webhook needs an async runtime; retry the run from POST /api/v1/workflows/{id}/run"
                    .to_string(),
            ),
            detail: json!({ "action": "call_webhook" }),
        };
    }
    if action.kind == "run_workflow" {
        let config = render_config_value(&action.config, payload, workflow);
        let target_raw = config
            .get("workflow_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        let Ok(target_id) = Uuid::parse_str(target_raw) else {
            return step_failed(
                format!("Invalid run_workflow target id: {target_raw}"),
                json!({}),
            );
        };
        if target_id == workflow.id {
            return step_failed(
                "a workflow cannot run itself".to_string(),
                json!({ "workflow_id": target_id.to_string() }),
            );
        }
        if depth >= MAX_WORKFLOW_CALL_DEPTH {
            return step_failed(
                format!(
                    "max workflow-call depth ({MAX_WORKFLOW_CALL_DEPTH}) reached; refusing to run workflow {target_id}"
                ),
                json!({ "workflow_id": target_id.to_string(), "depth": depth }),
            );
        }
        let Some(child) = state
            .workflows
            .get(&target_id)
            .map(|entry| entry.value().clone())
        else {
            return step_failed(
                format!("run_workflow target not found: {target_id}"),
                json!({ "workflow_id": target_id.to_string() }),
            );
        };
        if !child.is_active {
            return step_failed(
                format!("run_workflow target is disabled: {target_id}"),
                json!({ "workflow_id": target_id.to_string() }),
            );
        }
        let child_payload = payload.clone();
        return match begin_run(
            state,
            &child,
            "event",
            Some("workflows.run_workflow"),
            &child_payload,
            false,
            depth + 1,
        ) {
            RunStart::Dropped(dropped) => step_failed(
                format!(
                    "run_workflow target hit its rate limit (run {} dropped)",
                    dropped.id
                ),
                json!({ "workflow_id": target_id.to_string(), "run_id": dropped.id.to_string() }),
            ),
            RunStart::Started(child_run) => {
                let finished =
                    finish_run_sync(state, child_run, child, child_payload, depth + 1);
                if finished.status == RunStatus::Finished {
                    step_success(
                        json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                    )
                } else {
                    step_failed(
                        format!(
                            "run_workflow target run {} ended as {}",
                            finished.id,
                            finished.status.as_str()
                        ),
                        json!({ "workflow_id": target_id.to_string(), "run_id": finished.id.to_string() }),
                    )
                }
            }
        };
    }
    // Emit-based actions share the async implementation: build a
    // blocking-free future is impossible here, so reuse the same
    // code through a tiny local runtime-free path — the emits are
    // synchronous, so run them directly via a duplicate of the
    // emit branches.
    let config = render_config_value(&action.config, payload, workflow);
    match action.kind.as_str() {
        "send_notification" => {
            let kind = config
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("workflow.notification");
            let title = config
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("");
            let body = config
                .get("body")
                .and_then(Value::as_str)
                .unwrap_or("");
            let mut event_payload = json!({
                "kind": kind,
                "title": title,
                "body": body,
                "module": "workflows",
                "category": "workflows",
                "priority": config.get("priority").and_then(Value::as_str).unwrap_or("normal"),
                "link": config.get("link"),
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(user_id) = config.get("user_id").and_then(Value::as_str) {
                event_payload["user_id"] = Value::String(user_id.to_string());
            }
            if let Some(user_ids) = config.get("user_ids") {
                event_payload["user_ids"] = user_ids.clone();
            }
            if let Some(channels) = config.get("channels") {
                event_payload["channels"] = channels.clone();
            }
            emit_action(state, NOTIFICATION_REQUESTED, event_payload)
        }
        "send_email" => {
            let mut event_payload = json!({
                "to": config.get("to"),
                "subject": config.get("subject"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["body_text", "body_html", "template_id", "variables"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, MAIL_SEND_REQUESTED, event_payload)
        }
        "create_task" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            for field in ["description", "assignee_id", "priority", "due_at"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TASK_CREATE_REQUESTED, event_payload)
        }
        "create_ticket" => {
            let mut event_payload = json!({
                "title": config.get("title"),
                "body": config.get("body"),
                "user_id": workflow.owner_id,
                "module": "workflows",
                "workflow_id": workflow.id.to_string(),
            });
            if let Some(company_id) = &workflow.company_id {
                event_payload["company_id"] = Value::String(company_id.clone());
            }
            for field in ["priority", "category", "product", "department_id"] {
                if let Some(value) = config.get(field) {
                    event_payload[field] = value.clone();
                }
            }
            emit_action(state, TICKET_CREATE_REQUESTED, event_payload)
        }
        "update_record" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            emit_action(
                state,
                &format!("{module}.update_requested"),
                json!({
                    "id": config.get("target_id"),
                    "field": config.get("field"),
                    "value": config.get("value"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
            )
        }
        "assign_user" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            emit_action(
                state,
                &format!("{module}.assign_requested"),
                json!({
                    "id": config.get("target_id"),
                    "user_id": config.get("user_id"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
            )
        }
        "add_tag" => {
            let module = config
                .get("target_module")
                .and_then(Value::as_str)
                .unwrap_or("");
            emit_action(
                state,
                &format!("{module}.tag_requested"),
                json!({
                    "id": config.get("target_id"),
                    "tag": config.get("tag"),
                    "module": "workflows",
                    "workflow_id": workflow.id.to_string(),
                }),
            )
        }
        other => step_failed(format!("unknown action kind \"{other}\""), json!({})),
    }
}

/// Outcome of starting a run: either it started, or the rate
/// limit dropped it (the dropped run row is the log entry).
enum RunStart {
    Started(WorkflowRun),
    Dropped(WorkflowRun),
}

/// Create a run row and enforce the guards that apply before any
/// work starts: the per-minute rate limit (max 60 runs per
/// workflow per minute; dry runs are exempt and never counted).
/// Emits `workflows.run.started` for real starts and
/// `workflows.run.failed` (with reason `rate_limited`) for drops.
fn begin_run(
    state: &Arc<WorkflowsState>,
    workflow: &Workflow,
    trigger_kind: &str,
    trigger_name: Option<&str>,
    payload: &Value,
    dry_run: bool,
    depth: u8,
) -> RunStart {
    let now = Utc::now();
    let mut dropped = false;
    if !dry_run {
        let mut starts = state
            .run_starts
            .entry(workflow.id)
            .or_insert_with(Vec::new);
        starts.retain(|started| (*started - now).num_seconds().abs() < RATE_WINDOW_SECS && *started <= now);
        // `retain` above keeps starts within the last 60 seconds;
        // entries from the future (clock skew) are dropped too.
        if starts.len() >= MAX_RUNS_PER_MINUTE {
            dropped = true;
        } else {
            starts.push(now);
        }
    }
    let run = WorkflowRun {
        id: Uuid::now_v7(),
        workflow_id: workflow.id,
        trigger_kind: trigger_kind.to_string(),
        trigger_name: trigger_name.map(str::to_string),
        status: if dropped {
            RunStatus::Dropped
        } else {
            RunStatus::Running
        },
        payload_summary: summarize_payload(payload),
        dry_run,
        depth,
        condition_matched: false,
        skipped_reason: None,
        retried_from_run_id: None,
        duration_ms: None,
        error: if dropped {
            Some(format!(
                "Rate limit exceeded: at most {MAX_RUNS_PER_MINUTE} runs per workflow per minute; this run was dropped"
            ))
        } else {
            None
        },
        started_at: now,
        finished_at: if dropped { Some(now) } else { None },
        created_at: now,
        updated_at: now,
    };
    state.runs.insert(run.id, run.clone());
    if dropped {
        if let Some(mut stored) = state.workflows.get_mut(&workflow.id) {
            stored.dropped_count += 1;
            stored.last_run_at = Some(now);
            stored.last_run_status = Some(RunStatus::Dropped.as_str().to_string());
            stored.updated_at = now;
        }
        state.emit(
            "workflows.run.failed",
            json!({
                "workflow_id": workflow.id.to_string(),
                "run_id": run.id.to_string(),
                "status": "dropped",
                "reason": "rate_limited",
                "trigger_kind": trigger_kind,
            }),
        );
        return RunStart::Dropped(run);
    }
    state.emit(
        "workflows.run.started",
        json!({
            "workflow_id": workflow.id.to_string(),
            "run_id": run.id.to_string(),
            "trigger_kind": trigger_kind,
            "trigger_name": trigger_name,
            "dry_run": dry_run,
            "depth": depth,
        }),
    );
    RunStart::Started(run)
}

/// Finish a run: update counters on the workflow, stamp the run,
/// and emit `workflows.run.finished` / `workflows.run.failed`.
/// Dry runs never touch counters and never emit lifecycle events.
fn finalize_run(
    state: &Arc<WorkflowsState>,
    run: &mut WorkflowRun,
    status: RunStatus,
    error: Option<String>,
) {
    let now = Utc::now();
    run.status = status;
    run.error = error.clone();
    run.finished_at = Some(now);
    run.updated_at = now;
    run.duration_ms = Some(now.signed_duration_since(run.started_at).num_milliseconds().max(0));
    state.runs.insert(run.id, run.clone());
    if run.dry_run {
        return;
    }
    if let Some(mut stored) = state.workflows.get_mut(&run.workflow_id) {
        stored.run_count += 1;
        match status {
            RunStatus::Finished => stored.success_count += 1,
            RunStatus::Failed => stored.failure_count += 1,
            _ => {}
        }
        stored.last_run_at = Some(now);
        stored.last_run_status = Some(status.as_str().to_string());
        stored.updated_at = now;
    }
    let event = if status == RunStatus::Finished {
        "workflows.run.finished"
    } else {
        "workflows.run.failed"
    };
    state.emit(
        event,
        json!({
            "workflow_id": run.workflow_id.to_string(),
            "run_id": run.id.to_string(),
            "status": status.as_str(),
            "trigger_kind": run.trigger_kind,
            "condition_matched": run.condition_matched,
            "duration_ms": run.duration_ms,
            "error": error,
        }),
    );
}

/// Record one step row for a run.
fn record_step(
    state: &Arc<WorkflowsState>,
    run: &WorkflowRun,
    step_index: usize,
    action_kind: &str,
    outcome: &StepOutcome,
    duration_ms: i64,
) -> WorkflowRunStep {
    let now = Utc::now();
    let step = WorkflowRunStep {
        id: Uuid::now_v7(),
        run_id: run.id,
        workflow_id: run.workflow_id,
        step_index,
        action_kind: action_kind.to_string(),
        status: outcome.status.to_string(),
        attempts: outcome.attempts,
        error: outcome.error.clone(),
        detail: outcome.detail.clone(),
        duration_ms: Some(duration_ms),
        created_at: now,
        updated_at: now,
    };
    state.steps.insert(step.id, step.clone());
    step
}

/// Shared condition evaluation for both finish paths. Returns
/// `(matched, leaf evaluations)`. No conditions means "always
/// run".
fn evaluate_conditions(workflow: &Workflow, payload: &Value) -> (bool, Vec<LeafEvaluation>) {
    match &workflow.conditions {
        None => (true, Vec::new()),
        Some(group) => eval_group(group, payload),
    }
}

/// Async finish path: evaluates conditions, executes actions in
/// order (stopping at the first failed step; later steps are
/// recorded as skipped), and finalizes the run. A dry run records
/// every action as a planned step and executes nothing.
async fn finish_run_async(
    state: &Arc<WorkflowsState>,
    mut run: WorkflowRun,
    workflow: Workflow,
    payload: Value,
    depth: u8,
) -> WorkflowRun {
    let (matched, _leaves) = evaluate_conditions(&workflow, &payload);
    run.condition_matched = matched;
    state.runs.insert(run.id, run.clone());
    if !matched {
        run.skipped_reason = Some("Conditions did not match; no actions were executed.".to_string());
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    if run.dry_run {
        for (index, action) in workflow.actions.iter().enumerate() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &StepOutcome {
                    status: "skipped",
                    attempts: 0,
                    error: Some("dry run: planned, not executed".to_string()),
                    detail: json!({ "planned": true, "dry_run": true }),
                },
                0,
            );
        }
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    let mut failed: Option<String> = None;
    let mut deferred = 0usize;
    for (index, action) in workflow.actions.iter().enumerate() {
        if failed.is_some() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &step_skipped(
                    "a previous step failed; remaining steps were not executed",
                    json!({ "planned": true }),
                ),
                0,
            );
            continue;
        }
        let started = Utc::now();
        let outcome = execute_action(state, &workflow, action, &payload, depth).await;
        let duration_ms = Utc::now()
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0);
        if outcome.status == "deferred" {
            deferred += 1;
        }
        if outcome.status == "failed" {
            failed = outcome.error.clone();
        }
        record_step(state, &run, index, &action.kind, &outcome, duration_ms);
    }
    let mut run = run;
    if let Some(error) = failed {
        finalize_run(state, &mut run, RunStatus::Failed, Some(error));
    } else if deferred > 0 {
        finalize_run(
            state,
            &mut run,
            RunStatus::Finished,
            Some(format!(
                "{deferred} webhook step(s) were deferred; retry the run from POST /api/v1/workflows/{}/run",
                workflow.id
            )),
        );
    } else {
        finalize_run(state, &mut run, RunStatus::Finished, None);
    }
    run
}

/// Sync finish path (bus handlers without an async runtime).
/// Identical to the async path except webhook steps are deferred
/// instead of executed.
fn finish_run_sync(
    state: &Arc<WorkflowsState>,
    mut run: WorkflowRun,
    workflow: Workflow,
    payload: Value,
    depth: u8,
) -> WorkflowRun {
    let (matched, _leaves) = evaluate_conditions(&workflow, &payload);
    run.condition_matched = matched;
    state.runs.insert(run.id, run.clone());
    if !matched {
        run.skipped_reason = Some("Conditions did not match; no actions were executed.".to_string());
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    if run.dry_run {
        for (index, action) in workflow.actions.iter().enumerate() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &StepOutcome {
                    status: "skipped",
                    attempts: 0,
                    error: Some("dry run: planned, not executed".to_string()),
                    detail: json!({ "planned": true, "dry_run": true }),
                },
                0,
            );
        }
        let mut run = run;
        finalize_run(state, &mut run, RunStatus::Finished, None);
        return run;
    }
    let mut failed: Option<String> = None;
    let mut deferred = 0usize;
    for (index, action) in workflow.actions.iter().enumerate() {
        if failed.is_some() {
            record_step(
                state,
                &run,
                index,
                &action.kind,
                &step_skipped(
                    "a previous step failed; remaining steps were not executed",
                    json!({ "planned": true }),
                ),
                0,
            );
            continue;
        }
        let started = Utc::now();
        let outcome = execute_action_sync(state, &workflow, action, &payload, depth);
        let duration_ms = Utc::now()
            .signed_duration_since(started)
            .num_milliseconds()
            .max(0);
        if outcome.status == "deferred" {
            deferred += 1;
        }
        if outcome.status == "failed" {
            failed = outcome.error.clone();
        }
        record_step(state, &run, index, &action.kind, &outcome, duration_ms);
    }
    let mut run = run;
    if let Some(error) = failed {
        finalize_run(state, &mut run, RunStatus::Failed, Some(error));
    } else if deferred > 0 {
        finalize_run(
            state,
            &mut run,
            RunStatus::Finished,
            Some(format!(
                "{deferred} webhook step(s) were deferred; retry the run from POST /api/v1/workflows/{}/run",
                workflow.id
            )),
        );
    } else {
        finalize_run(state, &mut run, RunStatus::Finished, None);
    }
    run
}

// ---------------------------------------------------------------------------
// Trigger subscriptions + scheduler
// ---------------------------------------------------------------------------

/// Subscribe one workflow's event trigger: register a bus handler
/// for the workflow's event name. The handler is registered per
/// workflow, so several workflows can listen to the same event.
/// Called on create / enable / trigger change.
fn subscribe_workflow(state: &Arc<WorkflowsState>, workflow: &Workflow) {
    if workflow.trigger.kind != TriggerKind::Event || !workflow.is_active {
        return;
    }
    let Some(bus) = state.bus() else {
        return;
    };
    let Some(event_name) = workflow.trigger.event.clone() else {
        return;
    };
    let workflow_id = workflow.id;
    let handler_state = Arc::clone(state);
    let handler_event = event_name.clone();
    let handler_id = bus.register(
        &event_name,
        "workflows",
        Arc::new(move |ctx: &mut HookContext| {
            on_bus_event(
                Arc::clone(&handler_state),
                workflow_id,
                &handler_event,
                ctx.data.clone(),
            );
        }),
    );
    state
        .subscriptions
        .entry(workflow_id)
        .or_insert_with(Vec::new)
        .push((event_name, handler_id));
}

/// Remove every bus subscription of one workflow. Called on
/// disable / delete / trigger change.
fn unsubscribe_workflow(state: &Arc<WorkflowsState>, workflow_id: Uuid) {
    let Some(bus) = state.bus() else {
        state.subscriptions.remove(&workflow_id);
        return;
    };
    if let Some((_, entries)) = state.subscriptions.remove(&workflow_id) {
        for (event_name, handler_id) in entries {
            bus.unregister_by_id(&event_name, handler_id);
        }
    }
}

/// Queue one scheduled workflow through `scheduler.enqueue` with
/// kind `workflows.run`. Mirrors the events module's
/// `schedule_reminder`: the scheduler answers
/// `{"queued": true, "job_id": ...}` when it knows the kind, and
/// `{"queued": false, "error": "Unknown job kind: ..."}` today,
/// because `workflows.run` is not in `KNOWN_KINDS` yet. The
/// workflow is stored either way; `scheduler_status` records the
/// outcome honestly (`queued` / `declined`).
fn schedule_workflow(state: &Arc<WorkflowsState>, workflow: &mut Workflow) {
    if workflow.trigger.kind != TriggerKind::Schedule {
        workflow.scheduler_status = "not_scheduled".to_string();
        workflow.scheduler_job_id = None;
        return;
    }
    let Some(bus) = state.bus() else {
        workflow.scheduler_status = "declined".to_string();
        workflow.scheduler_job_id = None;
        return;
    };
    let answer = bus.emit(
        SCHEDULER_ENQUEUE,
        json!({
            "kind": JOB_KIND_WORKFLOWS_RUN,
            "payload": {
                "workflow_id": workflow.id.to_string(),
                "trigger": "schedule",
                "cron": workflow.trigger.cron,
                "timezone": workflow.trigger.timezone,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            workflow.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            workflow.scheduler_status = "queued".to_string();
        }
        _ => {
            workflow.scheduler_job_id = None;
            workflow.scheduler_status = "declined".to_string();
        }
    }
}

/// Handle a bus event for one workflow: the runtime trigger path.
/// Guards: inactive or deleted workflows ignore the event; a run
/// that would trigger itself is rejected (a `workflows.*` event
/// whose payload names this workflow). The run starts on the
/// caller's thread and finishes on a spawned task when a Tokio
/// runtime is available; without one it finishes inline on the
/// sync path (webhook steps are deferred, never silently lost).
fn on_bus_event(
    state: Arc<WorkflowsState>,
    workflow_id: Uuid,
    event_name: &str,
    payload: Value,
) {
    let Some(workflow) = state
        .workflows
        .get(&workflow_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if !workflow.is_active {
        return;
    }
    if event_name.starts_with("workflows.")
        && payload
            .get("workflow_id")
            .and_then(Value::as_str)
            .is_some_and(|value| value == workflow_id.to_string())
    {
        // A run that triggers itself is rejected.
        return;
    }
    let trigger_name = if workflow.trigger.kind == TriggerKind::Event {
        workflow.trigger.event.clone()
    } else {
        Some(event_name.to_string())
    };
    match begin_run(
        &state,
        &workflow,
        "event",
        trigger_name.as_deref(),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(_) => {}
        RunStart::Started(run) => {
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let task_state = Arc::clone(&state);
                let task_workflow = workflow.clone();
                handle.spawn(async move {
                    finish_run_async(&task_state, run, task_workflow, payload, 0).await;
                });
            } else {
                finish_run_sync(&state, run, workflow, payload, 0);
            }
        }
    }
}

/// Answer `workflows.run_requested`: the event the scheduler emits
/// when a `workflows.run` job fires. Runs the named scheduled
/// workflow; an unknown or inactive workflow id is ignored so a
/// stale job can never run a deleted workflow.
fn on_workflows_run_requested(state: Arc<WorkflowsState>, ctx: &mut HookContext) {
    let workflow_id = ctx
        .data
        .get("workflow_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(workflow_id) = workflow_id else {
        ctx.add_result(json!({ "started": false, "error": "workflows.run_requested requires 'workflow_id'" }));
        return;
    };
    let Some(workflow) = state
        .workflows
        .get(&workflow_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "started": false, "workflow_id": workflow_id.to_string(), "error": "Workflow not found" }));
        return;
    };
    if !workflow.is_active {
        ctx.add_result(json!({ "started": false, "workflow_id": workflow_id.to_string(), "reason": "Workflow is disabled" }));
        return;
    }
    if workflow.trigger.kind != TriggerKind::Schedule {
        ctx.add_result(json!({ "started": false, "workflow_id": workflow_id.to_string(), "reason": "Workflow has no schedule trigger" }));
        return;
    }
    let payload = ctx.data.get("payload").cloned().unwrap_or(Value::Null);
    match begin_run(
        &state,
        &workflow,
        "schedule",
        workflow.trigger.cron.as_deref(),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(dropped) => {
            ctx.add_result(json!({ "started": false, "run_id": dropped.id.to_string(), "status": "dropped", "reason": "rate_limited" }));
        }
        RunStart::Started(run) => {
            let run_id = run.id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let task_state = Arc::clone(&state);
                handle.spawn(async move {
                    finish_run_async(&task_state, run, workflow, payload, 0).await;
                });
            } else {
                finish_run_sync(&state, run, workflow, payload, 0);
            }
            ctx.add_result(json!({ "started": true, "run_id": run_id.to_string() }));
        }
    }
}

// ---------------------------------------------------------------------------
// Workflow handlers
// ---------------------------------------------------------------------------

async fn list_workflows(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let params = query.into_inner();
        let active_filter = params.filters.get("is_active").cloned();
        let trigger_filter = params.filters.get("trigger_kind").cloned();
        let company_filter = params.filters.get("company_id").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Workflow> = state
            .workflows
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|workflow| {
                if let Some(active) = &active_filter {
                    let expected = match active.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if workflow.is_active != expected {
                        return false;
                    }
                }
                if let Some(kind) = &trigger_filter {
                    if workflow.trigger.kind.as_str() != kind.as_str() {
                        return false;
                    }
                }
                if let Some(company_id) = &company_filter {
                    if workflow.company_id.as_deref() != Some(company_id.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        workflow.name,
                        workflow.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|workflow| workflow.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    body: web::Json<CreateWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        validate_trigger(&input.trigger)?;
        validate_conditions(&input.conditions)?;
        validate_actions(&input.actions, None)?;
        validate_allowed_hosts(&input.webhook_allowed_hosts)?;
        let company_id = match &input.company_id {
            Some(raw) => Some(require_text(raw, "company_id", 128)?),
            None => None,
        };
        let now = Utc::now();
        let mut workflow = Workflow {
            id: Uuid::now_v7(),
            name,
            description,
            trigger: input.trigger,
            conditions: input.conditions,
            actions: input.actions,
            is_active: input.is_active,
            owner_id: caller.actor.clone(),
            company_id,
            webhook_token_hash: None,
            webhook_allowed_hosts: input.webhook_allowed_hosts,
            run_count: 0,
            success_count: 0,
            failure_count: 0,
            dropped_count: 0,
            last_run_at: None,
            last_run_status: None,
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            created_at: now,
            updated_at: now,
        };
        // A webhook trigger gets its token now. The raw token is
        // returned once in the response and never stored.
        let mut webhook_token: Option<String> = None;
        if workflow.trigger.kind == TriggerKind::Webhook {
            let (token, hash) = generate_webhook_token();
            workflow.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, workflow.id);
            webhook_token = Some(token);
        }
        let state_arc = Arc::new(state.as_ref().clone());
        schedule_workflow(&state_arc, &mut workflow);
        state.workflows.insert(workflow.id, workflow.clone());
        subscribe_workflow(&state_arc, &workflow);
        state.emit(
            "workflows.created",
            json!({
                "workflow_id": workflow.id.to_string(),
                "name": workflow.name,
                "trigger_kind": workflow.trigger.kind.as_str(),
                "owner_id": workflow.owner_id,
                "company_id": workflow.company_id,
            }),
        );
        let mut data = json!({ "workflow": workflow });
        if let Some(token) = webhook_token {
            data["webhook_token"] = Value::String(token.clone());
            data["webhook_url"] = Value::String(format!("/api/v1/workflows/hooks/{token}"));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let workflow = load_workflow(&state, &path.into_inner())?;
        let runs: Vec<WorkflowRun> = state
            .runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| run.workflow_id == workflow.id)
            .collect();
        Ok(json!({
            "workflow": workflow,
            "has_webhook_token": workflow.webhook_token_hash.is_some(),
            "subscription_count": state.subscriptions.get(&workflow.id).map(|entry| entry.len()).unwrap_or(0),
            "recorded_runs": runs.len(),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let mut workflow = load_workflow(&state, &path.into_inner())?;
        let input = body.into_inner();
        let state_arc = Arc::new(state.as_ref().clone());
        if let Some(name) = input.name {
            workflow.name = require_text(&name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = input.description {
            workflow.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(trigger) = input.trigger {
            validate_trigger(&trigger)?;
            workflow.trigger = trigger;
        }
        if let Some(conditions) = input.conditions {
            validate_conditions(&conditions)?;
            workflow.conditions = conditions;
        }
        if let Some(actions) = input.actions {
            validate_actions(&actions, Some(workflow.id))?;
            workflow.actions = actions;
        }
        if let Some(is_active) = input.is_active {
            workflow.is_active = is_active;
        }
        if let Some(company_id) = input.company_id {
            workflow.company_id = match company_id {
                Some(raw) => Some(require_text(&raw, "company_id", 128)?),
                None => None,
            };
        }
        if let Some(hosts) = input.webhook_allowed_hosts {
            validate_allowed_hosts(&hosts)?;
            workflow.webhook_allowed_hosts = hosts;
        }
        // Token rotation: the old token stops working immediately.
        let mut webhook_token: Option<String> = None;
        if input.rotate_webhook_token {
            if workflow.trigger.kind != TriggerKind::Webhook {
                return Err(UnifiedError::bad_request(
                    "rotate_webhook_token only applies to a workflow with a webhook trigger",
                ));
            }
            if let Some(old_hash) = workflow.webhook_token_hash.take() {
                state.webhook_tokens.remove(&old_hash);
            }
            let (token, hash) = generate_webhook_token();
            workflow.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, workflow.id);
            webhook_token = Some(token);
        }
        // A webhook trigger that never had a token (for example a
        // workflow whose trigger was just changed to webhook) gets
        // one now.
        if workflow.trigger.kind == TriggerKind::Webhook
            && workflow.webhook_token_hash.is_none()
        {
            let (token, hash) = generate_webhook_token();
            workflow.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, workflow.id);
            webhook_token = Some(token);
        }
        // A trigger that is no longer a webhook drops its token.
        if workflow.trigger.kind != TriggerKind::Webhook {
            if let Some(old_hash) = workflow.webhook_token_hash.take() {
                state.webhook_tokens.remove(&old_hash);
            }
        }
        workflow.updated_at = Utc::now();
        schedule_workflow(&state_arc, &mut workflow);
        state.workflows.insert(workflow.id, workflow.clone());
        // Re-subscribe from a clean slate (all validation already
        // passed): the old handler is removed first so a stale
        // handler can never fire the workflow twice, and a
        // deactivated workflow keeps no subscription behind.
        unsubscribe_workflow(&state_arc, workflow.id);
        if workflow.is_active {
            subscribe_workflow(&state_arc, &workflow);
        }
        state.emit(
            "workflows.updated",
            json!({
                "workflow_id": workflow.id.to_string(),
                "name": workflow.name,
                "trigger_kind": workflow.trigger.kind.as_str(),
                "is_active": workflow.is_active,
            }),
        );
        let mut data = json!({ "workflow": workflow });
        if let Some(token) = webhook_token {
            data["webhook_token"] = Value::String(token.clone());
            data["webhook_url"] = Value::String(format!("/api/v1/workflows/hooks/{token}"));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let workflow = load_workflow(&state, &path.into_inner())?;
        let state_arc = Arc::new(state.as_ref().clone());
        unsubscribe_workflow(&state_arc, workflow.id);
        if let Some(hash) = &workflow.webhook_token_hash {
            state.webhook_tokens.remove(hash);
        }
        let run_ids: Vec<Uuid> = state
            .runs
            .iter()
            .filter(|entry| entry.value().workflow_id == workflow.id)
            .map(|entry| *entry.key())
            .collect();
        for run_id in &run_ids {
            state.runs.remove(run_id);
        }
        let step_ids: Vec<Uuid> = state
            .steps
            .iter()
            .filter(|entry| entry.value().workflow_id == workflow.id)
            .map(|entry| *entry.key())
            .collect();
        for step_id in &step_ids {
            state.steps.remove(step_id);
        }
        state.run_starts.remove(&workflow.id);
        state.workflows.remove(&workflow.id);
        state.emit(
            "workflows.deleted",
            json!({
                "workflow_id": workflow.id.to_string(),
                "name": workflow.name,
                "runs_removed": run_ids.len(),
                "steps_removed": step_ids.len(),
            }),
        );
        Ok(json!({ "deleted": true, "workflow_id": workflow.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn enable_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Workflow> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let mut workflow = load_workflow(&state, &path.into_inner())?;
        workflow.is_active = true;
        workflow.updated_at = Utc::now();
        let state_arc = Arc::new(state.as_ref().clone());
        schedule_workflow(&state_arc, &mut workflow);
        state.workflows.insert(workflow.id, workflow.clone());
        // Idempotent: a repeated enable must never double-subscribe.
        unsubscribe_workflow(&state_arc, workflow.id);
        subscribe_workflow(&state_arc, &workflow);
        state.emit(
            "workflows.updated",
            json!({
                "workflow_id": workflow.id.to_string(),
                "is_active": true,
            }),
        );
        Ok(workflow)
    })();
    match result {
        Ok(workflow) => ok(workflow),
        Err(error) => err(error, debug),
    }
}

async fn disable_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Workflow> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let mut workflow = load_workflow(&state, &path.into_inner())?;
        workflow.is_active = false;
        workflow.updated_at = Utc::now();
        let state_arc = Arc::new(state.as_ref().clone());
        unsubscribe_workflow(&state_arc, workflow.id);
        state.workflows.insert(workflow.id, workflow.clone());
        state.emit(
            "workflows.updated",
            json!({
                "workflow_id": workflow.id.to_string(),
                "is_active": false,
            }),
        );
        Ok(workflow)
    })();
    match result {
        Ok(workflow) => ok(workflow),
        Err(error) => err(error, debug),
    }
}

async fn duplicate_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let source = load_workflow(&state, &path.into_inner())?;
        let now = Utc::now();
        let mut copy = source.clone();
        copy.id = Uuid::now_v7();
        copy.name = format!("{} (copy)", source.name);
        // A copy starts disabled: it must never double-fire the
        // original's event trigger before anyone looks at it.
        copy.is_active = false;
        copy.owner_id = caller.actor.clone();
        copy.run_count = 0;
        copy.success_count = 0;
        copy.failure_count = 0;
        copy.dropped_count = 0;
        copy.last_run_at = None;
        copy.last_run_status = None;
        copy.scheduler_status = "not_scheduled".to_string();
        copy.scheduler_job_id = None;
        copy.webhook_token_hash = None;
        copy.created_at = now;
        copy.updated_at = now;
        let mut webhook_token: Option<String> = None;
        if copy.trigger.kind == TriggerKind::Webhook {
            let (token, hash) = generate_webhook_token();
            copy.webhook_token_hash = Some(hash.clone());
            state.webhook_tokens.insert(hash, copy.id);
            webhook_token = Some(token);
        }
        state.workflows.insert(copy.id, copy.clone());
        state.emit(
            "workflows.created",
            json!({
                "workflow_id": copy.id.to_string(),
                "name": copy.name,
                "duplicated_from": source.id.to_string(),
                "owner_id": copy.owner_id,
            }),
        );
        let mut data = json!({ "workflow": copy });
        if let Some(token) = webhook_token {
            data["webhook_token"] = Value::String(token.clone());
            data["webhook_url"] = Value::String(format!("/api/v1/workflows/hooks/{token}"));
        }
        Ok(data)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Run handlers
// ---------------------------------------------------------------------------

async fn run_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RunWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE) {
        return err(error, debug);
    }
    let workflow = match load_workflow(&state, &path.into_inner()) {
        Ok(workflow) => workflow,
        Err(error) => return err(error, debug),
    };
    let input = body.into_inner();
    if !input.dry_run && !workflow.is_active {
        return err(
            UnifiedError::conflict(
                "This workflow is disabled; enable it before running it (a dry run is still allowed)",
            ),
            debug,
        );
    }
    if !input.payload.is_null() && !input.payload.is_object() && !input.payload.is_array() {
        return err(
            UnifiedError::bad_request("payload must be a JSON object or array"),
            debug,
        );
    }
    let payload = if input.payload.is_null() {
        json!({})
    } else {
        input.payload
    };
    let state_arc = Arc::new(state.as_ref().clone());
    match begin_run(
        &state_arc,
        &workflow,
        "manual",
        Some("manual"),
        &payload,
        input.dry_run,
        0,
    ) {
        RunStart::Dropped(dropped) => ok(json!({
            "run": dropped,
            "steps": [],
            "dropped": true,
            "message": "Rate limit exceeded: at most 60 runs per workflow per minute; this run was dropped.",
        })),
        RunStart::Started(run) => {
            let run_id = run.id;
            let finished = finish_run_async(&state_arc, run, workflow, payload, 0).await;
            let steps = steps_for_run(&state, run_id);
            ok(json!({ "run": finished, "steps": steps }))
        }
    }
}

async fn test_workflow(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TestWorkflowRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE)?;
        let workflow = load_workflow(&state, &path.into_inner())?;
        let input = body.into_inner();
        let payload = if input.payload.is_null() {
            json!({})
        } else {
            input.payload
        };
        let (matched, leaves) = evaluate_conditions(&workflow, &payload);
        let planned: Vec<Value> = workflow
            .actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let mut rendered = render_config_value(&action.config, &payload, &workflow);
                // Webhook header values can carry credentials: show
                // the header names, never the values.
                if action.kind == "call_webhook" {
                    if let Some(headers) = rendered
                        .get_mut("headers")
                        .and_then(Value::as_object_mut)
                    {
                        for value in headers.values_mut() {
                            *value = Value::String("[redacted]".to_string());
                        }
                    }
                }
                json!({
                    "step_index": index,
                    "action_kind": action.kind,
                    "config": rendered,
                    "planned": true,
                })
            })
            .collect();
        Ok(json!({
            "workflow_id": workflow.id.to_string(),
            "condition_matched": matched,
            "conditions": leaves,
            "would_execute": matched,
            "planned_actions": planned,
            "message": if matched {
                "Conditions match. The planned actions above would execute on a real run; nothing was executed."
            } else {
                "Conditions did not match. No action would execute on a real run."
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_workflow_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let workflow = load_workflow(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let trigger_filter = params.filters.get("trigger_kind").cloned();
        let dry_run_filter = params.filters.get("dry_run").cloned();
        let mut items: Vec<WorkflowRun> = state
            .runs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|run| {
                if run.workflow_id != workflow.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if run.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(trigger) = &trigger_filter {
                    if &run.trigger_kind != trigger {
                        return false;
                    }
                }
                if let Some(dry_run) = &dry_run_filter {
                    let expected = match dry_run.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if run.dry_run != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|run| std::cmp::Reverse(run.started_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_workflow_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let run = load_run(&state, &path.into_inner())?;
        let workflow = state
            .workflows
            .get(&run.workflow_id)
            .map(|entry| entry.value().clone());
        let steps = steps_for_run(&state, run.id);
        Ok(json!({
            "run": run,
            "workflow": workflow,
            "steps": steps,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn retry_workflow_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RetryRunRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return err(error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_WORKFLOWS_MANAGE) {
        return err(error, debug);
    }
    let source_run = match load_run(&state, &path.into_inner()) {
        Ok(run) => run,
        Err(error) => return err(error, debug),
    };
    if source_run.status != RunStatus::Failed {
        return err(
            UnifiedError::conflict(format!(
                "Only a failed run can be retried (this run is \"{}\")",
                source_run.status.as_str()
            )),
            debug,
        );
    }
    let workflow = match state
        .workflows
        .get(&source_run.workflow_id)
        .map(|entry| entry.value().clone())
    {
        Some(workflow) => workflow,
        None => {
            return err(
                UnifiedError::not_found(format!(
                    "Workflow not found: {}",
                    source_run.workflow_id
                )),
                debug,
            )
        }
    };
    if !workflow.is_active {
        return err(
            UnifiedError::conflict("This workflow is disabled; enable it before retrying a run"),
            debug,
        );
    }
    // The original payload is not retained (only a redacted
    // summary is stored), so the retry uses the caller-supplied
    // payload, or an empty object when none is given.
    let input = body.into_inner();
    let payload = if input.payload.is_null() {
        json!({})
    } else {
        input.payload
    };
    let state_arc = Arc::new(state.as_ref().clone());
    match begin_run(
        &state_arc,
        &workflow,
        &source_run.trigger_kind,
        source_run.trigger_name.as_deref(),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(dropped) => ok(json!({
            "run": dropped,
            "steps": [],
            "dropped": true,
            "message": "Rate limit exceeded: at most 60 runs per workflow per minute; this retry was dropped.",
        })),
        RunStart::Started(mut run) => {
            run.retried_from_run_id = Some(source_run.id);
            state.runs.insert(run.id, run.clone());
            let run_id = run.id;
            let finished = finish_run_async(&state_arc, run, workflow, payload, 0).await;
            let steps = steps_for_run(&state, run_id);
            ok(json!({ "run": finished, "steps": steps }))
        }
    }
}

async fn list_trigger_subscriptions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<WorkflowsState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WORKFLOWS_VIEW)?;
        let params = query.into_inner();
        let mut by_event: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for entry in state.subscriptions.iter() {
            let workflow_id = entry.key().to_string();
            for (event_name, _) in entry.value().iter() {
                by_event
                    .entry(event_name.clone())
                    .or_default()
                    .push(workflow_id.clone());
            }
        }
        let mut items: Vec<Value> = by_event
            .into_iter()
            .map(|(event, workflow_ids)| {
                json!({ "event": event, "workflow_ids": workflow_ids })
            })
            .collect();
        items.sort_by(|a, b| {
            a.get("event")
                .and_then(Value::as_str)
                .cmp(&b.get("event").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Webhook trigger (token-authenticated; no capability check — see
// the public-access note in the module header)
// ---------------------------------------------------------------------------

async fn webhook_trigger(
    state: web::Data<WorkflowsState>,
    path: web::Path<String>,
    body: Option<web::Json<Value>>,
) -> HttpResponse {
    let token = path.into_inner();
    if token.len() != 64 || !token.chars().all(|c| c.is_ascii_hexdigit()) {
        return err(UnifiedError::unauthorized("Invalid webhook token"), false);
    }
    let presented_hash = token_hash(&token);
    // Constant-time compare against every stored hash: the lookup
    // itself must not leak which token exists.
    let workflow_id = state
        .webhook_tokens
        .iter()
        .find(|entry| hashes_equal(entry.key(), &presented_hash))
        .map(|entry| *entry.value());
    let Some(workflow_id) = workflow_id else {
        return err(UnifiedError::unauthorized("Invalid webhook token"), false);
    };
    let Some(workflow) = state
        .workflows
        .get(&workflow_id)
        .map(|entry| entry.value().clone())
    else {
        return err(UnifiedError::unauthorized("Invalid webhook token"), false);
    };
    if !workflow.is_active || workflow.trigger.kind != TriggerKind::Webhook {
        return err(
            UnifiedError::conflict("This workflow is disabled or no longer has a webhook trigger"),
            false,
        );
    }
    let payload = body.map(|inner| inner.into_inner()).unwrap_or(Value::Null);
    let payload = if payload.is_null() {
        json!({})
    } else {
        payload
    };
    let state_arc = Arc::new(state.as_ref().clone());
    match begin_run(
        &state_arc,
        &workflow,
        "webhook",
        Some("webhook"),
        &payload,
        false,
        0,
    ) {
        RunStart::Dropped(dropped) => ok(json!({
            "run_id": dropped.id.to_string(),
            "status": "dropped",
            "message": "Rate limit exceeded: at most 60 runs per workflow per minute; this run was dropped.",
        })),
        RunStart::Started(run) => {
            let run_id = run.id;
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                let task_state = Arc::clone(&state_arc);
                handle.spawn(async move {
                    finish_run_async(&task_state, run, workflow, payload, 0).await;
                });
                // 202: the run continues in the background.
                json::success_response(
                    json!({ "run_id": run_id.to_string(), "status": "running" }),
                    202,
                )
            } else {
                let finished = finish_run_sync(&state_arc, run, workflow, payload, 0);
                ok(json!({ "run_id": finished.id.to_string(), "status": finished.status.as_str() }))
            }
        }
    }
}

// Re-exported for the integrator's tests: the scheduler job kind
// this module enqueues, and the hook event the scheduler must emit
// when that job fires.
pub const WORKFLOWS_RUN_JOB_KIND: &str = JOB_KIND_WORKFLOWS_RUN;
pub const WORKFLOWS_RUN_REQUESTED_EVENT: &str = WORKFLOWS_RUN_REQUESTED;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct WorkflowsModule {
    state: WorkflowsState,
}

impl WorkflowsModule {
    pub fn new() -> Self {
        Self {
            state: WorkflowsState::default(),
        }
    }
}

impl Default for WorkflowsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WorkflowsModule {
    fn id(&self) -> &'static str {
        "workflows"
    }

    fn name(&self) -> &'static str {
        "Workflow Automation"
    }

    fn description(&self) -> &'static str {
        "Workflow automation / autopilot: event, schedule, webhook, and manual triggers; nested condition groups; ordered actions (notification, email, task, ticket, record update, outbound webhook, assign, tag, run another workflow); per-run logs, dry-run testing, retries, and loop / recursion guards."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "mail", "tickets", "tasks", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "mail", "tickets", "tasks", "scheduler"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "url", "sha2"]
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
            WORKFLOWS_RUN_REQUESTED,
            "workflows",
            Arc::new(move |ctx: &mut HookContext| {
                on_workflows_run_requested(Arc::clone(&state), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/workflows")
                .app_data(state)
                // Literal segments first: they must win over `/{id}`.
                .route("", web::get().to(list_workflows))
                .route("", web::post().to(create_workflow))
                .route("/triggers", web::get().to(list_trigger_subscriptions))
                .route("/hooks/{token}", web::post().to(webhook_trigger))
                .route("/runs/{run_id}", web::get().to(get_workflow_run))
                .route("/runs/{run_id}/retry", web::post().to(retry_workflow_run))
                .route("/{id}", web::get().to(get_workflow))
                .route("/{id}", web::patch().to(update_workflow))
                .route("/{id}", web::delete().to(delete_workflow))
                .route("/{id}/enable", web::post().to(enable_workflow))
                .route("/{id}/disable", web::post().to(disable_workflow))
                .route("/{id}/duplicate", web::post().to(duplicate_workflow))
                .route("/{id}/run", web::post().to(run_workflow))
                .route("/{id}/test", web::post().to(test_workflow))
                .route("/{id}/runs", web::get().to(list_workflow_runs)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/workflows", owner: "workflows", description: "List workflows (paginated, filter by is_active, trigger_kind, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/workflows", owner: "workflows", description: "Create a workflow (trigger, conditions, actions validated at save time; webhook trigger returns its token once)." },
            RouteInfo { method: "GET", path: "/api/v1/workflows/{id}", owner: "workflows", description: "Get one workflow with subscription and run counts." },
            RouteInfo { method: "PATCH", path: "/api/v1/workflows/{id}", owner: "workflows", description: "Update a workflow (re-subscribes event triggers; rotate_webhook_token rotates the webhook token)." },
            RouteInfo { method: "DELETE", path: "/api/v1/workflows/{id}", owner: "workflows", description: "Delete a workflow with its runs, steps, subscriptions, and webhook token." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/enable", owner: "workflows", description: "Enable a workflow (subscribes its event trigger; schedules it when it has a schedule trigger)." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/disable", owner: "workflows", description: "Disable a workflow (unsubscribes its event trigger; bus events are ignored while disabled)." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/duplicate", owner: "workflows", description: "Duplicate a workflow (the copy starts disabled with a fresh webhook token)." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/run", owner: "workflows", description: "Run a workflow manually with a payload (dry_run=true plans without side effects)." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/{id}/test", owner: "workflows", description: "Dry-run evaluation: condition results plus planned actions, no side effects, no run row." },
            RouteInfo { method: "GET", path: "/api/v1/workflows/{id}/runs", owner: "workflows", description: "List runs of a workflow (paginated, filter by status, trigger_kind, dry_run)." },
            RouteInfo { method: "GET", path: "/api/v1/workflows/runs/{run_id}", owner: "workflows", description: "Get one run with its steps." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/runs/{run_id}/retry", owner: "workflows", description: "Retry a failed run with a caller-supplied payload (the original payload is not retained)." },
            RouteInfo { method: "GET", path: "/api/v1/workflows/triggers", owner: "workflows", description: "List active bus-event subscriptions (event name -> workflow ids)." },
            RouteInfo { method: "POST", path: "/api/v1/workflows/hooks/{token}", owner: "workflows", description: "TOKEN-AUTHENTICATED: trigger a webhook workflow (no capability check; gated by the API Manager ApiAccessRule for this path)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WorkflowsModule::new()));
}
