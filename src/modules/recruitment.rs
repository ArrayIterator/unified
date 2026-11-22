// Recruitment / ATS module — job postings, applicants with a
// validated hiring pipeline, interviews, per-interviewer
// evaluations, offers, and the hire handoff to HR onboarding.
//
// Routes live under `/api/v1/recruitment`:
//
//   Jobs:
//     GET/POST /jobs — list jobs / create a job.
//     GET/PATCH/DELETE /jobs/{id}.
//     POST /jobs/{id}/publish, /jobs/{id}/close.
//     GET  /jobs/{id}/applicants — applicants of a job.
//     POST /jobs/{id}/applicants — add an applicant to a job.
//   Applicants:
//     GET  /applicants/{id} — one applicant, with the aggregated
//          evaluation score.
//     PATCH/DELETE /applicants/{id}.
//     POST /applicants/{id}/move — validated stage transition
//          (audited in the stage history).
//     GET  /applicants/{id}/history — stage history.
//   Interviews:
//     GET/POST /applicants/{id}/interviews.
//     PATCH /interviews/{id}.
//     POST /interviews/{id}/outcome — record the outcome.
//   Evaluations:
//     GET/POST /applicants/{id}/evaluations — per-interviewer
//          scorecards; the read side aggregates the score.
//   Offers:
//     GET/POST /applicants/{id}/offers.
//     GET  /offers/{id}.
//     PATCH /offers/{id}.
//     POST /offers/{id}/send, /accept, /decline, /expire.
//   Hiring:
//     POST /applicants/{id}/hire — hire; emits the HR onboarding
//          handoff event (no HR record is created here).
//   Public (no auth; see the public-access note below):
//     GET  /public/jobs — published jobs only.
//     GET  /public/jobs/{id} — one published job.
//     POST /public/jobs/{id}/apply — apply for a published job
//          (consent flag required in the body).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory job, applicant, stage-history, interview, evaluation,
// and offer management; the pipeline state machine
// (`applied` -> `screening` -> `interview` -> `offer` ->
// `hired`, with `rejected` reachable from any open stage and
// terminal afterwards) with every transition written to the
// stage history; per-interviewer scorecards aggregated on read
// (average score plus per-criterion averages — no score is ever
// invented for an interviewer who did not submit one); offer
// lifecycle (`draft` -> `sent` -> `accepted` / `declined` /
// `expired`); interview scheduling through `scheduler.enqueue`
// with a `recruitment.interview.reminder_requested` listener
// that notifies the interviewers. What is NOT built here: HR
// records — hiring emits `hr.onboarding.started` for the HR
// module and creates nothing on the HR side; resume bytes — the
// resume lives in the storage module and only its target + key
// are stored here as metadata; actual email / push delivery
// (the notifications module owns delivery — this module only
// emits `notification.requested`); and any check that a
// `department_id` names a real department — the Departments
// module owns departments and this module stores the id as a
// plain reference, exactly like the events module stores
// `company_id`. Consent: an applicant with
// `consent_contact: false` cannot be contacted — interview
// scheduling and offer sending both refuse with 409 until
// consent is recorded with a timestamp.
//
// Public-access gate: the API Manager module enforces public access
// through its admin-managed `ApiAccessRule` records (default deny:
// an endpoint is public only when a rule exists with
// `public_access: true`, and denied outright when a rule exists
// with `allowed: false`). There is no reusable per-request
// "is this endpoint public?" helper to call from a handler — the
// check lives in the API Manager middleware / `resolve_public`
// path — so the public routes below rely on that gate: an
// administrator must create the matching `ApiAccessRule` rows for
// `GET /api/v1/recruitment/public/*` and for
// `POST /api/v1/recruitment/public/jobs/{id}/apply`. The routes
// are registered in `route_info()` with
// `route_version_support()` defaulting to `All`, so the API
// Manager's per-version registry picks them up. Do NOT treat the
// absence of a capability check on a public route as an open
// door: without the rule the API Manager denies the call. Public
// routes expose published jobs only, and never expose applicant
// data, evaluations, or offers.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the ai module. Capability checks use the
// `role.check` hook and fail closed (no answer means not allowed).
//
// Events emitted on the hook bus:
//   - `recruitment.job.created`, `recruitment.job.updated`,
//     `recruitment.job.deleted`, `recruitment.job.published`,
//     `recruitment.job.closed`
//   - `recruitment.applicant.created`, `recruitment.applicant.updated`,
//     `recruitment.applicant.deleted`, `recruitment.applicant.moved`
//   - `recruitment.interview.scheduled`,
//     `recruitment.interview.updated`,
//     `recruitment.interview.outcome_recorded`
//   - `recruitment.evaluation.submitted`
//   - `recruitment.offer.created`, `recruitment.offer.updated`,
//     `recruitment.offer.sent`, `recruitment.offer.accepted`,
//     `recruitment.offer.declined`, `recruitment.offer.expired`
//   - `recruitment.hired`
//   - `hr.onboarding.started` (hire handoff for the HR module:
//     `{"applicant_id", "job_id", "user_id", "name", "email",
//     "start_date", "salary", "currency", "department_id"}` —
//     the HR module owns the onboarding record)
//   - `notification.requested` (interview reminders to the
//     interviewers) with the notifications module's payload
//     shape: `{"kind", "user_id", "title", "body", "module":
//     "recruitment", "link", "priority", "channels"}`.
//   - `scheduler.enqueue` with `{"kind":
//     "recruitment.interview.reminder", "payload":
//     {"interview_id", "applicant_id", ...}}` when an interview is
//     scheduled.
// Events this module listens to:
//   - `recruitment.interview.reminder_requested` — the event the
//     scheduler emits when a `recruitment.interview.reminder` job
//     fires, following its naming rule (`backup.run` ->
//     `backup.run_requested`); notifies the interviewers through
//     `notification.requested`.
//
// Scheduler note: `recruitment.interview.reminder` is NOT in the
// scheduler's `KNOWN_KINDS` list yet, so today the scheduler
// answers `{"queued": false, "error": "Unknown job kind: ..."}` and
// the interview row records `scheduler_status: "declined"` instead
// of pretending it was scheduled. The interview is still stored
// and stays usable; the follow-up is to add
// `"recruitment.interview.reminder"` to `KNOWN_KINDS` in
// `scheduler.rs` and arm the dispatch so the job emits
// `recruitment.interview.reminder_requested` with the interview
// payload (see the integrator snippet in the module report).

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
const MAX_DESCRIPTION_LEN: usize = 10000;
const MAX_LOCATION_LEN: usize = 200;
const MAX_NAME_LEN: usize = 200;
const MAX_EMAIL_LEN: usize = 254;
const MAX_NOTES_LEN: usize = 5000;
const MAX_REASON_LEN: usize = 1000;
const MAX_SOURCE_LEN: usize = 120;
const MAX_STORAGE_KEY_LEN: usize = 512;
const MAX_CRITERION_LEN: usize = 120;
/// Interview score range (inclusive) and evaluation score range.
const MIN_SCORE: i64 = 1;
const MAX_SCORE: i64 = 5;

/// Capability names checked through the `role.check` hook.
const PERM_RECRUITMENT_VIEW: &str = "recruitment.view";
const PERM_RECRUITMENT_MANAGE: &str = "recruitment.manage";

/// Hook event names this module listens to.
const INTERVIEW_REMINDER_REQUESTED: &str = "recruitment.interview.reminder_requested";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Lifecycle of a job posting: `draft` -> `published` -> `closed`.
/// A closed job never reopens; a new posting is created instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Draft,
    Published,
    Closed,
}

impl Default for JobStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl JobStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Published => "published",
            Self::Closed => "closed",
        }
    }
}

/// Employment type of a job posting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    FullTime,
    PartTime,
    Contract,
    Internship,
    Freelance,
}

impl Default for JobType {
    fn default() -> Self {
        Self::FullTime
    }
}

impl JobType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::FullTime => "full_time",
            Self::PartTime => "part_time",
            Self::Contract => "contract",
            Self::Internship => "internship",
            Self::Freelance => "freelance",
        }
    }
}

/// One job posting. Money (`salary_min` / `salary_max`) is
/// NUMERIC(19,4) in the migration, validated non-negative here and
/// rounded to 4 decimal places on write. `department_id` is a plain
/// reference to the Departments module — its existence is not
/// verified here (see the honesty note).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Job {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub department_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub job_type: JobType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salary_min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub salary_max: Option<f64>,
    pub currency: String,
    pub status: JobStatus,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Pipeline stage of an applicant. The allowed transitions are
/// enforced by `allowed_stage_transition` (forward only, with
/// `rejected` reachable from any open stage and terminal after
/// that).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplicantStage {
    Applied,
    Screening,
    Interview,
    Offer,
    Hired,
    Rejected,
}

impl Default for ApplicantStage {
    fn default() -> Self {
        Self::Applied
    }
}

impl ApplicantStage {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Applied => "applied",
            Self::Screening => "screening",
            Self::Interview => "interview",
            Self::Offer => "offer",
            Self::Hired => "hired",
            Self::Rejected => "rejected",
        }
    }
}

/// One applicant for one job. `resume_storage_target` /
/// `resume_storage_key` are metadata only: the resume bytes live
/// in the storage module. `consent_contact` gates every outbound
/// contact (interviews, offers); `consent_at` records when the
/// consent was given. `stage` is the live pipeline stage; the
/// full history lives in `ApplicantStageHistory`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Applicant {
    pub id: Uuid,
    pub job_id: Uuid,
    pub name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phone: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_storage_target: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_storage_key: Option<String>,
    pub consent_contact: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent_at: Option<DateTime<Utc>>,
    pub stage: ApplicantStage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One audited pipeline move. Rows are append-only: a move never
/// rewrites the applicant's past, it appends a record with the
/// previous stage, the new stage, the actor, and the reason.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicantStageHistory {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_stage: Option<ApplicantStage>,
    pub to_stage: ApplicantStage,
    pub moved_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Interview type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InterviewType {
    Phone,
    Video,
    Onsite,
    Panel,
}

impl Default for InterviewType {
    fn default() -> Self {
        Self::Video
    }
}

impl InterviewType {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Phone => "phone",
            Self::Video => "video",
            Self::Onsite => "onsite",
            Self::Panel => "panel",
        }
    }
}

/// One scheduled interview. `interviewers` holds user ids;
/// `outcome` is `None` until recorded (`passed`, `failed`, or
/// `no_show`). `scheduler_status` is `not_scheduled`, `queued`,
/// or `declined` (see the scheduler note in the module header).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Interview {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub scheduled_at: DateTime<Utc>,
    pub interview_type: InterviewType,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interviewers: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome_notes: Option<String>,
    /// `scheduled`, `completed`, `cancelled`.
    pub status: String,
    pub scheduler_status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One score on one criterion of an evaluation. A scorecard holds
/// one or more of these; the criterion name is free text so each
/// hiring team can use its own rubric.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvaluationScore {
    pub criterion: String,
    pub score: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// One per-interviewer evaluation (scorecard) of an applicant.
/// The aggregated score is computed on read from every scorecard
/// of the applicant — see `applicant_view` / `aggregate_scores`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evaluation {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub interviewer_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interview_id: Option<Uuid>,
    pub scores: Vec<EvaluationScore>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Lifecycle of an offer: `draft` -> `sent` -> `accepted` /
/// `declined` / `expired`. Only a `sent` offer can be accepted,
/// declined, or expired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Draft,
    Sent,
    Accepted,
    Declined,
    Expired,
}

impl Default for OfferStatus {
    fn default() -> Self {
        Self::Draft
    }
}

impl OfferStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Sent => "sent",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Expired => "expired",
        }
    }
}

/// One job offer. Money (`salary`) is NUMERIC(19,4) in the
/// migration, validated non-negative here and rounded to 4 decimal
/// places on write.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Offer {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub salary: f64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<String>,
    pub status: OfferStatus,
    pub created_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sent_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateJobRequest {
    pub title: String,
    pub description: Option<String>,
    pub department_id: Option<String>,
    pub location: Option<String>,
    pub job_type: Option<JobType>,
    pub salary_min: Option<f64>,
    pub salary_max: Option<f64>,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateJobRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub department_id: Option<Option<String>>,
    pub location: Option<Option<String>>,
    pub job_type: Option<JobType>,
    pub salary_min: Option<Option<f64>>,
    pub salary_max: Option<Option<f64>>,
    pub currency: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateApplicantRequest {
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub resume_storage_target: Option<String>,
    pub resume_storage_key: Option<String>,
    pub consent_contact: Option<bool>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateApplicantRequest {
    pub name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<Option<String>>,
    pub source: Option<Option<String>>,
    pub resume_storage_target: Option<Option<String>>,
    pub resume_storage_key: Option<Option<String>>,
    pub consent_contact: Option<bool>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct MoveApplicantRequest {
    pub to_stage: ApplicantStage,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateInterviewRequest {
    pub scheduled_at: DateTime<Utc>,
    pub interview_type: Option<InterviewType>,
    #[serde(default)]
    pub interviewers: Vec<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateInterviewRequest {
    pub scheduled_at: Option<DateTime<Utc>>,
    pub interview_type: Option<InterviewType>,
    pub interviewers: Option<Vec<String>>,
    pub notes: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct InterviewOutcomeRequest {
    pub outcome: String,
    pub outcome_notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateEvaluationRequest {
    pub interviewer_id: String,
    pub interview_id: Option<Uuid>,
    pub scores: Vec<EvaluationScore>,
    pub notes: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateOfferRequest {
    pub salary: f64,
    pub currency: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub terms: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateOfferRequest {
    pub salary: Option<f64>,
    pub currency: Option<String>,
    pub start_date: Option<Option<NaiveDate>>,
    pub terms: Option<Option<String>>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for recruitment. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000051_recruitment`; until that migration is
/// executed, recruitment data does not survive a restart.
#[derive(Clone)]
pub struct RecruitmentState {
    pub jobs: Arc<DashMap<Uuid, Job>>,
    pub applicants: Arc<DashMap<Uuid, Applicant>>,
    pub stage_history: Arc<DashMap<Uuid, ApplicantStageHistory>>,
    pub interviews: Arc<DashMap<Uuid, Interview>>,
    pub evaluations: Arc<DashMap<Uuid, Evaluation>>,
    pub offers: Arc<DashMap<Uuid, Offer>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for RecruitmentState {
    fn default() -> Self {
        Self {
            jobs: Arc::new(DashMap::new()),
            applicants: Arc::new(DashMap::new()),
            stage_history: Arc::new(DashMap::new()),
            interviews: Arc::new(DashMap::new()),
            evaluations: Arc::new(DashMap::new()),
            offers: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl RecruitmentState {
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
    /// `module: "recruitment"`, `link`, `priority`, `channels`.
    /// See `notifications.rs` `on_notification_requested` and
    /// `resolve_recipients`: a payload needs `user_id`, `user_ids`,
    /// or `audience`.
    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "recruitment",
                "category": "recruitment",
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
/// for capability checks), exactly like the ai module.
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
/// (same fail-closed shape as the ai module's capability check).
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

/// Money: finite, non-negative, rounded to 4 decimal places.
fn valid_price(price: f64) -> UnifiedResult<f64> {
    if !price.is_finite() || price < 0.0 {
        return Err(UnifiedError::bad_request("price must be a finite number >= 0"));
    }
    Ok((price * 10_000.0).round() / 10_000.0)
}

fn round_money(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Currency: 3-letter ISO code, uppercased.
fn valid_currency(currency: &str) -> UnifiedResult<String> {
    let upper = currency.trim().to_uppercase();
    if upper.len() != 3 || !upper.chars().all(|c| c.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code such as \"USD\"",
        ));
    }
    Ok(upper)
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > MAX_EMAIL_LEN || email.contains(' ') {
        return false;
    }
    let mut parts = email.split('@');
    let (Some(local), Some(domain)) = (parts.next(), parts.next()) else {
        return false;
    };
    if parts.next().is_some() || local.is_empty() || domain.is_empty() {
        return false;
    }
    domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.')
}

/// The pipeline state machine. Forward only:
/// `applied` -> `screening` -> `interview` -> `offer` -> `hired`,
/// with `rejected` reachable from any open stage. `hired` and
/// `rejected` are terminal — a rejected applicant is never moved
/// back, and a hired applicant never re-enters the pipeline.
fn allowed_stage_transition(from: ApplicantStage, to: ApplicantStage) -> bool {
    if from == to {
        return false;
    }
    match from {
        ApplicantStage::Applied => matches!(
            to,
            ApplicantStage::Screening | ApplicantStage::Rejected
        ),
        ApplicantStage::Screening => matches!(
            to,
            ApplicantStage::Interview | ApplicantStage::Rejected
        ),
        ApplicantStage::Interview => {
            matches!(to, ApplicantStage::Offer | ApplicantStage::Rejected)
        }
        ApplicantStage::Offer => {
            matches!(to, ApplicantStage::Hired | ApplicantStage::Rejected)
        }
        ApplicantStage::Hired | ApplicantStage::Rejected => false,
    }
}

fn load_job(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Job> {
    let id = parse_uuid(raw_id, "job")?;
    state
        .jobs
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Job not found: {id}")))
}

fn load_applicant(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Applicant> {
    let id = parse_uuid(raw_id, "applicant")?;
    state
        .applicants
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Applicant not found: {id}")))
}

fn load_interview(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Interview> {
    let id = parse_uuid(raw_id, "interview")?;
    state
        .interviews
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Interview not found: {id}")))
}

fn load_offer(state: &RecruitmentState, raw_id: &str) -> UnifiedResult<Offer> {
    let id = parse_uuid(raw_id, "offer")?;
    state
        .offers
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Offer not found: {id}")))
}

fn validate_salary_range(
    salary_min: Option<f64>,
    salary_max: Option<f64>,
) -> UnifiedResult<(Option<f64>, Option<f64>)> {
    let min = match salary_min {
        Some(value) => Some(valid_price(value)?),
        None => None,
    };
    let max = match salary_max {
        Some(value) => Some(valid_price(value)?),
        None => None,
    };
    if let (Some(min_value), Some(max_value)) = (min, max) {
        if max_value < min_value {
            return Err(UnifiedError::bad_request(
                "salary_max must not be below salary_min",
            ));
        }
    }
    Ok((min, max))
}

fn validate_scores(scores: &[EvaluationScore]) -> UnifiedResult<()> {
    if scores.is_empty() || scores.len() > 20 {
        return Err(UnifiedError::bad_request(
            "An evaluation needs between 1 and 20 criterion scores",
        ));
    }
    let mut seen: Vec<String> = Vec::new();
    for entry in scores {
        let criterion = entry.criterion.trim();
        if criterion.is_empty() || criterion.len() > MAX_CRITERION_LEN {
            return Err(UnifiedError::bad_request(
                "Each score criterion must not be empty and must be at most 120 characters",
            ));
        }
        if !(MIN_SCORE..=MAX_SCORE).contains(&entry.score) {
            return Err(UnifiedError::bad_request(format!(
                "Each score must be between {MIN_SCORE} and {MAX_SCORE}"
            )));
        }
        if entry.note.as_ref().is_some_and(|note| note.len() > MAX_NOTES_LEN) {
            return Err(UnifiedError::bad_request(
                "Each score note must be at most 5000 characters",
            ));
        }
        if seen.iter().any(|item| item == criterion) {
            return Err(UnifiedError::bad_request(format!(
                "Criterion \"{criterion}\" is scored twice in one evaluation"
            )));
        }
        seen.push(criterion.to_string());
    }
    Ok(())
}

/// Aggregate every evaluation of one applicant: the overall
/// average across all scores, the number of scorecards, and the
/// per-criterion averages. Returns `None` for the average when no
/// evaluation exists — no score is ever invented for an
/// interviewer who did not submit one.
fn aggregate_scores(state: &RecruitmentState, applicant_id: Uuid) -> Value {
    let evaluations: Vec<Evaluation> = state
        .evaluations
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|evaluation| evaluation.applicant_id == applicant_id)
        .collect();
    let mut total: i64 = 0;
    let mut count: i64 = 0;
    let mut by_criterion: Vec<(String, i64, i64)> = Vec::new();
    for evaluation in &evaluations {
        for score in &evaluation.scores {
            total += score.score;
            count += 1;
            match by_criterion
                .iter_mut()
                .find(|(criterion, _, _)| criterion == &score.criterion)
            {
                Some((_, sum, n)) => {
                    *sum += score.score;
                    *n += 1;
                }
                None => by_criterion.push((score.criterion.clone(), score.score, 1)),
            }
        }
    }
    let mut criteria: Vec<Value> = by_criterion
        .iter()
        .map(|(criterion, sum, n)| {
            json!({
                "criterion": criterion,
                "average": (*sum as f64) / (*n as f64),
                "scores": n,
            })
        })
        .collect();
    criteria.sort_by(|a, b| {
        a.get("criterion")
            .and_then(Value::as_str)
            .cmp(&b.get("criterion").and_then(Value::as_str))
    });
    json!({
        "evaluations": evaluations.len(),
        "average_score": if count > 0 { Some((total as f64) / (count as f64)) } else { None },
        "scores_total": count,
        "criteria": criteria,
    })
}

/// Applicant read view: the applicant plus its aggregated
/// evaluation score, so a list never needs a second request to
/// rank candidates.
fn applicant_view(state: &RecruitmentState, applicant: &Applicant) -> Value {
    let mut value = serde_json::to_value(applicant).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "evaluation_summary".to_string(),
            aggregate_scores(state, applicant.id),
        );
    }
    value
}

/// Append one stage-history row. The history is append-only; the
/// applicant row itself carries only the live stage.
fn record_stage_move(
    state: &RecruitmentState,
    applicant: &Applicant,
    from_stage: Option<ApplicantStage>,
    to_stage: ApplicantStage,
    moved_by: &str,
    reason: Option<String>,
) {
    let entry = ApplicantStageHistory {
        id: Uuid::now_v7(),
        applicant_id: applicant.id,
        job_id: applicant.job_id,
        from_stage,
        to_stage,
        moved_by: moved_by.to_string(),
        reason,
        created_at: Utc::now(),
    };
    state.stage_history.insert(entry.id, entry);
}

/// Refuse any outbound contact with an applicant who has not
/// consented. Consent is recorded with a timestamp; withdrawing
/// it (setting `consent_contact` to false) blocks interviews and
/// offers again until consent is given anew.
fn require_consent(applicant: &Applicant, action: &str) -> UnifiedResult<()> {
    if applicant.consent_contact {
        Ok(())
    } else {
        Err(UnifiedError::conflict(format!(
            "Applicant has not consented to be contacted, so {action} is refused"
        )))
    }
}

/// Queue one interview reminder through `scheduler.enqueue`.
/// Mirrors the events module's `schedule_reminder`: the scheduler
/// answers `{"queued": true, "job_id": ...}` when it knows the
/// kind, and `{"queued": false, "error": "Unknown job kind: ..."}`
/// today, because `recruitment.interview.reminder` is not in
/// `KNOWN_KINDS` yet. The interview is stored either way; the
/// row records `scheduler_status` honestly (`queued` /
/// `declined`).
fn schedule_interview_reminder(state: &RecruitmentState, interview: &mut Interview) {
    let Some(bus) = state.bus() else {
        interview.scheduler_job_id = None;
        interview.scheduler_status = "declined".to_string();
        return;
    };
    let answer = bus.emit(
        "scheduler.enqueue",
        json!({
            "kind": "recruitment.interview.reminder",
            "payload": {
                "interview_id": interview.id.to_string(),
                "applicant_id": interview.applicant_id.to_string(),
                "job_id": interview.job_id.to_string(),
                "scheduled_at": interview.scheduled_at.to_rfc3339(),
                "interviewers": interview.interviewers,
            },
        }),
    );
    match answer.results.first() {
        Some(result) if result.get("queued").and_then(Value::as_bool) == Some(true) => {
            interview.scheduler_job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .map(str::to_string);
            interview.scheduler_status = "queued".to_string();
        }
        _ => {
            interview.scheduler_job_id = None;
            interview.scheduler_status = "declined".to_string();
        }
    }
}

fn on_interview_reminder_requested(state: RecruitmentState, ctx: &mut HookContext) {
    let interview_id = ctx
        .data
        .get("interview_id")
        .and_then(|value| value.as_str())
        .and_then(|raw| Uuid::parse_str(raw).ok());
    let Some(interview_id) = interview_id else {
        ctx.add_result(json!({ "sent": false, "error": "recruitment.interview.reminder_requested requires 'interview_id'" }));
        return;
    };
    let Some(interview) = state
        .interviews
        .get(&interview_id)
        .map(|entry| entry.value().clone())
    else {
        ctx.add_result(json!({ "sent": false, "interview_id": interview_id.to_string(), "error": "Interview not found" }));
        return;
    };
    if interview.status != "scheduled" {
        ctx.add_result(json!({
            "sent": false,
            "interview_id": interview_id.to_string(),
            "reason": format!("Interview is {}", interview.status),
        }));
        return;
    }
    let applicant = state
        .applicants
        .get(&interview.applicant_id)
        .map(|entry| entry.value().clone());
    let applicant_name = applicant
        .as_ref()
        .map(|item| item.name.clone())
        .unwrap_or_default();
    let title = format!("Interview reminder: {applicant_name}");
    let body = format!(
        "Interview with {applicant_name} is scheduled for {} ({}).",
        interview.scheduled_at.to_rfc3339(),
        interview.interview_type.as_str()
    );
    let link = format!(
        "/api/v1/recruitment/applicants/{}/interviews",
        interview.applicant_id
    );
    let mut notified = 0;
    for interviewer in &interview.interviewers {
        state.notify(
            "recruitment.interview.reminder",
            interviewer,
            &title,
            &body,
            Some(link.clone()),
        );
        notified += 1;
    }
    ctx.add_result(json!({
        "sent": true,
        "interview_id": interview_id.to_string(),
        "recipients": notified,
    }));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

/// The Recruitment / ATS module.
pub struct RecruitmentModule {
    state: RecruitmentState,
}

impl RecruitmentModule {
    pub fn new() -> Self {
        Self {
            state: RecruitmentState::default(),
        }
    }
}

impl Default for RecruitmentModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RecruitmentModule {
    fn id(&self) -> &'static str {
        "recruitment"
    }

    fn name(&self) -> &'static str {
        "Recruitment / ATS"
    }

    fn description(&self) -> &'static str {
        "Job postings with a public listing, applicants with a validated hiring pipeline, interviews, per-interviewer evaluations, offers, and the hire handoff to HR onboarding."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["users", "roles", "departments", "notifications", "scheduler", "storage"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec![
            "users",
            "roles",
            "departments",
            "notifications",
            "scheduler",
            "storage",
        ]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["actix-web", "dashmap", "serde", "chrono", "uuid"]
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
            INTERVIEW_REMINDER_REQUESTED,
            "recruitment",
            Arc::new(move |ctx: &mut HookContext| {
                on_interview_reminder_requested(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/recruitment")
                .app_data(state)
                .route("/public/jobs", web::get().to(list_public_jobs))
                .route("/public/jobs/{id}", web::get().to(get_public_job))
                .route("/public/jobs/{id}/apply", web::post().to(apply_public))
                .route("/jobs", web::get().to(list_jobs))
                .route("/jobs", web::post().to(create_job))
                .route("/jobs/{id}", web::get().to(get_job))
                .route("/jobs/{id}", web::patch().to(update_job))
                .route("/jobs/{id}", web::delete().to(delete_job))
                .route("/jobs/{id}/publish", web::post().to(publish_job))
                .route("/jobs/{id}/close", web::post().to(close_job))
                .route("/jobs/{id}/applicants", web::get().to(list_job_applicants))
                .route("/jobs/{id}/applicants", web::post().to(create_applicant))
                .route("/applicants/{id}", web::get().to(get_applicant))
                .route("/applicants/{id}", web::patch().to(update_applicant))
                .route("/applicants/{id}", web::delete().to(delete_applicant))
                .route("/applicants/{id}/move", web::post().to(move_applicant))
                .route("/applicants/{id}/history", web::get().to(list_stage_history))
                .route("/applicants/{id}/interviews", web::get().to(list_interviews))
                .route("/applicants/{id}/interviews", web::post().to(create_interview))
                .route("/applicants/{id}/evaluations", web::get().to(list_evaluations))
                .route(
                    "/applicants/{id}/evaluations",
                    web::post().to(create_evaluation),
                )
                .route("/applicants/{id}/offers", web::get().to(list_offers))
                .route("/applicants/{id}/offers", web::post().to(create_offer))
                .route("/applicants/{id}/hire", web::post().to(hire_applicant))
                .route("/interviews/{id}", web::patch().to(update_interview))
                .route("/interviews/{id}/outcome", web::post().to(record_outcome))
                .route("/offers/{id}", web::get().to(get_offer))
                .route("/offers/{id}", web::patch().to(update_offer))
                .route("/offers/{id}/send", web::post().to(send_offer))
                .route("/offers/{id}/accept", web::post().to(accept_offer))
                .route("/offers/{id}/decline", web::post().to(decline_offer))
                .route("/offers/{id}/expire", web::post().to(expire_offer)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/recruitment/jobs", owner: "recruitment", description: "List jobs (paginated, filter by status, type, department_id, location)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs", owner: "recruitment", description: "Create a job posting (draft)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/jobs/{id}", owner: "recruitment", description: "Get one job." },
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/jobs/{id}", owner: "recruitment", description: "Update a job (draft or published only)." },
            RouteInfo { method: "DELETE", path: "/api/v1/recruitment/jobs/{id}", owner: "recruitment", description: "Delete a draft job with no applicants." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs/{id}/publish", owner: "recruitment", description: "Publish a draft job (emits recruitment.job.published)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs/{id}/close", owner: "recruitment", description: "Close a published job (emits recruitment.job.closed)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/jobs/{id}/applicants", owner: "recruitment", description: "List applicants of a job (paginated, filter by stage, source)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/jobs/{id}/applicants", owner: "recruitment", description: "Add an applicant to a job (consent flag recorded with a timestamp)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}", owner: "recruitment", description: "Get one applicant with the aggregated evaluation score." },
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/applicants/{id}", owner: "recruitment", description: "Update an applicant (consent changes re-stamp the consent time)." },
            RouteInfo { method: "DELETE", path: "/api/v1/recruitment/applicants/{id}", owner: "recruitment", description: "Delete an applicant (not after hiring)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/move", owner: "recruitment", description: "Move an applicant to the next pipeline stage (validated, audited)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/history", owner: "recruitment", description: "Stage history of an applicant (append-only, paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/interviews", owner: "recruitment", description: "List interviews of an applicant." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/interviews", owner: "recruitment", description: "Schedule an interview (consent required; reminder via scheduler.enqueue)." },
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/interviews/{id}", owner: "recruitment", description: "Update an interview (scheduled interviews only)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/interviews/{id}/outcome", owner: "recruitment", description: "Record an interview outcome (passed/failed/no_show)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/evaluations", owner: "recruitment", description: "List evaluations of an applicant with the aggregated score." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/evaluations", owner: "recruitment", description: "Submit a per-interviewer scorecard (one per interviewer per interview)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/applicants/{id}/offers", owner: "recruitment", description: "List offers of an applicant." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/offers", owner: "recruitment", description: "Create an offer for an applicant (draft)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/offers/{id}", owner: "recruitment", description: "Get one offer." },
            RouteInfo { method: "PATCH", path: "/api/v1/recruitment/offers/{id}", owner: "recruitment", description: "Update a draft offer." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/send", owner: "recruitment", description: "Send an offer (consent required)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/accept", owner: "recruitment", description: "Accept a sent offer (emits recruitment.offer.accepted)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/decline", owner: "recruitment", description: "Decline a sent offer." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/offers/{id}/expire", owner: "recruitment", description: "Expire a sent offer." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/applicants/{id}/hire", owner: "recruitment", description: "Hire an applicant (accepted offer required; emits recruitment.hired and hr.onboarding.started)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/public/jobs", owner: "recruitment", description: "PUBLIC: list published jobs (no auth; gated by the API Manager ApiAccessRule for this path)." },
            RouteInfo { method: "GET", path: "/api/v1/recruitment/public/jobs/{id}", owner: "recruitment", description: "PUBLIC: one published job (no auth; gated by the API Manager ApiAccessRule)." },
            RouteInfo { method: "POST", path: "/api/v1/recruitment/public/jobs/{id}/apply", owner: "recruitment", description: "PUBLIC: apply for a published job (no auth; gated by the API Manager ApiAccessRule; consent required)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RecruitmentModule::new()));
}

// ---------------------------------------------------------------------------
// Job handlers
// ---------------------------------------------------------------------------

fn public_job_view(job: &Job) -> Value {
    json!({
        "id": job.id,
        "title": job.title,
        "description": job.description,
        "department_id": job.department_id,
        "location": job.location,
        "job_type": job.job_type.as_str(),
        "salary_min": job.salary_min,
        "salary_max": job.salary_max,
        "currency": job.currency,
        "status": job.status.as_str(),
        "created_at": job.created_at,
        "updated_at": job.updated_at,
    })
}

async fn list_jobs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let type_filter = params.filters.get("type").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let location_filter = params.filters.get("location").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Job> = state
            .jobs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|job| {
                if let Some(status) = &status_filter {
                    if job.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                if let Some(job_type) = &type_filter {
                    if job.job_type.as_str() != job_type.as_str() {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if job.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if job.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        job.title,
                        job.description.clone().unwrap_or_default(),
                        job.location.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .collect();
        let sort = params.allowed_sort(&["title", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
                _ => a.created_at.cmp(&b.created_at),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    body: web::Json<CreateJobRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = match &input.description {
            Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let department_id = match &input.department_id {
            Some(raw) => Some(require_text(raw, "department_id", MAX_NAME_LEN)?),
            None => None,
        };
        let location = match &input.location {
            Some(raw) => Some(require_text(raw, "location", MAX_LOCATION_LEN)?),
            None => None,
        };
        let (salary_min, salary_max) =
            validate_salary_range(input.salary_min, input.salary_max)?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let now = Utc::now();
        let job = Job {
            id: Uuid::now_v7(),
            title,
            description,
            department_id,
            location,
            job_type: input.job_type.unwrap_or_default(),
            salary_min,
            salary_max,
            currency,
            status: JobStatus::Draft,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.created",
            json!({
                "job_id": job.id.to_string(),
                "title": job.title,
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        load_job(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateJobRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut job = load_job(&state, &path.into_inner())?;
        if job.status == JobStatus::Closed {
            return Err(UnifiedError::conflict("A closed job cannot be edited"));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.title {
            job.title = require_text(raw, "title", MAX_TITLE_LEN)?;
        }
        if let Some(raw) = &input.description {
            job.description = match raw {
                Some(value) => Some(require_text(value, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.department_id {
            job.department_id = match raw {
                Some(value) => Some(require_text(value, "department_id", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.location {
            job.location = match raw {
                Some(value) => Some(require_text(value, "location", MAX_LOCATION_LEN)?),
                None => None,
            };
        }
        if let Some(job_type) = input.job_type {
            job.job_type = job_type;
        }
        if input.salary_min.is_some() || input.salary_max.is_some() {
            let next_min = input.salary_min.unwrap_or(job.salary_min);
            let next_max = input.salary_max.unwrap_or(job.salary_max);
            let (salary_min, salary_max) = validate_salary_range(next_min, next_max)?;
            job.salary_min = salary_min;
            job.salary_max = salary_max;
        }
        if let Some(raw) = &input.currency {
            job.currency = valid_currency(raw)?;
        }
        job.updated_at = Utc::now();
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.updated",
            json!({
                "job_id": job.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Draft {
            return Err(UnifiedError::conflict(
                "Only a draft job can be deleted; close a published job instead",
            ));
        }
        if state
            .applicants
            .iter()
            .any(|entry| entry.value().job_id == job.id)
        {
            return Err(UnifiedError::conflict(
                "Job has applicants and cannot be deleted",
            ));
        }
        state.jobs.remove(&job.id);
        state.emit(
            "recruitment.job.deleted",
            json!({
                "job_id": job.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn publish_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft job can be published (job is {})",
                job.status.as_str()
            )));
        }
        job.status = JobStatus::Published;
        job.updated_at = Utc::now();
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.published",
            json!({
                "job_id": job.id.to_string(),
                "title": job.title,
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn close_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Job> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Published {
            return Err(UnifiedError::conflict(format!(
                "Only a published job can be closed (job is {})",
                job.status.as_str()
            )));
        }
        job.status = JobStatus::Closed;
        job.updated_at = Utc::now();
        state.jobs.insert(job.id, job.clone());
        state.emit(
            "recruitment.job.closed",
            json!({
                "job_id": job.id.to_string(),
                "title": job.title,
                "actor": caller.actor,
            }),
        );
        Ok(job)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Public job handlers (no auth; gated by the API Manager
// ApiAccessRule for these paths — see the module header).
// ---------------------------------------------------------------------------

async fn list_public_jobs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let type_filter = params.filters.get("type").cloned();
        let department_filter = params.filters.get("department_id").cloned();
        let location_filter = params.filters.get("location").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .jobs
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|job| job.status == JobStatus::Published)
            .filter(|job| {
                if let Some(job_type) = &type_filter {
                    if job.job_type.as_str() != job_type.as_str() {
                        return false;
                    }
                }
                if let Some(department) = &department_filter {
                    if job.department_id.as_deref() != Some(department.as_str()) {
                        return false;
                    }
                }
                if let Some(location) = &location_filter {
                    if job.location.as_deref() != Some(location.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {} {}",
                        job.title,
                        job.description.clone().unwrap_or_default(),
                        job.location.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|job| public_job_view(&job))
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

async fn get_public_job(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Published {
            return Err(UnifiedError::not_found(format!("Job not found: {}", job.id)));
        }
        Ok(public_job_view(&job))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Build an applicant from a request body, shared by the internal
/// create endpoint and the public apply endpoint. Consent is
/// required for a public application: without it the application
/// is refused, because an applicant who has not consented cannot
/// be contacted at all.
fn build_applicant(
    state: &RecruitmentState,
    job: &Job,
    input: CreateApplicantRequest,
    public_apply: bool,
) -> UnifiedResult<Applicant> {
    let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
    let email = input.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(UnifiedError::bad_request("email must be a valid address"));
    }
    let phone = match &input.phone {
        Some(raw) => Some(require_text(raw, "phone", MAX_NAME_LEN)?),
        None => None,
    };
    let source = match &input.source {
        Some(raw) => Some(require_text(raw, "source", MAX_SOURCE_LEN)?),
        None => None,
    };
    let resume_storage_target = match &input.resume_storage_target {
        Some(raw) => Some(require_text(raw, "resume_storage_target", MAX_STORAGE_KEY_LEN)?),
        None => None,
    };
    let resume_storage_key = match &input.resume_storage_key {
        Some(raw) => Some(require_text(raw, "resume_storage_key", MAX_STORAGE_KEY_LEN)?),
        None => None,
    };
    if resume_storage_target.is_some() != resume_storage_key.is_some() {
        return Err(UnifiedError::bad_request(
            "resume_storage_target and resume_storage_key must be given together",
        ));
    }
    let consent_contact = input.consent_contact.unwrap_or(false);
    if public_apply && !consent_contact {
        return Err(UnifiedError::bad_request(
            "Applying requires consent_contact: true so the applicant can be contacted",
        ));
    }
    let notes = match &input.notes {
        Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
        None => None,
    };
    if state.applicants.iter().any(|entry| {
        entry.value().job_id == job.id && entry.value().email == email
    }) {
        return Err(UnifiedError::conflict(format!(
            "An application from \"{email}\" already exists for this job"
        )));
    }
    let now = Utc::now();
    let applicant = Applicant {
        id: Uuid::now_v7(),
        job_id: job.id,
        name,
        email,
        phone,
        source,
        resume_storage_target,
        resume_storage_key,
        consent_contact,
        consent_at: if consent_contact { Some(now) } else { None },
        stage: ApplicantStage::Applied,
        notes,
        created_at: now,
        updated_at: now,
    };
    record_stage_move(
        state,
        &applicant,
        None,
        ApplicantStage::Applied,
        "system",
        None,
    );
    Ok(applicant)
}

async fn apply_public(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    path: web::Path<String>,
    body: web::Json<CreateApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let job = load_job(&state, &path.into_inner())?;
        if job.status != JobStatus::Published {
            return Err(UnifiedError::not_found(format!("Job not found: {}", job.id)));
        }
        let applicant = build_applicant(&state, &job, body.into_inner(), true)?;
        state.applicants.insert(applicant.id, applicant.clone());
        state.emit(
            "recruitment.applicant.created",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": job.id.to_string(),
                "source": "public_apply",
            }),
        );
        Ok(json!({
            "applicant_id": applicant.id,
            "job_id": job.id,
            "stage": applicant.stage.as_str(),
            "message": "Application received",
        }))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Applicant handlers
// ---------------------------------------------------------------------------

async fn list_job_applicants(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let job = load_job(&state, &path.into_inner())?;
        let params = query.into_inner();
        let stage_filter = params.filters.get("stage").cloned();
        let source_filter = params.filters.get("source").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .applicants
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|applicant| {
                if applicant.job_id != job.id {
                    return false;
                }
                if let Some(stage) = &stage_filter {
                    if applicant.stage.as_str() != stage.as_str() {
                        return false;
                    }
                }
                if let Some(source) = &source_filter {
                    if applicant.source.as_deref() != Some(source.as_str()) {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!("{} {}", applicant.name, applicant.email).to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|applicant| applicant_view(&state, &applicant))
            .collect();
        let sort = params.allowed_sort(&["name", "created_at"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "name" => a
                    .get("name")
                    .and_then(Value::as_str)
                    .cmp(&b.get("name").and_then(Value::as_str)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let job = load_job(&state, &path.into_inner())?;
        if job.status == JobStatus::Closed {
            return Err(UnifiedError::conflict("A closed job takes no new applicants"));
        }
        let applicant = build_applicant(&state, &job, body.into_inner(), false)?;
        state.applicants.insert(applicant.id, applicant.clone());
        state.emit(
            "recruitment.applicant.created",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": job.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut applicant = load_applicant(&state, &path.into_inner())?;
        if applicant.stage == ApplicantStage::Hired {
            return Err(UnifiedError::conflict("A hired applicant cannot be edited here"));
        }
        let input = body.into_inner();
        if let Some(raw) = &input.name {
            applicant.name = require_text(raw, "name", MAX_NAME_LEN)?;
        }
        if let Some(raw) = &input.email {
            let email = raw.trim().to_lowercase();
            if !valid_email(&email) {
                return Err(UnifiedError::bad_request("email must be a valid address"));
            }
            if state.applicants.iter().any(|entry| {
                entry.value().job_id == applicant.job_id
                    && entry.value().email == email
                    && entry.value().id != applicant.id
            }) {
                return Err(UnifiedError::conflict(format!(
                    "An application from \"{email}\" already exists for this job"
                )));
            }
            applicant.email = email;
        }
        if let Some(raw) = &input.phone {
            applicant.phone = match raw {
                Some(value) => Some(require_text(value, "phone", MAX_NAME_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.source {
            applicant.source = match raw {
                Some(value) => Some(require_text(value, "source", MAX_SOURCE_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.resume_storage_target {
            applicant.resume_storage_target = match raw {
                Some(value) => Some(require_text(value, "resume_storage_target", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if let Some(raw) = &input.resume_storage_key {
            applicant.resume_storage_key = match raw {
                Some(value) => Some(require_text(value, "resume_storage_key", MAX_STORAGE_KEY_LEN)?),
                None => None,
            };
        }
        if applicant.resume_storage_target.is_some() != applicant.resume_storage_key.is_some() {
            return Err(UnifiedError::bad_request(
                "resume_storage_target and resume_storage_key must be given together",
            ));
        }
        if let Some(consent) = input.consent_contact {
            applicant.consent_contact = consent;
            applicant.consent_at = if consent { Some(Utc::now()) } else { None };
        }
        if let Some(raw) = &input.notes {
            applicant.notes = match raw {
                Some(value) => Some(require_text(value, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        applicant.updated_at = Utc::now();
        state.applicants.insert(applicant.id, applicant.clone());
        state.emit(
            "recruitment.applicant.updated",
            json!({
                "applicant_id": applicant.id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn delete_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        if applicant.stage == ApplicantStage::Hired {
            return Err(UnifiedError::conflict(
                "A hired applicant cannot be deleted; the HR module owns the hired record",
            ));
        }
        if state
            .interviews
            .iter()
            .any(|entry| entry.value().applicant_id == applicant.id)
            || state
                .evaluations
                .iter()
                .any(|entry| entry.value().applicant_id == applicant.id)
            || state
                .offers
                .iter()
                .any(|entry| entry.value().applicant_id == applicant.id)
        {
            return Err(UnifiedError::conflict(
                "Applicant has interviews, evaluations, or offers and cannot be deleted; reject them instead",
            ));
        }
        state.applicants.remove(&applicant.id);
        state.emit(
            "recruitment.applicant.deleted",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn move_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MoveApplicantRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut applicant = load_applicant(&state, &path.into_inner())?;
        let input = body.into_inner();
        let reason = match &input.reason {
            Some(raw) => Some(require_text(raw, "reason", MAX_REASON_LEN)?),
            None => None,
        };
        if !allowed_stage_transition(applicant.stage, input.to_stage) {
            return Err(UnifiedError::conflict(format!(
                "Stage cannot move from {} to {}",
                applicant.stage.as_str(),
                input.to_stage.as_str()
            )));
        }
        if input.to_stage == ApplicantStage::Hired {
            return Err(UnifiedError::conflict(
                "Hiring goes through POST /applicants/{id}/hire, which requires an accepted offer"
            ));
        }
        let from_stage = applicant.stage;
        applicant.stage = input.to_stage;
        applicant.updated_at = Utc::now();
        state.applicants.insert(applicant.id, applicant.clone());
        record_stage_move(
            &state,
            &applicant,
            Some(from_stage),
            input.to_stage,
            &caller.actor,
            reason,
        );
        state.emit(
            "recruitment.applicant.moved",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "from_stage": from_stage.as_str(),
                "to_stage": input.to_stage.as_str(),
                "actor": caller.actor,
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_stage_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<ApplicantStageHistory> = state
            .stage_history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.applicant_id == applicant.id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Interview handlers
// ---------------------------------------------------------------------------

async fn list_interviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Interview> = state
            .interviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|interview| {
                if interview.applicant_id != applicant.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &interview.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.scheduled_at.cmp(&b.scheduled_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_interview(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateInterviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Interview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        require_consent(&applicant, "scheduling an interview")?;
        if !matches!(
            applicant.stage,
            ApplicantStage::Screening | ApplicantStage::Interview
        ) {
            return Err(UnifiedError::conflict(format!(
                "Interviews can only be scheduled while the applicant is in screening or interview (now: {})",
                applicant.stage.as_str()
            )));
        }
        let input = body.into_inner();
        if input.scheduled_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "scheduled_at must be in the future",
            ));
        }
        let mut interviewers: Vec<String> = Vec::new();
        for raw in &input.interviewers {
            let interviewer = require_text(raw, "interviewers[]", MAX_NAME_LEN)?;
            if !interviewers.contains(&interviewer) {
                interviewers.push(interviewer);
            }
        }
        if interviewers.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one interviewer is required",
            ));
        }
        if interviewers.len() > 20 {
            return Err(UnifiedError::bad_request(
                "At most 20 interviewers can be listed",
            ));
        }
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let mut interview = Interview {
            id: Uuid::now_v7(),
            applicant_id: applicant.id,
            job_id: applicant.job_id,
            scheduled_at: input.scheduled_at,
            interview_type: input.interview_type.unwrap_or_default(),
            interviewers,
            notes,
            outcome: None,
            outcome_notes: None,
            status: "scheduled".to_string(),
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        schedule_interview_reminder(&state, &mut interview);
        state.interviews.insert(interview.id, interview.clone());
        state.emit(
            "recruitment.interview.scheduled",
            json!({
                "interview_id": interview.id.to_string(),
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "scheduled_at": interview.scheduled_at.to_rfc3339(),
                "scheduler_status": interview.scheduler_status,
                "actor": caller.actor,
            }),
        );
        Ok(interview)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn update_interview(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInterviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Interview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut interview = load_interview(&state, &path.into_inner())?;
        if interview.status != "scheduled" {
            return Err(UnifiedError::conflict(format!(
                "Only a scheduled interview can be updated (interview is {})",
                interview.status
            )));
        }
        let input = body.into_inner();
        if let Some(scheduled_at) = input.scheduled_at {
            if scheduled_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "scheduled_at must be in the future",
                ));
            }
            interview.scheduled_at = scheduled_at;
        }
        if let Some(interview_type) = input.interview_type {
            interview.interview_type = interview_type;
        }
        if let Some(raw_list) = input.interviewers {
            let mut interviewers: Vec<String> = Vec::new();
            for raw in &raw_list {
                let interviewer = require_text(raw, "interviewers[]", MAX_NAME_LEN)?;
                if !interviewers.contains(&interviewer) {
                    interviewers.push(interviewer);
                }
            }
            if interviewers.is_empty() {
                return Err(UnifiedError::bad_request(
                    "At least one interviewer is required",
                ));
            }
            if interviewers.len() > 20 {
                return Err(UnifiedError::bad_request(
                    "At most 20 interviewers can be listed",
                ));
            }
            interview.interviewers = interviewers;
        }
        if let Some(raw) = &input.notes {
            interview.notes = match raw {
                Some(value) => Some(require_text(value, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        interview.updated_at = Utc::now();
        state.interviews.insert(interview.id, interview.clone());
        state.emit(
            "recruitment.interview.updated",
            json!({
                "interview_id": interview.id.to_string(),
                "applicant_id": interview.applicant_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(interview)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn record_outcome(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<InterviewOutcomeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Interview> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut interview = load_interview(&state, &path.into_inner())?;
        if interview.status != "scheduled" {
            return Err(UnifiedError::conflict(format!(
                "Only a scheduled interview can record an outcome (interview is {})",
                interview.status
            )));
        }
        let input = body.into_inner();
        let outcome = input.outcome.trim().to_lowercase();
        if !matches!(outcome.as_str(), "passed" | "failed" | "no_show") {
            return Err(UnifiedError::bad_request(
                "outcome must be \"passed\", \"failed\", or \"no_show\"",
            ));
        }
        let outcome_notes = match &input.outcome_notes {
            Some(raw) => Some(require_text(raw, "outcome_notes", MAX_NOTES_LEN)?),
            None => None,
        };
        interview.outcome = Some(outcome.clone());
        interview.outcome_notes = outcome_notes;
        interview.status = "completed".to_string();
        interview.updated_at = Utc::now();
        state.interviews.insert(interview.id, interview.clone());
        state.emit(
            "recruitment.interview.outcome_recorded",
            json!({
                "interview_id": interview.id.to_string(),
                "applicant_id": interview.applicant_id.to_string(),
                "outcome": outcome,
                "actor": caller.actor,
            }),
        );
        Ok(interview)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Evaluation handlers
// ---------------------------------------------------------------------------

async fn list_evaluations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let interviewer_filter = params.filters.get("interviewer_id").cloned();
        let mut items: Vec<Evaluation> = state
            .evaluations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|evaluation| {
                if evaluation.applicant_id != applicant.id {
                    return false;
                }
                if let Some(interviewer) = &interviewer_filter {
                    if &evaluation.interviewer_id != interviewer {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let page = paginate(items, &params);
        Ok(json!({
            "items": page.items,
            "pagination": page.pagination,
            "aggregate": aggregate_scores(&state, applicant.id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_evaluation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateEvaluationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Evaluation> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let input = body.into_inner();
        let interviewer_id = require_text(&input.interviewer_id, "interviewer_id", MAX_NAME_LEN)?;
        validate_scores(&input.scores)?;
        if let Some(interview_id) = input.interview_id {
            let interview = state
                .interviews
                .get(&interview_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Interview not found: {interview_id}"))
                })?;
            if interview.applicant_id != applicant.id {
                return Err(UnifiedError::bad_request(
                    "interview_id belongs to a different applicant",
                ));
            }
            if state.evaluations.iter().any(|entry| {
                entry.value().interview_id == Some(interview_id)
                    && entry.value().interviewer_id == interviewer_id
            }) {
                return Err(UnifiedError::conflict(
                    "This interviewer already submitted an evaluation for this interview",
                ));
            }
        }
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let evaluation = Evaluation {
            id: Uuid::now_v7(),
            applicant_id: applicant.id,
            job_id: applicant.job_id,
            interviewer_id: interviewer_id.clone(),
            interview_id: input.interview_id,
            scores: input.scores,
            notes,
            created_at: now,
            updated_at: now,
        };
        state.evaluations.insert(evaluation.id, evaluation.clone());
        state.emit(
            "recruitment.evaluation.submitted",
            json!({
                "evaluation_id": evaluation.id.to_string(),
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "interviewer_id": interviewer_id,
                "actor": caller.actor,
            }),
        );
        Ok(evaluation)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Offer handlers
// ---------------------------------------------------------------------------

async fn list_offers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Offer> = state
            .offers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|offer| {
                if offer.applicant_id != applicant.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if offer.status.as_str() != status.as_str() {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateOfferRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        if !matches!(applicant.stage, ApplicantStage::Interview | ApplicantStage::Offer) {
            return Err(UnifiedError::conflict(format!(
                "Offers can only be created while the applicant is in interview or offer (now: {})",
                applicant.stage.as_str()
            )));
        }
        let input = body.into_inner();
        let salary = valid_price(input.salary)?;
        let currency = match &input.currency {
            Some(raw) => valid_currency(raw)?,
            None => "USD".to_string(),
        };
        let terms = match &input.terms {
            Some(raw) => Some(require_text(raw, "terms", MAX_DESCRIPTION_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let offer = Offer {
            id: Uuid::now_v7(),
            applicant_id: applicant.id,
            job_id: applicant.job_id,
            salary,
            currency,
            start_date: input.start_date,
            terms,
            status: OfferStatus::Draft,
            created_by: caller.actor.clone(),
            sent_at: None,
            decided_at: None,
            created_at: now,
            updated_at: now,
        };
        state.offers.insert(offer.id, offer.clone());
        state.emit(
            "recruitment.offer.created",
            json!({
                "offer_id": offer.id.to_string(),
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(offer)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

async fn get_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        load_offer(&state, &path.into_inner())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateOfferRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        if offer.status != OfferStatus::Draft {
            return Err(UnifiedError::conflict(format!(
                "Only a draft offer can be updated (offer is {})",
                offer.status.as_str()
            )));
        }
        let input = body.into_inner();
        if let Some(salary) = input.salary {
            offer.salary = valid_price(salary)?;
        }
        if let Some(raw) = &input.currency {
            offer.currency = valid_currency(raw)?;
        }
        if let Some(raw) = &input.start_date {
            offer.start_date = *raw;
        }
        if let Some(raw) = &input.terms {
            offer.terms = match raw {
                Some(value) => Some(require_text(value, "terms", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        offer.updated_at = Utc::now();
        state.offers.insert(offer.id, offer.clone());
        state.emit(
            "recruitment.offer.updated",
            json!({
                "offer_id": offer.id.to_string(),
                "applicant_id": offer.applicant_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Shared offer decision: send / accept / decline / expire. Only
/// the transitions listed in the error message are allowed; send
/// additionally requires the applicant's contact consent.
fn transition_offer(
    state: &RecruitmentState,
    offer: &mut Offer,
    from: OfferStatus,
    to: OfferStatus,
    event: &str,
    actor: &str,
) -> UnifiedResult<()> {
    if offer.status != from {
        return Err(UnifiedError::conflict(format!(
            "Offer is {} and cannot move to {}",
            offer.status.as_str(),
            to.as_str()
        )));
    }
    if to == OfferStatus::Sent {
        let applicant = state
            .applicants
            .get(&offer.applicant_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| {
                UnifiedError::not_found(format!("Applicant not found: {}", offer.applicant_id))
            })?;
        require_consent(&applicant, "sending an offer")?;
        offer.sent_at = Some(Utc::now());
    }
    if matches!(
        to,
        OfferStatus::Accepted | OfferStatus::Declined | OfferStatus::Expired
    ) {
        offer.decided_at = Some(Utc::now());
    }
    offer.status = to;
    offer.updated_at = Utc::now();
    state.offers.insert(offer.id, offer.clone());
    state.emit(
        event,
        json!({
            "offer_id": offer.id.to_string(),
            "applicant_id": offer.applicant_id.to_string(),
            "job_id": offer.job_id.to_string(),
            "status": to.as_str(),
            "actor": actor,
        }),
    );
    Ok(())
}

async fn send_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Draft,
            OfferStatus::Sent,
            "recruitment.offer.sent",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn accept_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Sent,
            OfferStatus::Accepted,
            "recruitment.offer.accepted",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn decline_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Sent,
            OfferStatus::Declined,
            "recruitment.offer.declined",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn expire_offer(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Offer> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut offer = load_offer(&state, &path.into_inner())?;
        transition_offer(
            &state,
            &mut offer,
            OfferStatus::Sent,
            OfferStatus::Expired,
            "recruitment.offer.expired",
            &caller.actor,
        )?;
        Ok(offer)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Hire handler (HR onboarding handoff)
// ---------------------------------------------------------------------------

async fn hire_applicant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut applicant = load_applicant(&state, &path.into_inner())?;
        if applicant.stage != ApplicantStage::Offer {
            return Err(UnifiedError::conflict(format!(
                "Only an applicant in the offer stage can be hired (now: {})",
                applicant.stage.as_str()
            )));
        }
        let accepted_offer = state
            .offers
            .iter()
            .map(|entry| entry.value().clone())
            .find(|offer| {
                offer.applicant_id == applicant.id && offer.status == OfferStatus::Accepted
            })
            .ok_or_else(|| {
                UnifiedError::conflict(
                    "Hiring requires an accepted offer for this applicant",
                )
            })?;
        let from_stage = applicant.stage;
        applicant.stage = ApplicantStage::Hired;
        applicant.updated_at = Utc::now();
        state.applicants.insert(applicant.id, applicant.clone());
        record_stage_move(
            &state,
            &applicant,
            Some(from_stage),
            ApplicantStage::Hired,
            &caller.actor,
            None,
        );
        state.emit(
            "recruitment.hired",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "offer_id": accepted_offer.id.to_string(),
                "actor": caller.actor,
            }),
        );
        // Hire handoff: the HR module owns the onboarding record.
        // This module creates nothing on the HR side — it only
        // emits the event with everything HR needs to start.
        let job = state
            .jobs
            .get(&applicant.job_id)
            .map(|entry| entry.value().clone());
        state.emit(
            "hr.onboarding.started",
            json!({
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "user_id": applicant.email,
                "name": applicant.name,
                "email": applicant.email,
                "start_date": accepted_offer.start_date.map(|date| date.to_string()),
                "salary": round_money(accepted_offer.salary),
                "currency": accepted_offer.currency,
                "department_id": job.and_then(|job| job.department_id),
            }),
        );
        Ok(applicant_view(&state, &applicant))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
