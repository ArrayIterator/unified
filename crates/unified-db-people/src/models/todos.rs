// Diesel models for the todos domain of the unified-db-people bounded context.
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
// Todos (migration 000035_todos; batch 2 notes §7).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_lists)]
pub struct TodoListRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub archived: bool,
    pub sort_order: i32,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_lists)]
pub struct NewTodoListRow {
    pub id: Uuid,
    pub owner_id: String,
    pub name: String,
    pub color: String,
    pub icon: String,
    pub sort_order: i32,
    pub is_default: bool,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todos)]
pub struct TodoRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: String,
    pub status: String,
    pub tags: Value,
    pub subtasks: Value,
    pub attachments: Value,
    pub recurrence_frequency: Option<String>,
    pub recurrence_interval: i32,
    pub recurrence_count: Option<i32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<i32>,
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todos)]
pub struct NewTodoRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub title: String,
    pub notes: Option<String>,
    pub due_at: Option<DateTime<Utc>>,
    pub timezone: String,
    pub priority: String,
    pub status: String,
    pub tags: Value,
    pub subtasks: Value,
    pub attachments: Value,
    pub recurrence_frequency: Option<String>,
    pub recurrence_interval: i32,
    pub recurrence_count: Option<i32>,
    pub recurrence_until: Option<DateTime<Utc>>,
    pub recurrence_remaining: Option<i32>,
    pub recurrence_group_id: Option<Uuid>,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_subtasks)]
pub struct TodoSubtaskRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_subtasks)]
pub struct NewTodoSubtaskRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub title: String,
    pub done: bool,
    pub position: i32,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_shares)]
pub struct TodoShareRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_shares)]
pub struct NewTodoShareRow {
    pub id: Uuid,
    pub list_id: Uuid,
    pub owner_id: String,
    pub shared_with_user_id: String,
    pub permission: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_completion_log)]
pub struct TodoCompletionLogRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    pub action: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_completion_log)]
pub struct NewTodoCompletionLogRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub user_id: String,
    pub action: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = todo_reminders)]
pub struct TodoReminderRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = todo_reminders)]
pub struct NewTodoReminderRow {
    pub id: Uuid,
    pub todo_id: Uuid,
    pub owner_id: String,
    pub offset_minutes: i64,
    pub remind_at: DateTime<Utc>,
    pub due_at: DateTime<Utc>,
    pub status: String,
    pub scheduler_job_id: Option<String>,
    pub scheduler_status: String,
    pub sent_at: Option<DateTime<Utc>>,
}
