// timetracker — Request shapes for the HTTP handlers.

use super::*;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct StartTimerRequest {
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: Option<String>,
    #[serde(default)]
    pub billable: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateEntryRequest {
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    #[serde(default)]
    pub billable: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEntryRequest {
    pub project_id: Option<Option<Uuid>>,
    pub task_id: Option<Option<Uuid>>,
    pub description: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub billable: Option<bool>,
    /// Required when editing an entry: why it changed.
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteEntryRequest {
    /// Required when deleting an entry: why it was removed.
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct ReviewEntryRequest {
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReportActivityRequest {
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub activity_score: i64,
    /// Idle seconds observed by the agent in this interval.
    #[serde(default)]
    pub idle_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct ReportUsageRequest {
    pub application_name: String,
    pub domain: Option<String>,
    /// SHA-256 hex of the window title. The raw title is never
    /// accepted: it may contain secrets.
    pub window_title_hash: Option<String>,
    pub category: Option<UsageCategory>,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
pub struct AgentHeartbeatRequest {
    /// Idle seconds observed by the agent since the last heartbeat.
    #[serde(default)]
    pub idle_secs: i64,
    pub activity_score: Option<i64>,
    pub timer_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct ScreenshotUploadQuery {
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub captured_at: Option<DateTime<Utc>>,
    /// True when the agent blurred the image before upload.
    #[serde(default)]
    pub blurred: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateRateRequest {
    pub user_id: String,
    pub currency: Option<String>,
    pub rate: f64,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRateRequest {
    pub currency: Option<String>,
    pub rate: Option<f64>,
    pub effective_from: Option<DateTime<Utc>>,
    pub effective_to: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTimesheetRequest {
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    /// Entry ids to include. When empty, every pending entry of
    /// the caller in the period is included.
    #[serde(default)]
    pub entry_ids: Vec<Uuid>,
    /// When true, a manager may create a timesheet for another
    /// user of their team (`user_id` required).
    pub user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReviewTimesheetRequest {
    pub note: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub idle_threshold_secs: Option<i64>,
    pub retention_days: Option<i64>,
    pub overtime_hours_per_week: Option<f64>,
    pub overtime_multiplier: Option<f64>,
    pub low_activity_threshold: Option<i64>,
    pub storage_target: Option<String>,
    pub default_currency: Option<String>,
    pub auto_approve_timer_entries: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateMySettingsRequest {
    pub screenshots_enabled: Option<bool>,
    pub blur_screenshots: Option<bool>,
    pub idle_threshold_secs: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBuyerSettingsRequest {
    pub screenshots_enabled: Option<bool>,
    pub screenshot_interval_secs: Option<i64>,
    pub activity_level: Option<ActivityLevel>,
    pub idle_timeout_secs: Option<i64>,
    pub blur_screenshots: Option<bool>,
    pub allowed_apps: Option<Vec<String>>,
    pub blocked_apps: Option<Vec<String>>,
    pub allowed_websites: Option<Vec<String>>,
    pub blocked_websites: Option<Vec<String>>,
    pub default_hourly_rate: Option<f64>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub working_days: Option<Vec<i64>>,
    pub retention_days: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AddStaffRequest {
    pub user_id: String,
    pub role: Option<TrackerRole>,
    pub manager_id: Option<String>,
    pub team_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateStaffRequest {
    pub role: Option<TrackerRole>,
    pub manager_id: Option<Option<String>>,
    pub screenshots_enabled: Option<Option<bool>>,
    pub blur_screenshots: Option<Option<bool>>,
    pub idle_threshold_secs: Option<Option<i64>>,
    pub activity_level: Option<Option<ActivityLevel>>,
    pub timezone: Option<Option<String>>,
    pub working_hours_start: Option<Option<String>>,
    pub working_hours_end: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInviteRequest {
    pub email: String,
    pub role: Option<TrackerRole>,
    pub team_id: Option<Uuid>,
}

#[derive(Debug, Deserialize)]
pub struct CreateTeamRequest {
    pub name: String,
    pub description: Option<String>,
    pub manager_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTeamRequest {
    pub name: Option<String>,
    pub description: Option<Option<String>>,
    pub manager_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AddTeamMemberRequest {
    pub user_id: String,
    pub role: Option<TrackerRole>,
}
