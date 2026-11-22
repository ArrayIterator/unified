// Project Supervision module — site inspections, progress
// reporting, defect tracking, approvals, daily logs, and photo
// evidence for projects owned by the Project Management module.
//
// This module does NOT own projects, tasks, or milestones and
// never duplicates their rows. Every project, task, and milestone
// is referenced by its opaque string id only, and project
// existence / progress is resolved through hook queries answered
// by the projects module (see "Projects-module contract" below).
// The projects module is the single source of truth for project
// data; this module stores only supervision data about projects.
//
// Routes live under `/api/v1/supervision`:
//
//   Assignments:
//     GET/POST /assignments — list (filter by project_id,
//       user_id, role, status) / assign a supervisor.
//     GET/PATCH/DELETE /assignments/{id}.
//   Inspection templates:
//     GET/POST /templates — list (filter by project_id,
//       category) / create a template with checklist items.
//     GET/PATCH/DELETE /templates/{id}.
//   Inspections:
//     GET/POST /inspections — list (filter by project_id,
//       template_id, status, inspector_id, overdue) / create
//       (optionally from a template; template items are copied
//       into the inspection as pending results).
//     GET/PATCH /inspections/{id}.
//     POST /inspections/{id}/submit — inspector submits the
//       inspection for approval.
//     POST /inspections/{id}/complete — record the overall
//       verdict and the inspector signature.
//     POST /inspections/{id}/approve, /return — approval chain
//       decision (records an approval row).
//     POST /inspections/{id}/items/{item_id}/result — record
//       pass / fail / na plus notes for one checklist item.
//     POST /inspections/{id}/items/{item_id}/follow-up — ask
//       the projects module (via event) to create a follow-up
//       task for a failed item.
//   Progress reports:
//     GET/POST /progress-reports — list (filter by project_id,
//       milestone_id, status, period) / create a report. The
//       report stores a snapshot: the reported percent, the
//       planned percent, and the project percent read through
//       the hook query at creation time (plus its source), never
//       a second copy of project progress treated as truth.
//     GET/PATCH /progress-reports/{id}.
//     POST /progress-reports/{id}/submit — draft -> submitted.
//     POST /progress-reports/{id}/approve — submitted -> approved
//       (records an approval row).
//     POST /progress-reports/{id}/return — submitted -> returned
//       (records an approval row with a comment).
//   Defects:
//     GET/POST /defects — list (filter by project_id,
//       inspection_id, severity, status, task_id) / log a defect.
//     GET/PATCH /defects/{id}.
//     POST /defects/{id}/follow-up — ask the projects module
//       (via event) to create a follow-up task for the defect.
//     POST /defects/{id}/verify — resolved -> verified.
//   Approvals:
//     GET /approvals — list approval rows (filter by
//       target_kind, target_id, approver_id, decision).
//   Daily logs:
//     GET/POST /daily-logs — list (filter by project_id, date,
//       supervisor_id, signed) / create a site daily log.
//     GET/PATCH /daily-logs/{id}.
//     POST /daily-logs/{id}/sign-off — supervisor sign-off.
//   Photo evidence:
//     GET/POST /photos — list (filter by subject_kind,
//       subject_id, project_id) / attach a photo reference.
//       Metadata only: storage target + key + SHA-256 hash +
//       caption. No image bytes live in this module.
//     GET/DELETE /photos/{id}.
//   Dashboard:
//     GET /dashboard?project_id=... — per-project supervision
//       summary (open defects, overdue inspections, progress
//       variance) computed on read.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory supervision assignments, inspection templates,
// inspections with per-item results, progress-report snapshots
// with an approval workflow, a defect log with a status machine,
// approval rows with a full decision trail, daily logs with
// sign-off, photo metadata referencing the storage module, and a
// dashboard computed on read from this module's own tables plus
// project hook queries. What is NOT built here: projects, tasks,
// milestones, or sprints (the projects module owns them — this
// module only links by id); follow-up task creation itself (this
// module only emits `project.task.create_requested` and records
// that it asked — the projects module must answer it, see the
// contract below); project progress as a live value (the report
// stores the percent the hook query returned at snapshot time
// plus its source, and the dashboard re-queries on read); file
// upload or image bytes (the storage module owns bytes — this
// module stores references only); and actual email / push
// delivery (the notifications module owns delivery — this module
// only emits `notification.requested`).
//
// Projects-module contract (wired into the projects module,
// which answers all four queries below; until the wiring
// existed the calls failed closed):
//   - `project.get_requested` with `{"project_id": "<id>"}` is
//     answered with a result
//     `{"exists": true, "project": {"id", "name", "status",
//     "archived"}}` or `{"exists": false}`. Until it existed,
//     every endpoint that takes a project id returned an
//     internal error instead of inventing a project — fail
//     closed, never guess.
//   - `project.progress_requested` with
//     `{"project_id": "<id>", "milestone_id": "<id>" | null}` must
//     be answered with a result
//     `{"percent_complete": <0..100>, "milestone_id": ..., "milestone_reached": <bool>}`
//     computed from the projects module's own tasks / milestones
//     (the projects module has no stored percent field on
//     `Project`; it must derive it, for example from completed
//     vs total tasks). When unanswered, progress reports record
//     `project_percent_complete: null` and `source:
//     "unverified"` instead of a fabricated number.
//   - `project.task.create_requested` with
//     `{"project_id", "title", "description", "source":
//     "supervision", "inspection_id" | null, "defect_id" | null,
//     "requested_by"}` must be answered with a result
//     `{"created": true, "task_id": "<id>"}` (or
//     `{"created": false, "error": "..."}`). Until answered, the
//     follow-up endpoints record `follow_up_requested: true` and
//     `follow_up_task_id: null` and report that no task was
//     created yet — they never claim a task exists.
//   - `project.task.get_requested` with `{"task_id": "<id>"}` must
//     be answered with `{"exists": true, "task": {...}}` or
//     `{"exists": false}`; used to validate defect task links.
// Events this module listens to (all acknowledged; the projects
// module emits the full serialized project / task JSON):
//   - `project.created`, `project.updated` — acknowledged only;
//     this module keeps no project cache.
//   - `project.archived` — acknowledged; active assignments for
//     that project are suspended (status `suspended`).
//   - `project.task.created`, `project.task.assigned`,
//     `project.task.completed` — acknowledged only; a completed
//     task never auto-resolves a defect (resolution needs a human
//     verification step through `POST /defects/{id}/verify`).
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not
// allowed).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_TEXT_LEN: usize = 5000;
const MAX_LOCATION_LEN: usize = 500;
const MAX_CAPTION_LEN: usize = 500;
const MAX_COMMENT_LEN: usize = 2000;
const MAX_STORAGE_TARGET_LEN: usize = 128;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_SHA256_LEN: usize = 64;
const MAX_WEATHER_LEN: usize = 64;
/// Photo evidence size cap (20 MiB); the storage module enforces
/// its own limits as well — this is the supervision-side guard.
const MAX_PHOTO_BYTES: i64 = 20 * 1024 * 1024;

/// Capability names checked through the `role.check` hook.
const PERM_SUPERVISION_VIEW: &str = "supervision.view";
const PERM_SUPERVISION_CREATE: &str = "supervision.create";
const PERM_SUPERVISION_MANAGE: &str = "supervision.manage";
const PERM_SUPERVISION_APPROVE: &str = "supervision.approve";

/// Hook event names this module emits or listens to.
const PROJECT_GET_REQUESTED: &str = "project.get_requested";
const PROJECT_PROGRESS_REQUESTED: &str = "project.progress_requested";
const PROJECT_TASK_CREATE_REQUESTED: &str = "project.task.create_requested";
const PROJECT_TASK_GET_REQUESTED: &str = "project.task.get_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Role of a supervised assignment on a project site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentRole {
    Supervisor,
    Inspector,
    Approver,
}

impl AssignmentRole {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Supervisor => "supervisor",
            Self::Inspector => "inspector",
            Self::Approver => "approver",
        }
    }
}

/// Lifecycle of an assignment: `active` -> `suspended` (project
/// archived, or manually suspended) -> `active` again, or
/// `ended` when the assignment period finishes / is closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssignmentStatus {
    Active,
    Suspended,
    Ended,
}

impl AssignmentStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Suspended => "suspended",
            Self::Ended => "ended",
        }
    }
}

/// One supervision assignment: a user assigned to supervise,
/// inspect, or approve work on one project site for a period.
/// `project_id` is the opaque projects-module id; no project row
/// is duplicated here.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SupervisionAssignment {
    pub id: Uuid,
    pub project_id: String,
    pub user_id: String,
    pub role: AssignmentRole,
    pub status: AssignmentStatus,
    pub starts_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One checklist item inside an inspection template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemplateItem {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ordering within the template (1-based).
    pub position: u32,
    /// True when a failing result on this item blocks approval.
    #[serde(default)]
    pub required: bool,
}

/// One inspection template: a reusable checklist for a kind of
/// site inspection. `project_id` scopes the template to one
/// project when set; a null project id means a shared template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionTemplate {
    pub id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub items: Vec<TemplateItem>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of an inspection: `scheduled` -> `in_progress` ->
/// `submitted` -> `approved` (or `returned` -> back to
/// `in_progress` for rework). `cancelled` is reachable from
/// scheduled / in_progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionStatus {
    Scheduled,
    InProgress,
    Submitted,
    Approved,
    Returned,
    Cancelled,
}

impl InspectionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::InProgress => "in_progress",
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Returned => "returned",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Result of one checklist item in a conducted inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemResult {
    Pending,
    Pass,
    Fail,
    NotApplicable,
}

impl ItemResult {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::NotApplicable => "na",
        }
    }
}

/// Overall verdict of a completed inspection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectionVerdict {
    Pass,
    ConditionalPass,
    Fail,
}

impl InspectionVerdict {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::ConditionalPass => "conditional_pass",
            Self::Fail => "fail",
        }
    }
}

/// One conducted site inspection.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Inspection {
    pub id: Uuid,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub inspector_id: String,
    pub status: InspectionStatus,
    pub scheduled_for: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conducted_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<InspectionVerdict>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// Inspector signature: the signer's user id plus the time of
    /// signing. The signature is a recorded attestation, not a
    /// cryptographic signature — no key material is involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One checklist item result inside a conducted inspection. The
/// item title is copied from the template at creation time so the
/// inspection stays readable even if the template later changes;
/// `template_item_id` keeps the link back to the template item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InspectionItemResult {
    pub id: Uuid,
    pub inspection_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_item_id: Option<Uuid>,
    pub title: String,
    pub result: ItemResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// True when a follow-up task was requested for this item
    /// through `project.task.create_requested`.
    #[serde(default)]
    pub follow_up_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of a progress report: `draft` -> `submitted` ->
/// `approved`, or `submitted` -> `returned` -> `draft` (rework).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Draft,
    Submitted,
    Approved,
    Returned,
}

/// Where the project percent in a report snapshot came from:
/// `project_hook` means the projects module answered
/// `project.progress_requested` at snapshot time; `manual` means
/// the reporter supplied the number as an estimate; `unverified`
/// means no hook answer was available and no project percent is
/// recorded at all (`project_percent_complete` is null).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressSource {
    ProjectHook,
    Manual,
    Unverified,
}

impl ProgressSource {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ProjectHook => "project_hook",
            Self::Manual => "manual",
            Self::Unverified => "unverified",
        }
    }
}

/// One progress report: a snapshot of reported vs planned
/// progress for a project (optionally one milestone) over a
/// period. `reported_percent_complete` is the supervisor's
/// reported number; `project_percent_complete` is the percent
/// the projects module reported through the hook query at the
/// same moment (null when unverified); `planned_percent_complete`
/// is the plan target for the period end. The variance is
/// derived on read (`reported - planned`), never stored as a
/// second truth. The projects module remains the source of
/// truth for actual project progress.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProgressReport {
    pub id: Uuid,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<String>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub reported_percent_complete: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_percent_complete: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_percent_complete: Option<f64>,
    pub source: ProgressSource,
    pub status: ReportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub reported_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Severity of a defect found on site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefectSeverity {
    Low,
    Medium,
    High,
    Critical,
}

impl DefectSeverity {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

/// Lifecycle of a defect: `open` -> `in_progress` -> `resolved`
/// -> `verified`. A verified defect may be reopened to `open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefectStatus {
    Open,
    InProgress,
    Resolved,
    Verified,
}

impl DefectStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Resolved => "resolved",
            Self::Verified => "verified",
        }
    }
}

/// One defect found on site. `task_id` links to a projects-module
/// task (opaque id, validated through
/// `project.task.get_requested` when set); the task itself is
/// owned by the projects module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Defect {
    pub id: Uuid,
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub severity: DefectSeverity,
    pub status: DefectStatus,
    pub reported_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default)]
    pub follow_up_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What an approval row decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalTargetKind {
    ProgressReport,
    Inspection,
}

impl ApprovalTargetKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ProgressReport => "progress_report",
            Self::Inspection => "inspection",
        }
    }
}

/// Decision recorded on an approval step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Approved,
    Returned,
}

impl ApprovalDecision {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Approved => "approved",
            Self::Returned => "returned",
        }
    }
}

/// One step of an approval chain. The full audit trail for a
/// report or inspection is the ordered list of these rows for its
/// target id: approver, decision, comment, and timestamp. No
/// approval row is ever edited or deleted after creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Approval {
    pub id: Uuid,
    pub target_kind: ApprovalTargetKind,
    pub target_id: Uuid,
    pub project_id: String,
    pub approver_id: String,
    pub decision: ApprovalDecision,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub decided_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// One site daily log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DailyLog {
    pub id: Uuid,
    pub project_id: String,
    pub log_date: NaiveDate,
    pub supervisor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<String>,
    #[serde(default)]
    pub workforce_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_done: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materials_received: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incidents_on_site: Option<String>,
    /// Supervisor sign-off: signer user id plus time. Null until
    /// signed off through `POST /daily-logs/{id}/sign-off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// What a photo is evidence for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhotoSubjectKind {
    Inspection,
    Defect,
    DailyLog,
    ProgressReport,
}

impl PhotoSubjectKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Inspection => "inspection",
            Self::Defect => "defect",
            Self::DailyLog => "daily_log",
            Self::ProgressReport => "progress_report",
        }
    }
}

/// One photo evidence record. Metadata only: the bytes live in
/// the storage module under (`storage_target`, `storage_key`);
/// `content_sha256` is the SHA-256 hex of the uploaded bytes.
/// No photo bytes live in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Photo {
    pub id: Uuid,
    pub project_id: String,
    pub subject_kind: PhotoSubjectKind,
    pub subject_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateAssignmentRequest {
    pub project_id: String,
    pub user_id: String,
    pub role: AssignmentRole,
    pub starts_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAssignmentRequest {
    pub role: Option<AssignmentRole>,
    pub status: Option<AssignmentStatus>,
    pub starts_at: Option<DateTime<Utc>>,
    pub ends_at: Option<Option<DateTime<Utc>>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct TemplateItemInput {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateTemplateRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub category: Option<String>,
    #[serde(default)]
    pub items: Vec<TemplateItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateTemplateRequest {
    pub project_id: Option<Option<String>>,
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub category: Option<String>,
    pub items: Option<Vec<TemplateItemInput>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInspectionRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template_id: Option<Uuid>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub inspector_id: String,
    pub scheduled_for: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateInspectionRequest {
    pub title: Option<String>,
    pub location: Option<Option<String>>,
    pub inspector_id: Option<String>,
    pub scheduled_for: Option<DateTime<Utc>>,
    pub summary: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteInspectionRequest {
    pub verdict: InspectionVerdict,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    /// The inspector attests the inspection by signing with their
    /// own user id; it must match the caller.
    pub signed_by: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemResultRequest {
    pub result: ItemResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProgressReportRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub milestone_id: Option<String>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub reported_percent_complete: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planned_percent_complete: Option<f64>,
    /// Optional manual estimate of the project percent, used only
    /// when the projects module does not answer the progress
    /// query; the stored source then reads `manual`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manual_project_percent_complete: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProgressReportRequest {
    pub title: Option<String>,
    pub period_start: Option<NaiveDate>,
    pub period_end: Option<NaiveDate>,
    pub reported_percent_complete: Option<f64>,
    pub planned_percent_complete: Option<Option<f64>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDefectRequest {
    pub project_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspection_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub severity: DefectSeverity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDefectRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub severity: Option<DefectSeverity>,
    pub status: Option<DefectStatus>,
    pub assigned_to: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDailyLogRequest {
    pub project_id: String,
    pub log_date: NaiveDate,
    pub supervisor_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<String>,
    #[serde(default)]
    pub workforce_count: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_done: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub materials_received: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incidents_on_site: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDailyLogRequest {
    pub weather: Option<Option<String>>,
    pub workforce_count: Option<i64>,
    pub work_done: Option<Option<String>>,
    pub materials_received: Option<Option<String>>,
    pub incidents_on_site: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePhotoRequest {
    pub project_id: String,
    pub subject_kind: PhotoSubjectKind,
    pub subject_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for supervision. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000062_supervision`; until that migration is
/// executed, supervision data does not survive a restart.
#[derive(Clone)]
pub struct SupervisionState {
    pub assignments: Arc<DashMap<Uuid, SupervisionAssignment>>,
    pub templates: Arc<DashMap<Uuid, InspectionTemplate>>,
    pub inspections: Arc<DashMap<Uuid, Inspection>>,
    pub inspection_items: Arc<DashMap<Uuid, InspectionItemResult>>,
    pub progress_reports: Arc<DashMap<Uuid, ProgressReport>>,
    pub defects: Arc<DashMap<Uuid, Defect>>,
    pub approvals: Arc<DashMap<Uuid, Approval>>,
    pub daily_logs: Arc<DashMap<Uuid, DailyLog>>,
    pub photos: Arc<DashMap<Uuid, Photo>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SupervisionState {
    fn default() -> Self {
        Self {
            assignments: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            inspections: Arc::new(DashMap::new()),
            inspection_items: Arc::new(DashMap::new()),
            progress_reports: Arc::new(DashMap::new()),
            defects: Arc::new(DashMap::new()),
            approvals: Arc::new(DashMap::new()),
            daily_logs: Arc::new(DashMap::new()),
            photos: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SupervisionState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape: `kind`, `user_id`, `title`, `body`,
    /// `module: "supervision"`, `link`, `priority`, `channels`.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "supervision",
                "category": "supervision",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
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

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

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

fn opt_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return Ok(None);
            }
            if trimmed.len() > max {
                return Err(UnifiedError::bad_request(format!(
                    "{field} must be at most {max} characters"
                )));
            }
            Ok(Some(trimmed.to_string()))
        }
    }
}

fn require_id(value: &str, field: &str) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty and must be at most 128 characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn valid_percent(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || !(0.0..=100.0).contains(&value) {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a number between 0 and 100"
        )));
    }
    Ok((value * 100.0).round() / 100.0)
}

fn valid_sha256(value: &str) -> UnifiedResult<String> {
    let trimmed = value.trim().to_lowercase();
    if trimmed.len() != MAX_SHA256_LEN
        || !trimmed.chars().all(|c| c.is_ascii_hexdigit())
    {
        return Err(UnifiedError::bad_request(
            "content_sha256 must be a 64-character lowercase hex SHA-256 digest",
        ));
    }
    Ok(trimmed)
}

fn valid_content_type(value: &str) -> UnifiedResult<String> {
    match value.trim() {
        "image/png" | "image/jpeg" | "image/webp" => Ok(value.trim().to_string()),
        _ => Err(UnifiedError::bad_request(
            "content_type must be image/png, image/jpeg, or image/webp",
        )),
    }
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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

// ---------------------------------------------------------------------------
// Projects-module hook queries
// ---------------------------------------------------------------------------

/// Minimal project data returned by the `project.get_requested`
/// query. Only the fields this module needs: identity, name,
/// status, and whether the project is archived. Progress lives in
/// the projects module and is read separately (see
/// `query_project_progress`).
#[derive(Debug, Clone)]
struct ProjectRef {
    id: String,
    name: String,
    status: String,
    archived: bool,
}

/// Ask the projects module whether a project exists, through the
/// `project.get_requested` hook query. Fails closed: when no
/// handler answers (the projects module has not registered the
/// query handler yet), the call errors instead of assuming the
/// project exists. Callers surface this as an internal error so
/// operators can see the missing wiring.
fn query_project(ctx: &AppContext, project_id: &str) -> UnifiedResult<ProjectRef> {
    let context = ctx.hooks.emit(
        PROJECT_GET_REQUESTED,
        json!({ "project_id": project_id }),
    );
    let Some(answer) = context.results.into_iter().next() else {
        return Err(UnifiedError::internal(
            "project.get_requested was not answered: the projects module has not registered a project lookup handler yet",
        ));
    };
    let exists = answer
        .get("exists")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if !exists {
        return Err(UnifiedError::not_found(format!(
            "Project not found: {project_id}"
        )));
    }
    let project = answer.get("project").cloned().unwrap_or(Value::Null);
    Ok(ProjectRef {
        id: project
            .get("id")
            .and_then(|value| value.as_str())
            .unwrap_or(project_id)
            .to_string(),
        name: project
            .get("name")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        status: project
            .get("status")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string(),
        archived: project
            .get("archived")
            .and_then(|value| value.as_bool())
            .unwrap_or(false),
    })
}

/// Ask the projects module for the current progress of a project
/// (optionally one milestone) through `project.progress_requested`.
/// Returns `None` when no handler answers — the caller records
/// the snapshot as unverified instead of inventing a number.
fn query_project_progress(
    ctx: &AppContext,
    project_id: &str,
    milestone_id: Option<&str>,
) -> Option<f64> {
    let context = ctx.hooks.emit(
        PROJECT_PROGRESS_REQUESTED,
        json!({ "project_id": project_id, "milestone_id": milestone_id }),
    );
    let answer = context.results.into_iter().next()?;
    let percent = answer.get("percent_complete")?.as_f64()?;
    if percent.is_finite() && (0.0..=100.0).contains(&percent) {
        Some((percent * 100.0).round() / 100.0)
    } else {
        None
    }
}

/// Ask the projects module whether a task exists, through
/// `project.task.get_requested`. Fails closed like `query_project`.
fn query_task_exists(ctx: &AppContext, task_id: &str) -> UnifiedResult<()> {
    let context = ctx.hooks.emit(
        PROJECT_TASK_GET_REQUESTED,
        json!({ "task_id": task_id }),
    );
    let Some(answer) = context.results.into_iter().next() else {
        return Err(UnifiedError::internal(
            "project.task.get_requested was not answered: the projects module has not registered a task lookup handler yet",
        ));
    };
    let exists = answer
        .get("exists")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(UnifiedError::not_found(format!("Task not found: {task_id}")))
    }
}

/// Ask the projects module to create a follow-up task. Returns the
/// new task id when the projects module answers with one, `None`
/// when it answers that nothing was created, and an error when no
/// handler answers at all (the caller records the request as
/// pending instead of claiming success).
fn request_follow_up_task(
    ctx: &AppContext,
    project_id: &str,
    title: &str,
    description: &str,
    inspection_id: Option<Uuid>,
    defect_id: Option<Uuid>,
    requested_by: &str,
) -> UnifiedResult<Option<String>> {
    let context = ctx.hooks.emit(
        PROJECT_TASK_CREATE_REQUESTED,
        json!({
            "project_id": project_id,
            "title": title,
            "description": description,
            "source": "supervision",
            "inspection_id": inspection_id.map(|id| id.to_string()),
            "defect_id": defect_id.map(|id| id.to_string()),
            "requested_by": requested_by,
        }),
    );
    let Some(answer) = context.results.into_iter().next() else {
        return Err(UnifiedError::internal(
            "project.task.create_requested was not answered: the projects module has not registered a follow-up task handler yet",
        ));
    };
    if answer
        .get("created")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(answer
            .get("task_id")
            .and_then(|value| value.as_str())
            .map(str::to_string))
    } else {
        Ok(None)
    }
}

fn record_approval(
    state: &SupervisionState,
    target_kind: ApprovalTargetKind,
    target_id: Uuid,
    project_id: &str,
    approver_id: &str,
    decision: ApprovalDecision,
    comment: Option<String>,
) -> Approval {
    let now = Utc::now();
    let approval = Approval {
        id: Uuid::now_v7(),
        target_kind,
        target_id,
        project_id: project_id.to_string(),
        approver_id: approver_id.to_string(),
        decision,
        comment,
        decided_at: now,
        created_at: now,
    };
    state.approvals.insert(approval.id, approval.clone());
    state.emit(
        "supervision.approval.recorded",
        json!({
            "approval_id": approval.id.to_string(),
            "target_kind": target_kind.as_str(),
            "target_id": target_id.to_string(),
            "project_id": project_id,
            "approver_id": approver_id,
            "decision": decision.as_str(),
        }),
    );
    approval
}

fn items_for_inspection(state: &SupervisionState, inspection_id: Uuid) -> Vec<InspectionItemResult> {
    let mut items: Vec<InspectionItemResult> = state
        .inspection_items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.inspection_id == inspection_id)
        .collect();
    items.sort_by_key(|item| item.created_at);
    items
}

fn load_assignment(state: &SupervisionState, raw_id: &str) -> UnifiedResult<SupervisionAssignment> {
    let id = parse_uuid(raw_id, "assignment")?;
    state
        .assignments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Assignment not found: {id}")))
}

fn load_template(state: &SupervisionState, raw_id: &str) -> UnifiedResult<InspectionTemplate> {
    let id = parse_uuid(raw_id, "template")?;
    state
        .templates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Template not found: {id}")))
}

fn load_inspection(state: &SupervisionState, raw_id: &str) -> UnifiedResult<Inspection> {
    let id = parse_uuid(raw_id, "inspection")?;
    state
        .inspections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Inspection not found: {id}")))
}

fn load_report(state: &SupervisionState, raw_id: &str) -> UnifiedResult<ProgressReport> {
    let id = parse_uuid(raw_id, "progress report")?;
    state
        .progress_reports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Progress report not found: {id}")))
}

fn load_defect(state: &SupervisionState, raw_id: &str) -> UnifiedResult<Defect> {
    let id = parse_uuid(raw_id, "defect")?;
    state
        .defects
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Defect not found: {id}")))
}

fn load_daily_log(state: &SupervisionState, raw_id: &str) -> UnifiedResult<DailyLog> {
    let id = parse_uuid(raw_id, "daily log")?;
    state
        .daily_logs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Daily log not found: {id}")))
}

fn load_photo(state: &SupervisionState, raw_id: &str) -> UnifiedResult<Photo> {
    let id = parse_uuid(raw_id, "photo")?;
    state
        .photos
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Photo not found: {id}")))
}

fn build_template_items(inputs: &[TemplateItemInput]) -> UnifiedResult<Vec<TemplateItem>> {
    if inputs.len() > 200 {
        return Err(UnifiedError::bad_request(
            "a template may hold at most 200 items",
        ));
    }
    let mut items = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        items.push(TemplateItem {
            id: Uuid::now_v7(),
            title: require_text(&input.title, "items[].title", MAX_TITLE_LEN)?,
            description: opt_text(&input.description, "items[].description", MAX_TEXT_LEN)?,
            position: (index + 1) as u32,
            required: input.required,
        });
    }
    Ok(items)
}

/// Full inspection view: the inspection plus its item results.
fn inspection_view(state: &SupervisionState, inspection: &Inspection) -> Value {
    let mut value = serde_json::to_value(inspection).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "items".to_string(),
            serde_json::to_value(items_for_inspection(state, inspection.id))
                .unwrap_or(Value::Null),
        );
    }
    value
}

/// Full progress-report view: the report plus the derived
/// variance (reported minus planned), when a planned percent is
/// recorded. The variance is computed here, on read, and is never
/// stored as a second truth.
fn report_view(report: &ProgressReport) -> Value {
    let mut value = serde_json::to_value(report).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let variance = report
            .planned_percent_complete
            .map(|planned| (report.reported_percent_complete - planned))
            .map(|value| (value * 100.0).round() / 100.0);
        map.insert(
            "variance_points".to_string(),
            variance
                .map(|value| Value::from(value))
                .unwrap_or(Value::Null),
        );
        map.insert(
            "source".to_string(),
            Value::String(report.source.as_str().to_string()),
        );
    }
    value
}

// ---------------------------------------------------------------------------
// Assignment handlers
// ---------------------------------------------------------------------------

async fn list_assignments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let role_filter = params.filters.get("role").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<SupervisionAssignment> = state
            .assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|assignment| {
                if let Some(project_id) = &project_filter {
                    if &assignment.project_id != project_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &assignment.user_id != user_id {
                        return false;
                    }
                }
                if let Some(role) = &role_filter {
                    if assignment.role.as_str() != role {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if assignment.status.as_str() != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|assignment| assignment.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateAssignmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SupervisionAssignment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        let project = query_project(&ctx, &project_id)?;
        if project.archived {
            return Err(UnifiedError::conflict(
                "cannot assign a supervisor to an archived project",
            ));
        }
        let user_id = require_id(&input.user_id, "user_id")?;
        if let Some(ends_at) = input.ends_at {
            if ends_at <= input.starts_at {
                return Err(UnifiedError::bad_request(
                    "ends_at must be after starts_at",
                ));
            }
        }
        let notes = opt_text(&input.notes, "notes", MAX_TEXT_LEN)?;
        let now = Utc::now();
        let assignment = SupervisionAssignment {
            id: Uuid::now_v7(),
            project_id: project.id,
            user_id: user_id.clone(),
            role: input.role,
            status: AssignmentStatus::Active,
            starts_at: input.starts_at,
            ends_at: input.ends_at,
            notes,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state
            .assignments
            .insert(assignment.id, assignment.clone());
        state.emit(
            "supervision.assignment.created",
            json!({
                "assignment_id": assignment.id.to_string(),
                "project_id": assignment.project_id,
                "user_id": assignment.user_id,
                "role": assignment.role.as_str(),
            }),
        );
        state.notify(
            "supervision.assignment.created",
            &user_id,
            "Supervision assignment created",
            &format!(
                "You were assigned as {} on project \"{}\".",
                assignment.role.as_str(),
                project.name
            ),
            Some(format!("/api/v1/supervision/assignments/{}", assignment.id)),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => created(assignment),
        Err(error) => err(error, debug),
    }
}

async fn get_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SupervisionAssignment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_assignment(&state, &path.into_inner())
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}

async fn update_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateAssignmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SupervisionAssignment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut assignment = load_assignment(&state, &path.into_inner())?;
        let input = body.into_inner();
        if assignment.status == AssignmentStatus::Ended {
            return Err(UnifiedError::conflict(
                "an ended assignment cannot be updated",
            ));
        }
        if let Some(role) = input.role {
            assignment.role = role;
        }
        if let Some(status) = input.status {
            assignment.status = status;
        }
        if let Some(starts_at) = input.starts_at {
            assignment.starts_at = starts_at;
        }
        if let Some(ends_at) = input.ends_at {
            if let Some(end) = ends_at {
                if end <= assignment.starts_at {
                    return Err(UnifiedError::bad_request(
                        "ends_at must be after starts_at",
                    ));
                }
            }
            assignment.ends_at = ends_at;
        }
        if let Some(notes) = input.notes {
            assignment.notes = match notes {
                Some(raw) => Some(require_text(&raw, "notes", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        assignment.updated_at = Utc::now();
        state
            .assignments
            .insert(assignment.id, assignment.clone());
        state.emit(
            "supervision.assignment.updated",
            json!({
                "assignment_id": assignment.id.to_string(),
                "project_id": assignment.project_id,
                "status": assignment.status.as_str(),
            }),
        );
        Ok(assignment)
    })();
    match result {
        Ok(assignment) => ok(assignment),
        Err(error) => err(error, debug),
    }
}

async fn delete_assignment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let assignment = load_assignment(&state, &path.into_inner())?;
        state.assignments.remove(&assignment.id);
        state.emit(
            "supervision.assignment.deleted",
            json!({
                "assignment_id": assignment.id.to_string(),
                "project_id": assignment.project_id,
            }),
        );
        Ok(json!({ "deleted": true, "assignment_id": assignment.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Inspection template handlers
// ---------------------------------------------------------------------------

async fn list_templates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let category_filter = params.filters.get("category").cloned();
        let mut items: Vec<InspectionTemplate> = state
            .templates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|template| {
                if let Some(project_id) = &project_filter {
                    if template.project_id.as_deref() != Some(project_id.as_str()) {
                        return false;
                    }
                }
                if let Some(category) = &category_filter {
                    if &template.category != category {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|template| template.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = match &input.project_id {
            Some(raw) => {
                let id = require_id(raw, "project_id")?;
                // A project-scoped template must reference a real
                // project; a shared template (null) needs no lookup.
                query_project(&ctx, &id)?;
                Some(id)
            }
            None => None,
        };
        let category = match &input.category {
            Some(raw) => require_text(raw, "category", 80)?,
            None => "general".to_string(),
        };
        let items = build_template_items(&input.items)?;
        if items.is_empty() {
            return Err(UnifiedError::bad_request(
                "a template must contain at least one item",
            ));
        }
        let now = Utc::now();
        let template = InspectionTemplate {
            id: Uuid::now_v7(),
            project_id,
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            description: opt_text(&input.description, "description", MAX_TEXT_LEN)?,
            category,
            items,
            created_by: caller.actor,
            created_at: now,
            updated_at: now,
        };
        state.templates.insert(template.id, template.clone());
        state.emit(
            "supervision.template.created",
            json!({
                "template_id": template.id.to_string(),
                "project_id": template.project_id,
                "title": template.title,
            }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => created(template),
        Err(error) => err(error, debug),
    }
}

async fn get_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_template(&state, &path.into_inner())
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}

async fn update_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateTemplateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionTemplate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut template = load_template(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(project_id) = input.project_id {
            template.project_id = match project_id {
                Some(raw) => {
                    let id = require_id(&raw, "project_id")?;
                    query_project(&ctx, &id)?;
                    Some(id)
                }
                None => None,
            };
        }
        if let Some(title) = input.title {
            template.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            template.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(category) = input.category {
            template.category = require_text(&category, "category", 80)?;
        }
        if let Some(items) = input.items {
            let built = build_template_items(&items)?;
            if built.is_empty() {
                return Err(UnifiedError::bad_request(
                    "a template must contain at least one item",
                ));
            }
            template.items = built;
        }
        template.updated_at = Utc::now();
        state.templates.insert(template.id, template.clone());
        state.emit(
            "supervision.template.updated",
            json!({ "template_id": template.id.to_string() }),
        );
        Ok(template)
    })();
    match result {
        Ok(template) => ok(template),
        Err(error) => err(error, debug),
    }
}

async fn delete_template(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let template = load_template(&state, &path.into_inner())?;
        let in_use = state
            .inspections
            .iter()
            .any(|entry| entry.value().template_id == Some(template.id));
        if in_use {
            return Err(UnifiedError::conflict(
                "cannot delete a template that inspections were created from",
            ));
        }
        state.templates.remove(&template.id);
        state.emit(
            "supervision.template.deleted",
            json!({ "template_id": template.id.to_string() }),
        );
        Ok(json!({ "deleted": true, "template_id": template.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Inspection handlers
// ---------------------------------------------------------------------------

async fn list_inspections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let template_filter = params.filters.get("template_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let inspector_filter = params.filters.get("inspector_id").cloned();
        let overdue_only = params
            .filters
            .get("overdue")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .inspections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|inspection| {
                if let Some(project_id) = &project_filter {
                    if &inspection.project_id != project_id {
                        return false;
                    }
                }
                if let Some(template_id) = &template_filter {
                    if inspection.template_id.map(|id| id.to_string()) != Some(template_id.clone()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if inspection.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(inspector_id) = &inspector_filter {
                    if &inspection.inspector_id != inspector_id {
                        return false;
                    }
                }
                if overdue_only {
                    if inspection.scheduled_for >= now
                        || !matches!(
                            inspection.status,
                            InspectionStatus::Scheduled | InspectionStatus::InProgress
                        )
                    {
                        return false;
                    }
                }
                true
            })
            .map(|inspection| inspection_view(&state, &inspection))
            .collect();
        items.sort_by(|a, b| {
            a.get("scheduled_for")
                .and_then(Value::as_str)
                .cmp(&b.get("scheduled_for").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateInspectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        let template = match input.template_id {
            Some(id) => Some(load_template(&state, &id.to_string())?),
            None => None,
        };
        if let Some(template) = &template {
            if let Some(scope) = &template.project_id {
                if scope != &project_id {
                    return Err(UnifiedError::bad_request(
                        "template belongs to a different project",
                    ));
                }
            }
        }
        let inspector_id = require_id(&input.inspector_id, "inspector_id")?;
        let now = Utc::now();
        let inspection = Inspection {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            template_id: template.as_ref().map(|template| template.id),
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            location: opt_text(&input.location, "location", MAX_LOCATION_LEN)?,
            inspector_id,
            status: InspectionStatus::Scheduled,
            scheduled_for: input.scheduled_for,
            conducted_at: None,
            verdict: None,
            summary: opt_text(&input.summary, "summary", MAX_TEXT_LEN)?,
            signed_by: None,
            signed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        // Copy the template checklist into the inspection as
        // pending item results, so later template edits cannot
        // rewrite a conducted inspection.
        if let Some(template) = &template {
            for item in &template.items {
                let result = InspectionItemResult {
                    id: Uuid::now_v7(),
                    inspection_id: inspection.id,
                    template_item_id: Some(item.id),
                    title: item.title.clone(),
                    result: ItemResult::Pending,
                    notes: None,
                    follow_up_requested: false,
                    follow_up_task_id: None,
                    created_at: now,
                    updated_at: now,
                };
                state.inspection_items.insert(result.id, result);
            }
        }
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.created",
            json!({
                "inspection_id": inspection.id.to_string(),
                "project_id": inspection.project_id,
                "template_id": inspection.template_id.map(|id| id.to_string()),
                "inspector_id": inspection.inspector_id,
            }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let inspection = load_inspection(&state, &path.into_inner())?;
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInspectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !matches!(
            inspection.status,
            InspectionStatus::Scheduled | InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "only a scheduled, in-progress, or returned inspection can be updated (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            inspection.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(location) = input.location {
            inspection.location = match location {
                Some(raw) => Some(require_text(&raw, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(inspector_id) = input.inspector_id {
            inspection.inspector_id = require_id(&inspector_id, "inspector_id")?;
        }
        if let Some(scheduled_for) = input.scheduled_for {
            inspection.scheduled_for = scheduled_for;
        }
        if let Some(summary) = input.summary {
            inspection.summary = match summary {
                Some(raw) => Some(require_text(&raw, "summary", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        inspection.updated_at = Utc::now();
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.updated",
            json!({ "inspection_id": inspection.id.to_string() }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !matches!(
            inspection.status,
            InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "only an in-progress or returned inspection can be submitted (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        // Every checklist item must have a result before submit;
        // a pending item means the inspection is not finished.
        let pending = items_for_inspection(&state, inspection.id)
            .iter()
            .filter(|item| item.result == ItemResult::Pending)
            .count();
        if pending > 0 {
            return Err(UnifiedError::conflict(format!(
                "{pending} checklist item(s) still have no result"
            )));
        }
        if inspection.verdict.is_none() {
            return Err(UnifiedError::conflict(
                "an inspection must be completed with a verdict before it can be submitted",
            ));
        }
        inspection.status = InspectionStatus::Submitted;
        inspection.updated_at = Utc::now();
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.submitted",
            json!({
                "inspection_id": inspection.id.to_string(),
                "project_id": inspection.project_id,
                "inspector_id": inspection.inspector_id,
            }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn complete_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CompleteInspectionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut inspection = load_inspection(&state, &path.into_inner())?;
        if !matches!(
            inspection.status,
            InspectionStatus::Scheduled | InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "only a scheduled, in-progress, or returned inspection can be completed (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        let input = body.into_inner();
        // The signature must come from the caller themselves: the
        // inspector attests their own inspection.
        if input.signed_by.trim() != caller.actor {
            return Err(UnifiedError::forbidden(
                "signed_by must match the caller (X-User-Id)",
            ));
        }
        if let Some(summary) = input.summary {
            inspection.summary = Some(require_text(&summary, "summary", MAX_TEXT_LEN)?);
        }
        let now = Utc::now();
        inspection.verdict = Some(input.verdict);
        inspection.conducted_at = Some(now);
        inspection.signed_by = Some(input.signed_by.trim().to_string());
        inspection.signed_at = Some(now);
        inspection.status = InspectionStatus::InProgress;
        inspection.updated_at = now;
        state.inspections.insert(inspection.id, inspection.clone());
        state.emit(
            "supervision.inspection.completed",
            json!({
                "inspection_id": inspection.id.to_string(),
                "project_id": inspection.project_id,
                "verdict": input.verdict.as_str(),
                "signed_by": inspection.signed_by,
            }),
        );
        Ok(inspection_view(&state, &inspection))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn decide_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
    decision: ApprovalDecision,
) -> UnifiedResult<Value> {
    let caller = caller(&req)?;
    require_permission(&ctx, &caller.role, PERM_SUPERVISION_APPROVE)?;
    let mut inspection = load_inspection(&state, &path.into_inner())?;
    if inspection.status != InspectionStatus::Submitted {
        return Err(UnifiedError::conflict(format!(
            "only a submitted inspection can be decided (status is \"{}\")",
            inspection.status.as_str()
        )));
    }
    let input = body.into_inner();
    let comment = opt_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
    inspection.status = match decision {
        ApprovalDecision::Approved => InspectionStatus::Approved,
        ApprovalDecision::Returned => InspectionStatus::Returned,
    };
    inspection.updated_at = Utc::now();
    state.inspections.insert(inspection.id, inspection.clone());
    let approval = record_approval(
        &state,
        ApprovalTargetKind::Inspection,
        inspection.id,
        &inspection.project_id,
        &caller.actor,
        decision,
        comment,
    );
    state.emit(
        "supervision.inspection.decided",
        json!({
            "inspection_id": inspection.id.to_string(),
            "project_id": inspection.project_id,
            "decision": decision.as_str(),
            "approval_id": approval.id.to_string(),
        }),
    );
    let mut view = inspection_view(&state, &inspection);
    if let Value::Object(map) = &mut view {
        map.insert(
            "approval".to_string(),
            serde_json::to_value(approval).unwrap_or(Value::Null),
        );
    }
    Ok(view)
}

async fn approve_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_inspection(ctx, state, req, path, body, ApprovalDecision::Approved).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn return_inspection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_inspection(ctx, state, req, path, body, ApprovalDecision::Returned).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn record_item_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<ItemResultRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionItemResult> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let (inspection_id, item_id) = path.into_inner();
        let inspection = load_inspection(&state, &inspection_id)?;
        if !matches!(
            inspection.status,
            InspectionStatus::Scheduled | InspectionStatus::InProgress | InspectionStatus::Returned
        ) {
            return Err(UnifiedError::conflict(format!(
                "results can only be recorded on a scheduled, in-progress, or returned inspection (status is \"{}\")",
                inspection.status.as_str()
            )));
        }
        let id = parse_uuid(&item_id, "inspection item")?;
        let mut item = state
            .inspection_items
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|item| item.inspection_id == inspection.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Inspection item not found: {id}")))?;
        let input = body.into_inner();
        item.result = input.result;
        item.notes = opt_text(&input.notes, "notes", MAX_TEXT_LEN)?;
        item.updated_at = Utc::now();
        state.inspection_items.insert(item.id, item.clone());
        if inspection.status == InspectionStatus::Scheduled {
            let mut updated = inspection.clone();
            updated.status = InspectionStatus::InProgress;
            updated.updated_at = Utc::now();
            state.inspections.insert(updated.id, updated);
        }
        state.emit(
            "supervision.inspection.item_resulted",
            json!({
                "inspection_id": inspection.id.to_string(),
                "item_id": item.id.to_string(),
                "result": item.result.as_str(),
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

async fn follow_up_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<InspectionItemResult> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let (inspection_id, item_id) = path.into_inner();
        let inspection = load_inspection(&state, &inspection_id)?;
        let id = parse_uuid(&item_id, "inspection item")?;
        let mut item = state
            .inspection_items
            .get(&id)
            .map(|entry| entry.value().clone())
            .filter(|item| item.inspection_id == inspection.id)
            .ok_or_else(|| UnifiedError::not_found(format!("Inspection item not found: {id}")))?;
        if item.result != ItemResult::Fail {
            return Err(UnifiedError::conflict(
                "a follow-up task can only be requested for a failed item",
            ));
        }
        if item.follow_up_requested && item.follow_up_task_id.is_some() {
            return Err(UnifiedError::conflict(
                "a follow-up task was already created for this item",
            ));
        }
        let title = format!("Follow-up: {}", item.title);
        let description = format!(
            "Failed checklist item \"{}\" in inspection \"{}\" (project {}).{}",
            item.title,
            inspection.title,
            inspection.project_id,
            item.notes
                .as_ref()
                .map(|notes| format!(" Notes: {notes}"))
                .unwrap_or_default()
        );
        let task_id = match request_follow_up_task(
            &ctx,
            &inspection.project_id,
            &title,
            &description,
            Some(inspection.id),
            None,
            &caller.actor,
        ) {
            Ok(task_id) => task_id,
            Err(error) => {
                // No handler answered: record the request as
                // pending instead of failing the supervision
                // record — the follow-up itself is owned by the
                // projects module, not by this call.
                item.follow_up_requested = true;
                item.updated_at = Utc::now();
                state.inspection_items.insert(item.id, item.clone());
                state.emit(
                    "supervision.inspection.follow_up_unanswered",
                    json!({
                        "inspection_id": inspection.id.to_string(),
                        "item_id": item.id.to_string(),
                        "error": error.to_string(),
                    }),
                );
                return Ok(item);
            }
        };
        item.follow_up_requested = true;
        item.follow_up_task_id = task_id;
        item.updated_at = Utc::now();
        state.inspection_items.insert(item.id, item.clone());
        state.emit(
            "supervision.inspection.follow_up_requested",
            json!({
                "inspection_id": inspection.id.to_string(),
                "item_id": item.id.to_string(),
                "task_id": item.follow_up_task_id,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Progress report handlers
// ---------------------------------------------------------------------------

async fn list_progress_reports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let milestone_filter = params.filters.get("milestone_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Value> = state
            .progress_reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                if let Some(project_id) = &project_filter {
                    if &report.project_id != project_id {
                        return false;
                    }
                }
                if let Some(milestone_id) = &milestone_filter {
                    if report.milestone_id.as_deref() != Some(milestone_id.as_str()) {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    let current = match report.status {
                        ReportStatus::Draft => "draft",
                        ReportStatus::Submitted => "submitted",
                        ReportStatus::Approved => "approved",
                        ReportStatus::Returned => "returned",
                    };
                    if current != status.as_str() {
                        return false;
                    }
                }
                true
            })
            .map(|report| report_view(&report))
            .collect();
        items.sort_by(|a, b| {
            a.get("period_end")
                .and_then(Value::as_str)
                .cmp(&b.get("period_end").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateProgressReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        let milestone_id = match &input.milestone_id {
            Some(raw) => Some(require_id(raw, "milestone_id")?),
            None => None,
        };
        if input.period_end < input.period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        let reported = valid_percent(
            input.reported_percent_complete,
            "reported_percent_complete",
        )?;
        let planned = match input.planned_percent_complete {
            Some(value) => Some(valid_percent(value, "planned_percent_complete")?),
            None => None,
        };
        // Snapshot the project percent through the hook query.
        // When the projects module does not answer, fall back to
        // an explicit manual estimate when one was supplied, and
        // otherwise record the snapshot as unverified with no
        // project percent at all.
        let (project_percent, source) =
            match query_project_progress(&ctx, &project_id, milestone_id.as_deref()) {
                Some(percent) => (Some(percent), ProgressSource::ProjectHook),
                None => match input.manual_project_percent_complete {
                    Some(manual) => (
                        Some(valid_percent(manual, "manual_project_percent_complete")?),
                        ProgressSource::Manual,
                    ),
                    None => (None, ProgressSource::Unverified),
                },
            };
        let now = Utc::now();
        let report = ProgressReport {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            milestone_id,
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            period_start: input.period_start,
            period_end: input.period_end,
            reported_percent_complete: reported,
            planned_percent_complete: planned,
            project_percent_complete: project_percent,
            source,
            status: ReportStatus::Draft,
            notes: opt_text(&input.notes, "notes", MAX_TEXT_LEN)?,
            reported_by: caller.actor.clone(),
            reviewed_by: None,
            reviewed_at: None,
            created_at: now,
            updated_at: now,
        };
        state
            .progress_reports
            .insert(report.id, report.clone());
        state.emit(
            "supervision.progress_report.created",
            json!({
                "report_id": report.id.to_string(),
                "project_id": report.project_id,
                "milestone_id": report.milestone_id,
                "reported_percent_complete": report.reported_percent_complete,
                "project_percent_complete": report.project_percent_complete,
                "source": report.source.as_str(),
            }),
        );
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let report = load_report(&state, &path.into_inner())?;
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProgressReportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut report = load_report(&state, &path.into_inner())?;
        if !matches!(report.status, ReportStatus::Draft | ReportStatus::Returned) {
            return Err(UnifiedError::conflict(format!(
                "only a draft or returned report can be updated (status is \"{:?}\")",
                report.status
            )));
        }
        let input = body.into_inner();
        if let Some(title) = input.title {
            report.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(period_start) = input.period_start {
            report.period_start = period_start;
        }
        if let Some(period_end) = input.period_end {
            report.period_end = period_end;
        }
        if report.period_end < report.period_start {
            return Err(UnifiedError::bad_request(
                "period_end must not be before period_start",
            ));
        }
        if let Some(reported) = input.reported_percent_complete {
            report.reported_percent_complete =
                valid_percent(reported, "reported_percent_complete")?;
        }
        if let Some(planned) = input.planned_percent_complete {
            report.planned_percent_complete = match planned {
                Some(value) => Some(valid_percent(value, "planned_percent_complete")?),
                None => None,
            };
        }
        if let Some(notes) = input.notes {
            report.notes = match notes {
                Some(raw) => Some(require_text(&raw, "notes", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        // Re-snapshotting after an edit refreshes the project
        // percent from the hook query; a returned report goes back
        // to draft so it must be submitted again.
        report.project_percent_complete =
            query_project_progress(&ctx, &report.project_id, report.milestone_id.as_deref())
                .or(report.project_percent_complete);
        if report.project_percent_complete.is_some()
            && report.source == ProgressSource::Unverified
        {
            report.source = ProgressSource::ProjectHook;
        }
        report.status = ReportStatus::Draft;
        report.updated_at = Utc::now();
        state.progress_reports.insert(report.id, report.clone());
        state.emit(
            "supervision.progress_report.updated",
            json!({ "report_id": report.id.to_string() }),
        );
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut report = load_report(&state, &path.into_inner())?;
        if report.status != ReportStatus::Draft {
            return Err(UnifiedError::conflict(
                "only a draft report can be submitted",
            ));
        }
        report.status = ReportStatus::Submitted;
        report.updated_at = Utc::now();
        state.progress_reports.insert(report.id, report.clone());
        state.emit(
            "supervision.progress_report.submitted",
            json!({
                "report_id": report.id.to_string(),
                "project_id": report.project_id,
            }),
        );
        Ok(report_view(&report))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn decide_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
    decision: ApprovalDecision,
) -> UnifiedResult<Value> {
    let caller = caller(&req)?;
    require_permission(&ctx, &caller.role, PERM_SUPERVISION_APPROVE)?;
    let mut report = load_report(&state, &path.into_inner())?;
    if report.status != ReportStatus::Submitted {
        return Err(UnifiedError::conflict(
            "only a submitted report can be decided",
        ));
    }
    let input = body.into_inner();
    let comment = opt_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
    if decision == ApprovalDecision::Returned && comment.is_none() {
        return Err(UnifiedError::bad_request(
            "a comment is required when returning a report",
        ));
    }
    report.status = match decision {
        ApprovalDecision::Approved => ReportStatus::Approved,
        ApprovalDecision::Returned => ReportStatus::Returned,
    };
    report.reviewed_by = Some(caller.actor.clone());
    report.reviewed_at = Some(Utc::now());
    report.updated_at = Utc::now();
    state.progress_reports.insert(report.id, report.clone());
    let approval = record_approval(
        &state,
        ApprovalTargetKind::ProgressReport,
        report.id,
        &report.project_id,
        &caller.actor,
        decision,
        comment,
    );
    state.emit(
        "supervision.progress_report.decided",
        json!({
            "report_id": report.id.to_string(),
            "project_id": report.project_id,
            "decision": decision.as_str(),
            "approval_id": approval.id.to_string(),
        }),
    );
    let mut view = report_view(&report);
    if let Value::Object(map) = &mut view {
        map.insert(
            "approval".to_string(),
            serde_json::to_value(approval).unwrap_or(Value::Null),
        );
    }
    Ok(view)
}

async fn approve_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_progress_report(ctx, state, req, path, body, ApprovalDecision::Approved).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn return_progress_report(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match decide_progress_report(ctx, state, req, path, body, ApprovalDecision::Returned).await {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Defect handlers
// ---------------------------------------------------------------------------

async fn list_defects(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let inspection_filter = params.filters.get("inspection_id").cloned();
        let severity_filter = params.filters.get("severity").cloned();
        let status_filter = params.filters.get("status").cloned();
        let task_filter = params.filters.get("task_id").cloned();
        let mut items: Vec<Defect> = state
            .defects
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|defect| {
                if let Some(project_id) = &project_filter {
                    if &defect.project_id != project_id {
                        return false;
                    }
                }
                if let Some(inspection_id) = &inspection_filter {
                    if defect.inspection_id.map(|id| id.to_string()) != Some(inspection_id.clone()) {
                        return false;
                    }
                }
                if let Some(severity) = &severity_filter {
                    if defect.severity.as_str() != severity {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if defect.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(task_id) = &task_filter {
                    if defect.task_id.as_deref() != Some(task_id.as_str()) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|defect| defect.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateDefectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        let inspection_id = match input.inspection_id {
            Some(id) => {
                let inspection = load_inspection(&state, &id.to_string())?;
                if inspection.project_id != project_id {
                    return Err(UnifiedError::bad_request(
                        "inspection belongs to a different project",
                    ));
                }
                Some(id)
            }
            None => None,
        };
        let task_id = match &input.task_id {
            Some(raw) => {
                let id = require_id(raw, "task_id")?;
                query_task_exists(&ctx, &id)?;
                Some(id)
            }
            None => None,
        };
        let assigned_to = match &input.assigned_to {
            Some(raw) => Some(require_id(raw, "assigned_to")?),
            None => None,
        };
        let now = Utc::now();
        let defect = Defect {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            inspection_id,
            task_id,
            title: require_text(&input.title, "title", MAX_TITLE_LEN)?,
            description: opt_text(&input.description, "description", MAX_TEXT_LEN)?,
            location: opt_text(&input.location, "location", MAX_LOCATION_LEN)?,
            severity: input.severity,
            status: DefectStatus::Open,
            reported_by: caller.actor.clone(),
            assigned_to,
            follow_up_requested: false,
            follow_up_task_id: None,
            resolved_at: None,
            verified_by: None,
            verified_at: None,
            created_at: now,
            updated_at: now,
        };
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.created",
            json!({
                "defect_id": defect.id.to_string(),
                "project_id": defect.project_id,
                "inspection_id": defect.inspection_id.map(|id| id.to_string()),
                "severity": defect.severity.as_str(),
            }),
        );
        if defect.severity == DefectSeverity::Critical {
            state.notify(
                "supervision.defect.critical",
                &defect.reported_by,
                "Critical defect logged",
                &format!(
                    "Critical defect \"{}\" was logged on project {}.",
                    defect.title, defect.project_id
                ),
                Some(format!("/api/v1/supervision/defects/{}", defect.id)),
            );
        }
        Ok(defect)
    })();
    match result {
        Ok(defect) => created(defect),
        Err(error) => err(error, debug),
    }
}

async fn get_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_defect(&state, &path.into_inner())
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}

async fn update_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDefectRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut defect = load_defect(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = input.title {
            defect.title = require_text(&title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = input.description {
            defect.description = match description {
                Some(raw) => Some(require_text(&raw, "description", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(location) = input.location {
            defect.location = match location {
                Some(raw) => Some(require_text(&raw, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(severity) = input.severity {
            defect.severity = severity;
        }
        if let Some(status) = input.status {
            // Forward-only transitions; a verified defect may be
            // reopened to open. Jumping straight from open to
            // verified is rejected — resolution must be verified
            // by a human through the verify endpoint.
            let allowed = match (defect.status, status) {
                (DefectStatus::Open, DefectStatus::InProgress) => true,
                (DefectStatus::Open, DefectStatus::Resolved) => true,
                (DefectStatus::InProgress, DefectStatus::Resolved) => true,
                (DefectStatus::InProgress, DefectStatus::Open) => true,
                (DefectStatus::Resolved, DefectStatus::InProgress) => true,
                (DefectStatus::Verified, DefectStatus::Open) => true,
                (current, next) => current == next,
            };
            if !allowed {
                return Err(UnifiedError::conflict(format!(
                    "defect cannot move from \"{}\" to \"{}\"",
                    defect.status.as_str(),
                    status.as_str()
                )));
            }
            if status == DefectStatus::Resolved && defect.resolved_at.is_none() {
                defect.resolved_at = Some(Utc::now());
            }
            defect.status = status;
        }
        if let Some(assigned_to) = input.assigned_to {
            defect.assigned_to = match assigned_to {
                Some(raw) => Some(require_id(&raw, "assigned_to")?),
                None => None,
            };
        }
        defect.updated_at = Utc::now();
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.updated",
            json!({
                "defect_id": defect.id.to_string(),
                "project_id": defect.project_id,
                "status": defect.status.as_str(),
            }),
        );
        Ok(defect)
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}

async fn follow_up_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut defect = load_defect(&state, &path.into_inner())?;
        if defect.status == DefectStatus::Verified {
            return Err(UnifiedError::conflict(
                "a verified defect needs no follow-up task",
            ));
        }
        if defect.follow_up_requested && defect.follow_up_task_id.is_some() {
            return Err(UnifiedError::conflict(
                "a follow-up task was already created for this defect",
            ));
        }
        let title = format!("Fix defect: {}", defect.title);
        let description = format!(
            "Defect \"{}\" on project {} (severity {}).{}",
            defect.title,
            defect.project_id,
            defect.severity.as_str(),
            defect
                .description
                .as_ref()
                .map(|text| format!(" {text}"))
                .unwrap_or_default()
        );
        let task_id = match request_follow_up_task(
            &ctx,
            &defect.project_id,
            &title,
            &description,
            defect.inspection_id,
            Some(defect.id),
            &caller.actor,
        ) {
            Ok(task_id) => task_id,
            Err(error) => {
                defect.follow_up_requested = true;
                defect.updated_at = Utc::now();
                state.defects.insert(defect.id, defect.clone());
                state.emit(
                    "supervision.defect.follow_up_unanswered",
                    json!({
                        "defect_id": defect.id.to_string(),
                        "error": error.to_string(),
                    }),
                );
                return Ok(defect);
            }
        };
        defect.follow_up_requested = true;
        defect.follow_up_task_id = task_id.clone();
        // Link the defect to the task the projects module just
        // created, so later reads join the two without duplicating
        // task data here.
        if defect.task_id.is_none() {
            defect.task_id = task_id;
        }
        defect.updated_at = Utc::now();
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.follow_up_requested",
            json!({
                "defect_id": defect.id.to_string(),
                "task_id": defect.follow_up_task_id,
            }),
        );
        Ok(defect)
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}

async fn verify_defect(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Defect> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut defect = load_defect(&state, &path.into_inner())?;
        if defect.status != DefectStatus::Resolved {
            return Err(UnifiedError::conflict(format!(
                "only a resolved defect can be verified (status is \"{}\")",
                defect.status.as_str()
            )));
        }
        let now = Utc::now();
        defect.status = DefectStatus::Verified;
        defect.verified_by = Some(caller.actor.clone());
        defect.verified_at = Some(now);
        defect.updated_at = now;
        state.defects.insert(defect.id, defect.clone());
        state.emit(
            "supervision.defect.verified",
            json!({
                "defect_id": defect.id.to_string(),
                "project_id": defect.project_id,
                "verified_by": defect.verified_by,
            }),
        );
        Ok(defect)
    })();
    match result {
        Ok(defect) => ok(defect),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Approval handlers
// ---------------------------------------------------------------------------

async fn list_approvals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let target_kind_filter = params.filters.get("target_kind").cloned();
        let target_id_filter = params.filters.get("target_id").cloned();
        let approver_filter = params.filters.get("approver_id").cloned();
        let decision_filter = params.filters.get("decision").cloned();
        let project_filter = params.filters.get("project_id").cloned();
        let mut items: Vec<Approval> = state
            .approvals
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|approval| {
                if let Some(kind) = &target_kind_filter {
                    if approval.target_kind.as_str() != kind {
                        return false;
                    }
                }
                if let Some(target_id) = &target_id_filter {
                    if approval.target_id.to_string() != *target_id {
                        return false;
                    }
                }
                if let Some(approver_id) = &approver_filter {
                    if &approval.approver_id != approver_id {
                        return false;
                    }
                }
                if let Some(decision) = &decision_filter {
                    if approval.decision.as_str() != decision {
                        return false;
                    }
                }
                if let Some(project_id) = &project_filter {
                    if &approval.project_id != project_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|approval| approval.decided_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Daily log handlers
// ---------------------------------------------------------------------------

async fn list_daily_logs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let supervisor_filter = params.filters.get("supervisor_id").cloned();
        let date_filter = params.filters.get("date").cloned();
        let signed_only = params
            .filters
            .get("signed")
            .map(|raw| raw == "true")
            .unwrap_or(false);
        let mut items: Vec<DailyLog> = state
            .daily_logs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|log| {
                if let Some(project_id) = &project_filter {
                    if &log.project_id != project_id {
                        return false;
                    }
                }
                if let Some(supervisor_id) = &supervisor_filter {
                    if &log.supervisor_id != supervisor_id {
                        return false;
                    }
                }
                if let Some(date) = &date_filter {
                    if log.log_date.to_string() != *date {
                        return false;
                    }
                }
                if signed_only && log.signed_at.is_none() {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|log| log.log_date);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreateDailyLogRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        if input.workforce_count < 0 || input.workforce_count > 100_000 {
            return Err(UnifiedError::bad_request(
                "workforce_count must be between 0 and 100000",
            ));
        }
        // One log per project per day: a second log for the same
        // date would split the site record.
        let duplicate = state.daily_logs.iter().any(|entry| {
            entry.value().project_id == project_id && entry.value().log_date == input.log_date
        });
        if duplicate {
            return Err(UnifiedError::conflict(format!(
                "a daily log already exists for project {project_id} on {}",
                input.log_date
            )));
        }
        let weather = match &input.weather {
            Some(raw) => {
                let text = raw.trim();
                if text.len() > MAX_WEATHER_LEN {
                    return Err(UnifiedError::bad_request(
                        "weather must be at most 64 characters",
                    ));
                }
                if text.is_empty() {
                    None
                } else {
                    Some(text.to_string())
                }
            }
            None => None,
        };
        let now = Utc::now();
        let log = DailyLog {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            log_date: input.log_date,
            supervisor_id: require_id(&input.supervisor_id, "supervisor_id")?,
            weather,
            workforce_count: input.workforce_count,
            work_done: opt_text(&input.work_done, "work_done", MAX_TEXT_LEN)?,
            materials_received: opt_text(
                &input.materials_received,
                "materials_received",
                MAX_TEXT_LEN,
            )?,
            incidents_on_site: opt_text(
                &input.incidents_on_site,
                "incidents_on_site",
                MAX_TEXT_LEN,
            )?,
            signed_by: None,
            signed_at: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.daily_logs.insert(log.id, log.clone());
        state.emit(
            "supervision.daily_log.created",
            json!({
                "log_id": log.id.to_string(),
                "project_id": log.project_id,
                "log_date": log.log_date.to_string(),
            }),
        );
        Ok(log)
    })();
    match result {
        Ok(log) => created(log),
        Err(error) => err(error, debug),
    }
}

async fn get_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_daily_log(&state, &path.into_inner())
    })();
    match result {
        Ok(log) => ok(log),
        Err(error) => err(error, debug),
    }
}

async fn update_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateDailyLogRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let mut log = load_daily_log(&state, &path.into_inner())?;
        if log.signed_at.is_some() {
            return Err(UnifiedError::conflict(
                "a signed-off daily log cannot be edited",
            ));
        }
        let input = body.into_inner();
        if let Some(weather) = input.weather {
            log.weather = match weather {
                Some(raw) => {
                    if raw.trim().len() > MAX_WEATHER_LEN {
                        return Err(UnifiedError::bad_request(
                            "weather must be at most 64 characters",
                        ));
                    }
                    if raw.trim().is_empty() {
                        None
                    } else {
                        Some(raw.trim().to_string())
                    }
                }
                None => None,
            };
        }
        if let Some(count) = input.workforce_count {
            if count < 0 || count > 100_000 {
                return Err(UnifiedError::bad_request(
                    "workforce_count must be between 0 and 100000",
                ));
            }
            log.workforce_count = count;
        }
        if let Some(work_done) = input.work_done {
            log.work_done = match work_done {
                Some(raw) => Some(require_text(&raw, "work_done", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(materials) = input.materials_received {
            log.materials_received = match materials {
                Some(raw) => Some(require_text(&raw, "materials_received", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(incidents) = input.incidents_on_site {
            log.incidents_on_site = match incidents {
                Some(raw) => Some(require_text(&raw, "incidents_on_site", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        log.updated_at = Utc::now();
        state.daily_logs.insert(log.id, log.clone());
        state.emit(
            "supervision.daily_log.updated",
            json!({ "log_id": log.id.to_string() }),
        );
        Ok(log)
    })();
    match result {
        Ok(log) => ok(log),
        Err(error) => err(error, debug),
    }
}

async fn sign_off_daily_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DailyLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let mut log = load_daily_log(&state, &path.into_inner())?;
        if log.signed_at.is_some() {
            return Err(UnifiedError::conflict(
                "this daily log is already signed off",
            ));
        }
        // Only the log's supervisor signs it off.
        if log.supervisor_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "only the log's supervisor can sign it off",
            ));
        }
        let now = Utc::now();
        log.signed_by = Some(caller.actor.clone());
        log.signed_at = Some(now);
        log.updated_at = now;
        state.daily_logs.insert(log.id, log.clone());
        state.emit(
            "supervision.daily_log.signed_off",
            json!({
                "log_id": log.id.to_string(),
                "project_id": log.project_id,
                "signed_by": log.signed_by,
            }),
        );
        Ok(log)
    })();
    match result {
        Ok(log) => ok(log),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Photo handlers
// ---------------------------------------------------------------------------

async fn list_photos(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_filter = params.filters.get("project_id").cloned();
        let subject_kind_filter = params.filters.get("subject_kind").cloned();
        let subject_id_filter = params.filters.get("subject_id").cloned();
        let mut items: Vec<Photo> = state
            .photos
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|photo| {
                if let Some(project_id) = &project_filter {
                    if &photo.project_id != project_id {
                        return false;
                    }
                }
                if let Some(kind) = &subject_kind_filter {
                    if photo.subject_kind.as_str() != kind {
                        return false;
                    }
                }
                if let Some(subject_id) = &subject_id_filter {
                    if photo.subject_id.to_string() != *subject_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|photo| photo.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn subject_exists(state: &SupervisionState, kind: PhotoSubjectKind, subject_id: Uuid) -> bool {
    match kind {
        PhotoSubjectKind::Inspection => state.inspections.contains_key(&subject_id),
        PhotoSubjectKind::Defect => state.defects.contains_key(&subject_id),
        PhotoSubjectKind::DailyLog => state.daily_logs.contains_key(&subject_id),
        PhotoSubjectKind::ProgressReport => state.progress_reports.contains_key(&subject_id),
    }
}

async fn create_photo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    body: web::Json<CreatePhotoRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Photo> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_CREATE)?;
        let input = body.into_inner();
        let project_id = require_id(&input.project_id, "project_id")?;
        query_project(&ctx, &project_id)?;
        if !subject_exists(&state, input.subject_kind, input.subject_id) {
            return Err(UnifiedError::not_found(format!(
                "{} not found: {}",
                input.subject_kind.as_str(),
                input.subject_id
            )));
        }
        let storage_target = require_text(
            &input.storage_target,
            "storage_target",
            MAX_STORAGE_TARGET_LEN,
        )?;
        let storage_key =
            require_text(&input.storage_key, "storage_key", MAX_STORAGE_KEY_LEN)?;
        let content_sha256 = valid_sha256(&input.content_sha256)?;
        let content_type = valid_content_type(&input.content_type)?;
        if input.size_bytes <= 0 || input.size_bytes > MAX_PHOTO_BYTES {
            return Err(UnifiedError::bad_request(format!(
                "size_bytes must be between 1 and {MAX_PHOTO_BYTES}"
            )));
        }
        let caption = opt_text(&input.caption, "caption", MAX_CAPTION_LEN)?;
        let now = Utc::now();
        let photo = Photo {
            id: Uuid::now_v7(),
            project_id: project_id.clone(),
            subject_kind: input.subject_kind,
            subject_id: input.subject_id,
            storage_target,
            storage_key,
            content_sha256,
            content_type,
            size_bytes: input.size_bytes,
            caption,
            uploaded_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.photos.insert(photo.id, photo.clone());
        state.emit(
            "supervision.photo.attached",
            json!({
                "photo_id": photo.id.to_string(),
                "project_id": photo.project_id,
                "subject_kind": photo.subject_kind.as_str(),
                "subject_id": photo.subject_id.to_string(),
                "storage_target": photo.storage_target,
                "storage_key": photo.storage_key,
            }),
        );
        Ok(photo)
    })();
    match result {
        Ok(photo) => created(photo),
        Err(error) => err(error, debug),
    }
}

async fn get_photo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Photo> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        load_photo(&state, &path.into_inner())
    })();
    match result {
        Ok(photo) => ok(photo),
        Err(error) => err(error, debug),
    }
}

async fn delete_photo(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_MANAGE)?;
        let photo = load_photo(&state, &path.into_inner())?;
        // Deleting here removes only the supervision reference;
        // the stored bytes are owned by the storage module and are
        // not deleted by this call.
        state.photos.remove(&photo.id);
        state.emit(
            "supervision.photo.deleted",
            json!({
                "photo_id": photo.id.to_string(),
                "project_id": photo.project_id,
                "storage_target": photo.storage_target,
                "storage_key": photo.storage_key,
            }),
        );
        Ok(json!({ "deleted": true, "photo_id": photo.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Dashboard
// ---------------------------------------------------------------------------

/// Per-project supervision summary, computed on read.
///
/// Number provenance (exactly which numbers come from where):
///   - `project` (id, name, status, archived): the projects
///     module, via the `project.get_requested` hook query, read
///     at request time. Not stored here.
///   - `open_defects`, `defects_by_severity`,
///     `defects_by_status`: counted from this module's
///     `supervision_defects` rows for the project.
///   - `overdue_inspections`, `inspections_by_status`,
///     `inspections_due_next_7_days`: counted from this module's
///     `supervision_inspections` rows; an inspection is overdue
///     when `scheduled_for` is in the past and its status is
///     still `scheduled` or `in_progress`.
///   - `latest_report`, `progress_variance_points`: from this
///     module's latest approved or submitted
///     `supervision_progress_reports` row for the project
///     (variance = reported minus planned, computed on read);
///     `project_percent_complete` inside it is the snapshot the
///     hook query returned when the report was created, not a
///     live project value.
///   - `active_assignments`, `unsigned_daily_logs`: counted from
///     this module's `supervision_assignments` /
///     `supervision_daily_logs` rows.
async fn project_dashboard(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SupervisionState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SUPERVISION_VIEW)?;
        let params = query.into_inner();
        let project_id = params
            .filters
            .get("project_id")
            .cloned()
            .ok_or_else(|| {
                UnifiedError::bad_request("project_id query parameter is required")
            })?;
        let project = query_project(&ctx, &project_id)?;
        let now = Utc::now();
        let defects: Vec<Defect> = state
            .defects
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|defect| defect.project_id == project_id)
            .collect();
        let open_defects = defects
            .iter()
            .filter(|defect| {
                matches!(
                    defect.status,
                    DefectStatus::Open | DefectStatus::InProgress
                )
            })
            .count() as u64;
        let mut defects_by_severity = serde_json::Map::new();
        for severity in ["low", "medium", "high", "critical"] {
            defects_by_severity.insert(
                severity.to_string(),
                Value::from(
                    defects
                        .iter()
                        .filter(|defect| defect.severity.as_str() == severity)
                        .count() as u64,
                ),
            );
        }
        let mut defects_by_status = serde_json::Map::new();
        for status in ["open", "in_progress", "resolved", "verified"] {
            defects_by_status.insert(
                status.to_string(),
                Value::from(
                    defects
                        .iter()
                        .filter(|defect| defect.status.as_str() == status)
                        .count() as u64,
                ),
            );
        }
        let inspections: Vec<Inspection> = state
            .inspections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|inspection| inspection.project_id == project_id)
            .collect();
        let overdue_inspections = inspections
            .iter()
            .filter(|inspection| {
                inspection.scheduled_for < now
                    && matches!(
                        inspection.status,
                        InspectionStatus::Scheduled | InspectionStatus::InProgress
                    )
            })
            .count() as u64;
        let due_next_7_days = inspections
            .iter()
            .filter(|inspection| {
                inspection.scheduled_for >= now
                    && inspection.scheduled_for
                        < now + chrono::Duration::days(7)
                    && matches!(
                        inspection.status,
                        InspectionStatus::Scheduled | InspectionStatus::InProgress
                    )
            })
            .count() as u64;
        let mut inspections_by_status = serde_json::Map::new();
        for status in [
            "scheduled",
            "in_progress",
            "submitted",
            "approved",
            "returned",
            "cancelled",
        ] {
            inspections_by_status.insert(
                status.to_string(),
                Value::from(
                    inspections
                        .iter()
                        .filter(|inspection| inspection.status.as_str() == status)
                        .count() as u64,
                ),
            );
        }
        let mut reports: Vec<ProgressReport> = state
            .progress_reports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|report| {
                report.project_id == project_id
                    && matches!(
                        report.status,
                        ReportStatus::Submitted | ReportStatus::Approved
                    )
            })
            .collect();
        reports.sort_by_key(|report| report.period_end);
        let latest_report = reports.last().map(|report| report_view(report));
        let progress_variance_points = reports.last().and_then(|report| {
            report
                .planned_percent_complete
                .map(|planned| {
                    ((report.reported_percent_complete - planned) * 100.0).round() / 100.0
                })
        });
        let active_assignments = state
            .assignments
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id
                    && entry.value().status == AssignmentStatus::Active
            })
            .count() as u64;
        let unsigned_daily_logs = state
            .daily_logs
            .iter()
            .filter(|entry| {
                entry.value().project_id == project_id && entry.value().signed_at.is_none()
            })
            .count() as u64;
        Ok(json!({
            "project": {
                "id": project.id,
                "name": project.name,
                "status": project.status,
                "archived": project.archived,
                "source": "project.get_requested",
            },
            "open_defects": open_defects,
            "defects_by_severity": defects_by_severity,
            "defects_by_status": defects_by_status,
            "overdue_inspections": overdue_inspections,
            "inspections_due_next_7_days": due_next_7_days,
            "inspections_by_status": inspections_by_status,
            "latest_report": latest_report,
            "progress_variance_points": progress_variance_points,
            "active_assignments": active_assignments,
            "unsigned_daily_logs": unsigned_daily_logs,
            "sources": {
                "project": "project.get_requested (live hook query)",
                "defects": "supervision_defects (own table)",
                "inspections": "supervision_inspections (own table)",
                "progress": "supervision_progress_reports latest submitted/approved snapshot (project_percent_complete is the snapshot-time hook value, not live)",
                "assignments": "supervision_assignments (own table)",
                "daily_logs": "supervision_daily_logs (own table)",
            },
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hook listeners (projects-module events)
// ---------------------------------------------------------------------------

/// Acknowledge `project.created` / `project.updated`: this module
/// keeps no project cache, so there is nothing to update.
fn on_project_changed(_state: SupervisionState, ctx: &mut HookContext) {
    ctx.add_result(json!({ "acknowledged": true }));
}

/// Answer `project.archived`: suspend active assignments for the
/// archived project so no new supervision work starts on it.
fn on_project_archived(state: SupervisionState, ctx: &mut HookContext) {
    let project_id = ctx
        .data
        .get("id")
        .or_else(|| ctx.data.get("project_id"))
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let mut suspended = 0usize;
    if let Some(project_id) = &project_id {
        let now = Utc::now();
        let active: Vec<SupervisionAssignment> = state
            .assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|assignment| {
                assignment.project_id == *project_id
                    && assignment.status == AssignmentStatus::Active
            })
            .collect();
        for mut assignment in active {
            assignment.status = AssignmentStatus::Suspended;
            assignment.updated_at = now;
            state
                .assignments
                .insert(assignment.id, assignment);
            suspended += 1;
        }
    }
    ctx.add_result(json!({
        "acknowledged": true,
        "project_id": project_id,
        "assignments_suspended": suspended,
    }));
}

/// Acknowledge project task events: a completed task never
/// auto-resolves a defect — resolution needs human verification
/// through `POST /defects/{id}/verify`.
fn on_project_task_event(_state: SupervisionState, ctx: &mut HookContext) {
    ctx.add_result(json!({ "acknowledged": true }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct SupervisionModule {
    state: SupervisionState,
}

impl SupervisionModule {
    pub fn new() -> Self {
        Self {
            state: SupervisionState::default(),
        }
    }
}

impl Default for SupervisionModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for SupervisionModule {
    fn id(&self) -> &'static str {
        "supervision"
    }

    fn name(&self) -> &'static str {
        "Project Supervision"
    }

    fn description(&self) -> &'static str {
        "Site supervision for projects: supervisor assignments, inspection templates and checklists, conducted inspections with verdicts and signatures, progress-report snapshots with an approval workflow, a defect log with photo evidence, approval chains with a full audit trail, and daily site logs. Projects, tasks, and milestones stay owned by the Project Management module and are linked by id through hook queries only."
    }

    fn requires(&self) -> &[&'static str] {
        &["projects", "roles", "storage", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["projects", "roles", "storage", "notifications"]
    }

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
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "project.created",
            "supervision",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_changed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.updated",
            "supervision",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_changed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "project.archived",
            "supervision",
            Arc::new(move |ctx: &mut HookContext| {
                on_project_archived(state.clone(), ctx)
            }),
        );
        for event in [
            "project.task.created",
            "project.task.assigned",
            "project.task.completed",
        ] {
            let state = self.state.clone();
            bus.register(
                event,
                "supervision",
                Arc::new(move |ctx: &mut HookContext| {
                    on_project_task_event(state.clone(), ctx)
                }),
            );
        }
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/supervision")
                .app_data(state)
                .route("/dashboard", web::get().to(project_dashboard))
                .route("/assignments", web::get().to(list_assignments))
                .route("/assignments", web::post().to(create_assignment))
                .route("/assignments/{id}", web::get().to(get_assignment))
                .route("/assignments/{id}", web::patch().to(update_assignment))
                .route("/assignments/{id}", web::delete().to(delete_assignment))
                .route("/templates", web::get().to(list_templates))
                .route("/templates", web::post().to(create_template))
                .route("/templates/{id}", web::get().to(get_template))
                .route("/templates/{id}", web::patch().to(update_template))
                .route("/templates/{id}", web::delete().to(delete_template))
                .route("/inspections", web::get().to(list_inspections))
                .route("/inspections", web::post().to(create_inspection))
                .route("/inspections/{id}", web::get().to(get_inspection))
                .route("/inspections/{id}", web::patch().to(update_inspection))
                .route("/inspections/{id}/submit", web::post().to(submit_inspection))
                .route("/inspections/{id}/complete", web::post().to(complete_inspection))
                .route("/inspections/{id}/approve", web::post().to(approve_inspection))
                .route("/inspections/{id}/return", web::post().to(return_inspection))
                .route(
                    "/inspections/{id}/items/{item_id}/result",
                    web::post().to(record_item_result),
                )
                .route(
                    "/inspections/{id}/items/{item_id}/follow-up",
                    web::post().to(follow_up_item),
                )
                .route("/progress-reports", web::get().to(list_progress_reports))
                .route("/progress-reports", web::post().to(create_progress_report))
                .route("/progress-reports/{id}", web::get().to(get_progress_report))
                .route(
                    "/progress-reports/{id}",
                    web::patch().to(update_progress_report),
                )
                .route(
                    "/progress-reports/{id}/submit",
                    web::post().to(submit_progress_report),
                )
                .route(
                    "/progress-reports/{id}/approve",
                    web::post().to(approve_progress_report),
                )
                .route(
                    "/progress-reports/{id}/return",
                    web::post().to(return_progress_report),
                )
                .route("/defects", web::get().to(list_defects))
                .route("/defects", web::post().to(create_defect))
                .route("/defects/{id}", web::get().to(get_defect))
                .route("/defects/{id}", web::patch().to(update_defect))
                .route("/defects/{id}/follow-up", web::post().to(follow_up_defect))
                .route("/defects/{id}/verify", web::post().to(verify_defect))
                .route("/approvals", web::get().to(list_approvals))
                .route("/daily-logs", web::get().to(list_daily_logs))
                .route("/daily-logs", web::post().to(create_daily_log))
                .route("/daily-logs/{id}", web::get().to(get_daily_log))
                .route("/daily-logs/{id}", web::patch().to(update_daily_log))
                .route("/daily-logs/{id}/sign-off", web::post().to(sign_off_daily_log))
                .route("/photos", web::get().to(list_photos))
                .route("/photos", web::post().to(create_photo))
                .route("/photos/{id}", web::get().to(get_photo))
                .route("/photos/{id}", web::delete().to(delete_photo)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/supervision/dashboard", owner: "supervision", description: "Per-project supervision summary (open defects, overdue inspections, progress variance) computed on read; requires project_id." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/assignments", owner: "supervision", description: "List supervision assignments (paginated, filter by project_id, user_id, role, status)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/assignments", owner: "supervision", description: "Assign a supervisor / inspector / approver to a project (project validated via project.get_requested)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/assignments/{id}", owner: "supervision", description: "Get one supervision assignment." },
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/assignments/{id}", owner: "supervision", description: "Update a supervision assignment (role, status, period, notes)." },
            RouteInfo { method: "DELETE", path: "/api/v1/supervision/assignments/{id}", owner: "supervision", description: "Delete a supervision assignment." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/templates", owner: "supervision", description: "List inspection templates (paginated, filter by project_id, category)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/templates", owner: "supervision", description: "Create an inspection template with checklist items." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/templates/{id}", owner: "supervision", description: "Get one inspection template." },
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/templates/{id}", owner: "supervision", description: "Update an inspection template (items are replaced wholesale)." },
            RouteInfo { method: "DELETE", path: "/api/v1/supervision/templates/{id}", owner: "supervision", description: "Delete an inspection template with no inspections created from it." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/inspections", owner: "supervision", description: "List inspections (paginated, filter by project_id, template_id, status, inspector_id, overdue)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections", owner: "supervision", description: "Create an inspection, optionally from a template (template items copied as pending results)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/inspections/{id}", owner: "supervision", description: "Get one inspection with its item results." },
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/inspections/{id}", owner: "supervision", description: "Update a scheduled, in-progress, or returned inspection." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/submit", owner: "supervision", description: "Submit an inspection for approval (all items resulted, verdict recorded)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/complete", owner: "supervision", description: "Record the overall verdict and the inspector signature." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/approve", owner: "supervision", description: "Approve a submitted inspection (records an approval row)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/return", owner: "supervision", description: "Return a submitted inspection for rework (records an approval row)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/items/{item_id}/result", owner: "supervision", description: "Record pass / fail / na plus notes for one checklist item." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/inspections/{id}/items/{item_id}/follow-up", owner: "supervision", description: "Request a follow-up task in the projects module for a failed item." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/progress-reports", owner: "supervision", description: "List progress reports (paginated, filter by project_id, milestone_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports", owner: "supervision", description: "Create a progress report snapshot (reported vs planned; project percent from project.progress_requested, source recorded)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/progress-reports/{id}", owner: "supervision", description: "Get one progress report with its derived variance." },
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/progress-reports/{id}", owner: "supervision", description: "Update a draft or returned progress report (re-snapshots the project percent)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports/{id}/submit", owner: "supervision", description: "Submit a draft progress report for approval." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports/{id}/approve", owner: "supervision", description: "Approve a submitted progress report (records an approval row)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/progress-reports/{id}/return", owner: "supervision", description: "Return a submitted progress report (comment required; records an approval row)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/defects", owner: "supervision", description: "List defects (paginated, filter by project_id, inspection_id, severity, status, task_id)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/defects", owner: "supervision", description: "Log a defect found on site (optionally linked to an inspection or a project task)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/defects/{id}", owner: "supervision", description: "Get one defect." },
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/defects/{id}", owner: "supervision", description: "Update a defect (forward-only status transitions)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/defects/{id}/follow-up", owner: "supervision", description: "Request a follow-up task in the projects module for a defect." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/defects/{id}/verify", owner: "supervision", description: "Verify a resolved defect (resolved -> verified)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/approvals", owner: "supervision", description: "List approval rows (paginated, filter by target_kind, target_id, approver_id, decision, project_id)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/daily-logs", owner: "supervision", description: "List site daily logs (paginated, filter by project_id, date, supervisor_id, signed)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/daily-logs", owner: "supervision", description: "Create a site daily log (one per project per day)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/daily-logs/{id}", owner: "supervision", description: "Get one daily log." },
            RouteInfo { method: "PATCH", path: "/api/v1/supervision/daily-logs/{id}", owner: "supervision", description: "Update an unsigned daily log (signed-off logs are immutable)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/daily-logs/{id}/sign-off", owner: "supervision", description: "Supervisor sign-off for a daily log (only the log's supervisor)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/photos", owner: "supervision", description: "List photo evidence references (paginated, filter by project_id, subject_kind, subject_id)." },
            RouteInfo { method: "POST", path: "/api/v1/supervision/photos", owner: "supervision", description: "Attach photo evidence (storage reference + SHA-256 hash + caption; no bytes stored)." },
            RouteInfo { method: "GET", path: "/api/v1/supervision/photos/{id}", owner: "supervision", description: "Get one photo evidence record." },
            RouteInfo { method: "DELETE", path: "/api/v1/supervision/photos/{id}", owner: "supervision", description: "Delete a photo evidence reference (the stored bytes are owned by the storage module and are not deleted)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SupervisionModule::new()));
}
