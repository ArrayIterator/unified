// supervision module -- Shared types, records, request bodies, and constants.

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_TEXT_LEN: usize = 5000;

pub(crate) const MAX_LOCATION_LEN: usize = 500;

pub(crate) const MAX_CAPTION_LEN: usize = 500;

pub(crate) const MAX_COMMENT_LEN: usize = 2000;

pub(crate) const MAX_STORAGE_TARGET_LEN: usize = 128;

pub(crate) const MAX_STORAGE_KEY_LEN: usize = 512;

pub(crate) const MAX_SHA256_LEN: usize = 64;

pub(crate) const MAX_WEATHER_LEN: usize = 64;

/// Photo evidence size cap (20 MiB); the storage module enforces
/// its own limits as well -- this is the supervision-side guard.
pub(crate) const MAX_PHOTO_BYTES: i64 = 20 * 1024 * 1024;

/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_SUPERVISION_VIEW: &str = "supervision.view";

pub(crate) const PERM_SUPERVISION_CREATE: &str = "supervision.create";

pub(crate) const PERM_SUPERVISION_MANAGE: &str = "supervision.manage";

pub(crate) const PERM_SUPERVISION_APPROVE: &str = "supervision.approve";

/// Hook event names this module emits or listens to.
pub(crate) const PROJECT_GET_REQUESTED: &str = "project.get_requested";

pub(crate) const PROJECT_PROGRESS_REQUESTED: &str = "project.progress_requested";

pub(crate) const PROJECT_TASK_CREATE_REQUESTED: &str = "project.task.create_requested";

pub(crate) const PROJECT_TASK_GET_REQUESTED: &str = "project.task.get_requested";

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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
/// scheduled / in_progress / returned through PATCH with
/// `status: "cancelled"`.
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    /// cryptographic signature -- no key material is involved.
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

impl ReportStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
            Self::Approved => "approved",
            Self::Returned => "returned",
        }
    }
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    /// Only `cancelled` is accepted: an inspection is cancelled
    /// by sending `status: "cancelled"` while it is scheduled,
    /// in progress, or returned. Any other status value is
    /// rejected -- submit / complete / approve / return have
    /// their own endpoints.
    pub status: Option<InspectionStatus>,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assignment_role_as_str() {
        assert_eq!(AssignmentRole::Supervisor.as_str(), "supervisor");
        assert_eq!(AssignmentRole::Inspector.as_str(), "inspector");
        assert_eq!(AssignmentRole::Approver.as_str(), "approver");
    }

    #[test]
    fn assignment_role_serde_uses_snake_case() {
        let text = serde_json::to_string(&AssignmentRole::Inspector).expect("serializes");
        assert_eq!(text, "\"inspector\"");
        let back: AssignmentRole =
            serde_json::from_str("\"approver\"").expect("deserializes");
        assert_eq!(back, AssignmentRole::Approver);
    }

    #[test]
    fn report_status_as_str_matches_serde_snake_case() {
        assert_eq!(ReportStatus::Draft.as_str(), "draft");
        assert_eq!(ReportStatus::Submitted.as_str(), "submitted");
        assert_eq!(ReportStatus::Approved.as_str(), "approved");
        assert_eq!(ReportStatus::Returned.as_str(), "returned");
        for status in [
            ReportStatus::Draft,
            ReportStatus::Submitted,
            ReportStatus::Approved,
            ReportStatus::Returned,
        ] {
            let text = serde_json::to_string(&status).expect("serializes");
            assert_eq!(text, format!("\"{}\"", status.as_str()));
        }
    }

    #[test]
    fn update_inspection_request_accepts_cancelled_status() {
        let parsed: UpdateInspectionRequest =
            serde_json::from_str(r#"{"status": "cancelled"}"#).expect("deserializes");
        assert_eq!(parsed.status, Some(InspectionStatus::Cancelled));
        let parsed: UpdateInspectionRequest =
            serde_json::from_str(r#"{"title": "Site check"}"#).expect("deserializes");
        assert_eq!(parsed.status, None);
    }
}
