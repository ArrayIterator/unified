// TimeTracker module — Time Doctor-style time tracking, activity
// monitoring, screenshots, payroll, and timesheet approvals.
//
// Routes live under `/api/v1/timetracker` (browser / dashboard
// callers, capability-checked through the roles module) and
// `/api/v1/timetracker/agent/*` (desktop-agent callers, API-key
// authenticated through the apikeys module):
//
//   Status:
//     GET  / — module status and counts.
//   Timers:
//     GET  /timers — list timers (own timers; managers may filter
//       by user_id with `timetracker:view_team`).
//     POST /timers/start — start a timer (one active timer per user).
//     POST /timers/{id}/stop, /pause, /resume.
//     GET  /timers/active — the caller's active timer, if any.
//   Manual time entries:
//     GET/POST /entries — list / create a manual entry (with a
//       reason; approval state starts at `pending`).
//     GET/PATCH/DELETE /entries/{id} — read / edit / delete (edit
//       and delete only while the entry is still `pending`).
//     POST /entries/{id}/approve, /reject — approval with a note.
//   Activity (agent-reported):
//     GET/POST /activity — activity samples: score 0-100 per
//       interval plus agent-reported idle seconds.
//     GET/POST /usage — application / website usage intervals.
//   Screenshots:
//     GET  /screenshots, /screenshots/{id} — metadata plus a
//       short-lived signed URL (never raw bytes in the envelope).
//     DELETE /screenshots/{id} — delete metadata and the stored object.
//     POST /screenshots/purge — retention purge (also the manual
//       trigger for the scheduled purge; see below).
//   Payroll and rates:
//     GET/POST /rates, GET/PATCH/DELETE /rates/{id} — hourly rates
//       per user (currency + amount).
//     GET  /payroll — payable hours x rate, with overtime.
//   Timesheets:
//     GET/POST /timesheets, GET /timesheets/{id}.
//     POST /timesheets/{id}/submit, /approve, /reject.
//   Reports and monitoring:
//     GET  /reports/daily, /reports/weekly, /reports/summary.
//     GET  /dashboard/team — online now, today's activity,
//       low-activity flags.
//   Settings:
//     GET/PUT /settings — global settings (admin).
//     GET/PUT /settings/me — the caller's own tracking settings,
//       including the screenshot consent flag.
//   Agent endpoints (API-key authenticated):
//     POST /agent/heartbeat — presence + idle report; may auto-pause
//       the caller's timer past the idle threshold.
//     POST /agent/activity, /agent/usage — agent-reported samples.
//     POST /agent/screenshots — screenshot upload (raw bytes).
//     GET  /agent/status — the agent's own settings, consent, and
//       active timer.
//     POST /agent/timers/start, /agent/timers/stop.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// the server side of time tracking: timers, manual entries,
// approvals, activity and app-usage records, screenshot metadata
// with bytes stored through the storage module, hourly rates,
// payroll computation, timesheets, reports, and team monitoring.
// What is NOT built here: the desktop agent itself. No binary
// captures keystrokes, mouse movement, application windows, or
// screen pixels in this codebase; the agent endpoints below only
// receive and validate what an external agent reports (activity
// scores, idle seconds, application names, window-title hashes,
// screenshot bytes). Until a desktop agent exists, the agent
// endpoints can be exercised by any HTTP client holding a scoped
// API key, which is exactly how the agent will call them later.
// Follow-up (not started here): build the desktop agent (per-OS
// capture of input activity, foreground application, and periodic
// screenshots) and have it call `/agent/*` with a scoped key.
//
// Project / task linkage is by id only: timers and entries carry
// optional `project_id` / `task_id` (UUIDs from the projects
// module) and this module never imports or calls the projects
// module. All cross-module contact goes through the hook bus.
//
// Events emitted on the hook bus:
//   - `timetracker.timer.started`, `timetracker.timer.stopped`,
//     `timetracker.timer.paused`, `timetracker.timer.resumed`,
//     `timetracker.timer.auto_paused` (idle threshold reached)
//   - `timetracker.time.logged` — one entry became billable-tracked
//     time (manual create and timer stop); the billing module reads
//     this to price billable hours (see the payload shape in the
//     `emit_time_logged` helper; billing does not listen yet — the
//     integrator adds a handler there).
//   - `timetracker.entry.updated`, `timetracker.entry.deleted`,
//     `timetracker.entry.approved`, `timetracker.entry.rejected`
//   - `timetracker.activity.reported`, `timetracker.usage.reported`
//   - `timetracker.screenshot.uploaded`,
//     `timetracker.screenshot.deleted`,
//     `timetracker.screenshots.purged`
//   - `timetracker.timesheet.created`,
//     `timetracker.timesheet.submitted`,
//     `timetracker.timesheet.approved`,
//     `timetracker.timesheet.rejected`
//   - `timetracker.rate.created`, `timetracker.rate.updated`,
//     `timetracker.rate.deleted`
//   - `timetracker.settings.updated`
// Events this module listens to:
//   - `project.task.created` — acknowledged only (the task id is
//     returned so the caller can verify the link; no record is
//     created here). The name is confirmed by the audit module's
//     audited-events list; the projects module file itself was not
//     present in the staging tree when this module was written, so
//     the handler tolerates any payload shape.
//   - `project.task.completed` — any running timer linked to that
//     task is stopped automatically (the work is done; the timer
//     must not keep running) and its entry is logged.
//   - `project.time.logged` — acknowledged only. The projects
//     module keeps its own time entries; importing them here would
//     double-count billable hours, so this handler records nothing
//     and only answers `{"acknowledged": true}`.
//   - `timetracker.screenshots.purge_requested` — the retention
//     purge itself (deletes expired screenshot metadata and the
//     stored objects). The scheduler is expected to emit this event
//     through a `timetracker.screenshots.purge` job kind; see the
//     note on `request_screenshot_purge` — the scheduler's
//     `KNOWN_KINDS` list does not contain that kind yet, so today
//     the purge runs through `POST /screenshots/purge` instead.
// Events this module emits to other modules:
//   - `role.check` with `{"role", "permission", "allowed"}` — the
//     roles module answers capability checks (same shape as the
//     ai module's `check_capability`).
//   - `timetracker.team.check` with `{"manager_id", "user_id"}`,
//     expecting `{"manages": true}` in the first hook result. A
//     manager may only see another user's time, activity, or
//     screenshots when the team module answers that the user is on
//     their team. Fail-closed: no answer means not allowed.
//   - `scheduler.enqueue` with `{"kind":
//     "timetracker.screenshots.purge", "payload": {}}` — asks the
//     scheduler to run the retention purge (see
//     `request_screenshot_purge`).
//   - `storage.object.uploaded` / `storage.object.deleted` are NOT
//     emitted here: the storage module owns those events and emits
//     them on its own object routes. Screenshot uploads go through
//     the storage provider directly (see `upload_screenshot`), so
//     no duplicate storage event is faked.
//
// Screenshots and privacy:
//   - Screenshot bytes are NEVER stored in this module's tables:
//     only metadata (storage target + key, size, content type,
//     capture time, blur flag) lives here; bytes go to the storage
//     module's provider named by settings (`storage_target`,
//     default `"timetracker-screenshots"`). The target must exist;
//     a missing target fails the upload with 503 instead of
//     silently storing bytes somewhere else.
//   - Consent: screenshots are disabled for a user until that user
//     sets `screenshots_enabled` (their own `/settings/me`). The
//     agent upload is rejected with 403 without consent.
//   - Blur: when the user's `blur_screenshots` flag is set, the
//     agent is expected to blur the image before upload and the
//     record is marked `blurred: true`; the server records the flag
//     but cannot verify pixel content — the agent is trusted to
//     honour it, and the flag is visible on every screenshot record
//     so a manager can see which captures were blurred.
//   - Visibility: a user sees only their own screenshots. A manager
//     sees a team member's screenshots only with the
//     `timetracker:view_team_screenshots` capability AND a positive
//     `timetracker.team.check` answer. Screenshot reads return a
//     short-lived signed URL (15 minutes), never the bytes.
//   - Retention: screenshots older than `retention_days`
//     (default 90) are deleted — metadata and stored object — by
//     the purge. The purge never deletes anything younger than the
//     retention window, and every deletion is reported in the
//     `timetracker.screenshots.purged` event payload.
//
// Money: hourly rates and payroll amounts are `f64` in memory
// (the billing module's in-memory convention) and NUMERIC(19,4)
// in the migration. Amounts are rounded to 4 decimal places on
// write; the Diesel persistence types use `PgNumeric` (see the
// schema / models snippets in the module report).
//
// Actor identity: browser / dashboard callers send `X-User-Id`
// and `X-Role` (the ai module's shape); agent callers send an API
// key as `Authorization: Bearer <key>` or `X-Api-Key`, and the
// agent's user is the key's owner id (the apikeys module's
// `authenticate_key` + `key_has_scope`).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::storage::StorageRegistry;
use crate::core::pagination::{PageResult, PaginationParams};
use crate::modules::apikeys::{authenticate_key, key_has_scope, note_used, AuthenticatedKey};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::time::Instant;
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
const MAX_DESCRIPTION_LEN: usize = 2000;
const MAX_REASON_LEN: usize = 1000;
const MAX_NOTE_LEN: usize = 2000;
const MAX_APP_NAME_LEN: usize = 200;
const MAX_CURRENCY_LEN: usize = 3;
const MAX_TARGET_LEN: usize = 128;

/// A manual entry or a timer-derived entry may cover at most 24 hours.
const MAX_ENTRY_SECONDS: i64 = 86_400;
/// An activity / usage interval may cover at most 24 hours.
const MAX_INTERVAL_SECONDS: i64 = 86_400;
/// Screenshot upload cap (5 MiB). Larger captures must be downscaled
/// or recompressed by the agent before upload.
const MAX_SCREENSHOT_BYTES: usize = 5 * 1024 * 1024;
/// Signed-URL lifetime for a screenshot read (15 minutes).
const SCREENSHOT_SIGNED_URL_SECS: u64 = 900;
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
const ONLINE_WINDOW_SECS: i64 = 300;
/// Average activity below this score flags a user as low-activity.
const DEFAULT_LOW_ACTIVITY_THRESHOLD: i64 = 30;
/// Per-key screenshot upload rate limit (per minute).
const SCREENSHOT_RATE_PER_MINUTE: u32 = 60;
/// Per-key agent report rate limit (per minute).
const AGENT_RATE_PER_MINUTE: u32 = 300;

/// Event names emitted and listened to by this module.
pub mod events {
    pub const TIMER_STARTED: &str = "timetracker.timer.started";
    pub const TIMER_STOPPED: &str = "timetracker.timer.stopped";
    pub const TIMER_PAUSED: &str = "timetracker.timer.paused";
    pub const TIMER_RESUMED: &str = "timetracker.timer.resumed";
    pub const TIMER_AUTO_PAUSED: &str = "timetracker.timer.auto_paused";
    pub const TIME_LOGGED: &str = "timetracker.time.logged";
    pub const ENTRY_UPDATED: &str = "timetracker.entry.updated";
    pub const ENTRY_DELETED: &str = "timetracker.entry.deleted";
    pub const ENTRY_APPROVED: &str = "timetracker.entry.approved";
    pub const ENTRY_REJECTED: &str = "timetracker.entry.rejected";
    pub const ACTIVITY_REPORTED: &str = "timetracker.activity.reported";
    pub const USAGE_REPORTED: &str = "timetracker.usage.reported";
    pub const SCREENSHOT_UPLOADED: &str = "timetracker.screenshot.uploaded";
    pub const SCREENSHOT_DELETED: &str = "timetracker.screenshot.deleted";
    pub const SCREENSHOTS_PURGED: &str = "timetracker.screenshots.purged";
    pub const SCREENSHOTS_PURGE_REQUESTED: &str = "timetracker.screenshots.purge_requested";
    pub const TIMESHEET_CREATED: &str = "timetracker.timesheet.created";
    pub const TIMESHEET_SUBMITTED: &str = "timetracker.timesheet.submitted";
    pub const TIMESHEET_APPROVED: &str = "timetracker.timesheet.approved";
    pub const TIMESHEET_REJECTED: &str = "timetracker.timesheet.rejected";
    pub const RATE_CREATED: &str = "timetracker.rate.created";
    pub const RATE_UPDATED: &str = "timetracker.rate.updated";
    pub const RATE_DELETED: &str = "timetracker.rate.deleted";
    pub const SETTINGS_UPDATED: &str = "timetracker.settings.updated";
    /// Scheduler job kind for the retention purge (the scheduler
    /// must add this kind to its KNOWN_KINDS list and a dispatch arm
    /// that emits `timetracker.screenshots.purge_requested`; see
    /// `request_screenshot_purge`).
    pub const PURGE_JOB_KIND: &str = "timetracker.screenshots.purge";
    pub const STAFF_ADDED: &str = "timetracker.staff.added";
    pub const STAFF_UPDATED: &str = "timetracker.staff.updated";
    pub const STAFF_REMOVED: &str = "timetracker.staff.removed";
    pub const STAFF_INVITE_CREATED: &str = "timetracker.staff.invite.created";
    pub const STAFF_INVITE_ACCEPTED: &str = "timetracker.staff.invite.accepted";
    pub const STAFF_INVITE_REVOKED: &str = "timetracker.staff.invite.revoked";
    pub const TEAM_CREATED: &str = "timetracker.team.created";
    pub const TEAM_DELETED: &str = "timetracker.team.deleted";
    pub const TEAM_MEMBER_ADDED: &str = "timetracker.team.member.added";
    pub const TEAM_MEMBER_REMOVED: &str = "timetracker.team.member.removed";
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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
    /// Duration in seconds; always equals
    /// `ended_at - started_at` (validated on write).
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
    /// store the entry without re-deriving it — and without a
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
/// the hash of the window title is stored — never the raw title,
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
    fn for_user(user_id: &str) -> Self {
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

// ---------------------------------------------------------------------------
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
// and the team / company side answers
// `timetracker.company.membership.check` with `{"member": true}`.
// Both checks fail closed — an unanswered membership check means
// the caller is not treated as a member of that company, so a
// company admin can only ever manage their own company's staff.
//
// Per-buyer settings: the product ships with defaults (the global
// `TrackerSettings` above); each purchasing company overrides its
// own copy in `BuyerSettings`. Behaviour (screenshots, activity,
// idle timeout, retention) always reads the RESOLVED settings for
// the acting user — staff override, then buyer setting, then global
// default — never the global settings alone. See
// `resolve_settings_for_user`.
// ---------------------------------------------------------------------------

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
    fn for_company(company_id: Uuid) -> Self {
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

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for TimeTracker. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission, except the capability
/// and membership checks, which fail closed when there is no bus
/// to ask. Durable rows for these types are defined by migration
/// `2026-11-22-000032_timetracker`; until that migration lands,
/// nothing here survives a restart.
#[derive(Clone)]
pub struct TimeTrackerState {
    pub timers: Arc<DashMap<Uuid, TimeTrackerTimer>>,
    pub entries: Arc<DashMap<Uuid, TimeEntry>>,
    pub activity: Arc<DashMap<Uuid, ActivitySample>>,
    pub usage: Arc<DashMap<Uuid, AppUsageRecord>>,
    pub screenshots: Arc<DashMap<Uuid, ScreenshotRecord>>,
    pub rates: Arc<DashMap<Uuid, HourlyRate>>,
    pub timesheets: Arc<DashMap<Uuid, Timesheet>>,
    pub settings: Arc<RwLock<TrackerSettings>>,
    pub user_settings: Arc<DashMap<String, UserTrackerSettings>>,
    pub teams: Arc<DashMap<Uuid, Team>>,
    /// Keyed by `"{team_id}:{user_id}"`.
    pub team_members: Arc<DashMap<String, TeamMember>>,
    /// Keyed by user id. One staff record per user.
    pub staff: Arc<DashMap<String, StaffSettings>>,
    pub invites: Arc<DashMap<Uuid, StaffInvite>>,
    /// Keyed by company id.
    pub buyer_settings: Arc<DashMap<Uuid, BuyerSettings>>,
    /// Per-key per-minute agent call buckets: key id -> (window
    /// start, calls in this window).
    pub agent_buckets: Arc<DashMap<String, (Instant, u32)>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// Shared storage registry, attached in `init` (the registry
    /// lives in `AppContext`). The purge hook handler needs it
    /// without an HTTP request, so it is kept here next to the bus.
    pub storage_registry: Arc<RwLock<Option<Arc<StorageRegistry>>>>,
}

impl Default for TimeTrackerState {
    fn default() -> Self {
        Self {
            timers: Arc::new(DashMap::new()),
            entries: Arc::new(DashMap::new()),
            activity: Arc::new(DashMap::new()),
            usage: Arc::new(DashMap::new()),
            screenshots: Arc::new(DashMap::new()),
            rates: Arc::new(DashMap::new()),
            timesheets: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(TrackerSettings::default())),
            user_settings: Arc::new(DashMap::new()),
            teams: Arc::new(DashMap::new()),
            team_members: Arc::new(DashMap::new()),
            staff: Arc::new(DashMap::new()),
            invites: Arc::new(DashMap::new()),
            buyer_settings: Arc::new(DashMap::new()),
            agent_buckets: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            storage_registry: Arc::new(RwLock::new(None)),
        }
    }
}

impl TimeTrackerState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn global_settings(&self) -> TrackerSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn user_settings_for(&self, user_id: &str) -> UserTrackerSettings {
        self.user_settings
            .get(user_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| UserTrackerSettings::for_user(user_id))
    }

    fn buyer_settings_for(&self, company_id: Uuid) -> BuyerSettings {
        self.buyer_settings
            .get(&company_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| BuyerSettings::for_company(company_id))
    }

    /// Resolve the effective settings for one user: per-staff
    /// override wins, then the buyer's setting, then the global
    /// product default. This is the only settings lookup the
    /// screenshot, activity, idle, and retention paths use.
    fn resolve_settings_for_user(&self, user_id: &str) -> ResolvedSettings {
        let global = self.global_settings();
        let staff = self.staff.get(user_id).map(|entry| entry.value().clone());
        let company_id = staff.as_ref().map(|record| record.company_id);
        let buyer = company_id.map(|id| self.buyer_settings_for(id));
        let screenshots_enabled = staff
            .as_ref()
            .and_then(|record| record.screenshots_enabled)
            .or_else(|| buyer.as_ref().and_then(|settings| settings.screenshots_enabled))
            .unwrap_or(true);
        let screenshot_interval_secs = buyer
            .as_ref()
            .and_then(|settings| settings.screenshot_interval_secs)
            .unwrap_or(300);
        let activity_level = staff
            .as_ref()
            .and_then(|record| record.activity_level)
            .or_else(|| buyer.as_ref().and_then(|settings| settings.activity_level))
            .unwrap_or_default();
        let idle_threshold_secs = staff
            .as_ref()
            .and_then(|record| record.idle_threshold_secs)
            .or_else(|| buyer.as_ref().and_then(|settings| settings.idle_timeout_secs))
            .unwrap_or(global.idle_threshold_secs);
        let blur_screenshots = staff
            .as_ref()
            .and_then(|record| record.blur_screenshots)
            .or_else(|| buyer.as_ref().and_then(|settings| settings.blur_screenshots))
            .unwrap_or(false);
        let allowed_apps = buyer
            .as_ref()
            .map(|settings| settings.allowed_apps.clone())
            .unwrap_or_default();
        let blocked_apps = buyer
            .as_ref()
            .map(|settings| settings.blocked_apps.clone())
            .unwrap_or_default();
        let allowed_websites = buyer
            .as_ref()
            .map(|settings| settings.allowed_websites.clone())
            .unwrap_or_default();
        let blocked_websites = buyer
            .as_ref()
            .map(|settings| settings.blocked_websites.clone())
            .unwrap_or_default();
        let timezone = staff
            .as_ref()
            .and_then(|record| record.timezone.clone())
            .or_else(|| buyer.as_ref().and_then(|settings| settings.timezone.clone()))
            .unwrap_or_else(|| "UTC".to_string());
        let working_hours_start = staff
            .as_ref()
            .and_then(|record| record.working_hours_start.clone())
            .or_else(|| {
                buyer
                    .as_ref()
                    .and_then(|settings| settings.working_hours_start.clone())
            });
        let working_hours_end = staff
            .as_ref()
            .and_then(|record| record.working_hours_end.clone())
            .or_else(|| {
                buyer
                    .as_ref()
                    .and_then(|settings| settings.working_hours_end.clone())
            });
        let retention_days = buyer
            .as_ref()
            .and_then(|settings| settings.retention_days)
            .unwrap_or(global.retention_days);
        ResolvedSettings {
            company_id,
            screenshots_enabled,
            screenshot_interval_secs,
            activity_level,
            idle_threshold_secs,
            blur_screenshots,
            allowed_apps,
            blocked_apps,
            allowed_websites,
            blocked_websites,
            timezone,
            working_hours_start,
            working_hours_end,
            retention_days,
        }
    }

    /// Resolve the effective settings for a whole company (no
    /// per-staff layer): buyer setting, then global default.
    fn resolve_settings_for_company(&self, company_id: Uuid) -> ResolvedSettings {
        let global = self.global_settings();
        let buyer = self.buyer_settings_for(company_id);
        ResolvedSettings {
            company_id: Some(company_id),
            screenshots_enabled: buyer.screenshots_enabled.unwrap_or(true),
            screenshot_interval_secs: buyer.screenshot_interval_secs.unwrap_or(300),
            activity_level: buyer.activity_level.unwrap_or_default(),
            idle_threshold_secs: buyer
                .idle_timeout_secs
                .unwrap_or(global.idle_threshold_secs),
            blur_screenshots: buyer.blur_screenshots.unwrap_or(false),
            allowed_apps: buyer.allowed_apps.clone(),
            blocked_apps: buyer.blocked_apps.clone(),
            allowed_websites: buyer.allowed_websites.clone(),
            blocked_websites: buyer.blocked_websites.clone(),
            timezone: buyer.timezone.clone().unwrap_or_else(|| "UTC".to_string()),
            working_hours_start: buyer.working_hours_start.clone(),
            working_hours_end: buyer.working_hours_end.clone(),
            retention_days: buyer.retention_days.unwrap_or(global.retention_days),
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers: caller, permissions, agent auth, company scope
// ---------------------------------------------------------------------------

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against (the ai module's shape).
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// The company the caller claims to act for, from the
/// `X-Company-Id` header. The value is only a claim: every use of
/// it goes through `require_company_member`, which verifies the
/// company exists and the caller is a member of it.
fn claimed_company_id(req: &HttpRequest) -> UnifiedResult<Uuid> {
    let raw = req
        .headers()
        .get("x-company-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            UnifiedError::bad_request("Missing X-Company-Id header: company-scoped calls must name their company")
        })?;
    Uuid::parse_str(raw)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid company id: {raw}")))
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
/// (Same fail-closed shape as the ai module's capability check.)
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// The presented API key: `Authorization: Bearer <key>` first,
/// then `X-Api-Key` (the mcp module's shape).
fn presented_key(req: &HttpRequest) -> Option<String> {
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(text) = header.to_str() {
            if let Some(key) = text.strip_prefix("Bearer ") {
                let key = key.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    req.headers()
        .get("x-api-key")
        .and_then(|header| header.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Authenticate an agent call: a valid, unrevoked, unexpired API
/// key scoped to `timetracker:<action>`, whose owner id becomes
/// the acting user. Agent calls are always scoped to the key's
/// owner: a key without an owner cannot act for anyone.
fn authenticate_agent(
    state: &TimeTrackerState,
    req: &HttpRequest,
    action: &str,
    rate_limit: u32,
) -> UnifiedResult<(AuthenticatedKey, String)> {
    let presented = presented_key(req).ok_or_else(|| {
        UnifiedError::unauthorized("Missing API key: send Authorization: Bearer <key> or X-Api-Key")
    })?;
    let authenticated = authenticate_key(&presented)?;
    if !key_has_scope(&authenticated.scopes, "timetracker", action) {
        return Err(UnifiedError::forbidden(format!(
            "API key is not scoped to timetracker:{action}; grant scope \"timetracker:{action}\" or \"timetracker:*\""
        )));
    }
    let owner_id = authenticated.owner_id.ok_or_else(|| {
        UnifiedError::forbidden("API key has no owner: agent calls must be owned by a user")
    })?;
    check_agent_rate(state, &authenticated.id.to_string(), rate_limit.min(authenticated.requests_per_minute))?;
    note_used(authenticated.id);
    Ok((authenticated, owner_id.to_string()))
}

/// Per-key per-minute rate limit for agent calls. Returns 429 with
/// `retry_after_secs` in metadata when the window is exhausted.
fn check_agent_rate(
    state: &TimeTrackerState,
    key_id: &str,
    limit: u32,
) -> UnifiedResult<()> {
    if limit == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut bucket = state
        .agent_buckets
        .entry(key_id.to_string())
        .or_insert((now, 0));
    if now.duration_since(bucket.0).as_secs() >= 60 {
        *bucket = (now, 1);
        return Ok(());
    }
    if bucket.1 >= limit {
        let retry_after = 60 - now.duration_since(bucket.0).as_secs();
        return Err(UnifiedError::new(
            format!("Agent rate limit exceeded for this API key ({limit} calls per minute)"),
            429,
        )
        .with_metadata("retry_after_secs", json!(retry_after)));
    }
    bucket.1 += 1;
    Ok(())
}

/// True when the company module confirms the company exists,
/// through the `company.exists` hook (the same hook the SaaS
/// module uses). Fail-closed: no answer means it does not exist.
fn company_exists(ctx: &AppContext, company_id: Uuid) -> bool {
    let answer = ctx.hooks.emit(
        "company.exists",
        json!({ "company_id": company_id.to_string() }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("exists"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Verify the caller is a member of the company they claim, and
/// that the company exists. The membership question goes out as
/// `timetracker.company.membership.check` with
/// `{"company_id", "user_id"}`, expecting `{"member": true}` in
/// the first hook result; the company / users side answers it.
/// Fail-closed on both halves: an unknown company or an unanswered
/// membership check is a 403, never a pass.
fn require_company_member(
    ctx: &AppContext,
    user_id: &str,
    company_id: Uuid,
) -> UnifiedResult<()> {
    if !company_exists(ctx, company_id) {
        return Err(UnifiedError::forbidden(format!(
            "Company {company_id} does not exist"
        )));
    }
    let answer = ctx.hooks.emit(
        "timetracker.company.membership.check",
        json!({ "company_id": company_id.to_string(), "user_id": user_id }),
    );
    match answer.results.first() {
        Some(result) if result.get("member").and_then(Value::as_bool) == Some(true) => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "Caller is not a member of this company",
        )),
    }
}

/// True when `manager_id` may view `user_id`'s tracked data: the
/// caller themself, or a manager whose team contains the user.
/// Team membership is asked through `timetracker.team.check` with
/// `{"manager_id", "user_id"}`, expecting `{"manages": true}`.
/// Fail-closed: no answer means not allowed.
fn team_check(ctx: &AppContext, manager_id: &str, user_id: &str) -> bool {
    let answer = ctx.hooks.emit(
        "timetracker.team.check",
        json!({ "manager_id": manager_id, "user_id": user_id }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("manages"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Authorise reading another user's time, activity, or usage: the
/// user themself, or a caller holding `timetracker:view_team`
/// whose team contains the user.
fn can_view_user(ctx: &AppContext, caller: &Caller, user_id: &str) -> UnifiedResult<()> {
    if caller.actor == user_id {
        return Ok(());
    }
    require_permission(ctx, &caller.role, PERM_VIEW_TEAM)?;
    if team_check(ctx, &caller.actor, user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "User is not on the caller's team",
        ))
    }
}

/// Authorise reading another user's screenshots: stricter than
/// `can_view_user` — it needs the dedicated screenshot capability
/// on top of the team check.
fn can_view_screenshots(ctx: &AppContext, caller: &Caller, user_id: &str) -> UnifiedResult<()> {
    if caller.actor == user_id {
        return Ok(());
    }
    require_permission(ctx, &caller.role, PERM_VIEW_TEAM_SCREENSHOTS)?;
    if team_check(ctx, &caller.actor, user_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "User is not on the caller's team",
        ))
    }
}

// ---------------------------------------------------------------------------
// Helpers: validation and small utilities
// ---------------------------------------------------------------------------

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

/// Round a money amount to 4 decimal places (NUMERIC(19,4)).
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != MAX_CURRENCY_LEN
        || !upper.chars().all(|c| c.is_ascii_uppercase())
    {
        return Err(UnifiedError::bad_request(format!(
            "currency must be a 3-letter ISO 4217 code, got \"{currency}\""
        )));
    }
    Ok(upper)
}

/// A timezone is an IANA name (`"Asia/Jakarta"`, `"UTC"`): letters,
/// digits, `_`, `-`, `+`, and `/` only, no empty segments. Shape
/// only; no timezone conversion is done anywhere (all stored times
/// are UTC; the timezone is a display hint for reports).
fn valid_timezone(timezone: &str) -> UnifiedResult<String> {
    let timezone = timezone.trim();
    if timezone.is_empty() || timezone.len() > 64 {
        return Err(UnifiedError::bad_request("timezone must not be empty"));
    }
    if timezone == "UTC" {
        return Ok("UTC".to_string());
    }
    let shaped = timezone.split('/').all(|segment| {
        !segment.is_empty()
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '+')
    }) && timezone.contains('/');
    if !shaped {
        return Err(UnifiedError::bad_request(format!(
            "Invalid timezone: {timezone}. Use an IANA name such as \"Asia/Jakarta\" or \"UTC\""
        )));
    }
    Ok(timezone.to_string())
}

/// A working-hours value is `"HH:MM"` on a 24-hour clock.
fn valid_hhmm(value: &str, field: &str) -> UnifiedResult<String> {
    let value = value.trim();
    let mut parts = value.split(':');
    let hour: u32 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(|| UnifiedError::bad_request(format!("{field} must look like \"09:00\"")))?;
    let minute: u32 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .ok_or_else(|| UnifiedError::bad_request(format!("{field} must look like \"09:00\"")))?;
    if parts.next().is_some() || hour > 23 || minute > 59 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must look like \"09:00\" (24-hour clock)"
        )));
    }
    Ok(format!("{hour:02}:{minute:02}"))
}

fn valid_email(email: &str) -> UnifiedResult<String> {
    let email = email.trim().to_lowercase();
    let mut parts = email.split('@');
    let local = parts.next().unwrap_or("");
    let domain = parts.next().unwrap_or("");
    if local.is_empty() || domain.is_empty() || !domain.contains('.') || parts.next().is_some() {
        return Err(UnifiedError::bad_request(format!(
            "Invalid email address: {email}"
        )));
    }
    Ok(email)
}

/// SHA-256 hex digest (used for invite tokens and screenshot bytes).
fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A window-title hash must be exactly 64 lowercase hex chars
/// (SHA-256). Anything else is rejected: the raw title is never
/// accepted, so a malformed hash cannot smuggle one in.
fn valid_title_hash(raw: &str) -> UnifiedResult<String> {
    let raw = raw.trim().to_lowercase();
    if raw.len() != 64 || !raw.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(UnifiedError::bad_request(
            "window_title_hash must be a 64-character lowercase hex SHA-256 digest of the window title; the raw title is never accepted",
        ));
    }
    Ok(raw)
}

fn validate_interval(
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    max_secs: i64,
) -> UnifiedResult<i64> {
    if end <= start {
        return Err(UnifiedError::bad_request(
            "interval end must be after interval start",
        ));
    }
    let secs = (end - start).num_seconds();
    if secs > max_secs {
        return Err(UnifiedError::bad_request(format!(
            "interval must be at most {max_secs} seconds"
        )));
    }
    Ok(secs)
}

fn team_member_key(team_id: Uuid, user_id: &str) -> String {
    format!("{team_id}:{user_id}")
}

fn team_ids_for_user(state: &TimeTrackerState, user_id: &str) -> Vec<Uuid> {
    state
        .team_members
        .iter()
        .filter(|entry| entry.value().user_id == user_id)
        .map(|entry| entry.value().team_id)
        .collect()
}

fn active_timer_for(state: &TimeTrackerState, user_id: &str) -> Option<TimeTrackerTimer> {
    state
        .timers
        .iter()
        .map(|entry| entry.value().clone())
        .find(|timer| {
            timer.user_id == user_id
                && (timer.status == TimerStatus::Running || timer.status == TimerStatus::Paused)
        })
}

fn load_timer(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<TimeTrackerTimer> {
    let id = parse_uuid(raw_id, "timer")?;
    state
        .timers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {id}")))
}

fn load_entry(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<TimeEntry> {
    let id = parse_uuid(raw_id, "entry")?;
    state
        .entries
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Time entry not found: {id}")))
}

/// Enforce the buyer's application / website allowlist and
/// blocklist on one reported usage interval. An empty allowlist
/// means "everything is allowed" (only the blocklist applies).
fn check_usage_allowed(
    resolved: &ResolvedSettings,
    application_name: &str,
    domain: Option<&str>,
) -> UnifiedResult<()> {
    let app = application_name.trim().to_lowercase();
    let domain = domain.map(str::trim).map(str::to_lowercase);
    if resolved
        .blocked_apps
        .iter()
        .any(|blocked| blocked.trim().to_lowercase() == app)
    {
        return Err(UnifiedError::unprocessable(format!(
            "Application \"{application_name}\" is blocked by the buyer's settings"
        )));
    }
    if !resolved.allowed_apps.is_empty()
        && !resolved
            .allowed_apps
            .iter()
            .any(|allowed| allowed.trim().to_lowercase() == app)
    {
        return Err(UnifiedError::unprocessable(format!(
            "Application \"{application_name}\" is not on the buyer's allowed-applications list"
        )));
    }
    if let Some(domain) = domain {
        if resolved
            .blocked_websites
            .iter()
            .any(|blocked| blocked.trim().to_lowercase() == domain)
        {
            return Err(UnifiedError::unprocessable(format!(
                "Website \"{domain}\" is blocked by the buyer's settings"
            )));
        }
        if !resolved.allowed_websites.is_empty()
            && !resolved
                .allowed_websites
                .iter()
                .any(|allowed| allowed.trim().to_lowercase() == domain)
        {
            return Err(UnifiedError::unprocessable(format!(
                "Website \"{domain}\" is not on the buyer's allowed-websites list"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Core logic: entries, timers, purge, payroll, reports
// ---------------------------------------------------------------------------

/// Emit `timetracker.time.logged` for one entry.
///
/// Payload shape (the billing module reads this to price billable
/// hours; the integrator adds a `timetracker.time.logged` handler
/// in billing.rs — billing currently listens to `order.created`,
/// `payment.captured`, and `payment.refunded` only):
/// ```json
/// {
///   "entry_id": "<uuid>", "user_id": "<user>", "timer_id": "<uuid>|null",
///   "project_id": "<uuid>|null", "task_id": "<uuid>|null",
///   "description": "<text>", "started_at": "<rfc3339>",
///   "ended_at": "<rfc3339>", "duration_secs": 3600,
///   "hours": 1.0, "billable": true, "approval": "pending",
///   "currency": "USD", "hourly_rate": 25.0,
///   "amount": 25.0, "source": "timer|manual",
///   "project_time_entry": { "id": "<uuid>", "task_id": "<uuid>|null",
///     "project_id": "<uuid>|null", "user_id": "<user>",
///     "description": "<text>", "hours": 1.0, "billable": true,
///     "billed": false, "invoice_id": null,
///     "started_at": "<rfc3339>", "ended_at": "<rfc3339>",
///     "timer_running": false, "created_at": "<rfc3339>",
///     "updated_at": "<rfc3339>" }
/// }
/// ```
/// `hours` is `duration_secs / 3600`. `hourly_rate` is the user's
/// active rate at the entry's end (or the buyer's default rate, or
/// 0 when no rate exists) and `amount` is `hours * hourly_rate`
/// rounded to 4 decimals. Non-billable entries still emit the event
/// with `"billable": false` and `amount` 0 so billing can tell
/// tracked time apart from priced time. `project_time_entry` is
/// the shared `ProjectTimeEntry` shape (see
/// `TimeEntry::to_project_time_entry_json`); the projects module
/// stores it, billing never reads it — there is exactly one
/// billing path, this event.
fn emit_time_logged(state: &TimeTrackerState, entry: &TimeEntry, source: &str) {
    let rate = active_rate_for(state, &entry.user_id, entry.ended_at);
    let hours = entry.duration_secs as f64 / 3600.0;
    let amount = if entry.billable {
        round4(hours * rate.map(|r| r.rate).unwrap_or(0.0))
    } else {
        0.0
    };
    state.emit(
        events::TIME_LOGGED,
        json!({
            "entry_id": entry.id.to_string(),
            "user_id": entry.user_id,
            "timer_id": entry.timer_id.map(|id| id.to_string()),
            "project_id": entry.project_id.map(|id| id.to_string()),
            "task_id": entry.task_id.map(|id| id.to_string()),
            "description": entry.description,
            "started_at": entry.started_at.to_rfc3339(),
            "ended_at": entry.ended_at.to_rfc3339(),
            "duration_secs": entry.duration_secs,
            "hours": round4(hours),
            "billable": entry.billable,
            "approval": format!("{:?}", entry.approval).to_lowercase(),
            "currency": rate.map(|r| r.currency.clone()).unwrap_or_else(|| state.global_settings().default_currency),
            "hourly_rate": rate.map(|r| r.rate).unwrap_or(0.0),
            "amount": amount,
            "source": source,
            "project_time_entry": entry.to_project_time_entry_json(),
        }),
    );
}

/// The user's hourly rate in force at `at`: the latest
/// `effective_from` at or before `at` whose `effective_to` has
/// not passed. Falls back to the buyer's default rate when the
/// user has no rate row.
fn active_rate_for(
    state: &TimeTrackerState,
    user_id: &str,
    at: DateTime<Utc>,
) -> Option<HourlyRate> {
    let mut rates: Vec<HourlyRate> = state
        .rates
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|rate| {
            rate.user_id == user_id
                && rate.effective_from <= at
                && rate.effective_to.map(|to| to >= at).unwrap_or(true)
        })
        .collect();
    rates.sort_by(|a, b| b.effective_from.cmp(&a.effective_from));
    if let Some(rate) = rates.into_iter().next() {
        return Some(rate);
    }
    let resolved = state.resolve_settings_for_user(user_id);
    let buyer_rate = resolved
        .company_id
        .and_then(|company_id| state.buyer_settings_for(company_id).default_hourly_rate);
    buyer_rate.map(|rate| HourlyRate {
        id: Uuid::nil(),
        user_id: user_id.to_string(),
        currency: state.global_settings().default_currency,
        rate,
        effective_from: at,
        effective_to: None,
        created_at: at,
        updated_at: at,
    })
}

/// Stop a timer and turn it into a time entry. Shared by the stop
/// endpoint, the agent stop endpoint, and the
/// `project.task.completed` hook handler. The entry starts
/// `pending` unless the global settings auto-approve timer
/// entries. Emits `timetracker.timer.stopped` and
/// `timetracker.time.logged`.
fn stop_timer_inner(
    state: &TimeTrackerState,
    timer_id: Uuid,
    reason: &str,
) -> UnifiedResult<TimeEntry> {
    let mut timer = state
        .timers
        .get_mut(&timer_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {timer_id}")))?;
    if timer.status == TimerStatus::Stopped {
        return Err(UnifiedError::conflict("Timer is already stopped"));
    }
    let now = Utc::now();
    let duration_secs = timer.elapsed_secs(now);
    timer.status = TimerStatus::Stopped;
    timer.stopped_at = Some(now);
    timer.updated_at = now;
    let timer_snapshot = timer.clone();
    drop(timer);
    let auto_approve = state.global_settings().auto_approve_timer_entries;
    let entry = TimeEntry {
        id: Uuid::now_v7(),
        user_id: timer_snapshot.user_id.clone(),
        project_id: timer_snapshot.project_id,
        task_id: timer_snapshot.task_id,
        timer_id: Some(timer_snapshot.id),
        description: timer_snapshot
            .description
            .clone()
            .unwrap_or_else(|| "Tracked time".to_string()),
        started_at: timer_snapshot.started_at,
        ended_at: now,
        duration_secs,
        billable: timer_snapshot.billable,
        approval: if auto_approve {
            EntryApproval::Approved
        } else {
            EntryApproval::Pending
        },
        approved_by: None,
        approved_at: if auto_approve { Some(now) } else { None },
        rejection_note: None,
        reason: None,
        created_at: now,
        updated_at: now,
    };
    state.entries.insert(entry.id, entry.clone());
    if let Some(mut timer) = state.timers.get_mut(&timer_id) {
        timer.entry_id = Some(entry.id);
    }
    state.emit(
        events::TIMER_STOPPED,
        json!({
            "timer_id": timer_snapshot.id.to_string(),
            "user_id": timer_snapshot.user_id,
            "entry_id": entry.id.to_string(),
            "duration_secs": duration_secs,
            "reason": reason,
        }),
    );
    emit_time_logged(state, &entry, "timer");
    Ok(entry)
}

/// Auto-pause a running timer after the caller's idle threshold:
/// the idle seconds are added to the timer's idle total (recorded,
/// never discarded), the timer goes to `paused`, and
/// `timetracker.timer.auto_paused` is emitted with the idle period.
/// Returns true when a pause happened.
fn auto_pause_on_idle(
    state: &TimeTrackerState,
    user_id: &str,
    idle_secs: i64,
) -> bool {
    let Some(timer) = active_timer_for(state, user_id) else {
        return false;
    };
    if timer.status != TimerStatus::Running {
        return false;
    }
    let resolved = state.resolve_settings_for_user(user_id);
    if idle_secs < resolved.idle_threshold_secs {
        return false;
    }
    let now = Utc::now();
    let mut paused = false;
    if let Some(mut entry) = state.timers.get_mut(&timer.id) {
        entry.idle_total_secs += idle_secs;
        entry.status = TimerStatus::Paused;
        entry.paused_at = Some(now);
        entry.updated_at = now;
        paused = true;
    }
    if paused {
        state.emit(
            events::TIMER_AUTO_PAUSED,
            json!({
                "timer_id": timer.id.to_string(),
                "user_id": user_id,
                "idle_secs": idle_secs,
                "idle_threshold_secs": resolved.idle_threshold_secs,
                "idle_total_secs": timer.idle_total_secs + idle_secs,
            }),
        );
    }
    paused
}

/// Ask the scheduler to run the screenshot retention purge: emit
/// `scheduler.enqueue` with kind `timetracker.screenshots.purge`.
/// The scheduler answers `{"queued": true, "job_id": ...}` when it
/// knows the kind. CURRENT LIMIT, stated plainly: the scheduler's
/// `KNOWN_KINDS` list does not contain this kind yet, so today the
/// scheduler declines and the purge runs through
/// `POST /screenshots/purge` instead. The follow-up is to add the
/// kind to `KNOWN_KINDS` plus this dispatch arm in scheduler.rs:
/// `"timetracker.screenshots.purge" => { state.emit(
/// "timetracker.screenshots.purge_requested", payload.clone());
/// Ok(json!({ "dispatched": true, "event":
/// "timetracker.screenshots.purge_requested" })) }`
fn request_screenshot_purge(state: &TimeTrackerState) -> bool {
    let Some(bus) = state.bus() else {
        return false;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": events::PURGE_JOB_KIND,
            "payload": {},
        }),
    );
    answer
        .results
        .first()
        .and_then(|result| result.get("queued"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Delete every screenshot older than its owner's retention
/// window: the per-buyer retention when the owner belongs to a
/// company, else the global default. Both the metadata record and
/// the stored object are deleted, and the purge is reported with
/// `timetracker.screenshots.purged`. Returns (removed, failed).
async fn purge_expired_screenshots(
    state: &TimeTrackerState,
    registry: &StorageRegistry,
) -> (u64, u64) {
    let now = Utc::now();
    let expired: Vec<ScreenshotRecord> = state
        .screenshots
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|shot| {
            let resolved = state.resolve_settings_for_user(&shot.user_id);
            let cutoff = now - Duration::days(resolved.retention_days.max(1));
            shot.captured_at < cutoff
        })
        .collect();
    let mut removed = 0u64;
    let mut failed = 0u64;
    for shot in expired {
        let mut object_deleted = true;
        if let Some(provider) = registry.get(&shot.storage_target) {
            if provider.delete(&shot.storage_key).await.is_err() {
                object_deleted = false;
            }
        } else {
            object_deleted = false;
        }
        state.screenshots.remove(&shot.id);
        if object_deleted {
            removed += 1;
        } else {
            failed += 1;
        }
        state.emit(
            events::SCREENSHOT_DELETED,
            json!({
                "screenshot_id": shot.id.to_string(),
                "user_id": shot.user_id,
                "storage_target": shot.storage_target,
                "storage_key": shot.storage_key,
                "reason": "retention_purge",
                "object_deleted": object_deleted,
            }),
        );
    }
    state.emit(
        events::SCREENSHOTS_PURGED,
        json!({
            "removed": removed,
            "failed": failed,
            "purged_at": now.to_rfc3339(),
        }),
    );
    (removed, failed)
}

/// Compute payroll lines for approved entries in `[from, to]`.
/// Overtime is per user per ISO week: hours beyond
/// `overtime_hours_per_week` pay at `overtime_multiplier`. Only
/// approved entries are payable; pending and rejected entries are
/// ignored. A user without a rate line pays 0 at the global
/// default currency (the line is still returned so the gap is
/// visible instead of silently skipped).
fn compute_payroll(
    state: &TimeTrackerState,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    user_filter: Option<&str>,
) -> Vec<PayrollLine> {
    let global = state.global_settings();
    let mut per_user: HashMap<String, Vec<TimeEntry>> = HashMap::new();
    for entry in state.entries.iter() {
        let record = entry.value().clone();
        if record.approval != EntryApproval::Approved {
            continue;
        }
        if record.started_at < from || record.started_at > to {
            continue;
        }
        if let Some(wanted) = user_filter {
            if record.user_id != wanted {
                continue;
            }
        }
        per_user.entry(record.user_id.clone()).or_default().push(record);
    }
    let mut lines: Vec<PayrollLine> = Vec::new();
    for (user_id, entries) in per_user {
        // Weekly buckets for overtime: (iso_year, iso_week) -> secs.
        let mut weeks: HashMap<(i32, u32), i64> = HashMap::new();
        let mut total_secs = 0i64;
        let mut billable_secs = 0i64;
        for entry in &entries {
            total_secs += entry.duration_secs;
            if entry.billable {
                billable_secs += entry.duration_secs;
            }
            let week = entry.started_at.iso_week();
            *weeks
                .entry((week.year(), week.week()))
                .or_default() += entry.duration_secs;
        }
        let mut overtime_secs = 0i64;
        let threshold_secs = (global.overtime_hours_per_week * 3600.0).round() as i64;
        for secs in weeks.values() {
            if *secs > threshold_secs {
                overtime_secs += secs - threshold_secs;
            }
        }
        let regular_secs = total_secs - overtime_secs;
        let rate = active_rate_for(state, &user_id, to);
        let hourly = rate.as_ref().map(|r| r.rate).unwrap_or(0.0);
        let currency = rate
            .as_ref()
            .map(|r| r.currency.clone())
            .unwrap_or_else(|| global.default_currency.clone());
        let regular_hours = round4(regular_secs as f64 / 3600.0);
        let overtime_hours = round4(overtime_secs as f64 / 3600.0);
        let regular_pay = round4(regular_hours * hourly);
        let overtime_pay = round4(overtime_hours * hourly * global.overtime_multiplier);
        lines.push(PayrollLine {
            user_id: user_id.clone(),
            currency,
            total_hours: round4(total_secs as f64 / 3600.0),
            billable_hours: round4(billable_secs as f64 / 3600.0),
            non_billable_hours: round4((total_secs - billable_secs) as f64 / 3600.0),
            regular_hours,
            overtime_hours,
            hourly_rate: hourly,
            regular_pay,
            overtime_pay,
            total_pay: round4(regular_pay + overtime_pay),
        });
    }
    lines.sort_by(|a, b| a.user_id.cmp(&b.user_id));
    lines
}

/// Aggregate entries into daily report rows, optionally filtered
/// by user and project. Activity averages come from the samples in
/// the same window.
fn report_rows(
    state: &TimeTrackerState,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    user_filter: Option<&str>,
    project_filter: Option<Uuid>,
) -> Vec<ReportRow> {
    let mut grouped: HashMap<(String, Option<Uuid>, String), (i64, i64, usize)> = HashMap::new();
    for entry in state.entries.iter() {
        let record = entry.value();
        if record.started_at < from || record.started_at > to {
            continue;
        }
        if let Some(wanted) = user_filter {
            if record.user_id != wanted {
                continue;
            }
        }
        if let Some(wanted) = project_filter {
            if record.project_id != Some(wanted) {
                continue;
            }
        }
        let date = record.started_at.format("%Y-%m-%d").to_string();
        let slot = grouped
            .entry((record.user_id.clone(), record.project_id, date))
            .or_default();
        slot.0 += record.duration_secs;
        if record.billable {
            slot.1 += record.duration_secs;
        }
        slot.2 += 1;
    }
    let mut rows: Vec<ReportRow> = Vec::new();
    for ((user_id, project_id, date), (total_secs, billable_secs, count)) in grouped {
        let day_start = DateTime::parse_from_rfc3339(&format!("{date}T00:00:00+00:00"))
            .map(|parsed| parsed.with_timezone(&Utc))
            .unwrap_or(from);
        let day_end = day_start + Duration::days(1);
        let samples: Vec<i64> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sample| {
                sample.user_id == user_id
                    && sample.interval_start >= day_start
                    && sample.interval_start < day_end
                    && project_filter.map(|wanted| sample.project_id == Some(wanted)).unwrap_or(true)
            })
            .map(|sample| sample.activity_score)
            .collect();
        let avg_activity = if samples.is_empty() {
            None
        } else {
            Some(samples.iter().sum::<i64>() as f64 / samples.len() as f64)
        };
        rows.push(ReportRow {
            user_id,
            project_id,
            date,
            total_secs,
            billable_secs,
            non_billable_secs: total_secs - billable_secs,
            entry_count: count,
            avg_activity,
        });
    }
    rows.sort_by(|a, b| (a.date.clone(), a.user_id.clone()).cmp(&(b.date.clone(), b.user_id.clone())));
    rows
}

/// Build one staff member's activity overview.
fn staff_overview(state: &TimeTrackerState, staff: &StaffSettings) -> StaffOverview {
    let now = Utc::now();
    let today_start = now.date_naive().and_hms_opt(0, 0, 0).map(|naive| naive.and_utc()).unwrap_or(now);
    let week_start = today_start - Duration::days(7);
    let today_secs: i64 = state
        .entries
        .iter()
        .filter(|entry| {
            entry.value().user_id == staff.user_id && entry.value().started_at >= today_start
        })
        .map(|entry| entry.value().duration_secs)
        .sum();
    let week_secs: i64 = state
        .entries
        .iter()
        .filter(|entry| {
            entry.value().user_id == staff.user_id && entry.value().started_at >= week_start
        })
        .map(|entry| entry.value().duration_secs)
        .sum();
    let today_samples: Vec<ActivitySample> = state
        .activity
        .iter()
        .filter(|entry| {
            entry.value().user_id == staff.user_id && entry.value().interval_start >= today_start
        })
        .map(|entry| entry.value().clone())
        .collect();
    let avg_activity_today = if today_samples.is_empty() {
        None
    } else {
        Some(
            today_samples.iter().map(|sample| sample.activity_score).sum::<i64>() as f64
                / today_samples.len() as f64,
        )
    };
    let last_activity_at = today_samples
        .iter()
        .map(|sample| sample.interval_end)
        .max();
    let resolved = state.resolve_settings_for_user(&staff.user_id);
    StaffOverview {
        user_id: staff.user_id.clone(),
        company_id: staff.company_id,
        role: staff.role,
        manager_id: staff.manager_id.clone(),
        team_ids: team_ids_for_user(state, &staff.user_id),
        active_timer_id: active_timer_for(state, &staff.user_id).map(|timer| timer.id),
        today_secs,
        week_secs,
        avg_activity_today,
        last_activity_at,
        screenshots_enabled: resolved.screenshots_enabled
            && state.user_settings_for(&staff.user_id).screenshots_enabled,
    }
}

// ---------------------------------------------------------------------------
// Handlers: status, timers, entries
// ---------------------------------------------------------------------------

async fn get_status(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        Ok(json!({
            "module": "timetracker",
            "timers": state.timers.len(),
            "entries": state.entries.len(),
            "screenshots": state.screenshots.len(),
            "timesheets": state.timesheets.len(),
            "teams": state.teams.len(),
            "staff": state.staff.len(),
            "buyer_settings": state.buyer_settings.len(),
            "desktop_agent": "not_built",
            "desktop_agent_note": "No desktop or mobile agent exists yet. Agent endpoints under /api/v1/timetracker/agent/* are the server API the future agent app will call.",
        }))
    })();
    json::respond(result, debug)
}

async fn list_timers(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TimeTrackerTimer>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let status_filter = params.filters.get("status").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let mut items: Vec<TimeTrackerTimer> = state
            .timers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|timer| {
                match &user_filter {
                    Some(wanted) => &timer.user_id == wanted,
                    None => timer.user_id == caller.actor,
                }
            })
            .filter(|timer| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "running" => TimerStatus::Running,
                        "paused" => TimerStatus::Paused,
                        "stopped" => TimerStatus::Stopped,
                        _ => return false,
                    };
                    if timer.status != expected {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if timer.project_id.map(|id| id.to_string()) != Some(project_id.clone()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["started_at", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "started_at" => a.started_at.cmp(&b.started_at),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn start_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<StartTimerRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        if active_timer_for(&state, &caller.actor).is_some() {
            return Err(UnifiedError::conflict(
                "An active timer already exists for this user: stop or pause it before starting another",
            ));
        }
        let now = Utc::now();
        let timer = TimeTrackerTimer {
            id: Uuid::now_v7(),
            user_id: caller.actor.clone(),
            project_id: input.project_id,
            task_id: input.task_id,
            description,
            billable: input.billable,
            status: TimerStatus::Running,
            started_at: now,
            paused_total_secs: 0,
            paused_at: None,
            idle_total_secs: 0,
            stopped_at: None,
            entry_id: None,
            created_at: now,
            updated_at: now,
        };
        state.timers.insert(timer.id, timer.clone());
        state.emit(
            events::TIMER_STARTED,
            json!({
                "timer_id": timer.id.to_string(),
                "user_id": timer.user_id,
                "project_id": timer.project_id.map(|id| id.to_string()),
                "task_id": timer.task_id.map(|id| id.to_string()),
                "billable": timer.billable,
                "started_at": timer.started_at.to_rfc3339(),
            }),
        );
        Ok(timer)
    })();
    match result {
        Ok(timer) => json::success_response(timer, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn stop_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let timer = load_timer(&state, &path.into_inner())?;
        if timer.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timer owner can stop their timer",
            ));
        }
        stop_timer_inner(&state, timer.id, "manual_stop")
    })();
    json::respond(result, debug)
}

async fn pause_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let timer = load_timer(&state, &path.into_inner())?;
        if timer.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timer owner can pause their timer",
            ));
        }
        if timer.status != TimerStatus::Running {
            return Err(UnifiedError::conflict("Only a running timer can be paused"));
        }
        let now = Utc::now();
        let mut updated = timer.clone();
        updated.status = TimerStatus::Paused;
        updated.paused_at = Some(now);
        updated.updated_at = now;
        state.timers.insert(updated.id, updated.clone());
        state.emit(
            events::TIMER_PAUSED,
            json!({
                "timer_id": updated.id.to_string(),
                "user_id": updated.user_id,
                "paused_at": now.to_rfc3339(),
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn resume_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let timer = load_timer(&state, &path.into_inner())?;
        if timer.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timer owner can resume their timer",
            ));
        }
        if timer.status != TimerStatus::Paused {
            return Err(UnifiedError::conflict("Only a paused timer can be resumed"));
        }
        let now = Utc::now();
        let mut updated = timer.clone();
        if let Some(paused_at) = updated.paused_at {
            updated.paused_total_secs += (now - paused_at).num_seconds().max(0);
        }
        updated.status = TimerStatus::Running;
        updated.paused_at = None;
        updated.updated_at = now;
        state.timers.insert(updated.id, updated.clone());
        state.emit(
            events::TIMER_RESUMED,
            json!({
                "timer_id": updated.id.to_string(),
                "user_id": updated.user_id,
                "paused_total_secs": updated.paused_total_secs,
            }),
        );
        Ok(updated)
    })();
    json::respond(result, debug)
}

async fn get_active_timer(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let timer = active_timer_for(&state, &caller.actor);
        let elapsed_secs = timer.as_ref().map(|t| t.elapsed_secs(Utc::now()));
        Ok(json!({
            "active": timer.is_some(),
            "timer": timer,
            "elapsed_secs": elapsed_secs,
        }))
    })();
    json::respond(result, debug)
}

async fn list_entries(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TimeEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let approval_filter = params.filters.get("approval").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let billable_filter = params.filters.get("billable").cloned();
        let mut items: Vec<TimeEntry> = state
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                match &user_filter {
                    Some(wanted) => &entry.user_id == wanted,
                    None => entry.user_id == caller.actor,
                }
            })
            .filter(|entry| {
                if let Some(approval) = &approval_filter {
                    let expected = match approval.as_str() {
                        "pending" => EntryApproval::Pending,
                        "approved" => EntryApproval::Approved,
                        "rejected" => EntryApproval::Rejected,
                        _ => return false,
                    };
                    if entry.approval != expected {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if entry.project_id.map(|id| id.to_string()) != Some(project_id.clone()) {
                        return false;
                    }
                }
                if let Some(billable) = &billable_filter {
                    let wanted = billable == "true";
                    if entry.billable != wanted {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["started_at", "created_at", "duration_secs"], "started_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "created_at" => a.created_at.cmp(&b.created_at),
                "duration_secs" => a.duration_secs.cmp(&b.duration_secs),
                _ => a.started_at.cmp(&b.started_at),
            };
            if descending { ordering.reverse() } else { ordering }
        });
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let reason = optional_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let duration_secs = validate_interval(input.started_at, input.ended_at, MAX_ENTRY_SECONDS)?;
        let now = Utc::now();
        let entry = TimeEntry {
            id: Uuid::now_v7(),
            user_id: caller.actor.clone(),
            project_id: input.project_id,
            task_id: input.task_id,
            timer_id: None,
            description,
            started_at: input.started_at,
            ended_at: input.ended_at,
            duration_secs,
            billable: input.billable,
            approval: EntryApproval::Pending,
            approved_by: None,
            approved_at: None,
            rejection_note: None,
            reason,
            created_at: now,
            updated_at: now,
        };
        state.entries.insert(entry.id, entry.clone());
        emit_time_logged(&state, &entry, "manual");
        Ok(entry)
    })();
    match result {
        Ok(entry) => json::success_response(entry, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id != caller.actor {
            can_view_user(&ctx, &caller, &entry.user_id)?;
        }
        Ok(entry)
    })();
    json::respond(result, debug)
}

async fn update_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let mut entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the entry owner can edit their entry",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict(
                "Only a pending entry can be edited; approved or rejected entries are final",
            ));
        }
        let reason = require_text(
            input.reason.as_deref().unwrap_or(""),
            "reason",
            MAX_REASON_LEN,
        )?;
        if let Some(description) = &input.description {
            entry.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(project_id) = input.project_id {
            entry.project_id = project_id;
        }
        if let Some(task_id) = input.task_id {
            entry.task_id = task_id;
        }
        if let Some(billable) = input.billable {
            entry.billable = billable;
        }
        let started_at = input.started_at.unwrap_or(entry.started_at);
        let ended_at = input.ended_at.unwrap_or(entry.ended_at);
        entry.duration_secs = validate_interval(started_at, ended_at, MAX_ENTRY_SECONDS)?;
        entry.started_at = started_at;
        entry.ended_at = ended_at;
        entry.reason = Some(reason.clone());
        entry.updated_at = Utc::now();
        state.entries.insert(entry.id, entry.clone());
        state.emit(
            events::ENTRY_UPDATED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "reason": reason,
                "duration_secs": entry.duration_secs,
            }),
        );
        Ok(entry)
    })();
    json::respond(result, debug)
}

async fn delete_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DeleteEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        let entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the entry owner can delete their entry",
            ));
        }
        if entry.approval == EntryApproval::Approved {
            return Err(UnifiedError::conflict(
                "An approved entry cannot be deleted; ask an approver to reject it first",
            ));
        }
        state.entries.remove(&entry.id);
        state.emit(
            events::ENTRY_DELETED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "reason": reason,
                "duration_secs": entry.duration_secs,
            }),
        );
        Ok(json!({ "id": entry.id.to_string(), "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn approve_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "An entry cannot be approved by its own owner",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict("Only a pending entry can be approved"));
        }
        entry.approval = EntryApproval::Approved;
        entry.approved_by = Some(caller.actor.clone());
        entry.approved_at = Some(Utc::now());
        entry.rejection_note = None;
        entry.updated_at = Utc::now();
        state.entries.insert(entry.id, entry.clone());
        state.emit(
            events::ENTRY_APPROVED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "approved_by": caller.actor,
                "note": note,
            }),
        );
        Ok(entry)
    })();
    json::respond(result, debug)
}

async fn reject_entry(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TimeEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut entry = load_entry(&state, &path.into_inner())?;
        if entry.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "An entry cannot be rejected by its own owner",
            ));
        }
        if entry.approval != EntryApproval::Pending {
            return Err(UnifiedError::conflict("Only a pending entry can be rejected"));
        }
        entry.approval = EntryApproval::Rejected;
        entry.approved_by = Some(caller.actor.clone());
        entry.approved_at = Some(Utc::now());
        entry.rejection_note = note.clone();
        entry.updated_at = Utc::now();
        state.entries.insert(entry.id, entry.clone());
        state.emit(
            events::ENTRY_REJECTED,
            json!({
                "entry_id": entry.id.to_string(),
                "user_id": entry.user_id,
                "rejected_by": caller.actor,
                "note": note,
            }),
        );
        Ok(entry)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: activity, app usage, screenshots
// ---------------------------------------------------------------------------

/// Record one activity sample for `user_id`. Shared by the
/// dashboard endpoint and the agent endpoint. The buyer's activity
/// level gates it: `Off` rejects every report, `Minimal` and
/// `Full` accept scores. Idle seconds at or above the resolved
/// idle threshold auto-pause the user's running timer (the idle
/// period is recorded on the sample and on the timer).
fn record_activity(
    state: &TimeTrackerState,
    user_id: &str,
    input: ReportActivityRequest,
) -> UnifiedResult<ActivitySample> {
    let resolved = state.resolve_settings_for_user(user_id);
    if resolved.activity_level == ActivityLevel::Off {
        return Err(UnifiedError::unprocessable(
            "Activity tracking is disabled for this company (activity_level off)",
        ));
    }
    if !(0..=100).contains(&input.activity_score) {
        return Err(UnifiedError::bad_request(
            "activity_score must be between 0 and 100",
        ));
    }
    if input.idle_secs < 0 {
        return Err(UnifiedError::bad_request("idle_secs must not be negative"));
    }
    validate_interval(input.interval_start, input.interval_end, MAX_INTERVAL_SECONDS)?;
    if let Some(timer_id) = input.timer_id {
        let timer = state
            .timers
            .get(&timer_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {timer_id}")))?;
        if timer.user_id != user_id {
            return Err(UnifiedError::forbidden(
                "Activity cannot be reported against another user's timer",
            ));
        }
    }
    let now = Utc::now();
    let triggered_auto_pause = input.idle_secs >= resolved.idle_threshold_secs
        && auto_pause_on_idle(state, user_id, input.idle_secs);
    let sample = ActivitySample {
        id: Uuid::now_v7(),
        user_id: user_id.to_string(),
        timer_id: input.timer_id,
        project_id: input.project_id,
        task_id: input.task_id,
        activity_score: input.activity_score,
        idle_secs: input.idle_secs,
        interval_start: input.interval_start,
        interval_end: input.interval_end,
        triggered_auto_pause,
        created_at: now,
    };
    state.activity.insert(sample.id, sample.clone());
    state.emit(
        events::ACTIVITY_REPORTED,
        json!({
            "sample_id": sample.id.to_string(),
            "user_id": user_id,
            "activity_score": sample.activity_score,
            "idle_secs": sample.idle_secs,
            "triggered_auto_pause": triggered_auto_pause,
        }),
    );
    Ok(sample)
}

async fn report_activity(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReportActivityRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ActivitySample> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        record_activity(&state, &caller.actor, body.into_inner())
    })();
    match result {
        Ok(sample) => json::success_response(sample, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_activity(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ActivitySample>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let mut items: Vec<ActivitySample> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sample| match &user_filter {
                Some(wanted) => &sample.user_id == wanted,
                None => sample.user_id == caller.actor,
            })
            .collect();
        items.sort_by(|a, b| b.interval_start.cmp(&a.interval_start));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

/// Record one app / website usage interval for `user_id`. The
/// buyer's activity level gates it (`Full` only — `Minimal`
/// collects scores, not applications), then the buyer's
/// allowlist / blocklist is enforced on the application name and
/// the website domain.
fn record_usage(
    state: &TimeTrackerState,
    user_id: &str,
    input: ReportUsageRequest,
) -> UnifiedResult<AppUsageRecord> {
    let resolved = state.resolve_settings_for_user(user_id);
    match resolved.activity_level {
        ActivityLevel::Off => {
            return Err(UnifiedError::unprocessable(
                "Activity tracking is disabled for this company (activity_level off)",
            ));
        }
        ActivityLevel::Minimal => {
            return Err(UnifiedError::unprocessable(
                "App and website usage tracking is disabled for this company (activity_level minimal collects activity scores only)",
            ));
        }
        ActivityLevel::Full => {}
    }
    let application_name = require_text(
        &input.application_name,
        "application_name",
        MAX_APP_NAME_LEN,
    )?;
    let domain = match &input.domain {
        Some(raw) => Some(require_text(raw, "domain", MAX_APP_NAME_LEN)?.to_lowercase()),
        None => None,
    };
    let window_title_hash = match &input.window_title_hash {
        Some(raw) => Some(valid_title_hash(raw)?),
        None => None,
    };
    let category = input.category.unwrap_or_default();
    let duration_secs = validate_interval(
        input.interval_start,
        input.interval_end,
        MAX_INTERVAL_SECONDS,
    )?;
    check_usage_allowed(&resolved, &application_name, domain.as_deref())?;
    let now = Utc::now();
    let record = AppUsageRecord {
        id: Uuid::now_v7(),
        user_id: user_id.to_string(),
        application_name,
        domain,
        window_title_hash,
        category,
        duration_secs,
        interval_start: input.interval_start,
        interval_end: input.interval_end,
        created_at: now,
    };
    state.usage.insert(record.id, record.clone());
    state.emit(
        events::USAGE_REPORTED,
        json!({
            "usage_id": record.id.to_string(),
            "user_id": user_id,
            "application_name": record.application_name,
            "domain": record.domain,
            "category": format!("{:?}", record.category).to_lowercase(),
            "duration_secs": record.duration_secs,
        }),
    );
    Ok(record)
}

async fn report_usage(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReportUsageRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AppUsageRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        record_usage(&state, &caller.actor, body.into_inner())
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_usage(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AppUsageRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let category_filter = params.filters.get("category").cloned();
        let mut items: Vec<AppUsageRecord> = state
            .usage
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| match &user_filter {
                Some(wanted) => &record.user_id == wanted,
                None => record.user_id == caller.actor,
            })
            .filter(|record| {
                if let Some(category) = &category_filter {
                    let expected = match category.as_str() {
                        "work" => UsageCategory::Work,
                        "communication" => UsageCategory::Communication,
                        "development" => UsageCategory::Development,
                        "browsing" => UsageCategory::Browsing,
                        "social" => UsageCategory::Social,
                        "entertainment" => UsageCategory::Entertainment,
                        "other" => UsageCategory::Other,
                        _ => return false,
                    };
                    if record.category != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.interval_start.cmp(&a.interval_start));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn list_screenshots(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ScreenshotRecord>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_screenshots(&ctx, &caller, wanted)?;
        }
        let mut items: Vec<ScreenshotRecord> = state
            .screenshots
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|shot| match &user_filter {
                Some(wanted) => &shot.user_id == wanted,
                None => shot.user_id == caller.actor,
            })
            .collect();
        items.sort_by(|a, b| b.captured_at.cmp(&a.captured_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_screenshot(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let id = parse_uuid(&path.into_inner(), "screenshot")?;
        let shot = state
            .screenshots
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Screenshot not found: {id}")))?;
        if shot.user_id != caller.actor {
            can_view_screenshots(&ctx, &caller, &shot.user_id)?;
        }
        let provider = ctx
            .storage_registry
            .get(&shot.storage_target)
            .ok_or_else(|| {
                UnifiedError::new(
                    format!("Storage target \"{}\" is not registered", shot.storage_target),
                    503,
                )
            })?;
        let url = provider
            .signed_url(&shot.storage_key, SCREENSHOT_SIGNED_URL_SECS)
            .await?;
        Ok(json!({
            "screenshot": shot,
            "url": url,
            "expires_secs": SCREENSHOT_SIGNED_URL_SECS,
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn delete_screenshot(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let id = parse_uuid(&path.into_inner(), "screenshot")?;
        let shot = state
            .screenshots
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Screenshot not found: {id}")))?;
        if shot.user_id != caller.actor {
            require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        }
        if let Some(provider) = ctx.storage_registry.get(&shot.storage_target) {
            provider.delete(&shot.storage_key).await?;
        }
        state.screenshots.remove(&id);
        state.emit(
            events::SCREENSHOT_DELETED,
            json!({
                "screenshot_id": shot.id.to_string(),
                "user_id": shot.user_id,
                "storage_target": shot.storage_target,
                "storage_key": shot.storage_key,
                "reason": "manual_delete",
                "object_deleted": true,
            }),
        );
        Ok(json!({ "id": shot.id.to_string(), "deleted": true }))
    })()
    .await;
    json::respond(result, debug)
}

async fn purge_screenshots(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE)?;
        let (removed, failed) = purge_expired_screenshots(&state, &ctx.storage_registry).await;
        // Best-effort: ask the scheduler to run the purge on a
        // schedule from now on. False today: the scheduler does not
        // know the `timetracker.screenshots.purge` kind yet (see
        // `request_screenshot_purge`).
        let scheduler_queued = request_screenshot_purge(&state);
        Ok(json!({
            "removed": removed,
            "failed": failed,
            "purged_by": caller.actor,
            "scheduler_enqueue_queued": scheduler_queued,
        }))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: rates, payroll, timesheets, reports, dashboard, settings
// ---------------------------------------------------------------------------

async fn list_rates(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<HourlyRate>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let mut items: Vec<HourlyRate> = state
            .rates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|rate| match &user_filter {
                Some(wanted) => &rate.user_id == wanted,
                None => rate.user_id == caller.actor,
            })
            .collect();
        items.sort_by(|a, b| b.effective_from.cmp(&a.effective_from));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HourlyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_RATES)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 128)?;
        if input.rate < 0.0 || !input.rate.is_finite() {
            return Err(UnifiedError::bad_request("rate must be a finite number >= 0"));
        }
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => state.global_settings().default_currency,
        };
        let effective_from = input.effective_from.unwrap_or_else(Utc::now);
        if let Some(effective_to) = input.effective_to {
            if effective_to <= effective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must be after effective_from",
                ));
            }
        }
        let now = Utc::now();
        let rate = HourlyRate {
            id: Uuid::now_v7(),
            user_id,
            currency,
            rate: round4(input.rate),
            effective_from,
            effective_to: input.effective_to,
            created_at: now,
            updated_at: now,
        };
        state.rates.insert(rate.id, rate.clone());
        state.emit(
            events::RATE_CREATED,
            json!({
                "rate_id": rate.id.to_string(),
                "user_id": rate.user_id,
                "currency": rate.currency,
                "rate": rate.rate,
                "created_by": caller.actor,
            }),
        );
        Ok(rate)
    })();
    match result {
        Ok(rate) => json::success_response(rate, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HourlyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let id = parse_uuid(&path.into_inner(), "rate")?;
        let rate = state
            .rates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
        if rate.user_id != caller.actor {
            can_view_user(&ctx, &caller, &rate.user_id)?;
        }
        Ok(rate)
    })();
    json::respond(result, debug)
}

async fn update_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HourlyRate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_RATES)?;
        let input = body.into_inner();
        let id = parse_uuid(&path.into_inner(), "rate")?;
        let mut rate = state
            .rates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
        if let Some(raw) = &input.currency {
            rate.currency = valid_currency(raw)?;
        }
        if let Some(value) = input.rate {
            if value < 0.0 || !value.is_finite() {
                return Err(UnifiedError::bad_request("rate must be a finite number >= 0"));
            }
            rate.rate = round4(value);
        }
        if let Some(effective_from) = input.effective_from {
            rate.effective_from = effective_from;
        }
        if let Some(effective_to) = input.effective_to {
            rate.effective_to = effective_to;
        }
        if let Some(effective_to) = rate.effective_to {
            if effective_to <= rate.effective_from {
                return Err(UnifiedError::bad_request(
                    "effective_to must be after effective_from",
                ));
            }
        }
        rate.updated_at = Utc::now();
        state.rates.insert(rate.id, rate.clone());
        state.emit(
            events::RATE_UPDATED,
            json!({
                "rate_id": rate.id.to_string(),
                "user_id": rate.user_id,
                "currency": rate.currency,
                "rate": rate.rate,
                "updated_by": caller.actor,
            }),
        );
        Ok(rate)
    })();
    json::respond(result, debug)
}

async fn delete_rate(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_RATES)?;
        let id = parse_uuid(&path.into_inner(), "rate")?;
        let rate = state
            .rates
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Rate not found: {id}")))?;
        state.rates.remove(&id);
        state.emit(
            events::RATE_DELETED,
            json!({
                "rate_id": rate.id.to_string(),
                "user_id": rate.user_id,
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "id": rate.id.to_string(), "deleted": true }))
    })();
    json::respond(result, debug)
}

/// Parse a `from` / `to` window from query filters. Defaults to
/// the last 7 days. `from` must precede `to` and the window may
/// not exceed 366 days.
fn parse_window(params: &PaginationParams) -> UnifiedResult<(DateTime<Utc>, DateTime<Utc>)> {
    let now = Utc::now();
    let parse = |raw: &str| {
        DateTime::parse_from_rfc3339(raw)
            .map(|parsed| parsed.with_timezone(&Utc))
            .map_err(|_| UnifiedError::bad_request(format!("Invalid RFC 3339 timestamp: {raw}")))
    };
    let from = match params.filters.get("from") {
        Some(raw) => parse(raw)?,
        None => now - Duration::days(7),
    };
    let to = match params.filters.get("to") {
        Some(raw) => parse(raw)?,
        None => now,
    };
    if to <= from {
        return Err(UnifiedError::bad_request("to must be after from"));
    }
    if (to - from).num_days() > 366 {
        return Err(UnifiedError::bad_request("Window must be at most 366 days"));
    }
    Ok((from, to))
}

async fn get_payroll(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_PAYROLL)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let lines = compute_payroll(&state, from, to, user_filter.as_deref());
        let total_pay: f64 = round4(lines.iter().map(|line| line.total_pay).sum());
        Ok(json!({
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "lines": lines,
            "total_pay": total_pay,
            "currency_note": "Lines carry their own currency; total_pay is a plain sum across all lines and is only meaningful when every line shares one currency. Sum per currency when a company pays in several currencies.",
        }))
    })();
    json::respond(result, debug)
}

async fn list_timesheets(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Timesheet>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            can_view_user(&ctx, &caller, wanted)?;
        }
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Timesheet> = state
            .timesheets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sheet| match &user_filter {
                Some(wanted) => &sheet.user_id == wanted,
                None => sheet.user_id == caller.actor,
            })
            .filter(|sheet| {
                if let Some(status) = &status_filter {
                    let expected = match status.as_str() {
                        "draft" => TimesheetStatus::Draft,
                        "submitted" => TimesheetStatus::Submitted,
                        "approved" => TimesheetStatus::Approved,
                        "rejected" => TimesheetStatus::Rejected,
                        _ => return false,
                    };
                    if sheet.status != expected {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.period_start.cmp(&a.period_start));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateTimesheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        if input.period_end <= input.period_start {
            return Err(UnifiedError::bad_request(
                "period_end must be after period_start",
            ));
        }
        let owner = match &input.user_id {
            Some(wanted) if wanted != &caller.actor => {
                can_view_user(&ctx, &caller, wanted)?;
                wanted.clone()
            }
            _ => caller.actor.clone(),
        };
        let entry_ids: Vec<Uuid> = if input.entry_ids.is_empty() {
            state
                .entries
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|entry| {
                    entry.user_id == owner
                        && entry.approval == EntryApproval::Pending
                        && entry.started_at >= input.period_start
                        && entry.started_at <= input.period_end
                })
                .map(|entry| entry.id)
                .collect()
        } else {
            for entry_id in &input.entry_ids {
                let entry = state
                    .entries
                    .get(entry_id)
                    .map(|record| record.value().clone())
                    .ok_or_else(|| UnifiedError::not_found(format!("Time entry not found: {entry_id}")))?;
                if entry.user_id != owner {
                    return Err(UnifiedError::forbidden(
                        "A timesheet can only contain the owner's own entries",
                    ));
                }
                if entry.approval != EntryApproval::Pending {
                    return Err(UnifiedError::conflict(
                        "Only pending entries can be added to a timesheet",
                    ));
                }
            }
            input.entry_ids.clone()
        };
        if entry_ids.is_empty() {
            return Err(UnifiedError::bad_request(
                "No pending entries in this period to put on a timesheet",
            ));
        }
        let now = Utc::now();
        let sheet = Timesheet {
            id: Uuid::now_v7(),
            user_id: owner.clone(),
            period_start: input.period_start,
            period_end: input.period_end,
            status: TimesheetStatus::Draft,
            entry_ids,
            submitted_at: None,
            reviewed_by: None,
            reviewed_at: None,
            review_note: None,
            created_at: now,
            updated_at: now,
        };
        state.timesheets.insert(sheet.id, sheet.clone());
        state.emit(
            events::TIMESHEET_CREATED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": owner,
                "period_start": sheet.period_start.to_rfc3339(),
                "period_end": sheet.period_end.to_rfc3339(),
                "entry_count": sheet.entry_ids.len(),
                "created_by": caller.actor,
            }),
        );
        Ok(sheet)
    })();
    match result {
        Ok(sheet) => json::success_response(sheet, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

fn load_timesheet(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<Timesheet> {
    let id = parse_uuid(raw_id, "timesheet")?;
    state
        .timesheets
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Timesheet not found: {id}")))
}

async fn get_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id != caller.actor {
            can_view_user(&ctx, &caller, &sheet.user_id)?;
        }
        Ok(sheet)
    })();
    json::respond(result, debug)
}

async fn submit_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let mut sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the timesheet owner can submit it",
            ));
        }
        if sheet.status != TimesheetStatus::Draft {
            return Err(UnifiedError::conflict(
                "Only a draft timesheet can be submitted",
            ));
        }
        sheet.status = TimesheetStatus::Submitted;
        sheet.submitted_at = Some(Utc::now());
        sheet.updated_at = Utc::now();
        state.timesheets.insert(sheet.id, sheet.clone());
        state.emit(
            events::TIMESHEET_SUBMITTED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": sheet.user_id,
                "entry_count": sheet.entry_ids.len(),
                "submitted_by": caller.actor,
            }),
        );
        Ok(sheet)
    })();
    json::respond(result, debug)
}

async fn approve_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewTimesheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "A timesheet cannot be approved by its own owner",
            ));
        }
        if sheet.status != TimesheetStatus::Submitted {
            return Err(UnifiedError::conflict(
                "Only a submitted timesheet can be approved",
            ));
        }
        sheet.status = TimesheetStatus::Approved;
        sheet.reviewed_by = Some(caller.actor.clone());
        sheet.reviewed_at = Some(Utc::now());
        sheet.review_note = note.clone();
        sheet.updated_at = Utc::now();
        state.timesheets.insert(sheet.id, sheet.clone());
        // Approving the sheet approves its still-pending entries,
        // so payroll sees them as payable.
        let now = Utc::now();
        for entry_id in &sheet.entry_ids {
            if let Some(mut entry) = state.entries.get_mut(entry_id) {
                if entry.approval == EntryApproval::Pending {
                    entry.approval = EntryApproval::Approved;
                    entry.approved_by = Some(caller.actor.clone());
                    entry.approved_at = Some(now);
                    entry.updated_at = now;
                }
            }
        }
        state.emit(
            events::TIMESHEET_APPROVED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": sheet.user_id,
                "approved_by": caller.actor,
                "entry_count": sheet.entry_ids.len(),
                "note": note,
            }),
        );
        Ok(sheet)
    })();
    json::respond(result, debug)
}

async fn reject_timesheet(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReviewTimesheetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Timesheet> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_APPROVE)?;
        let input = body.into_inner();
        let note = optional_text(&input.note, "note", MAX_NOTE_LEN)?;
        let mut sheet = load_timesheet(&state, &path.into_inner())?;
        if sheet.user_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "A timesheet cannot be rejected by its own owner",
            ));
        }
        if sheet.status != TimesheetStatus::Submitted {
            return Err(UnifiedError::conflict(
                "Only a submitted timesheet can be rejected",
            ));
        }
        sheet.status = TimesheetStatus::Rejected;
        sheet.reviewed_by = Some(caller.actor.clone());
        sheet.reviewed_at = Some(Utc::now());
        sheet.review_note = note.clone();
        sheet.updated_at = Utc::now();
        state.timesheets.insert(sheet.id, sheet.clone());
        state.emit(
            events::TIMESHEET_REJECTED,
            json!({
                "timesheet_id": sheet.id.to_string(),
                "user_id": sheet.user_id,
                "rejected_by": caller.actor,
                "note": note,
            }),
        );
        Ok(sheet)
    })();
    json::respond(result, debug)
}

async fn report_daily(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ReportRow>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        if (to - from).num_days() > 1 {
            return Err(UnifiedError::bad_request(
                "Daily report covers at most one day; pass from/to within a single day",
            ));
        }
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let project_filter = params
            .filters
            .get("project_id")
            .map(|raw| parse_uuid(raw, "project_id"))
            .transpose()?;
        let rows = report_rows(&state, from, to, user_filter.as_deref(), project_filter);
        Ok(paginate(rows, &params))
    })();
    json::respond(result, debug)
}

async fn report_weekly(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ReportRow>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        if (to - from).num_days() > 8 {
            return Err(UnifiedError::bad_request(
                "Weekly report covers at most 8 days; pass a one-week from/to window",
            ));
        }
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let project_filter = params
            .filters
            .get("project_id")
            .map(|raw| parse_uuid(raw, "project_id"))
            .transpose()?;
        let rows = report_rows(&state, from, to, user_filter.as_deref(), project_filter);
        Ok(paginate(rows, &params))
    })();
    json::respond(result, debug)
}

async fn report_summary(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        let user_filter = params.filters.get("user_id").cloned();
        if let Some(wanted) = &user_filter {
            if wanted != &caller.actor {
                can_view_user(&ctx, &caller, wanted)?;
            }
        }
        let project_filter = params
            .filters
            .get("project_id")
            .map(|raw| parse_uuid(raw, "project_id"))
            .transpose()?;
        let rows = report_rows(&state, from, to, user_filter.as_deref(), project_filter);
        let total_secs: i64 = rows.iter().map(|row| row.total_secs).sum();
        let billable_secs: i64 = rows.iter().map(|row| row.billable_secs).sum();
        let entry_count: usize = rows.iter().map(|row| row.entry_count).sum();
        Ok(json!({
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "total_secs": total_secs,
            "total_hours": round4(total_secs as f64 / 3600.0),
            "billable_secs": billable_secs,
            "billable_hours": round4(billable_secs as f64 / 3600.0),
            "non_billable_secs": total_secs - billable_secs,
            "entry_count": entry_count,
            "rows": rows,
        }))
    })();
    json::respond(result, debug)
}

async fn team_dashboard(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_VIEW_TEAM)?;
        let company_id = claimed_company_id(&req)?;
        require_company_member(&ctx, &caller.actor, company_id)?;
        let global = state.global_settings();
        let now = Utc::now();
        let today_start = now
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .map(|naive| naive.and_utc())
            .unwrap_or(now);
        let online_cutoff = now - Duration::seconds(ONLINE_WINDOW_SECS);
        let members: Vec<StaffSettings> = state
            .staff
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|staff| staff.company_id == company_id)
            .collect();
        let mut statuses: Vec<TeamMemberStatus> = Vec::new();
        for staff in &members {
            let today_secs: i64 = state
                .entries
                .iter()
                .filter(|entry| {
                    entry.value().user_id == staff.user_id
                        && entry.value().started_at >= today_start
                })
                .map(|entry| entry.value().duration_secs)
                .sum();
            let today_samples: Vec<ActivitySample> = state
                .activity
                .iter()
                .filter(|entry| {
                    entry.value().user_id == staff.user_id
                        && entry.value().interval_start >= today_start
                })
                .map(|entry| entry.value().clone())
                .collect();
            let avg_activity_today = if today_samples.is_empty() {
                None
            } else {
                Some(
                    today_samples.iter().map(|sample| sample.activity_score).sum::<i64>() as f64
                        / today_samples.len() as f64,
                )
            };
            let last_activity_at = today_samples
                .iter()
                .map(|sample| sample.interval_end)
                .max();
            let active_timer = active_timer_for(state, &staff.user_id);
            let online_now = active_timer.is_some()
                || last_activity_at.map(|at| at >= online_cutoff).unwrap_or(false);
            let low_activity = avg_activity_today
                .map(|avg| avg < global.low_activity_threshold as f64)
                .unwrap_or(false);
            statuses.push(TeamMemberStatus {
                user_id: staff.user_id.clone(),
                online_now,
                active_timer_id: active_timer.map(|timer| timer.id),
                today_secs,
                avg_activity_today,
                low_activity,
                last_activity_at,
            });
        }
        let online_count = statuses.iter().filter(|status| status.online_now).count();
        let low_count = statuses.iter().filter(|status| status.low_activity).count();
        Ok(json!({
            "company_id": company_id.to_string(),
            "members": statuses,
            "online_now": online_count,
            "low_activity_count": low_count,
            "total_members": members.len(),
        }))
    })();
    json::respond(result, debug)
}

async fn get_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TrackerSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        Ok(state.global_settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TrackerSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_SETTINGS)?;
        let input = body.into_inner();
        let mut settings = state.global_settings();
        if let Some(value) = input.idle_threshold_secs {
            if !(30..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idle_threshold_secs must be between 30 and 86400",
                ));
            }
            settings.idle_threshold_secs = value;
        }
        if let Some(value) = input.retention_days {
            if !(1..=3650).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retention_days must be between 1 and 3650",
                ));
            }
            settings.retention_days = value;
        }
        if let Some(value) = input.overtime_hours_per_week {
            if !(0.0..=168.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "overtime_hours_per_week must be between 0 and 168",
                ));
            }
            settings.overtime_hours_per_week = value;
        }
        if let Some(value) = input.overtime_multiplier {
            if !(1.0..=4.0).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "overtime_multiplier must be between 1 and 4",
                ));
            }
            settings.overtime_multiplier = value;
        }
        if let Some(value) = input.low_activity_threshold {
            if !(0..=100).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "low_activity_threshold must be between 0 and 100",
                ));
            }
            settings.low_activity_threshold = value;
        }
        if let Some(raw) = &input.storage_target {
            settings.storage_target = require_text(raw, "storage_target", MAX_TARGET_LEN)?;
        }
        if let Some(raw) = &input.default_currency {
            settings.default_currency = valid_currency(raw)?;
        }
        if let Some(value) = input.auto_approve_timer_entries {
            settings.auto_approve_timer_entries = value;
        }
        settings.updated_at = Utc::now();
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            events::SETTINGS_UPDATED,
            json!({
                "updated_by": caller.actor,
                "idle_threshold_secs": settings.idle_threshold_secs,
                "retention_days": settings.retention_days,
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

async fn get_my_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let mine = state.user_settings_for(&caller.actor);
        let resolved = state.resolve_settings_for_user(&caller.actor);
        Ok(json!({
            "user_id": caller.actor,
            "screenshots_enabled": mine.screenshots_enabled,
            "blur_screenshots": mine.blur_screenshots,
            "idle_threshold_secs": mine.idle_threshold_secs,
            "resolved": {
                "company_id": resolved.company_id.map(|id| id.to_string()),
                "screenshots_enabled": resolved.screenshots_enabled,
                "screenshot_interval_secs": resolved.screenshot_interval_secs,
                "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
                "idle_threshold_secs": resolved.idle_threshold_secs,
                "blur_screenshots": resolved.blur_screenshots,
                "timezone": resolved.timezone,
                "retention_days": resolved.retention_days,
            },
        }))
    })();
    json::respond(result, debug)
}

async fn update_my_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateMySettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<UserTrackerSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_WRITE)?;
        let input = body.into_inner();
        let mut mine = state.user_settings_for(&caller.actor);
        if let Some(value) = input.screenshots_enabled {
            mine.screenshots_enabled = value;
        }
        if let Some(value) = input.blur_screenshots {
            mine.blur_screenshots = value;
        }
        if let Some(value) = input.idle_threshold_secs {
            if !(30..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idle_threshold_secs must be between 30 and 86400",
                ));
            }
            mine.idle_threshold_secs = Some(value);
        }
        mine.updated_at = Utc::now();
        state
            .user_settings
            .insert(caller.actor.clone(), mine.clone());
        Ok(mine)
    })();
    json::respond(result, debug)
}

fn validate_list(
    values: &[String],
    field: &str,
) -> UnifiedResult<Vec<String>> {
    let mut cleaned: Vec<String> = Vec::new();
    for raw in values {
        let value = require_text(raw, field, MAX_APP_NAME_LEN)?.to_lowercase();
        if !cleaned.contains(&value) {
            cleaned.push(value);
        }
    }
    Ok(cleaned)
}

async fn get_buyer_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = claimed_company_id(&req)?;
        require_company_member(&ctx, &caller.actor, company_id)?;
        let stored = state.buyer_settings_for(company_id);
        let resolved = state.resolve_settings_for_company(company_id);
        Ok(json!({
            "company_id": company_id.to_string(),
            "overrides": stored,
            "resolved": {
                "screenshots_enabled": resolved.screenshots_enabled,
                "screenshot_interval_secs": resolved.screenshot_interval_secs,
                "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
                "idle_threshold_secs": resolved.idle_threshold_secs,
                "blur_screenshots": resolved.blur_screenshots,
                "allowed_apps": resolved.allowed_apps,
                "blocked_apps": resolved.blocked_apps,
                "allowed_websites": resolved.allowed_websites,
                "blocked_websites": resolved.blocked_websites,
                "timezone": resolved.timezone,
                "working_hours_start": resolved.working_hours_start,
                "working_hours_end": resolved.working_hours_end,
                "retention_days": resolved.retention_days,
            },
        }))
    })();
    json::respond(result, debug)
}

async fn update_buyer_settings(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<UpdateBuyerSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BuyerSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_BUYER_SETTINGS)?;
        let company_id = claimed_company_id(&req)?;
        require_company_member(&ctx, &caller.actor, company_id)?;
        let input = body.into_inner();
        let mut settings = state.buyer_settings_for(company_id);
        if let Some(value) = input.screenshots_enabled {
            settings.screenshots_enabled = Some(value);
        }
        if let Some(value) = input.screenshot_interval_secs {
            if !(60..=3600).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "screenshot_interval_secs must be between 60 and 3600",
                ));
            }
            settings.screenshot_interval_secs = Some(value);
        }
        if let Some(value) = input.activity_level {
            settings.activity_level = Some(value);
        }
        if let Some(value) = input.idle_timeout_secs {
            if !(30..=86_400).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "idle_timeout_secs must be between 30 and 86400",
                ));
            }
            settings.idle_timeout_secs = Some(value);
        }
        if let Some(value) = input.blur_screenshots {
            settings.blur_screenshots = Some(value);
        }
        if let Some(values) = &input.allowed_apps {
            settings.allowed_apps = validate_list(values, "allowed_apps")?;
        }
        if let Some(values) = &input.blocked_apps {
            settings.blocked_apps = validate_list(values, "blocked_apps")?;
        }
        if let Some(values) = &input.allowed_websites {
            settings.allowed_websites = validate_list(values, "allowed_websites")?;
        }
        if let Some(values) = &input.blocked_websites {
            settings.blocked_websites = validate_list(values, "blocked_websites")?;
        }
        if let Some(value) = input.default_hourly_rate {
            if value < 0.0 || !value.is_finite() {
                return Err(UnifiedError::bad_request(
                    "default_hourly_rate must be a finite number >= 0",
                ));
            }
            settings.default_hourly_rate = Some(round4(value));
        }
        if let Some(raw) = &input.timezone {
            settings.timezone = Some(valid_timezone(raw)?);
        }
        if let Some(raw) = &input.working_hours_start {
            settings.working_hours_start = Some(valid_hhmm(raw, "working_hours_start")?);
        }
        if let Some(raw) = &input.working_hours_end {
            settings.working_hours_end = Some(valid_hhmm(raw, "working_hours_end")?);
        }
        if let Some(days) = &input.working_days {
            for day in days {
                if !(1..=7).contains(day) {
                    return Err(UnifiedError::bad_request(
                        "working_days entries must be ISO weekday numbers 1 (Monday) to 7 (Sunday)",
                    ));
                }
            }
            settings.working_days = days.clone();
        }
        if let Some(value) = input.retention_days {
            if !(1..=3650).contains(&value) {
                return Err(UnifiedError::bad_request(
                    "retention_days must be between 1 and 3650",
                ));
            }
            settings.retention_days = Some(value);
        }
        settings.updated_at = Utc::now();
        state
            .buyer_settings
            .insert(company_id, settings.clone());
        state.emit(
            events::SETTINGS_UPDATED,
            json!({
                "company_id": company_id.to_string(),
                "updated_by": caller.actor,
                "scope": "buyer",
            }),
        );
        Ok(settings)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: staff management and teams
// ---------------------------------------------------------------------------

/// The company a staff call acts on: the `X-Company-Id` claim,
/// verified to exist and to contain the caller. Every staff and
/// team handler starts here, so a company admin can only ever
/// manage their own company's staff.
fn staff_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> UnifiedResult<Uuid> {
    let company_id = claimed_company_id(req)?;
    require_company_member(ctx, &caller.actor, company_id)?;
    Ok(company_id)
}

async fn list_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StaffOverview>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let role_filter = params.filters.get("role").cloned();
        let team_filter = params
            .filters
            .get("team_id")
            .map(|raw| parse_uuid(raw, "team_id"))
            .transpose()?;
        let mut items: Vec<StaffOverview> = state
            .staff
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|staff| staff.company_id == company_id)
            .filter(|staff| {
                if let Some(role) = &role_filter {
                    let expected = match role.as_str() {
                        "admin" => TrackerRole::Admin,
                        "manager" => TrackerRole::Manager,
                        "member" => TrackerRole::Member,
                        "viewer" => TrackerRole::Viewer,
                        _ => return false,
                    };
                    if staff.role != expected {
                        return false;
                    }
                }
                if let Some(team_id) = team_filter {
                    if !team_ids_for_user(&state, &staff.user_id).contains(&team_id) {
                        return false;
                    }
                }
                true
            })
            .map(|staff| staff_overview(&state, &staff))
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn add_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<AddStaffRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffOverview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let user_id = require_text(&input.user_id, "user_id", 128)?;
        if state.staff.contains_key(&user_id) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already tracker staff"
            )));
        }
        let manager_id = match &input.manager_id {
            Some(raw) => Some(require_text(raw, "manager_id", 128)?),
            None => None,
        };
        let team_id = input.team_id;
        if let Some(team_id) = team_id {
            let team = state
                .teams
                .get(&team_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Team not found: {team_id}")))?;
            if team.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "Team belongs to a different company",
                ));
            }
        }
        let now = Utc::now();
        let role = input.role.unwrap_or_default();
        let staff = StaffSettings {
            user_id: user_id.clone(),
            company_id,
            role,
            manager_id,
            screenshots_enabled: None,
            blur_screenshots: None,
            idle_threshold_secs: None,
            activity_level: None,
            timezone: None,
            working_hours_start: None,
            working_hours_end: None,
            created_at: now,
            updated_at: now,
        };
        state.staff.insert(user_id.clone(), staff.clone());
        if let Some(team_id) = team_id {
            state.team_members.insert(
                team_member_key(team_id, &user_id),
                TeamMember {
                    team_id,
                    user_id: user_id.clone(),
                    role,
                    joined_at: now,
                },
            );
        }
        state.emit(
            "timetracker.staff.added",
            json!({
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "role": format!("{:?}", role).to_lowercase(),
                "team_id": team_id.map(|id| id.to_string()),
                "added_by": caller.actor,
            }),
        );
        Ok(staff_overview(&state, &staff))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffOverview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let user_id = path.into_inner();
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        Ok(staff_overview(&state, &staff))
    })();
    json::respond(result, debug)
}

async fn update_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateStaffRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffOverview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let user_id = path.into_inner();
        let mut staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        if let Some(role) = input.role {
            staff.role = role;
        }
        if let Some(manager_id) = input.manager_id {
            staff.manager_id = match manager_id {
                Some(raw) => Some(require_text(&raw, "manager_id", 128)?),
                None => None,
            };
        }
        if let Some(value) = input.screenshots_enabled {
            staff.screenshots_enabled = value;
        }
        if let Some(value) = input.blur_screenshots {
            staff.blur_screenshots = value;
        }
        if let Some(value) = input.idle_threshold_secs {
            staff.idle_threshold_secs = match value {
                Some(secs) => {
                    if !(30..=86_400).contains(&secs) {
                        return Err(UnifiedError::bad_request(
                            "idle_threshold_secs must be between 30 and 86400",
                        ));
                    }
                    Some(secs)
                }
                None => None,
            };
        }
        if let Some(value) = input.activity_level {
            staff.activity_level = value;
        }
        if let Some(value) = input.timezone {
            staff.timezone = match value {
                Some(raw) => Some(valid_timezone(&raw)?),
                None => None,
            };
        }
        if let Some(value) = input.working_hours_start {
            staff.working_hours_start = match value {
                Some(raw) => Some(valid_hhmm(&raw, "working_hours_start")?),
                None => None,
            };
        }
        if let Some(value) = input.working_hours_end {
            staff.working_hours_end = match value {
                Some(raw) => Some(valid_hhmm(&raw, "working_hours_end")?),
                None => None,
            };
        }
        staff.updated_at = Utc::now();
        state.staff.insert(user_id.clone(), staff.clone());
        state.emit(
            "timetracker.staff.updated",
            json!({
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "role": format!("{:?}", staff.role).to_lowercase(),
                "updated_by": caller.actor,
            }),
        );
        Ok(staff_overview(&state, &staff))
    })();
    json::respond(result, debug)
}

async fn remove_staff(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let user_id = path.into_inner();
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        // Removing staff removes the tracker link and the team
        // memberships only. Time entries, activity, screenshots,
        // and rates are history and are kept.
        state.staff.remove(&user_id);
        let memberships: Vec<String> = state
            .team_members
            .iter()
            .filter(|entry| entry.value().user_id == user_id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in memberships {
            state.team_members.remove(&key);
        }
        state.emit(
            "timetracker.staff.removed",
            json!({
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "removed_by": caller.actor,
            }),
        );
        Ok(json!({ "user_id": user_id, "removed": true }))
    })();
    json::respond(result, debug)
}

async fn staff_activity(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let user_id = path.into_inner();
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Staff member not found: {user_id}")))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        let params = query.into_inner();
        let (from, to) = parse_window(&params)?;
        let entries: Vec<TimeEntry> = state
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| {
                entry.user_id == user_id && entry.started_at >= from && entry.started_at <= to
            })
            .collect();
        let total_secs: i64 = entries.iter().map(|entry| entry.duration_secs).sum();
        let billable_secs: i64 = entries
            .iter()
            .filter(|entry| entry.billable)
            .map(|entry| entry.duration_secs)
            .sum();
        let samples: Vec<ActivitySample> = state
            .activity
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|sample| {
                sample.user_id == user_id
                    && sample.interval_start >= from
                    && sample.interval_start <= to
            })
            .collect();
        let avg_activity = if samples.is_empty() {
            None
        } else {
            Some(samples.iter().map(|sample| sample.activity_score).sum::<i64>() as f64 / samples.len() as f64)
        };
        let idle_secs: i64 = samples.iter().map(|sample| sample.idle_secs).sum();
        Ok(json!({
            "staff": staff_overview(&state, &staff),
            "from": from.to_rfc3339(),
            "to": to.to_rfc3339(),
            "total_secs": total_secs,
            "billable_secs": billable_secs,
            "entry_count": entries.len(),
            "avg_activity": avg_activity,
            "idle_secs": idle_secs,
            "activity_samples": samples.len(),
        }))
    })();
    json::respond(result, debug)
}

async fn list_invites(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StaffInvite>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<StaffInvite> = state
            .invites
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|invite| invite.company_id == company_id)
            .filter(|invite| {
                if let Some(status) = &status_filter {
                    if &invite.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_invite(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateInviteRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let email = valid_email(&input.email)?;
        if let Some(team_id) = input.team_id {
            let team = state
                .teams
                .get(&team_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Team not found: {team_id}")))?;
            if team.company_id != company_id {
                return Err(UnifiedError::forbidden(
                    "Team belongs to a different company",
                ));
            }
        }
        let pending_same_email = state.invites.iter().any(|entry| {
            entry.value().company_id == company_id
                && entry.value().email == email
                && entry.value().status == "pending"
        });
        if pending_same_email {
            return Err(UnifiedError::conflict(format!(
                "A pending invite already exists for {email} in this company"
            )));
        }
        let now = Utc::now();
        // Opaque token: 32 random bytes, hex-encoded. Only the hash
        // is stored; the raw token is returned once, in this
        // response, for the invitation message.
        let token: String = {
            use rand::{distr::Alphanumeric, Rng};
            rand::rng()
                .sample_iter(&Alphanumeric)
                .take(48)
                .map(char::from)
                .collect()
        };
        let invite = StaffInvite {
            id: Uuid::now_v7(),
            company_id,
            email: email.clone(),
            role: input.role.unwrap_or_default(),
            team_id: input.team_id,
            invited_by: caller.actor.clone(),
            token_hash: sha256_hex(token.as_bytes()),
            status: "pending".to_string(),
            expires_at: now + Duration::days(7),
            accepted_at: None,
            accepted_user_id: None,
            created_at: now,
            updated_at: now,
        };
        state.invites.insert(invite.id, invite.clone());
        state.emit(
            "timetracker.staff.invite.created",
            json!({
                "invite_id": invite.id.to_string(),
                "company_id": company_id.to_string(),
                "email": email,
                "role": format!("{:?}", invite.role).to_lowercase(),
                "invited_by": caller.actor,
                "expires_at": invite.expires_at.to_rfc3339(),
            }),
        );
        Ok(json!({
            "invite": invite,
            "token": token,
            "note": "The token is shown once here. Send it to the invitee out of band; only its hash is stored.",
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn revoke_invite(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StaffInvite> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let id = parse_uuid(&path.into_inner(), "invite")?;
        let mut invite = state
            .invites
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Invite not found: {id}")))?;
        if invite.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Invite belongs to a different company",
            ));
        }
        if invite.status != "pending" {
            return Err(UnifiedError::conflict(format!(
                "Only a pending invite can be revoked (current status: {})",
                invite.status
            )));
        }
        invite.status = "revoked".to_string();
        invite.updated_at = Utc::now();
        state.invites.insert(invite.id, invite.clone());
        state.emit(
            "timetracker.staff.invite.revoked",
            json!({
                "invite_id": invite.id.to_string(),
                "company_id": company_id.to_string(),
                "revoked_by": caller.actor,
            }),
        );
        Ok(invite)
    })();
    json::respond(result, debug)
}

async fn list_teams(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Team>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let params = query.into_inner();
        let mut items: Vec<Team> = state
            .teams
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|team| team.company_id == company_id)
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreateTeamRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Team> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_DESCRIPTION_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let manager_id = match &input.manager_id {
            Some(raw) => Some(require_text(raw, "manager_id", 128)?),
            None => None,
        };
        let now = Utc::now();
        let team = Team {
            id: Uuid::now_v7(),
            company_id,
            name,
            description,
            manager_id,
            created_at: now,
            updated_at: now,
        };
        state.teams.insert(team.id, team.clone());
        state.emit(
            "timetracker.team.created",
            json!({
                "team_id": team.id.to_string(),
                "company_id": company_id.to_string(),
                "name": team.name,
                "created_by": caller.actor,
            }),
        );
        Ok(team)
    })();
    match result {
        Ok(team) => json::success_response(team, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

fn load_team(state: &TimeTrackerState, raw_id: &str) -> UnifiedResult<Team> {
    let id = parse_uuid(raw_id, "team")?;
    state
        .teams
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Team not found: {id}")))
}

async fn get_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Team> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        Ok(team)
    })();
    json::respond(result, debug)
}

async fn update_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTeamRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Team> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let mut team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        if let Some(name) = &input.name {
            team.name = require_text(name, "name", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(description) = &input.description {
            team.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(manager_id) = input.manager_id {
            team.manager_id = match manager_id {
                Some(raw) => Some(require_text(&raw, "manager_id", 128)?),
                None => None,
            };
        }
        team.updated_at = Utc::now();
        state.teams.insert(team.id, team.clone());
        Ok(team)
    })();
    json::respond(result, debug)
}

async fn delete_team(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        state.teams.remove(&team.id);
        let memberships: Vec<String> = state
            .team_members
            .iter()
            .filter(|entry| entry.value().team_id == team.id)
            .map(|entry| entry.key().clone())
            .collect();
        for key in memberships {
            state.team_members.remove(&key);
        }
        state.emit(
            "timetracker.team.deleted",
            json!({
                "team_id": team.id.to_string(),
                "company_id": company_id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "id": team.id.to_string(), "deleted": true }))
    })();
    json::respond(result, debug)
}

async fn list_team_members(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<TeamMember>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_READ)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        let params = query.into_inner();
        let mut items: Vec<TeamMember> = state
            .team_members
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|member| member.team_id == team.id)
            .collect();
        items.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        Ok(paginate(items, &params))
    })();
    json::respond(result, debug)
}

async fn add_team_member(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddTeamMemberRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<TeamMember> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let input = body.into_inner();
        let team = load_team(&state, &path.into_inner())?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        let user_id = require_text(&input.user_id, "user_id", 128)?;
        let staff = state
            .staff
            .get(&user_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!(
                "User {user_id} is not tracker staff of this company; add them as staff first"
            )))?;
        if staff.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Staff member belongs to a different company",
            ));
        }
        let role = input.role.unwrap_or(staff.role);
        let key = team_member_key(team.id, &user_id);
        if state.team_members.contains_key(&key) {
            return Err(UnifiedError::conflict(format!(
                "User {user_id} is already a member of this team"
            )));
        }
        let member = TeamMember {
            team_id: team.id,
            user_id: user_id.clone(),
            role,
            joined_at: Utc::now(),
        };
        state.team_members.insert(key, member.clone());
        state.emit(
            "timetracker.team.member.added",
            json!({
                "team_id": team.id.to_string(),
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "role": format!("{:?}", role).to_lowercase(),
                "added_by": caller.actor,
            }),
        );
        Ok(member)
    })();
    match result {
        Ok(member) => json::success_response(member, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn remove_team_member(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MANAGE_STAFF)?;
        let company_id = staff_company(&ctx, &req, &caller)?;
        let (team_id, user_id) = path.into_inner();
        let team = load_team(&state, &team_id)?;
        if team.company_id != company_id {
            return Err(UnifiedError::forbidden(
                "Team belongs to a different company",
            ));
        }
        let key = team_member_key(team.id, &user_id);
        if !state.team_members.contains_key(&key) {
            return Err(UnifiedError::not_found(format!(
                "User {user_id} is not a member of this team"
            )));
        }
        state.team_members.remove(&key);
        state.emit(
            "timetracker.team.member.removed",
            json!({
                "team_id": team.id.to_string(),
                "user_id": user_id,
                "company_id": company_id.to_string(),
                "removed_by": caller.actor,
            }),
        );
        Ok(json!({ "team_id": team.id.to_string(), "user_id": user_id, "removed": true }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: desktop-agent API (API-key authenticated)
//
// These endpoints are the server side the future desktop / mobile
// agent app will call. No agent binary is built in this round: any
// HTTP client holding a key scoped to `timetracker:write` (or
// `timetracker:read` for status) can exercise them today, which is
// exactly the contract the future agent will use.
// ---------------------------------------------------------------------------

async fn agent_heartbeat(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<AgentHeartbeatRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        let input = body.into_inner();
        if input.idle_secs < 0 {
            return Err(UnifiedError::bad_request("idle_secs must not be negative"));
        }
        let resolved = state.resolve_settings_for_user(&user_id);
        if resolved.activity_level == ActivityLevel::Off {
            return Err(UnifiedError::unprocessable(
                "Activity tracking is disabled for this company (activity_level off)",
            ));
        }
        // Idle past the resolved threshold auto-pauses the running
        // timer; the idle period is recorded on the timer, never
        // discarded. A heartbeat without idle seconds changes
        // nothing.
        let auto_paused = if input.idle_secs > 0 {
            auto_pause_on_idle(&state, &user_id, input.idle_secs)
        } else {
            false
        };
        let timer = active_timer_for(&state, &user_id);
        Ok(json!({
            "user_id": user_id,
            "online": true,
            "idle_secs": input.idle_secs,
            "auto_paused": auto_paused,
            "active_timer": timer,
            "company_id": resolved.company_id.map(|id| id.to_string()),
            "screenshots_enabled": resolved.screenshots_enabled,
            "screenshot_interval_secs": resolved.screenshot_interval_secs,
            "blur_screenshots": resolved.blur_screenshots,
            "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
            "idle_threshold_secs": resolved.idle_threshold_secs,
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn agent_activity(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<ReportActivityRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<ActivitySample> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        record_activity(&state, &user_id, body.into_inner())
    })();
    match result {
        Ok(sample) => json::success_response(sample, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn agent_usage(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<ReportUsageRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<AppUsageRecord> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        record_usage(&state, &user_id, body.into_inner())
    })();
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn agent_upload_screenshot(
    state: web::Data<TimeTrackerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<ScreenshotUploadQuery>,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", SCREENSHOT_RATE_PER_MINUTE)?;
        let resolved = state.resolve_settings_for_user(&user_id);
        // Three gates, in order: the buyer's activity level
        // (screenshots need Full), the buyer's master switch, and
        // the user's own consent flag. Any closed gate is a 403
        // with the reason named, so the agent can surface it.
        if resolved.activity_level != ActivityLevel::Full {
            return Err(UnifiedError::forbidden(
                "Screenshots are disabled for this company: activity_level is not full",
            ));
        }
        if !resolved.screenshots_enabled {
            return Err(UnifiedError::forbidden(
                "Screenshots are disabled for this company by the buyer's settings",
            ));
        }
        if !state.user_settings_for(&user_id).screenshots_enabled {
            return Err(UnifiedError::forbidden(
                "Screenshots are disabled for this user: no screenshot consent (enable screenshots in /settings/me)",
            ));
        }
        let content_type = req
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let extension = match content_type.as_str() {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => {
                return Err(UnifiedError::bad_request(
                    "Screenshot content-type must be image/png, image/jpeg, or image/webp",
                ));
            }
        };
        if body.len() > MAX_SCREENSHOT_BYTES {
            return Err(UnifiedError::new(
                format!(
                    "Screenshot exceeds the maximum size of {MAX_SCREENSHOT_BYTES} bytes"
                ),
                413,
            ));
        }
        if body.is_empty() {
            return Err(UnifiedError::bad_request("Screenshot body must not be empty"));
        }
        let input = query.into_inner();
        if let Some(timer_id) = input.timer_id {
            let timer = state
                .timers
                .get(&timer_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {timer_id}")))?;
            if timer.user_id != user_id {
                return Err(UnifiedError::forbidden(
                    "Screenshot cannot be attached to another user's timer",
                ));
            }
        }
        // Bytes go to the storage module only. The target must
        // exist; a missing target fails the upload (503) instead
        // of storing bytes somewhere else.
        let target = state.global_settings().storage_target;
        let provider = ctx.storage_registry.get(&target).ok_or_else(|| {
            UnifiedError::new(
                format!("Storage target \"{target}\" is not registered; configure it before uploading screenshots"),
                503,
            )
        })?;
        let id = Uuid::now_v7();
        let key = format!("timetracker/{user_id}/{id}.{extension}");
        let stored_key = provider.upload(&key, body.to_vec()).await?;
        let now = Utc::now();
        let record = ScreenshotRecord {
            id,
            user_id: user_id.clone(),
            timer_id: input.timer_id,
            project_id: input.project_id,
            task_id: input.task_id,
            storage_target: target.clone(),
            storage_key: stored_key.clone(),
            content_sha256: sha256_hex(&body),
            content_type: content_type.clone(),
            size_bytes: body.len() as u64,
            blurred: input.blurred || resolved.blur_screenshots,
            captured_at: input.captured_at.unwrap_or(now),
            created_at: now,
        };
        state.screenshots.insert(record.id, record.clone());
        state.emit(
            events::SCREENSHOT_UPLOADED,
            json!({
                "screenshot_id": record.id.to_string(),
                "user_id": user_id,
                "timer_id": record.timer_id.map(|id| id.to_string()),
                "project_id": record.project_id.map(|id| id.to_string()),
                "task_id": record.task_id.map(|id| id.to_string()),
                "storage_target": target,
                "storage_key": stored_key,
                "content_type": content_type,
                "size_bytes": record.size_bytes,
                "blurred": record.blurred,
                "captured_at": record.captured_at.to_rfc3339(),
            }),
        );
        Ok(json!({ "screenshot": record }))
    })()
    .await;
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn agent_status(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "read", AGENT_RATE_PER_MINUTE)?;
        let resolved = state.resolve_settings_for_user(&user_id);
        let mine = state.user_settings_for(&user_id);
        let timer = active_timer_for(&state, &user_id);
        Ok(json!({
            "user_id": user_id,
            "company_id": resolved.company_id.map(|id| id.to_string()),
            "screenshots_enabled": resolved.screenshots_enabled,
            "screenshot_consent": mine.screenshots_enabled,
            "screenshot_interval_secs": resolved.screenshot_interval_secs,
            "blur_screenshots": resolved.blur_screenshots,
            "activity_level": format!("{:?}", resolved.activity_level).to_lowercase(),
            "idle_threshold_secs": resolved.idle_threshold_secs,
            "timezone": resolved.timezone,
            "retention_days": resolved.retention_days,
            "active_timer": timer,
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn agent_start_timer(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<StartTimerRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<TimeTrackerTimer> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        let input = body.into_inner();
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        if active_timer_for(&state, &user_id).is_some() {
            return Err(UnifiedError::conflict(
                "An active timer already exists for this user: stop or pause it before starting another",
            ));
        }
        let now = Utc::now();
        let timer = TimeTrackerTimer {
            id: Uuid::now_v7(),
            user_id: user_id.clone(),
            project_id: input.project_id,
            task_id: input.task_id,
            description,
            billable: input.billable,
            status: TimerStatus::Running,
            started_at: now,
            paused_total_secs: 0,
            paused_at: None,
            idle_total_secs: 0,
            stopped_at: None,
            entry_id: None,
            created_at: now,
            updated_at: now,
        };
        state.timers.insert(timer.id, timer.clone());
        state.emit(
            events::TIMER_STARTED,
            json!({
                "timer_id": timer.id.to_string(),
                "user_id": user_id,
                "project_id": timer.project_id.map(|id| id.to_string()),
                "task_id": timer.task_id.map(|id| id.to_string()),
                "billable": timer.billable,
                "started_at": timer.started_at.to_rfc3339(),
                "source": "agent",
            }),
        );
        Ok(timer)
    })();
    match result {
        Ok(timer) => json::success_response(timer, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn agent_stop_timer(
    state: web::Data<TimeTrackerState>,
    req: HttpRequest,
    body: web::Json<Value>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<TimeEntry> = (|| {
        let (_key, user_id) =
            authenticate_agent(&state, &req, "write", AGENT_RATE_PER_MINUTE)?;
        let timer_id = body
            .get("timer_id")
            .and_then(|value| value.as_str())
            .map(|raw| parse_uuid(raw, "timer_id"))
            .transpose()?;
        let timer = match timer_id {
            Some(id) => state
                .timers
                .get(&id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found(format!("Timer not found: {id}")))?,
            None => active_timer_for(&state, &user_id).ok_or_else(|| {
                UnifiedError::not_found("No active timer for this user")
            })?,
        };
        if timer.user_id != user_id {
            return Err(UnifiedError::forbidden(
                "An agent can only stop its own user's timer",
            ));
        }
        stop_timer_inner(&state, timer.id, "agent_stop")
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers (cross-module events)
// ---------------------------------------------------------------------------

/// Answer `project.task.completed`: stop every running timer
/// linked to that task — the work is done, so the timer must not
/// keep running — and log their entries. Unknown or missing task
/// ids are ignored (a stale event can never stop unrelated
/// timers). The event name is confirmed by the audit module's
/// audited-events list; the projects module file itself was not
/// present in the staging tree when this module was written, so
/// the handler only trusts the `task_id` / `id` fields in the
/// payload.
fn on_project_task_completed(state: TimeTrackerState, ctx: &mut HookContext) {
    let task_id = ctx
        .data
        .get("task_id")
        .or_else(|| ctx.data.get("id"))
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(task_id) = task_id else {
        ctx.add_result(json!({ "stopped_timers": 0, "error": "project.task.completed requires 'task_id'" }));
        return;
    };
    let running: Vec<Uuid> = state
        .timers
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|timer| {
            timer.task_id == Some(task_id)
                && (timer.status == TimerStatus::Running || timer.status == TimerStatus::Paused)
        })
        .map(|timer| timer.id)
        .collect();
    let mut stopped = 0usize;
    for timer_id in running {
        if stop_timer_inner(&state, timer_id, "task_completed").is_ok() {
            stopped += 1;
        }
    }
    ctx.add_result(json!({
        "stopped_timers": stopped,
        "task_id": task_id.to_string(),
    }));
}

/// Answer `project.task.created`: acknowledged only. The task id
/// is echoed back so the caller can verify the link; no record is
/// created here — timers and entries link to tasks by id when the
/// user starts tracking against one.
fn on_project_task_created(_state: TimeTrackerState, ctx: &mut HookContext) {
    let task_id = ctx
        .data
        .get("task_id")
        .or_else(|| ctx.data.get("id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    ctx.add_result(json!({ "acknowledged": true, "task_id": task_id }));
}

/// Answer `project.time.logged`: acknowledged only. The projects
/// module keeps its own time entries; importing them here would
/// double-count billable hours, so nothing is recorded. This
/// module's entries are the canonical billable source and are
/// published as `timetracker.time.logged` instead.
fn on_project_time_logged(_state: TimeTrackerState, ctx: &mut HookContext) {
    ctx.add_result(json!({ "acknowledged": true }));
}

/// Answer `user.created`: accept any pending staff invite whose
/// email matches the new user, and create their staff record (and
/// team membership) from the invite. The users module emits
/// `user.created` with `{"id", "email", "name"}`.
fn on_user_created(state: TimeTrackerState, ctx: &mut HookContext) {
    let email = ctx
        .data
        .get("email")
        .and_then(|value| value.as_str())
        .map(|raw| raw.trim().to_lowercase());
    let user_id = ctx
        .data
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let (Some(email), Some(user_id)) = (email, user_id) else {
        ctx.add_result(json!({ "accepted": 0, "error": "user.created requires 'id' and 'email'" }));
        return;
    };
    let pending: Vec<StaffInvite> = state
        .invites
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|invite| invite.email == email && invite.status == "pending")
        .collect();
    let now = Utc::now();
    let mut accepted = 0usize;
    for mut invite in pending {
        if invite.expires_at < now {
            invite.status = "expired".to_string();
            invite.updated_at = now;
            state.invites.insert(invite.id, invite);
            continue;
        }
        invite.status = "accepted".to_string();
        invite.accepted_at = Some(now);
        invite.accepted_user_id = Some(user_id.clone());
        invite.updated_at = now;
        state.invites.insert(invite.id, invite.clone());
        if !state.staff.contains_key(&user_id) {
            let staff = StaffSettings {
                user_id: user_id.clone(),
                company_id: invite.company_id,
                role: invite.role,
                manager_id: None,
                screenshots_enabled: None,
                blur_screenshots: None,
                idle_threshold_secs: None,
                activity_level: None,
                timezone: None,
                working_hours_start: None,
                working_hours_end: None,
                created_at: now,
                updated_at: now,
            };
            state.staff.insert(user_id.clone(), staff);
            if let Some(team_id) = invite.team_id {
                state.team_members.insert(
                    team_member_key(team_id, &user_id),
                    TeamMember {
                        team_id,
                        user_id: user_id.clone(),
                        role: invite.role,
                        joined_at: now,
                    },
                );
            }
            state.emit(
                events::STAFF_INVITE_ACCEPTED,
                json!({
                    "invite_id": invite.id.to_string(),
                    "user_id": user_id,
                    "company_id": invite.company_id.to_string(),
                }),
            );
            state.emit(
                events::STAFF_ADDED,
                json!({
                    "user_id": user_id,
                    "company_id": invite.company_id.to_string(),
                    "role": format!("{:?}", invite.role).to_lowercase(),
                    "source": "invite",
                }),
            );
        }
        accepted += 1;
    }
    ctx.add_result(json!({ "accepted": accepted, "user_id": user_id }));
}

/// Answer `timetracker.screenshots.purge_requested`: run the
/// retention purge. Hook handlers are synchronous, so the purge
/// runs on a spawned worker (the backup module's pattern: the
/// current Tokio runtime when there is one, otherwise a dedicated
/// thread with its own runtime). The result reports the dispatch,
/// not the counts — the counts arrive later in the
/// `timetracker.screenshots.purged` event the purge emits.
fn on_screenshots_purge_requested(state: TimeTrackerState, ctx: &mut HookContext) {
    let registry = state
        .storage_registry
        .read()
        .expect("storage registry lock poisoned")
        .clone();
    let Some(registry) = registry else {
        ctx.add_result(json!({ "purged": false, "error": "storage registry is not attached" }));
        return;
    };
    let future = async move {
        purge_expired_screenshots(&state, &registry).await;
    };
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::spawn(future);
    } else {
        std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return;
            };
            runtime.block_on(future);
        });
    }
    ctx.add_result(json!({ "purged": "async", "queued": true }));
}

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct TimeTrackerModule {
    state: TimeTrackerState,
}

impl TimeTrackerModule {
    pub fn new() -> Self {
        Self {
            state: TimeTrackerState::default(),
        }
    }
}

impl Default for TimeTrackerModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for TimeTrackerModule {
    fn id(&self) -> &'static str {
        "timetracker"
    }

    fn name(&self) -> &'static str {
        "TimeTracker"
    }

    fn description(&self) -> &'static str {
        "Time Doctor-style time tracking: timers, manual entries, approvals, activity and app-usage monitoring, screenshots through the storage module with consent, blur, and retention controls, hourly rates, payroll with overtime, timesheets, reports, team monitoring, staff and team management per company, and per-buyer settings. The desktop and mobile agent apps are a planned future product; the server-side agent API under /api/v1/timetracker/agent/* is what they will call."
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "storage", "billing", "notifications", "scheduler"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "storage", "billing", "notifications", "scheduler"]
    }

    /// Sellable-product declaration (batch 2, moved here from the
    /// inherent methods: the `Module` trait now declares these, so
    /// the registry reads this override). Available as a product
    /// in the productivity category.
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        *self
            .state
            .storage_registry
            .write()
            .expect("storage registry lock poisoned") =
            Some(Arc::clone(&ctx.storage_registry));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "project.task.completed",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_task_completed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.task.created",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_task_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.time.logged",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_time_logged(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "user.created",
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_user_created(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            events::SCREENSHOTS_PURGE_REQUESTED,
            "timetracker",
            Arc::new(move |ctx: &mut HookContext| {
                on_screenshots_purge_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/timetracker")
                .app_data(state)
                .route("", web::get().to(get_status))
                .route("/timers", web::get().to(list_timers))
                .route("/timers/active", web::get().to(get_active_timer))
                .route("/timers/start", web::post().to(start_timer))
                .route("/timers/{id}/stop", web::post().to(stop_timer))
                .route("/timers/{id}/pause", web::post().to(pause_timer))
                .route("/timers/{id}/resume", web::post().to(resume_timer))
                .route("/entries", web::get().to(list_entries))
                .route("/entries", web::post().to(create_entry))
                .route("/entries/{id}", web::get().to(get_entry))
                .route("/entries/{id}", web::patch().to(update_entry))
                .route("/entries/{id}", web::delete().to(delete_entry))
                .route("/entries/{id}/approve", web::post().to(approve_entry))
                .route("/entries/{id}/reject", web::post().to(reject_entry))
                .route("/activity", web::get().to(list_activity))
                .route("/activity", web::post().to(report_activity))
                .route("/usage", web::get().to(list_usage))
                .route("/usage", web::post().to(report_usage))
                .route("/screenshots", web::get().to(list_screenshots))
                .route("/screenshots/{id}", web::get().to(get_screenshot))
                .route("/screenshots/{id}", web::delete().to(delete_screenshot))
                .route("/screenshots/purge", web::post().to(purge_screenshots))
                .route("/rates", web::get().to(list_rates))
                .route("/rates", web::post().to(create_rate))
                .route("/rates/{id}", web::get().to(get_rate))
                .route("/rates/{id}", web::patch().to(update_rate))
                .route("/rates/{id}", web::delete().to(delete_rate))
                .route("/payroll", web::get().to(get_payroll))
                .route("/timesheets", web::get().to(list_timesheets))
                .route("/timesheets", web::post().to(create_timesheet))
                .route("/timesheets/{id}", web::get().to(get_timesheet))
                .route("/timesheets/{id}/submit", web::post().to(submit_timesheet))
                .route("/timesheets/{id}/approve", web::post().to(approve_timesheet))
                .route("/timesheets/{id}/reject", web::post().to(reject_timesheet))
                .route("/reports/daily", web::get().to(report_daily))
                .route("/reports/weekly", web::get().to(report_weekly))
                .route("/reports/summary", web::get().to(report_summary))
                .route("/dashboard/team", web::get().to(team_dashboard))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/settings/me", web::get().to(get_my_settings))
                .route("/settings/me", web::put().to(update_my_settings))
                .route("/settings/buyer", web::get().to(get_buyer_settings))
                .route("/settings/buyer", web::put().to(update_buyer_settings))
                .route("/staff/invites", web::get().to(list_invites))
                .route("/staff/invites", web::post().to(create_invite))
                .route("/staff/invites/{id}/revoke", web::post().to(revoke_invite))
                .route("/staff", web::get().to(list_staff))
                .route("/staff", web::post().to(add_staff))
                .route("/staff/{user_id}", web::get().to(get_staff))
                .route("/staff/{user_id}", web::patch().to(update_staff))
                .route("/staff/{user_id}", web::delete().to(remove_staff))
                .route("/staff/{user_id}/activity", web::get().to(staff_activity))
                .route("/teams", web::get().to(list_teams))
                .route("/teams", web::post().to(create_team))
                .route("/teams/{id}", web::get().to(get_team))
                .route("/teams/{id}", web::patch().to(update_team))
                .route("/teams/{id}", web::delete().to(delete_team))
                .route("/teams/{id}/members", web::get().to(list_team_members))
                .route("/teams/{id}/members", web::post().to(add_team_member))
                .route(
                    "/teams/{id}/members/{user_id}",
                    web::delete().to(remove_team_member),
                )
                .route("/agent/heartbeat", web::post().to(agent_heartbeat))
                .route("/agent/activity", web::post().to(agent_activity))
                .route("/agent/usage", web::post().to(agent_usage))
                .route("/agent/screenshots", web::post().to(agent_upload_screenshot))
                .route("/agent/status", web::get().to(agent_status))
                .route("/agent/timers/start", web::post().to(agent_start_timer))
                .route("/agent/timers/stop", web::post().to(agent_stop_timer)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/timetracker", owner: "timetracker", description: "Module status and counts." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timers", owner: "timetracker", description: "List timers (own timers; managers may filter by user_id with timetracker:view_team)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timers/active", owner: "timetracker", description: "The caller's active timer, if any, with elapsed seconds." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/start", owner: "timetracker", description: "Start a timer (one active timer per user)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/{id}/stop", owner: "timetracker", description: "Stop a timer and create its time entry (emits timetracker.time.logged)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/{id}/pause", owner: "timetracker", description: "Pause a running timer." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timers/{id}/resume", owner: "timetracker", description: "Resume a paused timer." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/entries", owner: "timetracker", description: "List time entries (paginated, filter by user_id, approval, project_id, billable)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/entries", owner: "timetracker", description: "Create a manual time entry with a reason (starts pending; emits timetracker.time.logged)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/entries/{id}", owner: "timetracker", description: "Get one time entry." },
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/entries/{id}", owner: "timetracker", description: "Edit a pending entry (reason required)." },
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/entries/{id}", owner: "timetracker", description: "Delete a non-approved entry (reason required)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/entries/{id}/approve", owner: "timetracker", description: "Approve a pending entry (approver cannot be the owner)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/entries/{id}/reject", owner: "timetracker", description: "Reject a pending entry with a note." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/activity", owner: "timetracker", description: "List activity samples (paginated, filter by user_id)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/activity", owner: "timetracker", description: "Report one activity sample (score 0-100, idle seconds; may auto-pause the timer)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/usage", owner: "timetracker", description: "List app/website usage intervals (paginated, filter by user_id, category)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/usage", owner: "timetracker", description: "Report one app/website usage interval (window-title hash only, never the raw title)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/screenshots", owner: "timetracker", description: "List screenshot metadata (own screenshots; team screenshots need timetracker:view_team_screenshots)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/screenshots/{id}", owner: "timetracker", description: "Get one screenshot's metadata plus a 15-minute signed URL (never raw bytes)." },
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/screenshots/{id}", owner: "timetracker", description: "Delete a screenshot's metadata and its stored object." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/screenshots/purge", owner: "timetracker", description: "Run the retention purge now (deletes screenshots older than each owner's retention window)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/rates", owner: "timetracker", description: "List hourly rates (paginated, filter by user_id)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/rates", owner: "timetracker", description: "Create an hourly rate for a user (currency + amount)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/rates/{id}", owner: "timetracker", description: "Get one hourly rate." },
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/rates/{id}", owner: "timetracker", description: "Update an hourly rate." },
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/rates/{id}", owner: "timetracker", description: "Delete an hourly rate." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/payroll", owner: "timetracker", description: "Payroll lines for approved entries in a window (payable hours x rate, weekly overtime)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timesheets", owner: "timetracker", description: "List timesheets (paginated, filter by user_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets", owner: "timetracker", description: "Create a timesheet for a period (explicit entries or all pending entries in the period)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/timesheets/{id}", owner: "timetracker", description: "Get one timesheet." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets/{id}/submit", owner: "timetracker", description: "Submit a draft timesheet for approval." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets/{id}/approve", owner: "timetracker", description: "Approve a submitted timesheet (also approves its pending entries)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/timesheets/{id}/reject", owner: "timetracker", description: "Reject a submitted timesheet with a note." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/reports/daily", owner: "timetracker", description: "Daily report rows per user/project (at most one day per call)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/reports/weekly", owner: "timetracker", description: "Weekly report rows per user/project (at most 8 days per call)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/reports/summary", owner: "timetracker", description: "Summary totals (billable vs non-billable) plus report rows for a window." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/dashboard/team", owner: "timetracker", description: "Team monitoring: online now, today's activity, low-activity flags (company-scoped)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/settings", owner: "timetracker", description: "Read the global product-default settings." },
            RouteInfo { method: "PUT", path: "/api/v1/timetracker/settings", owner: "timetracker", description: "Update the global product-default settings." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/settings/me", owner: "timetracker", description: "Read the caller's own tracking settings plus their resolved settings." },
            RouteInfo { method: "PUT", path: "/api/v1/timetracker/settings/me", owner: "timetracker", description: "Update the caller's own tracking settings (including screenshot consent)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/settings/buyer", owner: "timetracker", description: "Read a company's per-buyer settings overrides plus the resolved values (company-scoped)." },
            RouteInfo { method: "PUT", path: "/api/v1/timetracker/settings/buyer", owner: "timetracker", description: "Update a company's per-buyer settings (company admin, own company only)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff/invites", owner: "timetracker", description: "List staff invites for the caller's company." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/staff/invites", owner: "timetracker", description: "Invite a person to join the company as tracker staff (token shown once)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/staff/invites/{id}/revoke", owner: "timetracker", description: "Revoke a pending staff invite." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff", owner: "timetracker", description: "List staff of the caller's company with activity overviews (filter by role, team_id)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/staff", owner: "timetracker", description: "Add an existing user as tracker staff of the caller's company." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff/{user_id}", owner: "timetracker", description: "Get one staff member with their activity overview." },
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/staff/{user_id}", owner: "timetracker", description: "Update a staff member's tracker role, manager, and per-staff setting overrides." },
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/staff/{user_id}", owner: "timetracker", description: "Remove a staff member (tracker link and team memberships only; history is kept)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/staff/{user_id}/activity", owner: "timetracker", description: "Per-staff activity overview for a window (totals, average activity, idle seconds)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/teams", owner: "timetracker", description: "List teams of the caller's company." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/teams", owner: "timetracker", description: "Create a team in the caller's company." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/teams/{id}", owner: "timetracker", description: "Get one team." },
            RouteInfo { method: "PATCH", path: "/api/v1/timetracker/teams/{id}", owner: "timetracker", description: "Update a team." },
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/teams/{id}", owner: "timetracker", description: "Delete a team (memberships are removed with it)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/teams/{id}/members", owner: "timetracker", description: "List a team's members." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/teams/{id}/members", owner: "timetracker", description: "Add a staff member to a team." },
            RouteInfo { method: "DELETE", path: "/api/v1/timetracker/teams/{id}/members/{user_id}", owner: "timetracker", description: "Remove a member from a team." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/heartbeat", owner: "timetracker", description: "Agent heartbeat: presence + idle report; may auto-pause the timer (API-key authenticated)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/activity", owner: "timetracker", description: "Agent activity report (API-key authenticated)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/usage", owner: "timetracker", description: "Agent app/website usage report (API-key authenticated)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/screenshots", owner: "timetracker", description: "Agent screenshot upload, max 5 MiB, png/jpeg/webp (API-key authenticated; consent + buyer switch required)." },
            RouteInfo { method: "GET", path: "/api/v1/timetracker/agent/status", owner: "timetracker", description: "Agent status: resolved settings, consent, active timer (API-key authenticated)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/timers/start", owner: "timetracker", description: "Agent timer start (API-key authenticated)." },
            RouteInfo { method: "POST", path: "/api/v1/timetracker/agent/timers/stop", owner: "timetracker", description: "Agent timer stop (API-key authenticated; stops the active timer when no id is given)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(TimeTrackerModule::new()));
}
