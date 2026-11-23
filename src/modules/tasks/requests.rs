// tasks — Request shapes for the HTTP handlers.

use super::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateTaskInput {
    pub title: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub assignee_id: Option<Uuid>,
    #[serde(default)]
    pub reporter_id: Option<Uuid>,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub status: Option<TaskStatus>,
    #[serde(default)]
    pub due_date: Option<DateTime<Utc>>,
    #[serde(default)]
    pub company_id: Option<Uuid>,
    #[serde(default)]
    pub project_name: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub estimate_minutes: Option<i32>,
    #[serde(default)]
    pub spent_minutes: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTaskInput {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// `Some(Some(id))` sets the assignee, `Some(None)` clears it,
    /// `None` (field absent) leaves it unchanged.
    #[serde(default)]
    pub assignee_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub reporter_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub priority: Option<TaskPriority>,
    #[serde(default)]
    pub due_date: Option<Option<DateTime<Utc>>>,
    #[serde(default)]
    pub company_id: Option<Option<Uuid>>,
    #[serde(default)]
    pub project_name: Option<Option<String>>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub estimate_minutes: Option<Option<i32>>,
    #[serde(default)]
    pub spent_minutes: Option<i32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ChangeStatusInput {
    pub status: TaskStatus,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssignInput {
    /// Null unassigns the task.
    pub assignee_id: Option<Uuid>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddCommentInput {
    #[serde(default)]
    pub author_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AddChecklistInput {
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LogTimeInput {
    /// Minutes to add to `spent_minutes`. Must be positive.
    pub minutes: i32,
}
