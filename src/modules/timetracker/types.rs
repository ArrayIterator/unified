// timetracker - Shared types: constants, enums, records, and request/response shapes.

use super::*;
use unified_core::json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

/// Capability names checked through the `role.check` hook.
pub const PERM_READ: &str = "timetracker:read";

pub const PERM_WRITE: &str = "timetracker:write";

pub const PERM_MANAGE: &str = "timetracker:manage";

pub const PERM_APPROVE: &str = "timetracker:approve";

pub const PERM_VIEW_TEAM: &str = "timetracker:view_team";

pub const PERM_VIEW_TEAM_SCREENSHOTS: &str = "timetracker:view_team_screenshots";

pub const PERM_MANAGE_RATES: &str = "timetracker:manage_rates";

pub const PERM_MANAGE_SETTINGS: &str = "timetracker:manage_settings";

pub const PERM_PAYROLL: &str = "timetracker:payroll";

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_DESCRIPTION_LEN: usize = 2000;

pub(crate) const MAX_REASON_LEN: usize = 1000;

pub(crate) const MAX_NOTE_LEN: usize = 2000;

pub(crate) const MAX_APP_NAME_LEN: usize = 200;

pub(crate) const MAX_CURRENCY_LEN: usize = 3;

pub(crate) const MAX_TARGET_LEN: usize = 128;

/// A manual entry or a timer-derived entry may cover at most 24 hours.
pub(crate) const MAX_ENTRY_SECONDS: i64 = 86_400;

/// An activity / usage interval may cover at most 24 hours.
pub(crate) const MAX_INTERVAL_SECONDS: i64 = 86_400;

/// Screenshot upload cap (5 MiB). Larger captures must be downscaled
/// or recompressed by the agent before upload.
pub(crate) const MAX_SCREENSHOT_BYTES: usize = 5 * 1024 * 1024;

/// Signed-URL lifetime for a screenshot read (15 minutes).
pub(crate) const SCREENSHOT_SIGNED_URL_SECS: u64 = 900;

/// Default idle threshold in seconds (5 minutes) before a running
/// timer is auto-paused.
const DEFAULT_IDLE_THRESHOLD_SECS: i64 = 300;

/// Default screenshot retention in days.
const DEFAULT_RETENTION_DAYS: i64 = 90;

/// Default overtime threshold in hours per ISO week.
const DEFAULT_OVERTIME_HOURS_PER_WEEK: f64 = 40.0;

/// Default overtime pay multiplier.
const DEFAULT_OVERTIME_MULTIPLIER: f64 = 1.5;

/// Default currency for rates created without one.
const DEFAULT_CURRENCY: &str = "USD";

/// Default storage target for screenshot bytes.
const DEFAULT_STORAGE_TARGET: &str = "timetracker-screenshots";

/// Presence window: activity or a running timer newer than this is
/// "online now" on the team dashboard.
pub(crate) const ONLINE_WINDOW_SECS: i64 = 300;

/// Average activity below this score flags a user as low-activity.
const DEFAULT_LOW_ACTIVITY_THRESHOLD: i64 = 30;

/// Per-key screenshot upload rate limit (per minute).
pub(crate) const SCREENSHOT_RATE_PER_MINUTE: u32 = 60;

/// Per-key agent report rate limit (per minute).
pub(crate) const AGENT_RATE_PER_MINUTE: u32 = 300;

// Types

/// Lifecycle of a timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimerStatus {
    Running,
    Paused,
    Stopped,
}

/// Approval state of a time entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryApproval {
    Pending,
    Approved,
    Rejected,
}

/// Lifecycle of a timesheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimesheetStatus {
    Draft,
    Submitted,
    Approved,
    Rejected,
}

/// Usage category for one application / website interval. `other`
/// is the fallback for anything the agent cannot classify.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageCategory {
    Work,
    Communication,
    Development,
    Browsing,
    Social,
    Entertainment,
    Other,
}

impl Default for UsageCategory {
    fn default() -> Self {
        Self::Other
    }
}

/// One running or stopped timer. A timer is the live counterpart of
/// a time entry: stopping it creates the entry (see
/// `stop_timer_inner`). `paused_total_secs` accumulates every pause
/// so the billed duration never includes paused time.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeTrackerTimer {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub description: Option<String>,
    pub billable: bool,
    pub status: TimerStatus,
    pub started_at: DateTime<Utc>,
    /// Accumulated paused seconds (excludes the current pause, if any).
    pub paused_total_secs: i64,
    /// When the current pause started, or `None` while running.
    pub paused_at: Option<DateTime<Utc>>,
    /// Idle seconds reported by the agent while this timer ran.
    pub idle_total_secs: i64,
    pub stopped_at: Option<DateTime<Utc>>,
    /// The entry created when this timer stopped, if any.
    pub entry_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TimeTrackerTimer {
    /// Billable seconds so far: elapsed wall time minus paused time.
    /// While paused, the current pause counts as paused too.
    pub fn elapsed_secs(&self, now: DateTime<Utc>) -> i64 {
        let end = self.stopped_at.unwrap_or(now);
        let mut elapsed = (end - self.started_at).num_seconds().max(0);
        elapsed -= self.paused_total_secs;
        if let Some(paused_at) = self.paused_at {
            let pause_end = self.stopped_at.unwrap_or(now);
            elapsed -= (pause_end - paused_at).num_seconds().max(0);
        }
        elapsed.max(0)
    }
}

/// One tracked block of time: created manually, or created by
/// stopping a timer. `approval` gates payroll: only `approved`
/// entries are payable. `edit_reason` / `delete_reason` keep the
/// audit trail for manual changes; the values are also emitted on
/// the hook bus.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimeEntry {
    pub id: Uuid,
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub timer_id: Option<Uuid>,
    pub description: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    /// Duration in seconds. For manual entries it equals
    /// `ended_at - started_at` (validated on write). For
    /// timer-derived entries it is net of pauses: wall time minus
    /// paused time (see `TimeTrackerTimer::elapsed_secs`), so it is
    /// shorter than `ended_at - started_at` whenever the timer was
    /// paused.
    pub duration_secs: i64,
    pub billable: bool,
    pub approval: EntryApproval,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub rejection_note: Option<String>,
    /// Why this manual entry was created (manual entries only).
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl TimeEntry {
    /// The shared `ProjectTimeEntry` shape (the projects module's
    /// time-entry record) for this entry, as JSON. The projects
    /// module owns `ProjectTimeEntry`; this conversion is the one
    /// place that maps a timetracker entry onto it, so both
    /// modules read the same fields: string ids, hours from
    /// `duration_secs / 3600`, `billed: false` (billing marks it
    /// through the projects module), `invoice_id: null`,
    /// `timer_running: false` (a logged entry is finished).
    /// Entries without a project or task carry `null` for those
    /// ids. Included as `"project_time_entry"` in every
    /// `timetracker.time.logged` emit so the projects module can
    /// store the entry without re-deriving it - and without a
    /// second billing path: billing prices
    /// `timetracker.time.logged` only.
    pub fn to_project_time_entry_json(&self) -> Value {
        json!({
            "id": self.id.to_string(),
            "task_id": self.task_id.map(|id| id.to_string()),
            "project_id": self.project_id.map(|id| id.to_string()),
            "user_id": self.user_id,
            "description": self.description,
            "hours": round4(self.duration_secs as f64 / 3600.0),
            "billable": self.billable,
            "billed": false,
            "invoice_id": Value::Null,
            "started_at": self.started_at.to_rfc3339(),
            "ended_at": self.ended_at.to_rfc3339(),
            "timer_running": false,
            "created_at": self.created_at.to_rfc3339(),
            "updated_at": self.updated_at.to_rfc3339(),
        })
    }
}

/// One agent-reported activity sample: an activity score (0-100)
/// for one interval, plus the idle seconds the agent observed in
/// that interval. Idle time is recorded, never silently discarded:
/// it stays on the sample and feeds the timer auto-pause.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivitySample {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    /// Activity score, 0 (idle) to 100 (fully active).
    pub activity_score: i64,
    /// Idle seconds observed by the agent in this interval.
    pub idle_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
    /// True when this sample triggered an idle auto-pause.
    pub triggered_auto_pause: bool,
    pub created_at: DateTime<Utc>,
}

/// One agent-reported application / website usage interval. Only
/// the hash of the window title is stored - never the raw title,
/// which may contain secrets (document names, message previews,
/// credentials in a tab title). The agent must hash the title
/// (SHA-256 hex) before sending it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppUsageRecord {
    pub id: Uuid,
    pub user_id: String,
    pub application_name: String,
    /// Website domain for browser usage (for example
    /// `"github.com"`), or `None` for desktop applications. Used
    /// for the buyer's allowed / blocked website lists.
    pub domain: Option<String>,
    /// SHA-256 hex of the window title (64 lowercase hex chars).
    pub window_title_hash: Option<String>,
    pub category: UsageCategory,
    pub duration_secs: i64,
    pub interval_start: DateTime<Utc>,
    pub interval_end: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One screenshot. Metadata only: the bytes live in the storage
/// module under (`storage_target`, `storage_key`). `blurred` records
/// whether the agent blurred the image before upload (see the
/// module header: the server records the flag, it cannot verify
/// pixel content).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenshotRecord {
    pub id: Uuid,
    pub user_id: String,
    pub timer_id: Option<Uuid>,
    pub project_id: Option<Uuid>,
    pub task_id: Option<Uuid>,
    pub storage_target: String,
    pub storage_key: String,
    /// SHA-256 hex of the uploaded bytes (integrity / dedup aid).
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: u64,
    pub blurred: bool,
    pub captured_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One hourly rate for one user. `rate` is money (NUMERIC(19,4) in
/// the migration; `f64` here, following the billing module's
/// in-memory convention). `currency` is an ISO 4217 code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourlyRate {
    pub id: Uuid,
    pub user_id: String,
    pub currency: String,
    /// Hourly rate, rounded to 4 decimal places, never negative.
    pub rate: f64,
    pub effective_from: DateTime<Utc>,
    pub effective_to: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One timesheet: a set of entries for one user over one period,
/// moving draft -> submitted -> approved / rejected. Approving a
/// timesheet also approves its still-pending entries; rejecting it
/// leaves the entries pending with the rejection note on the sheet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Timesheet {
    pub id: Uuid,
    pub user_id: String,
    pub period_start: DateTime<Utc>,
    pub period_end: DateTime<Utc>,
    pub status: TimesheetStatus,
    pub entry_ids: Vec<Uuid>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub review_note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Global settings for the module (admin-managed).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackerSettings {
    /// Idle seconds before a running timer auto-pauses.
    pub idle_threshold_secs: i64,
    /// Screenshot retention in days before the purge deletes them.
    pub retention_days: i64,
    /// Overtime threshold in hours per ISO week.
    pub overtime_hours_per_week: f64,
    /// Overtime pay multiplier (1.5 means time-and-a-half).
    pub overtime_multiplier: f64,
    /// Average activity below this score flags low activity.
    pub low_activity_threshold: i64,
    /// Storage target that holds screenshot bytes.
    pub storage_target: String,
    /// Default currency for new rates.
    pub default_currency: String,
    /// When true, timer-derived entries skip approval (they start
    /// `approved`). Default false: everything needs approval.
    pub auto_approve_timer_entries: bool,
    pub updated_at: DateTime<Utc>,
}

impl Default for TrackerSettings {
    fn default() -> Self {
        Self {
            idle_threshold_secs: DEFAULT_IDLE_THRESHOLD_SECS,
            retention_days: DEFAULT_RETENTION_DAYS,
            overtime_hours_per_week: DEFAULT_OVERTIME_HOURS_PER_WEEK,
            overtime_multiplier: DEFAULT_OVERTIME_MULTIPLIER,
            low_activity_threshold: DEFAULT_LOW_ACTIVITY_THRESHOLD,
            storage_target: DEFAULT_STORAGE_TARGET.to_string(),
            default_currency: DEFAULT_CURRENCY.to_string(),
            auto_approve_timer_entries: false,
            updated_at: Utc::now(),
        }
    }
}

/// One user's own tracking settings, including screenshot consent.
/// Screenshots stay disabled until the user opts in here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserTrackerSettings {
    pub user_id: String,
    /// Consent to capture screenshots. Default false.
    pub screenshots_enabled: bool,
    /// Ask the agent to blur screenshots before upload.
    pub blur_screenshots: bool,
    /// Personal idle threshold override, or `None` for the global one.
    pub idle_threshold_secs: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

impl UserTrackerSettings {
    pub(crate) fn for_user(user_id: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            screenshots_enabled: false,
            blur_screenshots: false,
            idle_threshold_secs: None,
            updated_at: Utc::now(),
        }
    }
}

/// One payroll line: one user's payable time in one period.
#[derive(Debug, Clone, Serialize)]
pub struct PayrollLine {
    pub user_id: String,
    pub currency: String,
    /// Approved hours in the period (billable and non-billable).
    pub total_hours: f64,
    pub billable_hours: f64,
    pub non_billable_hours: f64,
    pub regular_hours: f64,
    pub overtime_hours: f64,
    pub hourly_rate: f64,
    pub regular_pay: f64,
    pub overtime_pay: f64,
    pub total_pay: f64,
}

/// One row of a daily / weekly report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportRow {
    pub user_id: String,
    pub project_id: Option<Uuid>,
    pub date: String,
    pub total_secs: i64,
    pub billable_secs: i64,
    pub non_billable_secs: i64,
    pub entry_count: usize,
    /// Mean activity score over the period, or `None` when no
    /// activity was reported.
    pub avg_activity: Option<f64>,
}

/// One team member's status on the monitoring dashboard.
#[derive(Debug, Clone, Serialize)]
pub struct TeamMemberStatus {
    pub user_id: String,
    pub online_now: bool,
    pub active_timer_id: Option<Uuid>,
    pub today_secs: i64,
    pub avg_activity_today: Option<f64>,
    pub low_activity: bool,
    pub last_activity_at: Option<DateTime<Utc>>,
}

// Staff management, teams, and per-buyer settings (batch addition).
//
// Staff are NOT duplicated here: a staff member is a user (from the
// users module) referenced by id, plus tracker-scoped records in
// this module (team membership, tracker role, per-staff settings,
// invites). The users module stays the source of truth for identity;
// this module only records how that user is organised inside the
// tracker and which company they belong to for tracking purposes.
// Company membership is verified through the hook bus (see
// `require_company_member`): the caller claims a company with the
// `X-Company-Id` header, the company module answers `company.exists`,
// and this module answers `timetracker.company.membership.check`
// with `{"member": true}` from its own staff map and team
// memberships (see `on_company_membership_check` in module.rs).
// Both checks fail closed - an unanswered membership check means
// the caller is not treated as a member of that company, so a
// company admin can only ever manage their own company's staff.
//
// Per-buyer settings: the product ships with defaults (the global
// `TrackerSettings` above); each purchasing company overrides its
// own copy in `BuyerSettings`. Behaviour (screenshots, activity,
// idle timeout, retention) always reads the RESOLVED settings for
// the acting user - staff override, then buyer setting, then global
// default - never the global settings alone. See
// `resolve_settings_for_user`.

/// Capability names for staff and buyer-settings management.
pub const PERM_MANAGE_STAFF: &str = "timetracker:manage_staff";

pub const PERM_MANAGE_BUYER_SETTINGS: &str = "timetracker:manage_buyer_settings";

/// Tracker-scoped role of a staff member inside TimeTracker. This
/// is NOT a platform role: it only governs what the person may do
/// inside the tracker (their platform capabilities still come from
/// the roles module). `admin` manages the company's staff and buyer
/// settings; `manager` sees their team's time and screenshots;
/// `member` tracks their own time; `viewer` reads reports only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrackerRole {
    Admin,
    Manager,
    Member,
    Viewer,
}

impl Default for TrackerRole {
    fn default() -> Self {
        Self::Member
    }
}

/// How much activity the buyer wants tracked for their company.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityLevel {
    /// No activity, usage, or screenshot collection at all; the
    /// agent endpoints reject reports for this company.
    Off,
    /// Activity scores only: app usage and screenshots are rejected.
    Minimal,
    /// Full tracking: activity scores, app usage, and screenshots.
    Full,
}

impl Default for ActivityLevel {
    fn default() -> Self {
        Self::Full
    }
}

/// One team (group) inside one company.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Team {
    pub id: Uuid,
    pub company_id: Uuid,
    pub name: String,
    pub description: Option<String>,
    /// The team's manager (a user id), or `None`.
    pub manager_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One membership of a user in a team, with a tracker role.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TeamMember {
    pub team_id: Uuid,
    pub user_id: String,
    pub role: TrackerRole,
    pub joined_at: DateTime<Utc>,
}

/// One staff record: a user attached to one company for tracking,
/// with their tracker role, direct manager, and per-staff setting
/// overrides (each `Option` falls through to the buyer setting,
/// then the global default, when `None`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaffSettings {
    pub user_id: String,
    pub company_id: Uuid,
    pub role: TrackerRole,
    /// Direct manager (a user id), or `None`.
    pub manager_id: Option<String>,
    pub screenshots_enabled: Option<bool>,
    pub blur_screenshots: Option<bool>,
    pub idle_threshold_secs: Option<i64>,
    pub activity_level: Option<ActivityLevel>,
    /// IANA timezone name for this staff member's reports.
    pub timezone: Option<String>,
    /// Working day start / end as `"HH:MM"` (24-hour clock).
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One invitation to join a company as tracker staff. The invitee
/// is identified by email (they may not be a platform user yet);
/// when the users module emits `user.created` with a matching
/// email, a pending invite is accepted automatically (see
/// `on_user_created`). The token is a random opaque string shown
/// only at creation time and stored as its SHA-256 hash.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaffInvite {
    pub id: Uuid,
    pub company_id: Uuid,
    pub email: String,
    pub role: TrackerRole,
    pub team_id: Option<Uuid>,
    pub invited_by: String,
    /// SHA-256 hex of the invite token. The raw token is returned
    /// only in the creation response and never stored.
    pub token_hash: String,
    /// `pending`, `accepted`, `expired`, `revoked`.
    pub status: String,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub accepted_user_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Per-buyer (per purchasing company) settings. Every field is an
/// override: `None` (or an empty list) falls back to the global
/// `TrackerSettings` / product default. A company admin edits only
/// their own company's row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BuyerSettings {
    pub company_id: Uuid,
    /// Master switch for screenshots in this company.
    pub screenshots_enabled: Option<bool>,
    /// Seconds between automatic screenshots (agent hint).
    pub screenshot_interval_secs: Option<i64>,
    pub activity_level: Option<ActivityLevel>,
    /// Idle seconds before a running timer auto-pauses.
    pub idle_timeout_secs: Option<i64>,
    pub blur_screenshots: Option<bool>,
    /// When non-empty, only these applications may be reported.
    pub allowed_apps: Vec<String>,
    /// Applications that must never be reported.
    pub blocked_apps: Vec<String>,
    /// When non-empty, only these website domains may be reported.
    pub allowed_websites: Vec<String>,
    /// Website domains that must never be reported.
    pub blocked_websites: Vec<String>,
    /// Default hourly rate for this buyer's payroll (money,
    /// NUMERIC(19,4) in the migration; `f64` here, following the
    /// billing module's in-memory convention).
    pub default_hourly_rate: Option<f64>,
    /// IANA timezone name for this company's reports.
    pub timezone: Option<String>,
    /// Working day start / end as `"HH:MM"` (24-hour clock).
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    /// Working days as ISO weekday numbers (1 = Monday .. 7 =
    /// Sunday). Empty means the product default (Monday-Friday).
    pub working_days: Vec<i64>,
    /// Screenshot / data retention in days for this company.
    pub retention_days: Option<i64>,
    pub updated_at: DateTime<Utc>,
}

impl BuyerSettings {
    pub(crate) fn for_company(company_id: Uuid) -> Self {
        Self {
            company_id,
            screenshots_enabled: None,
            screenshot_interval_secs: None,
            activity_level: None,
            idle_timeout_secs: None,
            blur_screenshots: None,
            allowed_apps: Vec::new(),
            blocked_apps: Vec::new(),
            allowed_websites: Vec::new(),
            blocked_websites: Vec::new(),
            default_hourly_rate: None,
            timezone: None,
            working_hours_start: None,
            working_hours_end: None,
            working_days: Vec::new(),
            retention_days: None,
            updated_at: Utc::now(),
        }
    }
}

/// Resolved settings for one user: every field concrete, after
/// merging staff override -> buyer setting -> global default. This
/// is what the screenshot, activity, idle, and retention behaviour
/// reads; nothing in this module reads `TrackerSettings` directly
/// for a user-scoped decision anymore.
#[derive(Debug, Clone)]
pub struct ResolvedSettings {
    pub company_id: Option<Uuid>,
    pub screenshots_enabled: bool,
    pub screenshot_interval_secs: i64,
    pub activity_level: ActivityLevel,
    pub idle_threshold_secs: i64,
    pub blur_screenshots: bool,
    pub allowed_apps: Vec<String>,
    pub blocked_apps: Vec<String>,
    pub allowed_websites: Vec<String>,
    pub blocked_websites: Vec<String>,
    pub timezone: String,
    pub working_hours_start: Option<String>,
    pub working_hours_end: Option<String>,
    pub retention_days: i64,
}

/// One staff member plus their activity overview, as returned by
/// the staff list / detail endpoints.
#[derive(Debug, Clone, Serialize)]
pub struct StaffOverview {
    pub user_id: String,
    pub company_id: Uuid,
    pub role: TrackerRole,
    pub manager_id: Option<String>,
    pub team_ids: Vec<Uuid>,
    pub active_timer_id: Option<Uuid>,
    pub today_secs: i64,
    pub week_secs: i64,
    pub avg_activity_today: Option<f64>,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub screenshots_enabled: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn timer_at(start: DateTime<Utc>, stopped: Option<DateTime<Utc>>) -> TimeTrackerTimer {
        TimeTrackerTimer {
            id: Uuid::new_v4(),
            user_id: "u1".to_string(),
            project_id: None,
            task_id: None,
            description: None,
            billable: true,
            status: TimerStatus::Running,
            started_at: start,
            paused_total_secs: 0,
            paused_at: None,
            idle_total_secs: 0,
            stopped_at: stopped,
            entry_id: None,
            created_at: start,
            updated_at: start,
        }
    }

    #[test]
    fn timer_elapsed_secs_accounts_for_pauses() {
        let start = "2026-11-16T10:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let stop = "2026-11-16T11:00:00Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(timer_at(start, Some(stop)).elapsed_secs(stop), 3600);
        let mut paused_total = timer_at(start, Some(stop));
        paused_total.paused_total_secs = 600;
        assert_eq!(paused_total.elapsed_secs(stop), 3000);
        // Currently paused: the open pause counts too.
        let mut open_pause = timer_at(start, None);
        open_pause.status = TimerStatus::Paused;
        open_pause.paused_at = Some(
            "2026-11-16T10:30:00Z".parse::<DateTime<Utc>>().unwrap(),
        );
        assert_eq!(open_pause.elapsed_secs(stop), 1800);
        // Never negative.
        let mut over_paused = timer_at(start, Some(stop));
        over_paused.paused_total_secs = 999_999;
        assert_eq!(over_paused.elapsed_secs(stop), 0);
    }

    #[test]
    fn entry_to_project_json_shape() {
        let start = "2026-11-16T09:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let end = "2026-11-16T11:00:00Z".parse::<DateTime<Utc>>().unwrap();
        let entry = TimeEntry {
            id: Uuid::new_v4(),
            user_id: "u1".to_string(),
            project_id: None,
            task_id: None,
            timer_id: None,
            description: "Deep work".to_string(),
            started_at: start,
            ended_at: end,
            duration_secs: 7200,
            billable: true,
            approval: EntryApproval::Approved,
            approved_by: None,
            approved_at: None,
            rejection_note: None,
            reason: None,
            created_at: start,
            updated_at: start,
        };
        let json = entry.to_project_time_entry_json();
        assert_eq!(json["hours"].as_f64(), Some(2.0));
        assert_eq!(json["billed"].as_bool(), Some(false));
        assert_eq!(json["timer_running"].as_bool(), Some(false));
        assert_eq!(json["billable"].as_bool(), Some(true));
        assert_eq!(json["description"], "Deep work");
        assert_eq!(json["project_id"], Value::Null);
    }
}
