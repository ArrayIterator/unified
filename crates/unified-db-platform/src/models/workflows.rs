// Diesel models for the workflows domain of the unified-db-platform bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Workflows (migration 000060_workflows).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = workflows)]
pub struct Workflow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger: Value,
    pub conditions: Option<Value>,
    pub actions: Value,
    pub is_active: bool,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub webhook_token_hash: Option<String>,
    pub webhook_allowed_hosts: Value,
    pub run_count: i64,
    pub success_count: i64,
    pub failure_count: i64,
    pub dropped_count: i64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_status: Option<String>,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = workflows)]
pub struct NewWorkflow {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub trigger: Value,
    pub conditions: Option<Value>,
    pub webhook_token_hash: Option<String>,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub last_run_at: Option<DateTime<Utc>>,
    pub last_run_status: Option<String>,
    pub scheduler_job_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = workflow_runs)]
pub struct WorkflowRun {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub trigger_kind: String,
    pub trigger_name: Option<String>,
    pub status: String,
    pub payload_summary: Value,
    pub dry_run: bool,
    pub depth: i32,
    pub condition_matched: bool,
    pub skipped_reason: Option<String>,
    pub retried_from_run_id: Option<Uuid>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = workflow_runs)]
pub struct NewWorkflowRun {
    pub id: Uuid,
    pub workflow_id: Uuid,
    pub trigger_kind: String,
    pub trigger_name: Option<String>,
    pub skipped_reason: Option<String>,
    pub retried_from_run_id: Option<Uuid>,
    pub duration_ms: Option<i64>,
    pub error: Option<String>,
    pub finished_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = workflow_run_steps)]
pub struct WorkflowRunStep {
    pub id: Uuid,
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub step_index: i32,
    pub action_kind: String,
    pub status: String,
    pub attempts: i32,
    pub error: Option<String>,
    pub detail: Value,
    pub duration_ms: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = workflow_run_steps)]
pub struct NewWorkflowRunStep {
    pub id: Uuid,
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    pub step_index: i32,
    pub action_kind: String,
    pub error: Option<String>,
    pub duration_ms: Option<i64>,
}
