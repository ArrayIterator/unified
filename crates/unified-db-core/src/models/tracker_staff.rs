// Diesel models for the tracker_staff domain of the unified-db-core bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = timetracker_settings)]
pub struct TrackerSettings {
    pub id: Uuid,
    pub scope: String,
    pub idle_threshold_secs: i64,
    pub retention_days: i64,
    pub overtime_hours_per_week: PgNumeric,
    pub overtime_multiplier: PgNumeric,
    pub low_activity_threshold: i64,
    pub storage_target: String,
    pub default_currency: String,
    pub auto_approve_timer_entries: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = timetracker_settings)]
pub struct NewTrackerSettings {
    pub id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_user_settings)]
pub struct UserTrackerSettings {
    pub id: Uuid,
    pub user_id: String,
    pub screenshots_enabled: bool,
    pub blur_screenshots: bool,
    pub idle_threshold_secs: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_user_settings)]
pub struct NewUserTrackerSettings {
    pub id: Uuid,
    pub user_id: String,
    pub idle_threshold_secs: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_teams)]
pub struct Team {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub manager_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_teams)]
pub struct NewTeam {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    pub manager_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_team_members)]
pub struct TeamMember {
    pub id: Uuid,
    pub team_id: Uuid,
    pub user_id: String,
    pub role: String,
    pub joined_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_team_members)]
pub struct NewTeamMember {
    pub id: Uuid,
    pub team_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_staff_settings)]
pub struct StaffSettings {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Uuid,
    pub role: String,
    pub manager_id: Option<String>,
    pub screenshots_enabled: Option<bool>,
    pub blur_screenshots: Option<bool>,
    pub idle_threshold_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_staff_settings)]
pub struct NewStaffSettings {
    pub id: Uuid,
    pub user_id: String,
    pub company_id: Uuid,
    pub manager_id: Option<String>,
    pub screenshots_enabled: Option<bool>,
    pub blur_screenshots: Option<bool>,
    pub idle_threshold_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = timetracker_staff_invites)]
pub struct StaffInvite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub email: String,
    pub role: String,
    pub team_id: Option<Uuid>,
    pub invited_by: String,
    pub token_hash: String,
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub accepted_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = timetracker_staff_invites)]
pub struct NewStaffInvite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub email: String,
    pub team_id: Option<Uuid>,
    pub invited_by: String,
    pub token_hash: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub accepted_user_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = timetracker_buyer_settings)]
pub struct BuyerSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub screenshots_enabled: Option<bool>,
    pub screenshot_interval_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub idle_timeout_secs: Option<i64>,
    pub blur_screenshots: Option<bool>,
    pub allowed_apps: Vec<String>,
    pub blocked_apps: Vec<String>,
    pub allowed_websites: Vec<String>,
    pub blocked_websites: Vec<String>,
    pub default_hourly_rate: Option<PgNumeric>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub working_days: Vec<i64>,
    pub retention_days: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = timetracker_buyer_settings)]
pub struct NewBuyerSettings {
    pub id: Uuid,
    pub company_id: Uuid,
    pub screenshots_enabled: Option<bool>,
    pub screenshot_interval_secs: Option<i64>,
    pub activity_level: Option<String>,
    pub idle_timeout_secs: Option<i64>,
    pub blur_screenshots: Option<bool>,
    pub default_hourly_rate: Option<PgNumeric>,
    pub timezone: Option<String>,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub retention_days: Option<i64>,
}
