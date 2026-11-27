// Diesel models for the tasks domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Tasks and system records (2026-11-22-000011_tasks_records).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::tasks` is the schema file-module (schema/tasks.rs),
// not the table: the `pub use tasks::*;` re-export in schema.rs
// cannot surface the inner `tasks` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::tasks::tasks`. Pointing `table_name` at the
// file-module (or the bare name `tasks`) makes every field fail
// with E0425 "cannot find type ... in module `tasks`".
#[diesel(table_name = crate::schema::tasks::tasks)]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub assignee_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    pub priority: String,
    pub status: String,
    pub due_date: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub tags: Value,
    pub estimate_minutes: Option<i32>,
    pub spent_minutes: i32,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::tasks::tasks)]
pub struct NewTask {
    pub id: Uuid,
    pub title: String,
    pub assignee_id: Option<Uuid>,
    pub reporter_id: Option<Uuid>,
    pub due_date: Option<DateTime<Utc>>,
    pub company_id: Option<Uuid>,
    pub project_name: Option<String>,
    pub estimate_minutes: Option<i32>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = task_comments)]
pub struct TaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = task_comments)]
pub struct NewTaskComment {
    pub id: Uuid,
    pub task_id: Uuid,
    pub author_id: Option<Uuid>,
    pub body: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = task_checklist_items)]
pub struct TaskChecklistItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub done: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = task_checklist_items)]
pub struct NewTaskChecklistItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub title: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = system_records)]
pub struct SystemRecord {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub summary: String,
    pub details: Value,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = system_records)]
pub struct NewSystemRecord {
    pub id: Uuid,
    pub seq: i64,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub ip: Option<String>,
}

// Logs and billing (2026-11-22-000012_logs_billing).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = app_logs)]
pub struct AppLog {
    pub id: Uuid,
    pub seq: i64,
    pub timestamp: DateTime<Utc>,
    pub level: String,
    pub kind: String,
    pub source: String,
    pub message: String,
    pub context: Value,
    pub request_id: Option<String>,
    pub user_id: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<i32>,
    pub duration_ms: Option<i64>,
    pub ip: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = app_logs)]
pub struct NewAppLog {
    pub id: Uuid,
    pub seq: i64,
    pub level: String,
    pub kind: String,
    pub source: String,
    pub message: String,
    pub request_id: Option<String>,
    pub user_id: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub status: Option<i32>,
    pub duration_ms: Option<i64>,
    pub ip: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = logs_settings)]
pub struct LogsSettings {
    pub key: String,
    pub retention_days: i32,
    pub max_entries: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = logs_settings)]
pub struct NewLogsSettings {
    pub key: String,
}
