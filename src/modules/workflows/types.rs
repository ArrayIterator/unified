// Workflows module - shared types and request/response shapes.
//
// Split from `src/modules/workflows.rs`; the module root re-exports the public API of this module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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

pub(crate) fn default_active() -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trigger_kind_names() {
        assert_eq!(TriggerKind::Event.as_str(), "event");
        assert_eq!(TriggerKind::Schedule.as_str(), "schedule");
        assert_eq!(TriggerKind::Webhook.as_str(), "webhook");
        assert_eq!(TriggerKind::Manual.as_str(), "manual");
    }

    #[test]
    fn run_status_names() {
        assert_eq!(RunStatus::Running.as_str(), "running");
        assert_eq!(RunStatus::Finished.as_str(), "finished");
        assert_eq!(RunStatus::Failed.as_str(), "failed");
        assert_eq!(RunStatus::Dropped.as_str(), "dropped");
    }
}
