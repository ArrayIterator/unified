// Compliance Center module — GDPR / ISO support: data export
// requests, data deletion requests with an approval step, an
// append-only consent log, retention policies, processing-activity
// records (RoPA), compliance checklists with evidence, and a breach
// log.
//
// Routes live under `/api/v1/compliance`:
//
//   Exports:
//     GET/POST /exports — list / request an export.
//     GET  /exports/{id}.
//     POST /exports/{id}/start — mark processing (manager).
//     POST /exports/{id}/complete — attach the storage link with
//          expiry (manager).
//     GET  /exports/{id}/download — the storage link while it is
//          still valid.
//   Deletions:
//     GET/POST /deletions — list / request a deletion.
//     GET  /deletions/{id}.
//     POST /deletions/{id}/approve, /deletions/{id}/reject.
//     POST /deletions/{id}/execute — dispatch per-module execution
//          (manager, only after approval).
//     GET  /deletions/{id}/items.
//     POST /deletions/{id}/items/{item_id}/mark-executed — confirm
//          one item executed (manager; for owning modules without
//          a hook listener).
//   Consents (append-only):
//     GET/POST /consents — list / record consent or withdrawal.
//     GET  /consents/{id}.
//   Retention policies:
//     GET/POST /retention-policies — list / create.
//     GET/PATCH/DELETE /retention-policies/{id}.
//     POST /retention-policies/{id}/review — enqueue the
//          retention-review scheduler job (flags items, never
//          deletes).
//     GET  /retention-reviews — review runs and their flags.
//   Processing-activity records (RoPA):
//     GET/POST /processing-records — list / create.
//     GET/PATCH/DELETE /processing-records/{id}.
//   Checklists:
//     GET/POST /checklists — list / create.
//     GET/PATCH/DELETE /checklists/{id}.
//   Breach log:
//     GET/POST /breaches — list / report a breach.
//     GET/PATCH /breaches/{id}.
//     POST /breaches/{id}/timeline — append a timeline entry.
//     GET  /breaches/{id}/timeline.
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory export, deletion, consent, retention-policy,
// processing-record, checklist, and breach-log management; the
// export state machine (requested -> processing -> ready, with a
// storage link and expiry; failed and expired states included);
// the deletion state machine (requested -> approved -> executing
// -> executed, with rejection); per-module deletion items whose
// execution is dispatched to the owning modules through the
// `compliance.deletion.item.execute_requested` event and confirmed
// back through `compliance.deletion.item.executed` (or through the
// manual mark-executed endpoint); an append-only consent log
// (withdrawal is a new row, never an overwrite, and there is no
// update or delete route); retention policies whose enforcement is
// a scheduler job (`compliance.retention.review`) that only flags
// items — nothing in this module ever auto-deletes data; RoPA
// records; checklists with storage references for evidence; and a
// breach log with an append-only timeline and notification status.
// What is NOT built here: the export file itself (assembly belongs
// to the owning modules — this module requests it through
// `compliance.export.requested` and records the storage link the
// assembler returns through the complete endpoint); actual
// deletion of other modules' rows (this module tracks and audits
// only — owning modules delete their own rows when they answer
// the execute event); actual delivery of breach notifications
// (the notifications module owns delivery — this module only
// records notification status and emits
// `notification.requested`); and durable storage (the DashMap
// stores are authoritative; migration
// `2026-11-22-000057_compliance` is the persistence target).
//
// Scheduler note: `compliance.retention.review` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}`
// and the review run records `scheduler_status: "declined"`
// instead of pretending it was scheduled. The policy row and the
// review request are still stored; the follow-up is to add
// `"compliance.retention.review"` to `KNOWN_KINDS` in
// `scheduler.rs` and arm the dispatch so the job emits
// `compliance.retention.review_requested` with the policy id.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `compliance.export.requested`, `compliance.export.started`,
//     `compliance.export.completed`, `compliance.export.failed`
//   - `compliance.deletion.requested`,
//     `compliance.deletion.approved`,
//     `compliance.deletion.rejected`,
//     `compliance.deletion.executing`,
//     `compliance.deletion.executed`
//   - `compliance.deletion.item.execute_requested` (per item, for
//     the owning module: `{"deletion_id", "item_id", "module",
//     "subject", "requester_id"}`)
//   - `compliance.deletion.item.executed` (emitted by this module
//     when an item is confirmed executed)
//   - `compliance.consent.recorded`,
//     `compliance.consent.withdrawn`
//   - `compliance.retention.policy.created`,
//     `compliance.retention.review.requested`,
//     `compliance.retention.review.completed`
//   - `compliance.processing_record.created`
//   - `compliance.checklist.created`,
//     `compliance.checklist.updated`
//   - `compliance.breach.reported`, `compliance.breach.updated`
//   - `notification.requested` (breach reported, in the
//     notifications module's payload shape; the caller supplies
//     `notify_user_id` when a specific recipient must be told)
//   - `scheduler.enqueue` with `{"kind":
//     "compliance.retention.review", "payload": {"policy_id",
//     "review_id", ...}}` when a retention review is requested.
// Events this module listens to:
//   - `compliance.deletion.item.executed` — confirms one deletion
//     item as executed; when every item of a request is executed,
//     the request moves to `executed`.
//   - `compliance.retention.review_requested` — the event the
//     scheduler emits when a `compliance.retention.review` job
//     fires; records the review run as completed with the flag
//     counts the job payload carries.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// Maximum lengths enforced on user supplied text.
const MAX_NAME_LEN: usize = 200;
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 2000;
const MAX_PURPOSE_LEN: usize = 500;
const MAX_CATEGORY_LEN: usize = 120;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_BASIS_LEN: usize = 120;
const MAX_SCOPE_LEN: usize = 500;
/// Default download-link lifetime for a completed export, in hours.
const DEFAULT_DOWNLOAD_LINK_HOURS: i64 = 72;

/// Capability names checked through the `role.check` hook.
const PERM_COMPLIANCE_VIEW: &str = "compliance.view";
const PERM_COMPLIANCE_MANAGE: &str = "compliance.manage";

/// Scheduler job kind for retention reviews, and the event the
/// scheduler must emit when that job fires.
pub const COMPLIANCE_RETENTION_JOB_KIND: &str = "compliance.retention.review";
const RETENTION_REVIEW_REQUESTED: &str = "compliance.retention.review_requested";
const DELETION_ITEM_EXECUTED: &str = "compliance.deletion.item.executed";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of an export request: `requested` -> `processing` ->
/// `ready` (download link attached, expiry set); `failed` when
/// assembly fails; `expired` once the link expiry passes (derived
/// at read time, see `export_view`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportStatus {
    Requested,
    Processing,
    Ready,
    Failed,
    Expired,
}

impl Default for ExportStatus {
    fn default() -> Self {
        Self::Requested
    }
}

impl ExportStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Processing => "processing",
            Self::Ready => "ready",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }
}

/// One data export request. `scope` names the modules / data
/// categories the export covers (free-form list, validated as
/// short strings). The file itself is assembled by the owning
/// modules; this row tracks the request and, once ready, the
/// storage link (`storage_target` + `storage_key`) and its expiry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportRequest {
    pub id: Uuid,
    pub requester_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope: Vec<String>,
    pub status: ExportStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download_expires_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of a deletion request: `requested` -> `approved` (or
/// `rejected`) -> `executing` (items dispatched) -> `executed`
/// (every item confirmed).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionStatus {
    Requested,
    Approved,
    Rejected,
    Executing,
    Executed,
}

impl Default for DeletionStatus {
    fn default() -> Self {
        Self::Requested
    }
}

impl DeletionStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Executing => "executing",
            Self::Executed => "executed",
        }
    }
}

/// One data deletion request. This module never deletes another
/// module's rows directly: it tracks the request, the approval,
/// and an audit record of WHAT was deleted, per module, in
/// `DeletionItem` rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionRequest {
    pub id: Uuid,
    pub requester_id: String,
    pub reason: String,
    pub status: DeletionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One per-module item of a deletion request: which module owns
/// the data and what was deleted. `status` is `pending`,
/// `dispatched`, or `executed`; `detail` records what was deleted
/// (for example "3 posts, 12 votes") once the owning module
/// confirms.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeletionItem {
    pub id: Uuid,
    pub deletion_request_id: Uuid,
    /// Id of the module that owns the data (for example "community").
    pub module_id: String,
    /// What is to be deleted, as named by the requester (for
    /// example "all forum posts and votes").
    pub subject: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One append-only consent record: who consented to what, when,
/// and the version of the consent text. `granted` true means
/// consent given, false means withdrawn. Rows are never updated or
/// deleted; a withdrawal is a new row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsentLog {
    pub id: Uuid,
    pub user_id: String,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
    pub recorded_at: DateTime<Utc>,
}

/// One retention policy: how long one data category is kept and
/// what the review action is. Policies are records only;
/// enforcement is the `compliance.retention.review` scheduler job,
/// which flags items and never auto-deletes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionPolicy {
    pub id: Uuid,
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One retention-review run: a policy reviewed at a point in time,
/// with how many items were flagged for review. `scheduler_status`
/// is `not_scheduled`, `queued`, or `declined` (see the scheduler
/// note in the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionReview {
    pub id: Uuid,
    pub policy_id: Uuid,
    pub items_flagged: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub reviewed_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One processing-activity record (RoPA): purpose, data
/// categories, recipients, retention, and lawful basis.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessingRecord {
    pub id: Uuid,
    pub purpose: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_categories: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recipients: Vec<String>,
    pub retention: String,
    pub lawful_basis: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One compliance checklist item. Evidence is a storage-module
/// reference (target + key); no file bytes live in this module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplianceChecklist {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// `open`, `in_progress`, `done`.
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_storage_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub due_date: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One breach-log entry with its timeline. `notification_status`
/// is `not_started`, `in_progress`, or `notified`. Timeline
/// entries are append-only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreachLog {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    /// `reported`, `investigating`, `contained`, `notified`, `closed`.
    pub status: String,
    pub notification_status: String,
    pub reported_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One append-only timeline entry on a breach.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreachTimelineEntry {
    pub id: Uuid,
    pub breach_id: Uuid,
    pub author_id: String,
    pub entry: String,
    pub recorded_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateExportRequest {
    #[serde(default)]
    pub scope: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct CompleteExportRequest {
    pub storage_target: String,
    pub storage_key: String,
    /// Link lifetime in hours; defaults to 72.
    pub expires_in_hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct FailExportRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateDeletionRequest {
    pub reason: String,
    /// Per-module items: which module owns what data.
    pub items: Vec<DeletionItemInput>,
}

#[derive(Debug, Deserialize)]
pub struct DeletionItemInput {
    pub module_id: String,
    pub subject: String,
}

#[derive(Debug, Deserialize)]
pub struct DecisionRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MarkItemExecutedRequest {
    /// What was deleted, for the audit record (for example
    /// "3 posts, 12 votes").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RecordConsentRequest {
    /// The user the consent is for. Defaults to the caller;
    /// recording for another user requires the manage capability.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    pub purpose: String,
    pub consent_text_version: String,
    pub granted: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateRetentionPolicyRequest {
    pub data_category: String,
    pub retain_days: i64,
    pub review_action: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRetentionPolicyRequest {
    pub data_category: Option<String>,
    pub retain_days: Option<i64>,
    pub review_action: Option<String>,
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateProcessingRecordRequest {
    pub purpose: String,
    #[serde(default)]
    pub data_categories: Vec<String>,
    #[serde(default)]
    pub recipients: Vec<String>,
    pub retention: String,
    pub lawful_basis: String,
    pub owner: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProcessingRecordRequest {
    pub purpose: Option<String>,
    pub data_categories: Option<Vec<String>>,
    pub recipients: Option<Vec<String>>,
    pub retention: Option<String>,
    pub lawful_basis: Option<String>,
    pub owner: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateChecklistRequest {
    pub title: String,
    pub description: Option<String>,
    pub status: Option<String>,
    pub evidence_storage_target: Option<String>,
    pub evidence_storage_key: Option<String>,
    pub due_date: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateChecklistRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub status: Option<String>,
    pub evidence_storage_target: Option<Option<String>>,
    pub evidence_storage_key: Option<Option<String>>,
    pub due_date: Option<Option<DateTime<Utc>>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateBreachRequest {
    pub title: String,
    pub description: String,
    pub discovered_at: DateTime<Utc>,
    pub affected_scope: String,
    /// Optional user to notify through `notification.requested`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notify_user_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateBreachRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub affected_scope: Option<String>,
    pub status: Option<String>,
    pub notification_status: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AddTimelineRequest {
    pub entry: String,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for compliance. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000057_compliance`; until that migration is
/// executed, compliance data does not survive a restart.
#[derive(Clone)]
pub struct ComplianceState {
    pub exports: Arc<DashMap<Uuid, ExportRequest>>,
    pub deletions: Arc<DashMap<Uuid, DeletionRequest>>,
    pub deletion_items: Arc<DashMap<Uuid, DeletionItem>>,
    pub consents: Arc<DashMap<Uuid, ConsentLog>>,
    pub retention_policies: Arc<DashMap<Uuid, RetentionPolicy>>,
    pub retention_reviews: Arc<DashMap<Uuid, RetentionReview>>,
    pub processing_records: Arc<DashMap<Uuid, ProcessingRecord>>,
    pub checklists: Arc<DashMap<Uuid, ComplianceChecklist>>,
    pub breaches: Arc<DashMap<Uuid, BreachLog>>,
    pub breach_timeline: Arc<DashMap<Uuid, BreachTimelineEntry>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ComplianceState {
    fn default() -> Self {
        Self {
            exports: Arc::new(DashMap::new()),
            deletions: Arc::new(DashMap::new()),
            deletion_items: Arc::new(DashMap::new()),
            consents: Arc::new(DashMap::new()),
            retention_policies: Arc::new(DashMap::new()),
            retention_reviews: Arc::new(DashMap::new()),
            processing_records: Arc::new(DashMap::new()),
            checklists: Arc::new(DashMap::new()),
            breaches: Arc::new(DashMap::new()),
            breach_timeline: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ComplianceState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "compliance",
                "category": "compliance",
                "priority": "high",
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
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

fn can_manage(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_COMPLIANCE_MANAGE)
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

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

fn load_export(state: &ComplianceState, raw_id: &str) -> UnifiedResult<ExportRequest> {
    let id = parse_uuid(raw_id, "export request")?;
    state
        .exports
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Export request not found: {id}")))
}

fn load_deletion(state: &ComplianceState, raw_id: &str) -> UnifiedResult<DeletionRequest> {
    let id = parse_uuid(raw_id, "deletion request")?;
    state
        .deletions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Deletion request not found: {id}")))
}

fn load_deletion_item(
    state: &ComplianceState,
    deletion_id: Uuid,
    raw_id: &str,
) -> UnifiedResult<DeletionItem> {
    let id = parse_uuid(raw_id, "deletion item")?;
    state
        .deletion_items
        .get(&id)
        .map(|entry| entry.value().clone())
        .filter(|item| item.deletion_request_id == deletion_id)
        .ok_or_else(|| UnifiedError::not_found(format!("Deletion item not found: {id}")))
}

fn load_consent(state: &ComplianceState, raw_id: &str) -> UnifiedResult<ConsentLog> {
    let id = parse_uuid(raw_id, "consent record")?;
    state
        .consents
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Consent record not found: {id}")))
}

fn load_policy(state: &ComplianceState, raw_id: &str) -> UnifiedResult<RetentionPolicy> {
    let id = parse_uuid(raw_id, "retention policy")?;
    state
        .retention_policies
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Retention policy not found: {id}")))
}

fn load_processing_record(
    state: &ComplianceState,
    raw_id: &str,
) -> UnifiedResult<ProcessingRecord> {
    let id = parse_uuid(raw_id, "processing record")?;
    state
        .processing_records
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Processing record not found: {id}")))
}

fn load_checklist(state: &ComplianceState, raw_id: &str) -> UnifiedResult<ComplianceChecklist> {
    let id = parse_uuid(raw_id, "checklist")?;
    state
        .checklists
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Checklist not found: {id}")))
}

fn load_breach(state: &ComplianceState, raw_id: &str) -> UnifiedResult<BreachLog> {
    let id = parse_uuid(raw_id, "breach")?;
    state
        .breaches
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Breach not found: {id}")))
}

/// Effective export status at read time: a `ready` export whose
/// download link expired reads as `expired` (the stored status is
/// unchanged until the next write).
fn export_view(export: &ExportRequest, now: DateTime<Utc>) -> Value {
    let mut value = serde_json::to_value(export).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let effective = match export.status {
            ExportStatus::Ready => match export.download_expires_at {
                Some(expires_at) if now >= expires_at => ExportStatus::Expired,
                _ => ExportStatus::Ready,
            },
            other => other,
        };
        map.insert(
            "status".to_string(),
            Value::String(effective.as_str().to_string()),
        );
    }
    value
}

/// A requester may only see their own export / deletion requests
/// unless they hold the manage capability.
fn check_own_or_manage(
    ctx: &AppContext,
    role: &str,
    owner_id: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if owner_id == actor || can_manage(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Request not found"))
    }
}

fn valid_checklist_status(status: &str) -> bool {
    matches!(status, "open" | "in_progress" | "done")
}

fn valid_breach_status(status: &str) -> bool {
    matches!(
        status,
        "reported" | "investigating" | "contained" | "notified" | "closed"
    )
}

fn valid_notification_status(status: &str) -> bool {
    matches!(status, "not_started" | "in_progress" | "notified")
}

/// Mark every item of a deletion request `executed` when all of
/// them are confirmed, and move the request itself to `executed`.
/// Called after any single item is confirmed.
fn maybe_finish_deletion(state: &ComplianceState, deletion_id: Uuid) {
    let items: Vec<DeletionItem> = state
        .deletion_items
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.deletion_request_id == deletion_id)
        .collect();
    if items.is_empty() {
        return;
    }
    if !items.iter().all(|item| item.status == "executed") {
        return;
    }
    let Some(mut request) = state
        .deletions
        .get_mut(&deletion_id)
        .map(|entry| entry.value().clone())
    else {
        return;
    };
    if request.status == DeletionStatus::Executed {
        return;
    }
    let now = Utc::now();
    request.status = DeletionStatus::Executed;
    request.executed_at = Some(now);
    request.updated_at = now;
    state.deletions.insert(request.id, request.clone());
    state.emit(
        "compliance.deletion.executed",
        json!({
            "deletion_id": request.id.to_string(),
            "requester_id": request.requester_id,
            "executed_at": now.to_rfc3339(),
            "items": items.iter().map(|item| json!({
                "item_id": item.id.to_string(),
                "module": item.module_id,
                "subject": item.subject,
                "detail": item.detail,
            })).collect::<Vec<_>>(),
        }),
    );
}

/// Confirm one deletion item as executed (shared by the hook
/// listener and the manual mark-executed endpoint).
fn confirm_item_executed(
    state: &ComplianceState,
    item: &DeletionItem,
    detail: Option<String>,
) -> DeletionItem {
    let now = Utc::now();
    let mut updated = item.clone();
    updated.status = "executed".to_string();
    if detail.is_some() {
        updated.detail = detail;
    }
    updated.executed_at = Some(now);
    updated.updated_at = now;
    state.deletion_items.insert(updated.id, updated.clone());
    state.emit(
        "compliance.deletion.item.executed",
        json!({
            "deletion_id": updated.deletion_request_id.to_string(),
            "item_id": updated.id.to_string(),
            "module": updated.module_id,
            "subject": updated.subject,
            "detail": updated.detail,
            "executed_at": now.to_rfc3339(),
        }),
    );
    maybe_finish_deletion(state, updated.deletion_request_id);
    updated
}

// ---------------------------------------------------------------------------
// Deletion listeners
// ---------------------------------------------------------------------------

/// `compliance.deletion.item.executed` carries `item_id` (and
/// optionally `detail`). An unknown item id is ignored so a stale
/// confirmation can never mark the wrong row.
fn on_deletion_item_executed(state: ComplianceState, ctx: &mut HookContext) {
    let Some(raw_id) = ctx.get("item_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "compliance.deletion.item.executed carries no item_id" }));
        return;
    };
    let Ok(id) = Uuid::parse_str(raw_id) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "Invalid item id" }));
        return;
    };
    let Some(item) = state
        .deletion_items
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "compliance": "skipped", "item_id": raw_id, "reason": "deletion item not found" }));
        return;
    };
    if item.status == "executed" {
        ctx.add_result(json!({ "compliance": "already_executed", "item_id": raw_id }));
        return;
    }
    let detail = ctx
        .get("detail")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let updated = confirm_item_executed(&state, &item, detail);
    ctx.add_result(json!({
        "compliance": "item_executed",
        "item_id": updated.id.to_string(),
        "deletion_id": updated.deletion_request_id.to_string(),
    }));
}

/// `compliance.retention.review_requested` carries `review_id`
/// (and optionally `items_flagged` / `notes`). Records the review
/// run as completed; an unknown review id is ignored.
fn on_retention_review_requested(state: ComplianceState, ctx: &mut HookContext) {
    let Some(raw_id) = ctx.get("review_id").and_then(|value| value.as_str()) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "compliance.retention.review_requested carries no review_id" }));
        return;
    };
    let Ok(id) = Uuid::parse_str(raw_id) else {
        ctx.add_result(json!({ "compliance": "skipped", "reason": "Invalid review id" }));
        return;
    };
    let Some(mut review) = state
        .retention_reviews
        .get(&id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "compliance": "skipped", "review_id": raw_id, "reason": "retention review not found" }));
        return;
    };
    if let Some(flagged) = ctx.get("items_flagged").and_then(|value| value.as_i64()) {
        if flagged >= 0 {
            review.items_flagged = flagged;
        }
    }
    if let Some(notes) = ctx.get("notes").and_then(|value| value.as_str()) {
        review.notes = Some(notes.to_string());
    }
    review.scheduler_status = "queued".to_string();
    review.reviewed_at = Utc::now();
    review.updated_at = Utc::now();
    state.retention_reviews.insert(review.id, review.clone());
    state.emit(
        "compliance.retention.review.completed",
        json!({
            "review_id": review.id.to_string(),
            "policy_id": review.policy_id.to_string(),
            "items_flagged": review.items_flagged,
        }),
    );
    ctx.add_result(json!({
        "compliance": "review_completed",
        "review_id": review.id.to_string(),
        "items_flagged": review.items_flagged,
    }));
}

// ---------------------------------------------------------------------------
// Export handlers
// ---------------------------------------------------------------------------

async fn list_exports(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let requester_filter = params.filters.get("requester_id").cloned();
        let now = Utc::now();
        let mut items: Vec<Value> = state
            .exports
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|export| {
                if !is_manager && export.requester_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    let effective = match export.status {
                        ExportStatus::Ready => match export.download_expires_at {
                            Some(expires_at) if now >= expires_at => "expired",
                            _ => "ready",
                        },
                        other => other.as_str(),
                    };
                    if effective != status {
                        return false;
                    }
                }
                if let Some(requester) = &requester_filter {
                    if &export.requester_id != requester {
                        return false;
                    }
                }
                true
            })
            .map(|export| export_view(&export, now))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let input = body.into_inner();
        let mut scope: Vec<String> = Vec::new();
        for raw in &input.scope {
            scope.push(require_text(raw, "scope entry", MAX_SCOPE_LEN)?);
        }
        // One open export per requester at a time: a second request
        // while one is still requested / processing is a conflict,
        // so owning modules never assemble the same export twice.
        if state.exports.iter().any(|entry| {
            entry.value().requester_id == caller.actor
                && matches!(
                    entry.value().status,
                    ExportStatus::Requested | ExportStatus::Processing
                )
        }) {
            return Err(UnifiedError::conflict(
                "You already have an open export request; wait for it to finish first",
            ));
        }
        let now = Utc::now();
        let export = ExportRequest {
            id: Uuid::now_v7(),
            requester_id: caller.actor.clone(),
            scope,
            status: ExportStatus::Requested,
            storage_target: None,
            storage_key: None,
            download_expires_at: None,
            failure_reason: None,
            created_at: now,
            updated_at: now,
        };
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.requested",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
                "scope": export.scope,
            }),
        );
        Ok(export)
    })();
    match result {
        Ok(export) => created(export),
        Err(error) => err(error, debug),
    }
}

async fn get_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let export = load_export(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &export.requester_id, &caller.actor)?;
        Ok(export_view(&export, Utc::now()))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn start_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut export = load_export(&state, &path.into_inner())?;
        if export.status != ExportStatus::Requested {
            return Err(UnifiedError::conflict(format!(
                "Only a requested export can start processing (status is \"{}\")",
                export.status.as_str()
            )));
        }
        export.status = ExportStatus::Processing;
        export.updated_at = Utc::now();
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.started",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
            }),
        );
        Ok(export)
    })();
    match result {
        Ok(export) => ok(export),
        Err(error) => err(error, debug),
    }
}

async fn complete_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CompleteExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut export = load_export(&state, &path.into_inner())?;
        if export.status != ExportStatus::Processing {
            return Err(UnifiedError::conflict(format!(
                "Only a processing export can be completed (status is \"{}\")",
                export.status.as_str()
            )));
        }
        let input = body.into_inner();
        let storage_target = require_text(&input.storage_target, "storage_target", MAX_STORAGE_KEY_LEN)?;
        let storage_key = require_text(&input.storage_key, "storage_key", MAX_STORAGE_KEY_LEN)?;
        let hours = input.expires_in_hours.unwrap_or(DEFAULT_DOWNLOAD_LINK_HOURS);
        if hours < 1 || hours > 24 * 30 {
            return Err(UnifiedError::bad_request(
                "expires_in_hours must be between 1 and 720",
            ));
        }
        let now = Utc::now();
        export.status = ExportStatus::Ready;
        export.storage_target = Some(storage_target);
        export.storage_key = Some(storage_key);
        export.download_expires_at = Some(now + Duration::hours(hours));
        export.updated_at = now;
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.completed",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
                "storage_target": export.storage_target,
                "storage_key": export.storage_key,
                "download_expires_at": export.download_expires_at.map(|value| value.to_rfc3339()),
            }),
        );
        state.notify(
            "compliance.export.completed",
            &export.requester_id,
            "Your data export is ready",
            "Your requested data export is ready to download. The link expires soon.",
            Some(format!("/api/v1/compliance/exports/{}/download", export.id)),
        );
        Ok(export_view(&export, now))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn fail_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FailExportRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExportRequest> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut export = load_export(&state, &path.into_inner())?;
        if !matches!(
            export.status,
            ExportStatus::Requested | ExportStatus::Processing
        ) {
            return Err(UnifiedError::conflict(format!(
                "Only a requested or processing export can fail (status is \"{}\")",
                export.status.as_str()
            )));
        }
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        export.status = ExportStatus::Failed;
        export.failure_reason = reason;
        export.updated_at = Utc::now();
        state.exports.insert(export.id, export.clone());
        state.emit(
            "compliance.export.failed",
            json!({
                "export_id": export.id.to_string(),
                "requester_id": export.requester_id,
                "failure_reason": export.failure_reason,
            }),
        );
        Ok(export)
    })();
    match result {
        Ok(export) => ok(export),
        Err(error) => err(error, debug),
    }
}

async fn download_export(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let export = load_export(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &export.requester_id, &caller.actor)?;
        if export.status != ExportStatus::Ready {
            return Err(UnifiedError::conflict(format!(
                "This export is not ready for download (status is \"{}\")",
                export.status.as_str()
            )));
        }
        let now = Utc::now();
        let expires_at = export.download_expires_at.ok_or_else(|| {
            UnifiedError::conflict("This export has no download link attached")
        })?;
        if now >= expires_at {
            return Err(UnifiedError::conflict(
                "The download link for this export has expired; request a new export",
            ));
        }
        let (target, key) = match (&export.storage_target, &export.storage_key) {
            (Some(target), Some(key)) => (target.clone(), key.clone()),
            _ => {
                return Err(UnifiedError::conflict(
                    "This export has no download link attached",
                ))
            }
        };
        Ok(json!({
            "export_id": export.id.to_string(),
            "storage_target": target,
            "storage_key": key,
            "download_expires_at": expires_at.to_rfc3339(),
            "note": "Download the file through the storage module using this target and key before the link expires.",
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Deletion handlers
// ---------------------------------------------------------------------------

async fn list_deletions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let requester_filter = params.filters.get("requester_id").cloned();
        let mut items: Vec<Value> = state
            .deletions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|deletion| {
                if !is_manager && deletion.requester_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if deletion.status.as_str() != status {
                        return false;
                    }
                }
                if let Some(requester) = &requester_filter {
                    if &deletion.requester_id != requester {
                        return false;
                    }
                }
                true
            })
            .map(|deletion| deletion_view(&state, &deletion))
            .collect();
        items.sort_by(|a, b| {
            b.get("created_at")
                .and_then(Value::as_str)
                .cmp(&a.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn deletion_view(state: &ComplianceState, deletion: &DeletionRequest) -> Value {
    let mut value = serde_json::to_value(deletion).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let items: Vec<DeletionItem> = {
            let mut rows: Vec<DeletionItem> = state
                .deletion_items
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|item| item.deletion_request_id == deletion.id)
                .collect();
            rows.sort_by_key(|item| item.created_at);
            rows
        };
        map.insert(
            "items".to_string(),
            serde_json::to_value(items).unwrap_or(Value::Null),
        );
    }
    value
}

async fn create_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateDeletionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_REASON_LEN)?;
        if input.items.is_empty() || input.items.len() > 50 {
            return Err(UnifiedError::bad_request(
                "items must list between 1 and 50 per-module deletion items",
            ));
        }
        let mut seen_modules: Vec<String> = Vec::new();
        for item in &input.items {
            let module_id = require_text(&item.module_id, "items[].module_id", MAX_NAME_LEN)?;
            if seen_modules.contains(&module_id) {
                return Err(UnifiedError::bad_request(format!(
                    "module \"{module_id}\" is listed twice in items; use one item per module"
                )));
            }
            seen_modules.push(module_id);
            require_text(&item.subject, "items[].subject", MAX_DESCRIPTION_LEN)?;
        }
        if state.deletions.iter().any(|entry| {
            entry.value().requester_id == caller.actor
                && matches!(
                    entry.value().status,
                    DeletionStatus::Requested
                        | DeletionStatus::Approved
                        | DeletionStatus::Executing
                )
        }) {
            return Err(UnifiedError::conflict(
                "You already have an open deletion request; wait for it to finish first",
            ));
        }
        let now = Utc::now();
        let deletion = DeletionRequest {
            id: Uuid::now_v7(),
            requester_id: caller.actor.clone(),
            reason,
            status: DeletionStatus::Requested,
            approved_by: None,
            approved_at: None,
            executed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.deletions.insert(deletion.id, deletion.clone());
        for item in &input.items {
            let row = DeletionItem {
                id: Uuid::now_v7(),
                deletion_request_id: deletion.id,
                module_id: item.module_id.trim().to_string(),
                subject: item.subject.trim().to_string(),
                status: "pending".to_string(),
                detail: None,
                executed_at: None,
                created_at: now,
                updated_at: now,
            };
            state.deletion_items.insert(row.id, row);
        }
        state.emit(
            "compliance.deletion.requested",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "reason": deletion.reason,
                "items": input.items.iter().map(|item| json!({
                    "module": item.module_id,
                    "subject": item.subject,
                })).collect::<Vec<_>>(),
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let deletion = load_deletion(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &deletion.requester_id, &caller.actor)?;
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn approve_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut deletion = load_deletion(&state, &path.into_inner())?;
        if deletion.status != DeletionStatus::Requested {
            return Err(UnifiedError::conflict(format!(
                "Only a requested deletion can be approved (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        if deletion.requester_id == caller.actor {
            return Err(UnifiedError::forbidden(
                "A manager cannot approve their own deletion request; another manager must approve it",
            ));
        }
        let _reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        let now = Utc::now();
        deletion.status = DeletionStatus::Approved;
        deletion.approved_by = Some(caller.actor.clone());
        deletion.approved_at = Some(now);
        deletion.updated_at = now;
        state.deletions.insert(deletion.id, deletion.clone());
        state.emit(
            "compliance.deletion.approved",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "approved_by": caller.actor,
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn reject_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecisionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut deletion = load_deletion(&state, &path.into_inner())?;
        if deletion.status != DeletionStatus::Requested {
            return Err(UnifiedError::conflict(format!(
                "Only a requested deletion can be rejected (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        let reason = match &body.into_inner().reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        deletion.status = DeletionStatus::Rejected;
        deletion.updated_at = Utc::now();
        state.deletions.insert(deletion.id, deletion.clone());
        state.emit(
            "compliance.deletion.rejected",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "rejected_by": caller.actor,
                "reason": reason,
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn execute_deletion(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut deletion = load_deletion(&state, &path.into_inner())?;
        if deletion.status != DeletionStatus::Approved {
            return Err(UnifiedError::conflict(format!(
                "Only an approved deletion can be executed (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        // Dispatch execution to the owning modules. This module
        // never deletes their rows itself; each owning module
        // deletes its own rows when it answers the event and
        // confirms back through `compliance.deletion.item.executed`
        // (or the manual mark-executed endpoint).
        let items: Vec<DeletionItem> = {
            let mut rows: Vec<DeletionItem> = state
                .deletion_items
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|item| item.deletion_request_id == deletion.id)
                .collect();
            rows.sort_by_key(|item| item.created_at);
            rows
        };
        if items.is_empty() {
            return Err(UnifiedError::conflict(
                "This deletion request has no items to execute",
            ));
        }
        let now = Utc::now();
        for item in &items {
            let mut updated = item.clone();
            updated.status = "dispatched".to_string();
            updated.updated_at = now;
            state.deletion_items.insert(updated.id, updated);
            state.emit(
                "compliance.deletion.item.execute_requested",
                json!({
                    "deletion_id": deletion.id.to_string(),
                    "item_id": item.id.to_string(),
                    "module": item.module_id,
                    "subject": item.subject,
                    "requester_id": deletion.requester_id,
                }),
            );
        }
        deletion.status = DeletionStatus::Executing;
        deletion.updated_at = now;
        state.deletions.insert(deletion.id, deletion.clone());
        state.emit(
            "compliance.deletion.executing",
            json!({
                "deletion_id": deletion.id.to_string(),
                "requester_id": deletion.requester_id,
                "executed_by": caller.actor,
                "items_dispatched": items.len(),
            }),
        );
        Ok(deletion_view(&state, &deletion))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_deletion_items(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let deletion = load_deletion(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &deletion.requester_id, &caller.actor)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let module_filter = params.filters.get("module").cloned();
        let mut items: Vec<DeletionItem> = state
            .deletion_items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                if item.deletion_request_id != deletion.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &item.status != status {
                        return false;
                    }
                }
                if let Some(module) = &module_filter {
                    if &item.module_id != module {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|item| item.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn mark_item_executed(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<MarkItemExecutedRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DeletionItem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let (deletion_id, item_id) = path.into_inner();
        let deletion = load_deletion(&state, &deletion_id)?;
        if !matches!(
            deletion.status,
            DeletionStatus::Executing | DeletionStatus::Approved
        ) {
            return Err(UnifiedError::conflict(format!(
                "Deletion items can only be confirmed while the request is approved or executing (status is \"{}\")",
                deletion.status.as_str()
            )));
        }
        let item = load_deletion_item(&state, deletion.id, &item_id)?;
        if item.status == "executed" {
            return Err(UnifiedError::conflict("This deletion item is already executed"));
        }
        let detail = match &body.into_inner().detail {
            Some(raw) => Some(require_text(raw, "detail", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        Ok(confirm_item_executed(&state, &item, detail))
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Consent handlers (append-only)
// ---------------------------------------------------------------------------

async fn list_consents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let params = query.into_inner();
        let user_filter = params.filters.get("user_id").cloned();
        let purpose_filter = params.filters.get("purpose").cloned();
        let granted_filter = params
            .filters
            .get("granted")
            .and_then(|raw| raw.parse::<bool>().ok());
        let mut items: Vec<ConsentLog> = state
            .consents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|consent| {
                // A requester sees only their own consent history
                // unless they hold the manage capability.
                if !is_manager && consent.user_id != caller.actor {
                    return false;
                }
                if let Some(user) = &user_filter {
                    if &consent.user_id != user {
                        return false;
                    }
                }
                if let Some(purpose) = &purpose_filter {
                    if &consent.purpose != purpose {
                        return false;
                    }
                }
                if let Some(granted) = granted_filter {
                    if consent.granted != granted {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|consent| std::cmp::Reverse(consent.recorded_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn record_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<RecordConsentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let is_manager = can_manage(&ctx, &caller.role);
        let input = body.into_inner();
        let user_id = match &input.user_id {
            Some(raw) => {
                if !is_manager && raw != &caller.actor {
                    return Err(UnifiedError::forbidden(
                        "Recording consent for another user requires the manage capability",
                    ));
                }
                require_text(raw, "user_id", MAX_NAME_LEN)?
            }
            None => caller.actor.clone(),
        };
        let purpose = require_text(&input.purpose, "purpose", MAX_PURPOSE_LEN)?;
        let version = require_text(
            &input.consent_text_version,
            "consent_text_version",
            MAX_NAME_LEN,
        )?;
        let record = ConsentLog {
            id: Uuid::now_v7(),
            user_id: user_id.clone(),
            purpose: purpose.clone(),
            consent_text_version: version.clone(),
            granted: input.granted,
            recorded_at: Utc::now(),
        };
        state.consents.insert(record.id, record.clone());
        // Withdrawal is recorded the same way, as a new row with
        // granted=false; the old row is never overwritten.
        let event_name = if input.granted {
            "compliance.consent.recorded"
        } else {
            "compliance.consent.withdrawn"
        };
        state.emit(
            event_name,
            json!({
                "consent_id": record.id.to_string(),
                "user_id": user_id,
                "purpose": purpose,
                "consent_text_version": version,
                "granted": input.granted,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => created(record),
        Err(error) => err(error, debug),
    }
}

async fn get_consent(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ConsentLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let consent = load_consent(&state, &path.into_inner())?;
        check_own_or_manage(&ctx, &caller.role, &consent.user_id, &caller.actor)?;
        Ok(consent)
    })();
    match result {
        Ok(consent) => ok(consent),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Retention-policy handlers
// ---------------------------------------------------------------------------

async fn list_policies(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("data_category").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<RetentionPolicy> = state
            .retention_policies
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|policy| {
                if let Some(category) = &category_filter {
                    if &policy.data_category != category {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!(
                        "{} {} {}",
                        policy.data_category,
                        policy.review_action,
                        policy.description.clone().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|policy| policy.data_category.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateRetentionPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let data_category = require_text(&input.data_category, "data_category", MAX_CATEGORY_LEN)?;
        if state
            .retention_policies
            .iter()
            .any(|entry| entry.value().data_category == data_category)
        {
            return Err(UnifiedError::conflict(format!(
                "A retention policy already exists for data category \"{data_category}\""
            )));
        }
        if input.retain_days < 0 || input.retain_days > 365 * 100 {
            return Err(UnifiedError::bad_request(
                "retain_days must be between 0 and 36500",
            ));
        }
        let review_action = require_text(&input.review_action, "review_action", MAX_PURPOSE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let policy = RetentionPolicy {
            id: Uuid::now_v7(),
            data_category,
            retain_days: input.retain_days,
            review_action,
            description,
            created_at: now,
            updated_at: now,
        };
        state.retention_policies.insert(policy.id, policy.clone());
        state.emit(
            "compliance.retention.policy.created",
            json!({
                "policy_id": policy.id.to_string(),
                "data_category": policy.data_category,
                "retain_days": policy.retain_days,
            }),
        );
        Ok(policy)
    })();
    match result {
        Ok(policy) => created(policy),
        Err(error) => err(error, debug),
    }
}

async fn get_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        load_policy(&state, &path.into_inner())
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn update_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRetentionPolicyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionPolicy> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut policy = load_policy(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(data_category) = &input.data_category {
            let checked = require_text(data_category, "data_category", MAX_CATEGORY_LEN)?;
            if state.retention_policies.iter().any(|entry| {
                entry.value().data_category == checked && entry.value().id != policy.id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "A retention policy already exists for data category \"{checked}\""
                )));
            }
            policy.data_category = checked;
        }
        if let Some(retain_days) = input.retain_days {
            if retain_days < 0 || retain_days > 365 * 100 {
                return Err(UnifiedError::bad_request(
                    "retain_days must be between 0 and 36500",
                ));
            }
            policy.retain_days = retain_days;
        }
        if let Some(review_action) = &input.review_action {
            policy.review_action = require_text(review_action, "review_action", MAX_PURPOSE_LEN)?;
        }
        if let Some(description) = &input.description {
            policy.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        policy.updated_at = Utc::now();
        state.retention_policies.insert(policy.id, policy.clone());
        Ok(policy)
    })();
    match result {
        Ok(policy) => ok(policy),
        Err(error) => err(error, debug),
    }
}

async fn delete_policy(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let id = policy.id;
        state.retention_policies.remove(&id);
        Ok(json!({ "deleted": true, "policy_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Queue a retention review through `scheduler.enqueue` with kind
/// `compliance.retention.review`. The job only flags items for
/// review — it never deletes anything. See the scheduler note in
/// the module header: the kind is not in `KNOWN_KINDS` yet, so the
/// run records `scheduler_status: "declined"` today.
async fn request_policy_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RetentionReview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let policy = load_policy(&state, &path.into_inner())?;
        let now = Utc::now();
        let mut review = RetentionReview {
            id: Uuid::now_v7(),
            policy_id: policy.id,
            items_flagged: 0,
            notes: None,
            scheduler_status: "declined".to_string(),
            scheduler_job_id: None,
            reviewed_at: now,
            created_at: now,
            updated_at: now,
        };
        if let Some(bus) = state.bus() {
            let answer = bus.emit(
                "scheduler.enqueue",
                json!({
                    "kind": COMPLIANCE_RETENTION_JOB_KIND,
                    "payload": {
                        "policy_id": policy.id.to_string(),
                        "review_id": review.id.to_string(),
                        "data_category": policy.data_category,
                        "retain_days": policy.retain_days,
                    },
                }),
            );
            match answer.results.first() {
                Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
                    review.scheduler_job_id = result
                        .get("job_id")
                        .and_then(|value| value.as_str())
                        .map(str::to_string);
                    review.scheduler_status = "queued".to_string();
                }
                _ => {
                    review.scheduler_job_id = None;
                    review.scheduler_status = "declined".to_string();
                }
            }
        }
        state.retention_reviews.insert(review.id, review.clone());
        state.emit(
            "compliance.retention.review.requested",
            json!({
                "review_id": review.id.to_string(),
                "policy_id": policy.id.to_string(),
                "data_category": policy.data_category,
                "scheduler_status": review.scheduler_status,
            }),
        );
        Ok(review)
    })();
    match result {
        Ok(review) => created(review),
        Err(error) => err(error, debug),
    }
}

async fn list_retention_reviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let policy_filter = params
            .filters
            .get("policy_id")
            .and_then(|raw| Uuid::parse_str(raw).ok());
        let mut items: Vec<RetentionReview> = state
            .retention_reviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|review| {
                if let Some(policy_id) = policy_filter {
                    if review.policy_id != policy_id {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|review| std::cmp::Reverse(review.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Processing-record handlers (RoPA)
// ---------------------------------------------------------------------------

async fn list_processing_records(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let purpose_filter = params.filters.get("purpose").cloned();
        let basis_filter = params.filters.get("lawful_basis").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ProcessingRecord> = state
            .processing_records
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                if let Some(purpose) = &purpose_filter {
                    if &record.purpose != purpose {
                        return false;
                    }
                }
                if let Some(basis) = &basis_filter {
                    if &record.lawful_basis != basis {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!(
                        "{} {} {}",
                        record.purpose,
                        record.data_categories.join(" "),
                        record.recipients.join(" ")
                    )
                    .to_lowercase()
                    .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|record| record.purpose.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateProcessingRecordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProcessingRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let purpose = require_text(&input.purpose, "purpose", MAX_PURPOSE_LEN)?;
        let mut data_categories: Vec<String> = Vec::new();
        for raw in &input.data_categories {
            data_categories.push(require_text(raw, "data_categories entry", MAX_CATEGORY_LEN)?);
        }
        let mut recipients: Vec<String> = Vec::new();
        for raw in &input.recipients {
            recipients.push(require_text(raw, "recipients entry", MAX_NAME_LEN)?);
        }
        let retention = require_text(&input.retention, "retention", MAX_PURPOSE_LEN)?;
        let lawful_basis = require_text(&input.lawful_basis, "lawful_basis", MAX_BASIS_LEN)?;
        let owner = match &input.owner {
            Some(raw) => Some(require_text(raw, "owner", MAX_NAME_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let record = ProcessingRecord {
            id: Uuid::now_v7(),
            purpose,
            data_categories,
            recipients,
            retention,
            lawful_basis,
            owner,
            created_at: now,
            updated_at: now,
        };
        state.processing_records.insert(record.id, record.clone());
        state.emit(
            "compliance.processing_record.created",
            json!({
                "record_id": record.id.to_string(),
                "purpose": record.purpose,
                "lawful_basis": record.lawful_basis,
            }),
        );
        Ok(record)
    })();
    match result {
        Ok(record) => created(record),
        Err(error) => err(error, debug),
    }
}

async fn get_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProcessingRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        load_processing_record(&state, &path.into_inner())
    })();
    match result {
        Ok(record) => ok(record),
        Err(error) => err(error, debug),
    }
}

async fn update_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProcessingRecordRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProcessingRecord> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut record = load_processing_record(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(purpose) = &input.purpose {
            record.purpose = require_text(purpose, "purpose", MAX_PURPOSE_LEN)?;
        }
        if let Some(categories) = &input.data_categories {
            let mut checked: Vec<String> = Vec::new();
            for raw in categories {
                checked.push(require_text(raw, "data_categories entry", MAX_CATEGORY_LEN)?);
            }
            record.data_categories = checked;
        }
        if let Some(recipients) = &input.recipients {
            let mut checked: Vec<String> = Vec::new();
            for raw in recipients {
                checked.push(require_text(raw, "recipients entry", MAX_NAME_LEN)?);
            }
            record.recipients = checked;
        }
        if let Some(retention) = &input.retention {
            record.retention = require_text(retention, "retention", MAX_PURPOSE_LEN)?;
        }
        if let Some(basis) = &input.lawful_basis {
            record.lawful_basis = require_text(basis, "lawful_basis", MAX_BASIS_LEN)?;
        }
        if let Some(owner) = &input.owner {
            record.owner = match owner {
                Some(raw) => Some(require_text(raw, "owner", MAX_NAME_LEN)?),
                None => None,
            };
        }
        record.updated_at = Utc::now();
        state.processing_records.insert(record.id, record.clone());
        Ok(record)
    })();
    match result {
        Ok(record) => ok(record),
        Err(error) => err(error, debug),
    }
}

async fn delete_processing_record(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let record = load_processing_record(&state, &path.into_inner())?;
        let id = record.id;
        state.processing_records.remove(&id);
        Ok(json!({ "deleted": true, "record_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Checklist handlers
// ---------------------------------------------------------------------------

async fn list_checklists(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<ComplianceChecklist> = state
            .checklists
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                if let Some(status) = &status_filter {
                    if &item.status != status {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!(
                        "{} {}",
                        item.title,
                        item.description.clone().unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|item| item.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateChecklistRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ComplianceChecklist> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let status = match &input.status {
            Some(raw) => {
                let checked = raw.trim().to_lowercase();
                if !valid_checklist_status(&checked) {
                    return Err(UnifiedError::bad_request(
                        "status must be \"open\", \"in_progress\", or \"done\"",
                    ));
                }
                checked
            }
            None => "open".to_string(),
        };
        let (target, key) = match (&input.evidence_storage_target, &input.evidence_storage_key) {
            (Some(target), Some(key)) => (
                Some(require_text(target, "evidence_storage_target", MAX_STORAGE_KEY_LEN)?),
                Some(require_text(key, "evidence_storage_key", MAX_STORAGE_KEY_LEN)?),
            ),
            (None, None) => (None, None),
            _ => {
                return Err(UnifiedError::bad_request(
                    "evidence_storage_target and evidence_storage_key must be given together (storage module reference)",
                ))
            }
        };
        let now = Utc::now();
        let item = ComplianceChecklist {
            id: Uuid::now_v7(),
            title,
            description,
            status,
            evidence_storage_target: target,
            evidence_storage_key: key,
            due_date: input.due_date,
            created_at: now,
            updated_at: now,
        };
        state.checklists.insert(item.id, item.clone());
        state.emit(
            "compliance.checklist.created",
            json!({ "checklist_id": item.id.to_string(), "title": item.title }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

async fn get_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ComplianceChecklist> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        load_checklist(&state, &path.into_inner())
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

async fn update_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateChecklistRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ComplianceChecklist> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut item = load_checklist(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            item.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            item.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(status) = &input.status {
            let checked = status.trim().to_lowercase();
            if !valid_checklist_status(&checked) {
                return Err(UnifiedError::bad_request(
                    "status must be \"open\", \"in_progress\", or \"done\"",
                ));
            }
            item.status = checked;
        }
        if let Some(target) = &input.evidence_storage_target {
            item.evidence_storage_target = match target {
                Some(raw) => Some(require_text(raw, "evidence_storage_target", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(key) = &input.evidence_storage_key {
            item.evidence_storage_key = match key {
                Some(raw) => Some(require_text(raw, "evidence_storage_key", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if item.evidence_storage_key.is_some() != item.evidence_storage_target.is_some() {
            return Err(UnifiedError::bad_request(
                "evidence_storage_target and evidence_storage_key must be given together",
            ));
        }
        if let Some(due_date) = &input.due_date {
            item.due_date = due_date.clone();
        }
        item.updated_at = Utc::now();
        state.checklists.insert(item.id, item.clone());
        state.emit(
            "compliance.checklist.updated",
            json!({ "checklist_id": item.id.to_string(), "status": item.status }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => ok(item),
        Err(error) => err(error, debug),
    }
}

async fn delete_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let item = load_checklist(&state, &path.into_inner())?;
        let id = item.id;
        state.checklists.remove(&id);
        Ok(json!({ "deleted": true, "checklist_id": id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Breach-log handlers
// ---------------------------------------------------------------------------

async fn list_breaches(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let notification_filter = params.filters.get("notification_status").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<BreachLog> = state
            .breaches
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|breach| {
                if let Some(status) = &status_filter {
                    if &breach.status != status {
                        return false;
                    }
                }
                if let Some(notification) = &notification_filter {
                    if &breach.notification_status != notification {
                        return false;
                    }
                }
                if !search.is_empty()
                    && !format!("{} {} {}", breach.title, breach.description, breach.affected_scope)
                        .to_lowercase()
                        .contains(&search)
                {
                    return false;
                }
                true
            })
            .collect();
        items.sort_by_key(|breach| std::cmp::Reverse(breach.discovered_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_breach(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    body: web::Json<CreateBreachRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BreachLog> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let affected_scope = require_text(&input.affected_scope, "affected_scope", MAX_SCOPE_LEN)?;
        let now = Utc::now();
        let breach = BreachLog {
            id: Uuid::now_v7(),
            title,
            description,
            discovered_at: input.discovered_at,
            affected_scope,
            status: "reported".to_string(),
            notification_status: "not_started".to_string(),
            reported_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.breaches.insert(breach.id, breach.clone());
        // The first timeline entry records the report itself.
        let first_entry = BreachTimelineEntry {
            id: Uuid::now_v7(),
            breach_id: breach.id,
            author_id: caller.actor.clone(),
            entry: format!("Breach reported: {}", breach.title),
            recorded_at: now,
        };
        state.breach_timeline.insert(first_entry.id, first_entry);
        state.emit(
            "compliance.breach.reported",
            json!({
                "breach_id": breach.id.to_string(),
                "title": breach.title,
                "affected_scope": breach.affected_scope,
                "discovered_at": breach.discovered_at.to_rfc3339(),
                "reported_by": breach.reported_by,
            }),
        );
        if let Some(notify_user_id) = &input.notify_user_id {
            let target = require_text(notify_user_id, "notify_user_id", MAX_NAME_LEN)?;
            state.notify(
                "compliance.breach.reported",
                &target,
                &format!("Data breach reported: {}", breach.title),
                &format!(
                    "A data breach was reported affecting {}. See the compliance breach log.",
                    breach.affected_scope
                ),
                Some(format!("/api/v1/compliance/breaches/{}", breach.id)),
            );
        }
        Ok(breach)
    })();
    match result {
        Ok(breach) => created(breach),
        Err(error) => err(error, debug),
    }
}

async fn get_breach(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let breach = load_breach(&state, &path.into_inner())?;
        Ok(breach_view(&state, &breach))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn breach_view(state: &ComplianceState, breach: &BreachLog) -> Value {
    let mut value = serde_json::to_value(breach).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        let timeline: Vec<BreachTimelineEntry> = {
            let mut rows: Vec<BreachTimelineEntry> = state
                .breach_timeline
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|entry| entry.breach_id == breach.id)
                .collect();
            rows.sort_by_key(|entry| entry.recorded_at);
            rows
        };
        map.insert(
            "timeline".to_string(),
            serde_json::to_value(timeline).unwrap_or(Value::Null),
        );
    }
    value
}

async fn update_breach(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBreachRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let mut breach = load_breach(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            breach.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            breach.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(scope) = &input.affected_scope {
            breach.affected_scope = require_text(scope, "affected_scope", MAX_SCOPE_LEN)?;
        }
        if let Some(status) = &input.status {
            let checked = status.trim().to_lowercase();
            if !valid_breach_status(&checked) {
                return Err(UnifiedError::bad_request(
                    "status must be \"reported\", \"investigating\", \"contained\", \"notified\", or \"closed\"",
                ));
            }
            breach.status = checked;
        }
        if let Some(notification) = &input.notification_status {
            let checked = notification.trim().to_lowercase();
            if !valid_notification_status(&checked) {
                return Err(UnifiedError::bad_request(
                    "notification_status must be \"not_started\", \"in_progress\", or \"notified\"",
                ));
            }
            breach.notification_status = checked;
        }
        breach.updated_at = Utc::now();
        state.breaches.insert(breach.id, breach.clone());
        state.emit(
            "compliance.breach.updated",
            json!({
                "breach_id": breach.id.to_string(),
                "status": breach.status,
                "notification_status": breach.notification_status,
            }),
        );
        Ok(breach_view(&state, &breach))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_breach_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddTimelineRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<BreachTimelineEntry> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_MANAGE)?;
        let breach = load_breach(&state, &path.into_inner())?;
        let entry_text = require_text(&body.into_inner().entry, "entry", MAX_DESCRIPTION_LEN)?;
        let entry = BreachTimelineEntry {
            id: Uuid::now_v7(),
            breach_id: breach.id,
            author_id: caller.actor.clone(),
            entry: entry_text,
            recorded_at: Utc::now(),
        };
        state.breach_timeline.insert(entry.id, entry.clone());
        Ok(entry)
    })();
    match result {
        Ok(entry) => created(entry),
        Err(error) => err(error, debug),
    }
}

async fn list_breach_timeline(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ComplianceState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_COMPLIANCE_VIEW)?;
        let breach = load_breach(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<BreachTimelineEntry> = state
            .breach_timeline
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.breach_id == breach.id)
            .collect();
        items.sort_by_key(|entry| entry.recorded_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ComplianceModule {
    state: ComplianceState,
}

impl ComplianceModule {
    pub fn new() -> Self {
        Self {
            state: ComplianceState::default(),
        }
    }
}

impl Default for ComplianceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ComplianceModule {
    fn id(&self) -> &'static str {
        "compliance"
    }

    fn name(&self) -> &'static str {
        "Compliance Center"
    }

    fn description(&self) -> &'static str {
        "GDPR / ISO compliance: data export requests with expiring storage links, deletion requests with a manager approval step and per-module audit items executed by the owning modules, an append-only consent log, retention policies enforced by a flag-only scheduler job, processing-activity records (RoPA), checklists with storage-backed evidence, and a breach log with an append-only timeline."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "notifications", "scheduler", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["users", "roles", "notifications", "scheduler", "storage"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("communication")
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
            DELETION_ITEM_EXECUTED,
            "compliance",
            Arc::new(move |ctx: &mut HookContext| {
                on_deletion_item_executed(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            RETENTION_REVIEW_REQUESTED,
            "compliance",
            Arc::new(move |ctx: &mut HookContext| {
                on_retention_review_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/compliance")
                .app_data(state)
                .route("/exports", web::get().to(list_exports))
                .route("/exports", web::post().to(create_export))
                .route("/exports/{id}", web::get().to(get_export))
                .route("/exports/{id}/start", web::post().to(start_export))
                .route("/exports/{id}/complete", web::post().to(complete_export))
                .route("/exports/{id}/fail", web::post().to(fail_export))
                .route("/exports/{id}/download", web::get().to(download_export))
                .route("/deletions", web::get().to(list_deletions))
                .route("/deletions", web::post().to(create_deletion))
                .route("/deletions/{id}", web::get().to(get_deletion))
                .route("/deletions/{id}/approve", web::post().to(approve_deletion))
                .route("/deletions/{id}/reject", web::post().to(reject_deletion))
                .route("/deletions/{id}/execute", web::post().to(execute_deletion))
                .route("/deletions/{id}/items", web::get().to(list_deletion_items))
                .route(
                    "/deletions/{id}/items/{item_id}/mark-executed",
                    web::post().to(mark_item_executed),
                )
                .route("/consents", web::get().to(list_consents))
                .route("/consents", web::post().to(record_consent))
                .route("/consents/{id}", web::get().to(get_consent))
                .route("/retention-policies", web::get().to(list_policies))
                .route("/retention-policies", web::post().to(create_policy))
                .route("/retention-policies/{id}", web::get().to(get_policy))
                .route("/retention-policies/{id}", web::patch().to(update_policy))
                .route("/retention-policies/{id}", web::delete().to(delete_policy))
                .route(
                    "/retention-policies/{id}/review",
                    web::post().to(request_policy_review),
                )
                .route("/retention-reviews", web::get().to(list_retention_reviews))
                .route("/processing-records", web::get().to(list_processing_records))
                .route(
                    "/processing-records",
                    web::post().to(create_processing_record),
                )
                .route(
                    "/processing-records/{id}",
                    web::get().to(get_processing_record),
                )
                .route(
                    "/processing-records/{id}",
                    web::patch().to(update_processing_record),
                )
                .route(
                    "/processing-records/{id}",
                    web::delete().to(delete_processing_record),
                )
                .route("/checklists", web::get().to(list_checklists))
                .route("/checklists", web::post().to(create_checklist))
                .route("/checklists/{id}", web::get().to(get_checklist))
                .route("/checklists/{id}", web::patch().to(update_checklist))
                .route("/checklists/{id}", web::delete().to(delete_checklist))
                .route("/breaches", web::get().to(list_breaches))
                .route("/breaches", web::post().to(create_breach))
                .route("/breaches/{id}", web::get().to(get_breach))
                .route("/breaches/{id}", web::patch().to(update_breach))
                .route(
                    "/breaches/{id}/timeline",
                    web::post().to(add_breach_timeline),
                )
                .route(
                    "/breaches/{id}/timeline",
                    web::get().to(list_breach_timeline),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/compliance/exports", owner: "compliance", description: "List export requests (a requester sees only their own unless they hold the manage capability)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports", owner: "compliance", description: "Request a data export (one open request per requester)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/exports/{id}", owner: "compliance", description: "Get one export request (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports/{id}/start", owner: "compliance", description: "Mark an export as processing (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports/{id}/complete", owner: "compliance", description: "Attach the storage link with expiry to a processing export (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/exports/{id}/fail", owner: "compliance", description: "Mark an export as failed with a reason (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/exports/{id}/download", owner: "compliance", description: "Return the storage link of a ready export while it is still valid (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/deletions", owner: "compliance", description: "List deletion requests with their per-module items (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions", owner: "compliance", description: "Request a deletion with per-module items (one open request per requester)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/deletions/{id}", owner: "compliance", description: "Get one deletion request with its items (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/approve", owner: "compliance", description: "Approve a deletion request (manager, never the requester themselves)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/reject", owner: "compliance", description: "Reject a deletion request (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/execute", owner: "compliance", description: "Dispatch execution to the owning modules (manager, only after approval)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/deletions/{id}/items", owner: "compliance", description: "List the per-module items of a deletion request." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/deletions/{id}/items/{item_id}/mark-executed", owner: "compliance", description: "Confirm one deletion item executed with an audit detail (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/consents", owner: "compliance", description: "List consent records (own history only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/consents", owner: "compliance", description: "Record consent or a withdrawal (append-only; withdrawal is a new row)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/consents/{id}", owner: "compliance", description: "Get one consent record (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/retention-policies", owner: "compliance", description: "List retention policies (paginated, filter by data_category)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/retention-policies", owner: "compliance", description: "Create a retention policy (one per data category, manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/retention-policies/{id}", owner: "compliance", description: "Get one retention policy." },
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/retention-policies/{id}", owner: "compliance", description: "Update a retention policy (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/compliance/retention-policies/{id}", owner: "compliance", description: "Delete a retention policy (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/retention-policies/{id}/review", owner: "compliance", description: "Enqueue a retention review job (flags items, never deletes; manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/retention-reviews", owner: "compliance", description: "List retention review runs and their flag counts." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/processing-records", owner: "compliance", description: "List processing-activity records (RoPA)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/processing-records", owner: "compliance", description: "Create a processing-activity record (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/processing-records/{id}", owner: "compliance", description: "Get one processing-activity record." },
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/processing-records/{id}", owner: "compliance", description: "Update a processing-activity record (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/compliance/processing-records/{id}", owner: "compliance", description: "Delete a processing-activity record (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/checklists", owner: "compliance", description: "List compliance checklists (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/checklists", owner: "compliance", description: "Create a checklist item with optional storage-backed evidence (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/checklists/{id}", owner: "compliance", description: "Get one checklist item." },
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/checklists/{id}", owner: "compliance", description: "Update a checklist item (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/compliance/checklists/{id}", owner: "compliance", description: "Delete a checklist item (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/breaches", owner: "compliance", description: "List breach log entries (paginated, filter by status and notification_status)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/breaches", owner: "compliance", description: "Report a breach (manager; optionally notifies one user)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/breaches/{id}", owner: "compliance", description: "Get one breach with its timeline." },
            RouteInfo { method: "PATCH", path: "/api/v1/compliance/breaches/{id}", owner: "compliance", description: "Update a breach status or notification status (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/compliance/breaches/{id}/timeline", owner: "compliance", description: "Append a timeline entry to a breach (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/compliance/breaches/{id}/timeline", owner: "compliance", description: "List the timeline entries of a breach." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ComplianceModule::new()));
}
