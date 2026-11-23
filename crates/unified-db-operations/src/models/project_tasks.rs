// Diesel models for the project_tasks domain of the unified-db-operations bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_tasks)]
pub struct ProjectTask {
    pub id: Uuid,
    pub project_id: Uuid,
    pub board_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub epic_id: Option<Uuid>,
    pub title: String,
    pub description: String,
    pub task_type: String,
    pub status: String,
    pub priority: String,
    pub assignee_id: Option<String>,
    pub reporter_id: String,
    pub estimate_points: Option<f64>,
    pub estimate_hours: Option<f64>,
    pub labels: Value,
    pub custom_values: Value,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sprint_id: Option<Uuid>,
    pub phase_id: Option<Uuid>,
    pub cycle_id: Option<Uuid>,
    pub pi_id: Option<Uuid>,
    pub milestone_id: Option<Uuid>,
    pub process_group: Option<String>,
    pub knowledge_area: Option<String>,
    pub waste_flag: Option<bool>,
    pub pair_partner_id: Option<String>,
    pub test_first: bool,
    pub dmaic_phase: Option<String>,
    pub appetite_weeks: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_tasks)]
pub struct NewProjectTask {
    pub id: Uuid,
    pub project_id: Uuid,
    pub board_id: Option<Uuid>,
    pub parent_id: Option<Uuid>,
    pub epic_id: Option<Uuid>,
    pub title: String,
    pub status: String,
    pub assignee_id: Option<String>,
    pub reporter_id: String,
    pub estimate_points: Option<f64>,
    pub estimate_hours: Option<f64>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sprint_id: Option<Uuid>,
    pub phase_id: Option<Uuid>,
    pub cycle_id: Option<Uuid>,
    pub pi_id: Option<Uuid>,
    pub milestone_id: Option<Uuid>,
    pub process_group: Option<String>,
    pub knowledge_area: Option<String>,
    pub waste_flag: Option<bool>,
    pub pair_partner_id: Option<String>,
    pub dmaic_phase: Option<String>,
    pub appetite_weeks: Option<i32>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_dependencies)]
pub struct ProjectTaskDependency {
    pub id: Uuid,
    pub task_id: Uuid,
    pub depends_on_task_id: Uuid,
    pub kind: String,
    pub lag_days: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_dependencies)]
pub struct NewProjectTaskDependency {
    pub id: Uuid,
    pub task_id: Uuid,
    pub depends_on_task_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_comments)]
pub struct ProjectTaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_comments)]
pub struct NewProjectTaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_watchers)]
pub struct ProjectTaskWatcher {
    pub id: Uuid,
    pub task_id: Uuid,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_watchers)]
pub struct NewProjectTaskWatcher {
    pub id: Uuid,
    pub task_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_task_attachments)]
pub struct ProjectTaskAttachment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub target: String,
    pub key: String,
    pub stored_key: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_task_attachments)]
pub struct NewProjectTaskAttachment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub target: String,
    pub key: String,
    pub stored_key: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_time_entries)]
pub struct ProjectTimeEntry {
    pub id: Uuid,
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub description: String,
    pub hours: f64,
    pub billable: bool,
    pub billed: bool,
    pub invoice_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub timer_running: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_time_entries)]
pub struct NewProjectTimeEntry {
    pub id: Uuid,
    pub task_id: Uuid,
    pub project_id: Uuid,
    pub user_id: String,
    pub invoice_id: Option<String>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_automation_rules)]
pub struct ProjectAutomationRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub trigger: String,
    pub conditions: Value,
    pub actions: Value,
    pub enabled: bool,
    pub run_count: i64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_automation_rules)]
pub struct NewProjectAutomationRule {
    pub id: Uuid,
    pub project_id: Uuid,
    pub name: String,
    pub trigger: String,
    pub last_run_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_templates)]
pub struct ProjectTemplate {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub boards: Value,
    pub columns_: Value,
    pub statuses: Value,
    pub phases: Value,
    pub custom_fields: Value,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_templates)]
pub struct NewProjectTemplate {
    pub id: Uuid,
    pub name: String,
    pub methodology: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = project_activity)]
pub struct ProjectActivity {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = project_activity)]
pub struct NewProjectActivity {
    pub id: Uuid,
    pub project_id: Uuid,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
}
