// Diesel models for the timetracker domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// TimeTracker (migration 000032_timetracker; batch 2 notes §4).
// Field names/types mirror the in-module structs in timetracker.rs.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_timers)]
pub struct TimeTrackerTimer {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: Option<String>,
    pub billable: bool,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub paused_total_secs: i64,
    pub paused_at: Option<DateTime<Utc>>,
    pub idle_total_secs: i64,
    pub stopped_at: Option<DateTime<Utc>>,
    pub entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_timers)]
pub struct NewTimeTrackerTimer {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: Option<String>,
    pub status: String,
    pub paused_at: Option<DateTime<Utc>>,
    pub stopped_at: Option<DateTime<Utc>>,
    pub entry_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_entries)]
pub struct TimeEntry {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub timer_id: Option<Uuid>,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: i64,
    pub billable: bool,
    pub approval: String,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub rejection_note: Option<String>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_entries)]
pub struct NewTimeEntry {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub timer_id: Option<Uuid>,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub duration_secs: i64,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub rejection_note: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_activity)]
pub struct ActivitySample {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub activity_score: i32,
    pub idle_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
    pub triggered_auto_pause: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_activity)]
pub struct NewActivitySample {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub activity_score: i32,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_app_usage)]
pub struct AppUsageRecord {
    pub id: Uuid,
    pub user_id: String,
    pub application_name: String,
    pub domain: Option<String>,
    pub window_title_hash: Option<String>,
    pub category: String,
    pub duration_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_app_usage)]
pub struct NewAppUsageRecord {
    pub id: Uuid,
    pub user_id: String,
    pub application_name: String,
    pub domain: Option<String>,
    pub window_title_hash: Option<String>,
    pub duration_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_screenshots)]
pub struct ScreenshotRecord {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub blurred: bool,
    pub captured_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_screenshots)]
pub struct NewScreenshotRecord {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = timetracker_rates)]
pub struct HourlyRate {
    pub id: Uuid,
    pub user_id: String,
    pub currency: String,
    pub rate: PgNumeric,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = timetracker_rates)]
pub struct NewHourlyRate {
    pub id: Uuid,
    pub user_id: String,
    pub currency: String,
    pub rate: PgNumeric,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_timesheets)]
pub struct Timesheet {
    pub id: Uuid,
    pub user_id: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub status: String,
    pub entry_ids: Vec<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_timesheets)]
pub struct NewTimesheet {
    pub id: Uuid,
    pub user_id: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
}
