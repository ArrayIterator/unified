// Projects module — task, dependency, comment, and automation inputs.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use super::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;

// Project tasks, subtasks, dependencies, comments, watchers, attachments,
// automation rules, and the ticket bridge for the Projects module.
//
// Section file: this file is assembled into src/modules/projects.rs after
// the section that owns the imports, the entity structs, the ProjectsState
// helpers (now, new_id, paginate, matches_search, viewer_of,
// require_capability, require_project_access, methodology_profile) and the
// module definition. Nothing defined there is redefined here. Every private
// helper in this file is prefixed with `s3_` so it cannot collide with
// helpers from other sections.
//
// Path shapes assumed by these handlers (routes live in the assembling
// section): project-scoped task lists and creation take the project id;
// single-task handlers take the task id; nested comment, watcher,
// attachment and dependency removals take (task id, child id); automation
// rule detail handlers take (project id, rule id). The ticket bridge
// creation handler takes its project id from the request body.
//
// Viewer identity for every handler comes from the request
// headers (`X-User-Id` / `X-Role`) through the s1 helper
// `viewer_of`, so handlers take an `HttpRequest` extractor; no
// handler reads identity from query parameters or the body.

// ---------------------------------------------------------------------------
// Input structs
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateProjectTask {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub task_type: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<String>,
    #[serde(default)]
    pub reporter_id: Option<String>,
    #[serde(default)]
    pub estimate_points: Option<f64>,
    #[serde(default)]
    pub estimate_hours: Option<f64>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub custom_values: Value,
    #[serde(default)]
    pub start_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub sprint_id: Option<String>,
    #[serde(default)]
    pub phase_id: Option<String>,
    #[serde(default)]
    pub cycle_id: Option<String>,
    #[serde(default)]
    pub pi_id: Option<String>,
    #[serde(default)]
    pub milestone_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub epic_id: Option<String>,
    #[serde(default)]
    pub board_id: Option<String>,
    #[serde(default)]
    pub process_group: Option<String>,
    #[serde(default)]
    pub knowledge_area: Option<String>,
    #[serde(default)]
    pub waste_flag: Option<bool>,
    #[serde(default)]
    pub pair_partner_id: Option<String>,
    #[serde(default)]
    pub test_first: Option<bool>,
    #[serde(default)]
    pub dmaic_phase: Option<String>,
    #[serde(default)]
    pub appetite_weeks: Option<u32>,
}

/// Partial task update. Nullable reference and date fields use a double
/// Option: absent means unchanged, explicit null clears the value.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateProjectTask {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub priority: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<Option<String>>,
    #[serde(default)]
    pub estimate_points: Option<f64>,
    #[serde(default)]
    pub estimate_hours: Option<f64>,
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    #[serde(default)]
    pub custom_values: Option<Value>,
    #[serde(default)]
    pub start_date: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub due_date: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub sprint_id: Option<Option<String>>,
    #[serde(default)]
    pub phase_id: Option<Option<String>>,
    #[serde(default)]
    pub cycle_id: Option<Option<String>>,
    #[serde(default)]
    pub pi_id: Option<Option<String>>,
    #[serde(default)]
    pub milestone_id: Option<Option<String>>,
    #[serde(default)]
    pub process_group: Option<Option<String>>,
    #[serde(default)]
    pub knowledge_area: Option<Option<String>>,
    #[serde(default)]
    pub waste_flag: Option<bool>,
    #[serde(default)]
    pub pair_partner_id: Option<Option<String>>,
    #[serde(default)]
    pub test_first: Option<bool>,
    #[serde(default)]
    pub dmaic_phase: Option<Option<String>>,
    #[serde(default)]
    pub appetite_weeks: Option<Option<u32>>,
    #[serde(default)]
    pub board_id: Option<Option<String>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MoveTask {
    #[serde(default)]
    pub board_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub column_status_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignTask {
    pub assignee_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskDependency {
    pub depends_on_task_id: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub lag_days: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskComment {
    pub author_id: String,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskComment {
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskWatcher {
    pub user_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddTaskAttachment {
    pub target: String,
    pub key: String,
    #[serde(default)]
    pub stored_key: Option<String>,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    pub uploaded_by: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateAutomationRule {
    pub name: String,
    pub trigger: String,
    #[serde(default)]
    pub conditions: Value,
    pub actions: Value,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct UpdateAutomationRule {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub trigger: Option<String>,
    #[serde(default)]
    pub conditions: Option<Value>,
    #[serde(default)]
    pub actions: Option<Value>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ConvertTaskToTicket {
    #[serde(default)]
    pub department_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TaskFromTicket {
    pub ticket_id: String,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    pub user_id: String,
    pub project_id: String,
    #[serde(default)]
    pub department_id: Option<String>,
}
