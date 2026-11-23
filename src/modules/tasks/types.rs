// tasks — Shared types: constants, enums, records, and request/response shapes.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Event names emitted by this module.
// ---------------------------------------------------------------------------

pub const TASK_CREATED: &str = "task.created";

pub const TASK_ASSIGNED: &str = "task.assigned";

pub const TASK_STATUS_CHANGED: &str = "task.status.changed";

pub const TASK_COMPLETED: &str = "task.completed";

pub const TASK_COMMENT_ADDED: &str = "task.comment.added";

/// Emitted when a task is deleted. Payload is the task event payload
/// for the removed task, with `actor_id` set to the caller who
/// deleted it. The records module does not subscribe to this event
/// yet (see the follow-up note in the module report); it must add
/// `"task.deleted"` to its recorded-events list to audit deletions.
pub const TASK_DELETED: &str = "task.deleted";

/// Emitted when a previously assigned task is unassigned (assignee
/// set to null). Re-assigning the same person emits nothing, and
/// unassigning an already unassigned task emits nothing.
pub const TASK_UNASSIGNED: &str = "task.unassigned";

// ---------------------------------------------------------------------------
// Permissions.
// ---------------------------------------------------------------------------

/// List tasks, get one task, and list task comments.
pub const PERM_TASKS_READ: &str = "tasks:read";

/// Create, update, delete, reassign, change the status of, log time
/// on, comment on, and manage checklist items of tasks.
pub const PERM_TASKS_MANAGE: &str = "tasks:manage";

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Urgent,
}

impl TaskPriority {
    pub(crate) fn rank(self) -> u8 {
        match self {
            TaskPriority::Low => 0,
            TaskPriority::Medium => 1,
            TaskPriority::High => 2,
            TaskPriority::Urgent => 3,
        }
    }
}

impl Default for TaskPriority {
    fn default() -> Self {
        TaskPriority::Medium
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Review,
    Done,
    Blocked,
    Cancelled,
}

impl TaskStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            TaskStatus::Todo => "todo",
            TaskStatus::InProgress => "in_progress",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Blocked => "blocked",
            TaskStatus::Cancelled => "cancelled",
        }
    }

    /// Statuses this status may move to. `done` and `cancelled` are
    /// terminal.
    pub(crate) fn allowed_next(self) -> &'static [TaskStatus] {
        match self {
            TaskStatus::Todo => &[
                TaskStatus::InProgress,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
            ],
            TaskStatus::InProgress => &[
                TaskStatus::Review,
                TaskStatus::Todo,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
            ],
            TaskStatus::Review => &[
                TaskStatus::Done,
                TaskStatus::InProgress,
                TaskStatus::Blocked,
                TaskStatus::Cancelled,
            ],
            TaskStatus::Blocked => &[
                TaskStatus::Todo,
                TaskStatus::InProgress,
                TaskStatus::Cancelled,
            ],
            TaskStatus::Done | TaskStatus::Cancelled => &[],
        }
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "todo" => Some(TaskStatus::Todo),
            "in_progress" => Some(TaskStatus::InProgress),
            "review" => Some(TaskStatus::Review),
            "done" => Some(TaskStatus::Done),
            "blocked" => Some(TaskStatus::Blocked),
            "cancelled" => Some(TaskStatus::Cancelled),
            _ => None,
        }
    }
}

impl Default for TaskStatus {
    fn default() -> Self {
        TaskStatus::Todo
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub done: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// Stored task record. `overdue` is computed, never stored.
#[derive(Debug, Clone, Serialize)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    pub priority: TaskPriority,
    pub status: TaskStatus,
    pub due_date: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub tags: Vec<String>,
    pub comments: Vec<TaskComment>,
    pub checklist: Vec<ChecklistItem>,
    /// Estimated effort in minutes.
    pub estimate_minutes: Option<i32>,
    /// Time actually spent, in minutes.
    pub spent_minutes: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// True when the due date has passed and the task is not in a
    /// terminal status.
    pub fn is_overdue(&self, now: DateTime<Utc>) -> bool {
        match self.due_date {
            Some(due) => {
                due < now
                    && self.status != TaskStatus::Done
                    && self.status != TaskStatus::Cancelled
            }
            None => false,
        }
    }
}

/// Public task representation, with the computed `overdue` flag.
#[derive(Debug, Clone, Serialize)]
pub struct TaskResponse {
    #[serde(flatten)]
    pub task: Task,
    pub overdue: bool,
}

impl From<&Task> for TaskResponse {
    fn from(task: &Task) -> Self {
        let overdue = task.is_overdue(Utc::now());
        Self {
            task: task.clone(),
            overdue,
        }
    }
}

pub type TasksStore = Arc<DashMap<Uuid, Task>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_transitions_and_parsing() {
        assert!(
            TaskStatus::Todo
                .allowed_next()
                .contains(&TaskStatus::InProgress)
        );
        assert!(!TaskStatus::Todo.allowed_next().contains(&TaskStatus::Done));
        assert!(
            TaskStatus::InProgress
                .allowed_next()
                .contains(&TaskStatus::Review)
        );
        assert!(TaskStatus::Done.allowed_next().is_empty());
        assert!(TaskStatus::Cancelled.allowed_next().is_empty());
        assert_eq!(TaskStatus::parse("in_progress"), Some(TaskStatus::InProgress));
        assert_eq!(TaskStatus::parse("done"), Some(TaskStatus::Done));
        assert_eq!(TaskStatus::parse("bogus"), None);
        assert_eq!(TaskStatus::Done.as_str(), "done");
        assert_eq!(TaskStatus::Review.as_str(), "review");
        assert_eq!(TaskStatus::default(), TaskStatus::Todo);
    }

    #[test]
    fn priority_ranking_and_defaults() {
        assert!(TaskPriority::Low.rank() < TaskPriority::Medium.rank());
        assert!(TaskPriority::Medium.rank() < TaskPriority::High.rank());
        assert!(TaskPriority::High.rank() < TaskPriority::Urgent.rank());
        assert_eq!(TaskPriority::default(), TaskPriority::Medium);
    }

    #[test]
    fn event_and_permission_constants() {
        assert_eq!(TASK_CREATED, "task.created");
        assert_eq!(TASK_ASSIGNED, "task.assigned");
        assert_eq!(TASK_UNASSIGNED, "task.unassigned");
        assert_eq!(TASK_STATUS_CHANGED, "task.status.changed");
        assert_eq!(TASK_COMPLETED, "task.completed");
        assert_eq!(TASK_COMMENT_ADDED, "task.comment.added");
        assert_eq!(TASK_DELETED, "task.deleted");
        assert_eq!(PERM_TASKS_READ, "tasks:read");
        assert_eq!(PERM_TASKS_MANAGE, "tasks:manage");
        assert_ne!(PERM_TASKS_READ, PERM_TASKS_MANAGE);
    }
}
