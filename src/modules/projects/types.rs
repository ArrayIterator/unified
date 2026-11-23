// Projects module — project, board, task, and planning entity types.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Entities
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub status: String,
    pub visibility: String,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub client_id: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub labels: Vec<String>,
    pub custom_field_definitions: Value,
    pub archived: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMember {
    pub id: String,
    pub project_id: String,
    pub user_id: String,
    /// `owner`, `manager`, `member`, or `viewer`.
    pub role: String,
    pub joined_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBoard {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub kind: String,
    pub wip_limit: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBoardColumn {
    pub id: String,
    pub board_id: String,
    pub name: String,
    pub position: u32,
    pub status_key: String,
    pub wip_limit: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTask {
    pub id: String,
    pub project_id: String,
    pub board_id: Option<String>,
    pub parent_id: Option<String>,
    pub epic_id: Option<String>,
    pub title: String,
    pub description: String,
    /// `task`, `story`, `bug`, `spike`, `feature`, `enabler`, or `chore`.
    pub task_type: String,
    pub status: String,
    pub priority: String,
    pub assignee_id: Option<String>,
    pub reporter_id: String,
    pub estimate_points: Option<f64>,
    pub estimate_hours: Option<f64>,
    pub labels: Vec<String>,
    pub custom_values: Value,
    pub start_date: Option<DateTime<Utc>>,
    pub due_date: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub sprint_id: Option<String>,
    pub phase_id: Option<String>,
    pub cycle_id: Option<String>,
    pub pi_id: Option<String>,
    pub milestone_id: Option<String>,
    pub process_group: Option<String>,
    pub knowledge_area: Option<String>,
    pub waste_flag: Option<bool>,
    pub pair_partner_id: Option<String>,
    pub test_first: bool,
    pub dmaic_phase: Option<String>,
    pub appetite_weeks: Option<u32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskDependency {
    pub id: String,
    pub task_id: String,
    pub depends_on_task_id: String,
    /// `finish_start`, `start_start`, `finish_finish`, `start_finish`,
    /// `blocks`, or `related`.
    pub kind: String,
    pub lag_days: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskComment {
    pub id: String,
    pub task_id: String,
    pub author_id: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskWatcher {
    pub id: String,
    pub task_id: String,
    pub user_id: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTaskAttachment {
    pub id: String,
    pub task_id: String,
    pub target: String,
    pub key: String,
    pub stored_key: String,
    pub filename: String,
    pub content_type: Option<String>,
    pub size_bytes: Option<u64>,
    pub uploaded_by: String,
    pub uploaded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectMilestone {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub description: String,
    pub due_date: Option<DateTime<Utc>>,
    pub reached: bool,
    pub reached_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectSprint {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub goal: String,
    /// `planned`, `active`, or `completed`.
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub velocity_planned: Option<f64>,
    pub velocity_actual: Option<f64>,
    pub standup_notes: Value,
    pub review_notes: Option<String>,
    pub retrospective_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectPhase {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// `waterfall`, `prince2_stage`, `dmaic`, or `hybrid`.
    pub kind: String,
    pub position: u32,
    /// `planned`, `active`, or `completed`.
    pub status: String,
    /// `pending`, `approved`, or `rejected`.
    pub gate_status: String,
    pub gate_decision: Option<String>,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTimeEntry {
    pub id: String,
    pub task_id: String,
    pub project_id: String,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectAutomationRule {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// `task_created`, `status_changed`, `task_completed`, `due_soon`,
    /// or `label_added`.
    pub trigger: String,
    pub conditions: Value,
    pub actions: Value,
    pub enabled: bool,
    pub run_count: u64,
    pub last_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub methodology: String,
    pub boards: Value,
    pub columns: Value,
    pub statuses: Value,
    pub phases: Value,
    pub custom_fields: Value,
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectCycle {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub appetite_weeks: u32,
    /// `shaping`, `betting`, `building`, `cooldown`, or `done`.
    pub status: String,
    pub started_at: Option<DateTime<Utc>>,
    pub ends_at: Option<DateTime<Utc>>,
    pub cooldown_weeks: u32,
    pub pitch: Option<String>,
    pub betting_decision: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectProgramIncrement {
    pub id: String,
    pub project_id: String,
    pub name: String,
    /// `planned`, `active`, or `completed`.
    pub status: String,
    pub start_date: Option<DateTime<Utc>>,
    pub end_date: Option<DateTime<Utc>>,
    pub objectives: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectBusinessCase {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub justification: String,
    pub benefits: Value,
    pub costs: Value,
    pub risks: Value,
    /// `draft`, `approved`, or `rejected`.
    pub status: String,
    pub exception_report: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectActivity {
    pub id: String,
    pub project_id: String,
    pub actor_id: String,
    pub action: String,
    pub target_type: String,
    pub target_id: Option<String>,
    pub details: Value,
    pub created_at: DateTime<Utc>,
}
