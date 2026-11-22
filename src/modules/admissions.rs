// Admissions module — new-student registration (batch 13, builder B3).
//
// Admission periods per academic year and term, online registration
// through the Forms module, selection stages with weighted scoring,
// result announcement, registration fees through Billing, a document
// checklist through the Documents module, an ordered waitlist, and
// enrolment handoff to the academic system. Interconnection happens
// only through the hook bus — no module calls another module directly.
//
// Identity rule: an application stores the applicant user id ONLY.
// Names, emails, and all other profile data live in the Users module
// and are resolved by callers through `GET /api/v1/users/{id}`.
// Programs and levels are ids supplied by callers and owned by the
// academic system: this module stores them, matches quotas against
// them, and never duplicates program records. Document files live in
// the Documents module: this module stores checklist metadata and
// document ids only, never file bytes. Money (registration fees) is
// NUMERIC(19,4) in the migration and `f64` rounded to four decimals
// in memory, the same convention as the HR payroll amounts.
//
// Role-based access (enforced server-side on every handler):
//   - `admissions:read`      — read admission records (applicants see
//                              only their own applications and results).
//   - `admissions:read_all`  — read every application and result.
//   - `admissions:manage`    — create/update/delete periods, quotas,
//                              stages, required documents, decisions,
//                              checklist verification, fee waivers.
//   - `admissions:apply`     — file and edit your own application.
//   - `admissions:score`     — record and correct stage scores.
// Capabilities are checked through the `role.check` hook and fail
// closed: no answer means not allowed. No `admissions:*` capability
// is granted by the default roles; an administrator must grant them.
//
// Status vocabularies are DATA, not code: the allowed values for
// period, application, checklist, waitlist, and result-decision
// statuses live in the settings table (keys `status.period`,
// `status.application`, `status.checklist`, `status.waitlist`,
// `decision.result`) seeded with the defaults listed below. An
// administrator edits them through `PUT /api/v1/admissions/settings`.
// The code only carries the seed defaults and the transition rules
// between them. Other configurable defaults: `defaults.fee_currency`
// (3-letter code, default `USD`), `defaults.score_scale_max`
// (default `100`), `defaults.waitlist_order`
// (`score_desc_submitted_asc` or `submitted_asc`, default the
// former), `defaults.result_visible_to_applicant` (`true`/`false`).
// Weights are per-stage data on the stage definition; quotas are
// per-program/per-level data on the period.
//
// Events emitted:
//   - `admissions.period_created`      {period_id, academic_year, term}
//   - `admissions.period_opened`       {period_id}
//   - `admissions.period_closed`       {period_id}
//   - `admissions.application_created` {application_id, period_id, applicant_user_id, source}
//   - `admissions.application_received` {application_id, period_id, applicant_user_id, form_submission_id}
//   - `admissions.application_submitted` {application_id, period_id}
//   - `admissions.application_withdrawn` {application_id, period_id}
//   - `admissions.score_recorded`      {application_id, period_id, stage_id, score, total_score}
//   - `admissions.decision_made`       {application_id, period_id, decision}
//   - `admissions.results_published`   {period_id, result_count}
//   - `admissions.results_unpublished` {period_id}
//   - `admissions.fee_invoice_requested` {application_id, period_id, amount, currency, reference}
//   - `admissions.fee_paid`            {application_id, period_id, invoice_id}
//   - `admissions.waitlist_promoted`   {application_id, period_id, waitlist_entry_id}
//   - `admissions.document_attached`   {application_id, checklist_item_id, document_id}
//   - `admissions.document_verified`   {application_id, checklist_item_id}
//   - `admissions.enrollment_requested` (see `academic.student.create_requested` below)
//   - `academic.student.create_requested` {application_id, period_id, applicant_user_id, full_name, email, phone, program_id, level, academic_year, term, enrollment_year}
//   - `billing.invoice.requested`      {application_id, period_id, customer_user_id, customer_name, customer_email, amount, currency, description, reference}
//   - `notification.requested`         result publication, waitlist
//                                      promotion, and fee requests
//                                      (the Notifications module listens).
// Events consumed:
//   - `forms.mapping.requested`        target `admissions_application`:
//                                      creates an application from a
//                                      mapped form submission (see the
//                                      contract in `on_forms_mapping_requested`).
//   - `forms.submission.received`      {form_id, submission_id, slug}:
//                                      records the submission id on the
//                                      matching draft application when
//                                      the form is linked to a period.
//   - `document.uploaded`              full document record: when
//                                      `module_id` is `admissions` and
//                                      the document `name` equals a
//                                      pending checklist item's
//                                      `document_key` for an application
//                                      owned by `owner_id`, the item is
//                                      marked `submitted`.
//   - `document.deleted`               {id, ...}: clears the link on
//                                      any checklist item pointing at
//                                      the deleted document.
//   - `invoice.paid`                   {invoice, method}: when the
//                                      paid invoice carries
//                                      `order_id` equal to an
//                                      application id, the fee is
//                                      marked paid.
//   - `academic.student.created`       {application_id, student_id}:
//                                      links the created student record
//                                      to the enrolled application.
//
// CONVERGENCE (for the integrator):
//   1. `forms.submission.received` carries no field values (only
//      `form_id`, `submission_id`, `slug`), so the mapping path
//      (`forms.mapping.requested` with target
//      `admissions_application`) is the real registration path: the
//      admin configures the registration form's mapping with that
//      target and a field map. The submission handler only links the
//      submission id to a draft application created by the mapping.
//   2. `billing.invoice.requested` has no listener in the billing
//      module on disk today (the same open contract the appointments
//      module already uses). Billing must set the created invoice's
//      `order_id` to the application id when fulfilling the request,
//      so the `invoice.paid` handler here can match the payment.
//      Until then, fee payment is confirmed manually by recording
//      the invoice id through the fee endpoint with action `mark_paid`.
//   3. The academic module subscribes to
//      `academic.student.create_requested` and answers with
//      `academic.student.created` carrying the same
//      `application_id` (integration round, convergence W6); the
//      handler here links the returned `student_id` onto the
//      application. This module never creates student records
//      itself and never duplicates them. Note the enrol endpoint
//      marks the application `enrolled` before emitting, so the
//      `status == "accepted"` branch in
//      `on_academic_student_created` is a no-op on the normal path
//      — the student id link is what matters there.
//   4. The documents module has no synchronous lookup hook, so a
//      document id supplied to the attach endpoint cannot be verified
//      against the documents store here. Attach records the link and
//      marks the item `submitted`; the `document.uploaded` handler
//      auto-links uploads that follow the naming convention
//      (`module_id` `admissions`, document `name` equal to the
//      checklist item's `document_key`). The integrator may add a
//      documents lookup hook later.
//   5. Program and level ids are caller-supplied references owned by
//      the academic system. Quota matching is exact string equality
//      on (`program_id`, `level`); an application whose pair has no
//      quota row is treated as quota-unlimited only when the period
//      has no quota rows at all — otherwise acceptance is refused
//      with a 409 until a quota row exists for the pair.
//   6. `route_version_support` returns `All`, so the API Manager's
//      per-version registry picks every route up. No route here is
//      public: online registration goes through the forms module's
//      public submit routes, gated by the API Manager's
//      `ApiAccessRule` records.
//
// Public-access note: there is no public route in this module. The
// applicant-facing flow is the forms module's public render/submit
// (`GET/POST /api/v1/forms/public/{slug}`), gated by the API Manager.
// Applicants read their own application and result only after
// authenticating with `X-User-Id` / `X-Role` like every other module.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext, HookHandler};
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

// ---------------------------------------------------------------------------
// Capabilities (checked through the `role.check` hook; fail closed).
// ---------------------------------------------------------------------------

/// Read admission records (applicants see only their own).
pub const PERM_ADMISSIONS_READ: &str = "admissions:read";
/// Read every application and result.
pub const PERM_ADMISSIONS_READ_ALL: &str = "admissions:read_all";
/// Manage periods, quotas, stages, required documents, decisions,
/// checklist verification, fee waivers, and result publication.
pub const PERM_ADMISSIONS_MANAGE: &str = "admissions:manage";
/// File and edit your own application.
pub const PERM_ADMISSIONS_APPLY: &str = "admissions:apply";
/// Record and correct stage scores.
pub const PERM_ADMISSIONS_SCORE: &str = "admissions:score";

/// Longest free-text field accepted (names, notes, reasons).
const MAX_TEXT: usize = 2000;
/// Longest short field accepted (titles, codes, keys, slugs).
const MAX_SHORT: usize = 255;
/// Longest identifier accepted (user ids, program ids, form ids).
const MAX_ID: usize = 128;
/// Largest money amount accepted in a request. NUMERIC(19,4) holds at
/// most 15 integer digits, so anything at or above 1e15 is rejected.
const MAX_MONEY: f64 = 1_000_000_000_000_000.0;
/// Longest settings key/value accepted.
const MAX_SETTING_KEY: usize = 128;
const MAX_SETTING_VALUE: usize = 2000;

/// Seed defaults for the data-driven vocabularies and defaults.
/// These are CONFIGURABLE DEFAULTS: they seed the settings table on
/// first use and an administrator may change them at any time
/// through `PUT /api/v1/admissions/settings`. Nothing in the handler
/// logic treats these strings as fixed law except the transition
/// rules documented on the handlers that use them.
const SEED_PERIOD_STATUSES: &str = "draft,open,closed,archived";
const SEED_APPLICATION_STATUSES: &str =
    "draft,submitted,under_review,accepted,rejected,waitlisted,enrolled,withdrawn";
const SEED_CHECKLIST_STATUSES: &str = "pending,submitted,verified,rejected";
const SEED_WAITLIST_STATUSES: &str = "waiting,promoted,declined,expired";
const SEED_DECISIONS: &str = "accepted,rejected,waitlisted";
const SEED_FEE_CURRENCY: &str = "USD";
const SEED_SCORE_SCALE_MAX: &str = "100";
const SEED_WAITLIST_ORDER: &str = "score_desc_submitted_asc";
const SEED_RESULT_VISIBLE: &str = "true";

/// Settings keys for the vocabularies and defaults above.
const KEY_PERIOD_STATUSES: &str = "status.period";
const KEY_APPLICATION_STATUSES: &str = "status.application";
const KEY_CHECKLIST_STATUSES: &str = "status.checklist";
const KEY_WAITLIST_STATUSES: &str = "status.waitlist";
const KEY_DECISIONS: &str = "decision.result";
const KEY_FEE_CURRENCY: &str = "defaults.fee_currency";
const KEY_SCORE_SCALE_MAX: &str = "defaults.score_scale_max";
const KEY_WAITLIST_ORDER: &str = "defaults.waitlist_order";
const KEY_RESULT_VISIBLE: &str = "defaults.result_visible_to_applicant";

/// Waitlist ordering policies understood by the ordering helper.
/// The policy itself is data (the `defaults.waitlist_order`
/// setting); only the small set of policy names is fixed here.
const WAITLIST_ORDERS: [&str; 2] = ["score_desc_submitted_asc", "submitted_asc"];

/// Fee invoice states, tracked on the application itself.
const FEE_STATES: [&str; 4] = ["none", "requested", "paid", "waived"];

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One admission period: one academic year and term, with open and
/// close dates, an optional registration fee, and an optional link to
/// the forms-module form used for online registration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionPeriod {
    pub id: String,
    pub academic_year: String,
    pub term: String,
    pub name: String,
    pub open_date: NaiveDate,
    pub close_date: NaiveDate,
    /// `draft`, `open`, `closed`, or `archived` (see the settings
    /// vocabulary `status.period`).
    pub status: String,
    pub registration_fee_amount: Option<f64>,
    pub fee_currency: String,
    /// Forms-module form id used for online registration, if any.
    pub registration_form_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreatePeriod {
    pub academic_year: String,
    pub term: String,
    pub name: Option<String>,
    pub open_date: String,
    pub close_date: String,
    pub registration_fee_amount: Option<f64>,
    pub fee_currency: Option<String>,
    pub registration_form_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePeriod {
    pub academic_year: Option<String>,
    pub term: Option<String>,
    pub name: Option<String>,
    pub open_date: Option<String>,
    pub close_date: Option<String>,
    pub registration_fee_amount: Option<Option<f64>>,
    pub fee_currency: Option<String>,
    pub registration_form_id: Option<Option<String>>,
    /// Only `archived` is accepted here, and only from `closed`.
    /// Opening and closing use their own endpoints.
    pub status: Option<String>,
}

/// One per-program, per-level seat quota inside a period. Quotas are
/// data: an administrator adds one row per (`program_id`, `level`)
/// pair that accepts students in this period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeriodQuota {
    pub id: String,
    pub period_id: String,
    pub program_id: String,
    pub level: String,
    pub seats: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateQuota {
    pub program_id: String,
    pub level: String,
    pub seats: u32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateQuota {
    pub seats: Option<u32>,
}

/// One selection stage of a period (for example a document check, a
/// written test, or an interview). Stage definitions are data: an
/// administrator defines the stages, their order, their weight in
/// the total, and the minimum passing score, per period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StageDefinition {
    pub id: String,
    pub period_id: String,
    pub stage_key: String,
    pub name: String,
    pub position: u32,
    /// Weight of this stage in the weighted total (0 or more).
    pub weight: f64,
    /// Minimum score that passes this stage, on the configured
    /// score scale; `None` means the stage has no pass threshold.
    pub pass_score: Option<f64>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateStage {
    pub stage_key: String,
    pub name: String,
    pub position: Option<u32>,
    pub weight: Option<f64>,
    pub pass_score: Option<f64>,
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateStage {
    pub name: Option<String>,
    pub position: Option<u32>,
    pub weight: Option<f64>,
    pub pass_score: Option<Option<f64>>,
    pub required: Option<bool>,
}

/// One document a period requires from every applicant. Required
/// documents are data, defined per period.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequiredDocument {
    pub id: String,
    pub period_id: String,
    /// Stable key used to match uploads (`document.name` must equal
    /// this key when the file is uploaded through the documents
    /// module with `module_id` `admissions`).
    pub document_key: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateRequiredDocument {
    pub document_key: String,
    pub title: String,
    pub description: Option<String>,
    pub required: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRequiredDocument {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub required: Option<bool>,
}

/// One application for one period. Only the applicant user id is
/// stored; profile data lives in the Users module. `program_id` and
/// `level` are references owned by the academic system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Application {
    pub id: String,
    pub period_id: String,
    pub applicant_user_id: String,
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub program_id: String,
    pub level: String,
    /// Forms-module submission id this application came from, if any.
    pub form_submission_id: Option<String>,
    /// Application status (see the settings vocabulary
    /// `status.application`).
    pub status: String,
    /// Weighted total score on the configured scale, recomputed on
    /// every score change; `None` until the first score is recorded.
    pub total_score: Option<f64>,
    pub fee_amount: Option<f64>,
    pub fee_currency: String,
    /// `none`, `requested`, `paid`, or `waived`.
    pub fee_invoice_status: String,
    pub fee_invoice_id: Option<String>,
    /// Student record id, set when the academic system answers
    /// `academic.student.created`; never created here.
    pub student_id: Option<String>,
    /// `manual` or `form`.
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateApplication {
    pub period_id: String,
    pub applicant_user_id: Option<String>,
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub program_id: String,
    pub level: String,
    pub form_submission_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateApplication {
    pub full_name: Option<String>,
    pub email: Option<String>,
    pub phone: Option<Option<String>>,
    pub program_id: Option<String>,
    pub level: Option<String>,
}

/// One recorded score for one application in one stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplicationScore {
    pub id: String,
    pub application_id: String,
    pub period_id: String,
    pub stage_id: String,
    pub stage_key: String,
    pub score: f64,
    pub scored_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CreateScore {
    pub stage_id: String,
    pub score: f64,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateScore {
    pub score: Option<f64>,
    pub note: Option<Option<String>>,
}

/// One checklist item: one required document for one application.
/// Created from the period's required documents when the application
/// is created; status is driven by document events and verification.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChecklistItem {
    pub id: String,
    pub application_id: String,
    pub period_id: String,
    pub required_document_id: String,
    pub document_key: String,
    pub title: String,
    pub required: bool,
    /// `pending`, `submitted`, `verified`, or `rejected` (see the
    /// settings vocabulary `status.checklist`).
    pub status: String,
    pub document_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AttachDocument {
    pub document_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RejectChecklistItem {
    pub reason: String,
}

/// One waitlist entry: an application waiting for a freed seat.
/// `position` orders the waitlist (1 is first in line).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaitlistEntry {
    pub id: String,
    pub period_id: String,
    pub application_id: String,
    pub position: u32,
    /// `waiting`, `promoted`, `declined`, or `expired` (see the
    /// settings vocabulary `status.waitlist`).
    pub status: String,
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PromoteWaitlist {
    pub application_id: Option<String>,
}

/// One announced result for one application. `published` controls
/// whether the applicant may see it; staff always see it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionResult {
    pub id: String,
    pub period_id: String,
    pub application_id: String,
    /// `accepted`, `rejected`, or `waitlisted` (see the settings
    /// vocabulary `decision.result`).
    pub decision: String,
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub decided_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecideApplication {
    pub decision: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FeeAction {
    /// `request`, `waive`, or `mark_paid`.
    pub action: String,
    /// Invoice id, required for `mark_paid` (manual confirmation
    /// until billing answers `billing.invoice.requested`).
    pub invoice_id: Option<String>,
}

/// One admin-editable setting (vocabulary or default). Values are
/// plain strings; comma-separated lists hold vocabularies.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdmissionSetting {
    pub key: String,
    pub value: String,
    pub description: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateSetting {
    pub key: String,
    pub value: String,
    pub description: Option<String>,
}

/// Weighted total plus the per-stage breakdown, as returned by the
/// score-total endpoint.
#[derive(Debug, Clone, Serialize)]
pub struct ScoreTotal {
    pub application_id: String,
    pub total_score: Option<f64>,
    pub scale_max: f64,
    pub stages: Vec<ScoreBreakdown>,
    /// True when every required stage has a passing score.
    pub all_required_stages_passed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScoreBreakdown {
    pub stage_id: String,
    pub stage_key: String,
    pub weight: f64,
    pub score: Option<f64>,
    pub passed: Option<bool>,
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct AdmissionsState {
    hooks: RwLock<Arc<HookBus>>,
    periods: DashMap<String, AdmissionPeriod>,
    quotas: DashMap<String, PeriodQuota>,
    stages: DashMap<String, StageDefinition>,
    required_documents: DashMap<String, RequiredDocument>,
    applications: DashMap<String, Application>,
    scores: DashMap<String, ApplicationScore>,
    checklist: DashMap<String, ChecklistItem>,
    waitlist: DashMap<String, WaitlistEntry>,
    results: DashMap<String, AdmissionResult>,
    settings: DashMap<String, AdmissionSetting>,
}

impl AdmissionsState {
    fn new() -> Self {
        let state = Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            periods: DashMap::new(),
            quotas: DashMap::new(),
            stages: DashMap::new(),
            required_documents: DashMap::new(),
            applications: DashMap::new(),
            scores: DashMap::new(),
            checklist: DashMap::new(),
            waitlist: DashMap::new(),
            results: DashMap::new(),
            settings: DashMap::new(),
        };
        state.seed_settings();
        state
    }

    /// Seed the configurable defaults. Seeding runs once per process
    /// start into the in-memory store; an administrator's edits win
    /// afterwards (a seeded key is never overwritten here).
    fn seed_settings(&self) {
        let seeds: &[(&str, &str, &str)] = &[
            (
                KEY_PERIOD_STATUSES,
                SEED_PERIOD_STATUSES,
                "Allowed admission-period statuses, comma-separated.",
            ),
            (
                KEY_APPLICATION_STATUSES,
                SEED_APPLICATION_STATUSES,
                "Allowed application statuses, comma-separated.",
            ),
            (
                KEY_CHECKLIST_STATUSES,
                SEED_CHECKLIST_STATUSES,
                "Allowed document-checklist item statuses, comma-separated.",
            ),
            (
                KEY_WAITLIST_STATUSES,
                SEED_WAITLIST_STATUSES,
                "Allowed waitlist entry statuses, comma-separated.",
            ),
            (
                KEY_DECISIONS,
                SEED_DECISIONS,
                "Allowed result decisions, comma-separated.",
            ),
            (
                KEY_FEE_CURRENCY,
                SEED_FEE_CURRENCY,
                "Default 3-letter currency code for registration fees.",
            ),
            (
                KEY_SCORE_SCALE_MAX,
                SEED_SCORE_SCALE_MAX,
                "Maximum score on the scoring scale (scores run 0..=max).",
            ),
            (
                KEY_WAITLIST_ORDER,
                SEED_WAITLIST_ORDER,
                "Waitlist ordering policy: score_desc_submitted_asc or submitted_asc.",
            ),
            (
                KEY_RESULT_VISIBLE,
                SEED_RESULT_VISIBLE,
                "Whether applicants may see their own result once published (true/false).",
            ),
        ];
        for (key, value, description) in seeds {
            if !self.settings.contains_key(*key) {
                self.settings.insert(
                    key.to_string(),
                    AdmissionSetting {
                        key: key.to_string(),
                        value: value.to_string(),
                        description: description.to_string(),
                        updated_at: now(),
                    },
                );
            }
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Read a setting value, falling back to `default` when the key
    /// is absent or blank.
    fn setting(&self, key: &str, default: &str) -> String {
        self.settings
            .get(key)
            .map(|entry| entry.value().value.clone())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    /// The allowed values for a vocabulary setting: the configured
    /// comma-separated list, or `defaults` when the setting is
    /// missing or empty.
    fn vocabulary(&self, key: &str, defaults: &[&str]) -> Vec<String> {
        let raw = self.setting(key, "");
        if raw.trim().is_empty() {
            return defaults.iter().map(|value| value.to_string()).collect();
        }
        raw.split(',')
            .map(|part| part.trim().to_string())
            .filter(|part| !part.is_empty())
            .collect()
    }

    /// Maximum score on the configured scale (default 100).
    fn score_scale_max(&self) -> f64 {
        self.setting(KEY_SCORE_SCALE_MAX, SEED_SCORE_SCALE_MAX)
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value > 0.0 && *value <= 1000.0)
            .unwrap_or(100.0)
    }

    /// Waitlist ordering policy from settings, falling back to the
    /// seed policy when the configured value is unknown.
    fn waitlist_order(&self) -> String {
        let configured = self.setting(KEY_WAITLIST_ORDER, SEED_WAITLIST_ORDER);
        if WAITLIST_ORDERS.contains(&configured.trim()) {
            configured.trim().to_string()
        } else {
            SEED_WAITLIST_ORDER.to_string()
        }
    }

    /// True when applicants may see their own published result.
    fn results_visible_to_applicants(&self) -> bool {
        self.setting(KEY_RESULT_VISIBLE, SEED_RESULT_VISIBLE)
            .trim()
            .eq_ignore_ascii_case("true")
    }

    fn period(&self, period_id: &str) -> UnifiedResult<AdmissionPeriod> {
        self.periods
            .get(period_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Admission period not found"))
    }

    fn application(&self, application_id: &str) -> UnifiedResult<Application> {
        self.applications
            .get(application_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Application not found"))
    }

    /// True when the period accepts submissions today: status `open`
    /// and today inside [open_date, close_date].
    fn period_accepts_submissions(&self, period: &AdmissionPeriod) -> bool {
        let today = today();
        period.status == "open" && period.open_date <= today && today <= period.close_date
    }

    /// Stages of one period, in definition order.
    fn stages_of(&self, period_id: &str) -> Vec<StageDefinition> {
        let mut stages: Vec<StageDefinition> = self
            .stages
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|stage| stage.period_id == period_id)
            .collect();
        stages.sort_by(|a, b| {
            a.position
                .cmp(&b.position)
                .then_with(|| a.stage_key.cmp(&b.stage_key))
        });
        stages
    }

    /// Required documents of one period, in creation order.
    fn required_documents_of(&self, period_id: &str) -> Vec<RequiredDocument> {
        let mut documents: Vec<RequiredDocument> = self
            .required_documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| document.period_id == period_id)
            .collect();
        documents.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        documents
    }

    /// Scores of one application, ordered by stage position.
    fn scores_of(&self, application_id: &str) -> Vec<ApplicationScore> {
        let mut scores: Vec<ApplicationScore> = self
            .scores
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|score| score.application_id == application_id)
            .collect();
        scores.sort_by(|a, b| {
            let position_a = self
                .stages
                .get(&a.stage_id)
                .map(|stage| stage.position)
                .unwrap_or(u32::MAX);
            let position_b = self
                .stages
                .get(&b.stage_id)
                .map(|stage| stage.position)
                .unwrap_or(u32::MAX);
            position_a
                .cmp(&position_b)
                .then_with(|| a.stage_key.cmp(&b.stage_key))
        });
        scores
    }

    /// Checklist items of one application, in creation order.
    fn checklist_of(&self, application_id: &str) -> Vec<ChecklistItem> {
        let mut items: Vec<ChecklistItem> = self
            .checklist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.application_id == application_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Waiting waitlist entries of one period, in promotion order:
    /// by the configured policy — highest weighted total first, then
    /// earliest submission — or by submission time only.
    fn waiting_entries(&self, period_id: &str) -> Vec<WaitlistEntry> {
        let mut entries: Vec<WaitlistEntry> = self
            .waitlist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.period_id == period_id && entry.status == "waiting")
            .collect();
        let order = self.waitlist_order();
        entries.sort_by(|a, b| {
            if order == "score_desc_submitted_asc" {
                let score_a = self
                    .applications
                    .get(&a.application_id)
                    .and_then(|application| application.total_score)
                    .unwrap_or(0.0);
                let score_b = self
                    .applications
                    .get(&b.application_id)
                    .and_then(|application| application.total_score)
                    .unwrap_or(0.0);
                score_b
                    .partial_cmp(&score_a)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.created_at.cmp(&b.created_at))
            } else {
                a.created_at.cmp(&b.created_at)
            }
        });
        entries
    }

    /// Number of accepted (not yet enrolled or withdrawn) seats taken
    /// for one (`program_id`, `level`) pair inside a period.
    fn accepted_count(&self, period_id: &str, program_id: &str, level: &str) -> u32 {
        self.applications
            .iter()
            .filter(|entry| {
                let application = entry.value();
                application.period_id == period_id
                    && application.program_id == program_id
                    && application.level == level
                    && matches!(application.status.as_str(), "accepted" | "enrolled")
            })
            .count() as u32
    }

    /// Seats configured for one (`program_id`, `level`) pair, if a
    /// quota row exists. `None` means no quota row for the pair.
    fn quota_seats(&self, period_id: &str, program_id: &str, level: &str) -> Option<u32> {
        self.quotas
            .iter()
            .find(|entry| {
                let quota = entry.value();
                quota.period_id == period_id
                    && quota.program_id == program_id
                    && quota.level == level
            })
            .map(|entry| entry.value().seats)
    }

    /// True when another accepted application fits the quota for the
    /// application's (`program_id`, `level`) pair. When the period
    /// has no quota rows at all the pair is unlimited; when quota
    /// rows exist but none covers the pair, acceptance is refused.
    fn seat_available(&self, application: &Application) -> UnifiedResult<bool> {
        let any_quota = self
            .quotas
            .iter()
            .any(|entry| entry.value().period_id == application.period_id);
        if !any_quota {
            return Ok(true);
        }
        match self.quota_seats(
            &application.period_id,
            &application.program_id,
            &application.level,
        ) {
            Some(seats) => Ok(self.accepted_count(
                &application.period_id,
                &application.program_id,
                &application.level,
            ) < seats),
            None => Err(UnifiedError::conflict(format!(
                "No quota is configured for program \"{}\" level \"{}\" in this period",
                application.program_id, application.level
            ))),
        }
    }

    /// Weighted total for one application on the configured scale,
    /// plus the per-stage breakdown and the pass state of every
    /// required stage. Stages without a score are skipped; the total
    /// is the weight-normalised mean of the scored stages, so a
    /// partially scored application already has a meaningful total.
    fn compute_total(&self, application_id: &str) -> ScoreTotal {
        let application = self
            .applications
            .get(application_id)
            .map(|entry| entry.value().clone());
        let period_id = application
            .as_ref()
            .map(|value| value.period_id.clone())
            .unwrap_or_default();
        let scale_max = self.score_scale_max();
        let stages = self.stages_of(&period_id);
        let scores = self.scores_of(application_id);
        let mut breakdown: Vec<ScoreBreakdown> = Vec::new();
        let mut weighted_sum = 0.0;
        let mut weight_sum = 0.0;
        let mut all_required_passed = true;
        for stage in &stages {
            let score = scores
                .iter()
                .find(|entry| entry.stage_id == stage.id)
                .map(|entry| entry.score);
            let passed = match (score, stage.pass_score) {
                (Some(value), Some(threshold)) => Some(value >= threshold),
                _ => None,
            };
            if stage.required {
                match passed {
                    Some(true) => {}
                    _ => all_required_passed = false,
                }
            }
            if let Some(value) = score {
                if stage.weight > 0.0 {
                    weighted_sum += value * stage.weight;
                    weight_sum += stage.weight;
                }
            }
            breakdown.push(ScoreBreakdown {
                stage_id: stage.id.clone(),
                stage_key: stage.stage_key.clone(),
                weight: stage.weight,
                score,
                passed,
            });
        }
        let total = if weight_sum > 0.0 {
            Some(round4(weighted_sum / weight_sum))
        } else {
            None
        };
        ScoreTotal {
            application_id: application_id.to_string(),
            total_score: total,
            scale_max,
            stages: breakdown,
            all_required_stages_passed: all_required_passed,
        }
    }

    /// Recompute and store the weighted total on an application.
    /// Returns the fresh total.
    fn refresh_total(&self, application_id: &str) -> Option<f64> {
        let total = self.compute_total(application_id).total_score;
        if let Some(mut application) = self.applications.get_mut(application_id) {
            application.total_score = total;
            application.updated_at = now();
        }
        total
    }

    /// Create checklist items for an application from the period's
    /// current required documents. Existing items are left alone, so
    /// re-running this is safe.
    fn ensure_checklist(&self, application: &Application) {
        for document in self.required_documents_of(&application.period_id) {
            let exists = self.checklist.iter().any(|entry| {
                let item = entry.value();
                item.application_id == application.id
                    && item.required_document_id == document.id
            });
            if exists {
                continue;
            }
            let item = ChecklistItem {
                id: new_id(),
                application_id: application.id.clone(),
                period_id: application.period_id.clone(),
                required_document_id: document.id.clone(),
                document_key: document.document_key.clone(),
                title: document.title.clone(),
                required: document.required,
                status: "pending".to_string(),
                document_id: None,
                verified_by: None,
                verified_at: None,
                rejection_reason: None,
                created_at: now(),
                updated_at: now(),
            };
            self.checklist.insert(item.id.clone(), item);
        }
    }

    /// True when every required checklist item of an application is
    /// verified.
    fn checklist_complete(&self, application_id: &str) -> bool {
        self.checklist_of(application_id)
            .iter()
            .filter(|item| item.required)
            .all(|item| item.status == "verified")
    }

    /// Promote the first waiting entry of a period when a seat is
    /// free for its (`program_id`, `level`) pair. Returns the
    /// promoted application, if any. The caller owns the surrounding
    /// decision (a seat freed by withdrawal or rejection).
    fn promote_top_waitlisted(&self, period_id: &str) -> Option<Application> {
        for entry in self.waiting_entries(period_id) {
            let Some(application) = self
                .applications
                .get(&entry.application_id)
                .map(|value| value.value().clone())
            else {
                continue;
            };
            if application.status != "waitlisted" {
                continue;
            }
            if self.seat_available(&application).unwrap_or(false) {
                let promoted = self.promote_waitlist_entry(&entry, &application);
                return Some(promoted);
            }
        }
        None
    }

    /// Apply a promotion: entry `promoted`, application `accepted`,
    /// result decision `accepted`, events and notification emitted.
    fn promote_waitlist_entry(
        &self,
        entry: &WaitlistEntry,
        application: &Application,
    ) -> Application {
        let mut promoted = application.clone();
        if let Some(mut waitlist_entry) = self.waitlist.get_mut(&entry.id) {
            waitlist_entry.status = "promoted".to_string();
            waitlist_entry.promoted_at = Some(now());
            waitlist_entry.updated_at = now();
        }
        if let Some(mut record) = self.applications.get_mut(&application.id) {
            record.status = "accepted".to_string();
            record.updated_at = now();
            promoted = record.clone();
        }
        self.upsert_result(&promoted, "accepted", "system", None);
        self.emit(
            "admissions.waitlist_promoted",
            json!({
                "application_id": promoted.id,
                "period_id": promoted.period_id,
                "waitlist_entry_id": entry.id,
            }),
        );
        self.emit(
            "notification.requested",
            json!({
                "user_id": promoted.applicant_user_id,
                "title": "A seat opened for your application",
                "body": "Your application was promoted from the waitlist and is now accepted.",
                "kind": "admissions.waitlist_promoted",
            }),
        );
        promoted
    }

    /// Create or update the result record for an application.
    fn upsert_result(
        &self,
        application: &Application,
        decision: &str,
        decided_by: &str,
        note: Option<String>,
    ) -> AdmissionResult {
        if let Some(mut existing) = self.results.iter_mut().find(|entry| {
            entry.value().application_id == application.id
        }) {
            existing.decision = decision.to_string();
            existing.decided_by = decided_by.to_string();
            if let Some(note) = note {
                existing.note = Some(note);
            }
            existing.updated_at = now();
            return existing.clone();
        }
        let result = AdmissionResult {
            id: new_id(),
            period_id: application.period_id.clone(),
            application_id: application.id.clone(),
            decision: decision.to_string(),
            published: false,
            published_at: None,
            decided_by: decided_by.to_string(),
            note,
            created_at: now(),
            updated_at: now(),
        };
        self.results.insert(result.id.clone(), result.clone());
        result
    }

    /// Ask the Notifications module to tell an applicant about a
    /// published result.
    fn notify_result(&self, application: &Application, decision: &str) {
        let (title, body) = match decision {
            "accepted" => (
                "Your application was accepted",
                "Congratulations — your application was accepted. Follow the enrolment instructions to complete your registration.",
            ),
            "rejected" => (
                "Your application result is available",
                "Your application was not accepted this time. Thank you for applying.",
            ),
            _ => (
                "Your application is on the waitlist",
                "Your application is on the waitlist. You will be notified if a seat opens.",
            ),
        };
        self.emit(
            "notification.requested",
            json!({
                "user_id": application.applicant_user_id,
                "title": title,
                "body": body,
                "kind": "admissions.result_published",
            }),
        );
    }

    /// Request a notification through the notifications module.
    fn request_notification(&self, user_id: &str, title: &str, body: &str, kind: &str) {
        self.emit(
            "notification.requested",
            json!({
                "user_id": user_id,
                "title": title,
                "body": body,
                "kind": kind,
            }),
        );
    }
}

pub struct AdmissionsModule {
    state: Arc<AdmissionsState>,
}

impl AdmissionsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AdmissionsState::new()),
        }
    }
}

impl Default for AdmissionsModule {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation, views
// ---------------------------------------------------------------------------

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// Money rounded to four decimals (NUMERIC(19,4)).
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// The caller, from the headers every module in this codebase uses:
/// `X-User-Id` identifies the actor, `X-Role` carries the role slug
/// or id the capability check runs against.
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

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
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

/// True when the caller may read `application`: staff with read-all,
/// or the applicant themself.
fn can_read_application(ctx: &AppContext, caller: &Caller, application: &Application) -> bool {
    check_capability(ctx, &caller.role, PERM_ADMISSIONS_READ_ALL)
        || application.applicant_user_id == caller.actor
}

/// Enforce read access for one application, hiding the not-found
/// case: a caller who may not read the record gets 404, so
/// application ids cannot be probed.
fn load_application_for_read(
    state: &AdmissionsState,
    ctx: &AppContext,
    caller: &Caller,
    application_id: &str,
) -> UnifiedResult<Application> {
    let application = state.application(application_id)?;
    if can_read_application(ctx, caller, &application) {
        Ok(application)
    } else {
        Err(UnifiedError::not_found("Application not found"))
    }
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

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        None => Ok(None),
        Some(text) => Ok(Some(require_text(text, field, max)?)),
    }
}

fn require_id(value: &str, field: &str) -> UnifiedResult<String> {
    require_text(value, field, MAX_ID)
}

fn parse_date(raw: &str, field: &str) -> UnifiedResult<NaiveDate> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be a date (YYYY-MM-DD)")))
}

fn parse_money(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value >= MAX_MONEY {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a finite amount from 0 up to 999999999999999.9999"
        )));
    }
    Ok(round4(value))
}

fn valid_currency(raw: &str) -> UnifiedResult<String> {
    let currency = raw.trim().to_uppercase();
    if currency.len() != 3 || !currency.chars().all(|ch| ch.is_ascii_uppercase()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code (for example USD)",
        ));
    }
    Ok(currency)
}

fn valid_email(raw: &str) -> UnifiedResult<String> {
    let email = raw.trim().to_lowercase();
    if email.len() > MAX_SHORT
        || email.is_empty()
        || !email.contains('@')
        || email.starts_with('@')
        || email.ends_with('@')
        || email.contains(' ')
    {
        return Err(UnifiedError::bad_request(
            "email must be a valid email address",
        ));
    }
    Ok(email)
}

/// `Birth Certificate` -> `birth-certificate`.
fn slugify(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true;
    for ch in name.to_lowercase().chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    slug
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

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Hook handlers (events consumed)
// ---------------------------------------------------------------------------

/// Handle `forms.mapping.requested` for target
/// `admissions_application`.
///
/// CONTRACT — the registration form's mapping must use target
/// `admissions_application` and a field map that produces these
/// fields (all values are the submitted form values, already mapped
/// by the forms module):
///   - `period_id` (required): the admission period id.
///   - `applicant_user_id` (optional): the applicant's user id; when
///     absent the submission is recorded without an owner and staff
///     assign one later.
///   - `full_name` (required), `email` (required), `phone`
///     (optional), `program_id` (required), `level` (required).
/// The handler answers with `{"created": true, "id": "<application
/// id>"}` on success or `{"created": false, "error": "..."}` on
/// failure, through the first result slot the forms module reads.
/// The created application starts in status `draft` with source
/// `form`; the applicant (or staff) completes it and submits it
/// through `POST /api/v1/admissions/applications/{id}/submit`.
/// Submissions for other targets are ignored (no result added).
fn on_forms_mapping_requested(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let target = hook_ctx
        .data
        .get("target")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if target != "admissions_application" {
        return;
    }
    let fields = hook_ctx
        .data
        .get("fields")
        .and_then(|value| value.as_object())
        .cloned()
        .unwrap_or_default();
    let submission_id = hook_ctx
        .data
        .get("submission_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let field_str = |key: &str| -> Option<String> {
        fields
            .get(key)
            .and_then(|value| value.as_str())
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    let fail = |message: &str| {
        hook_ctx.add_result(json!({ "created": false, "error": message }));
    };
    let period_id = match field_str("period_id") {
        Some(value) if value.len() <= MAX_ID => value,
        _ => {
            fail("period_id is required and must be at most 128 characters");
            return;
        }
    };
    let Ok(period) = state.period(&period_id) else {
        fail("Admission period not found");
        return;
    };
    if !state.period_accepts_submissions(&period) {
        fail("Admission period is not open for submissions");
        return;
    }
    let full_name = match field_str("full_name") {
        Some(value) if value.len() <= MAX_TEXT => value,
        _ => {
            fail("full_name is required and must be at most 2000 characters");
            return;
        }
    };
    let email = match field_str("email") {
        Some(value) => {
            let email = value.to_lowercase();
            if email.len() > MAX_SHORT || !email.contains('@') || email.contains(' ') {
                fail("email must be a valid email address");
                return;
            }
            email
        }
        None => {
            fail("email is required");
            return;
        }
    };
    let program_id = match field_str("program_id") {
        Some(value) if value.len() <= MAX_ID => value,
        _ => {
            fail("program_id is required and must be at most 128 characters");
            return;
        }
    };
    let level = match field_str("level") {
        Some(value) if value.len() <= MAX_SHORT => value,
        _ => {
            fail("level is required and must be at most 255 characters");
            return;
        }
    };
    let applicant_user_id = field_str("applicant_user_id")
        .filter(|value| value.len() <= MAX_ID)
        .unwrap_or_default();
    if let Some(existing_submission_id) = &submission_id {
        if state.applications.iter().any(|entry| {
            entry
                .value()
                .form_submission_id
                .as_deref()
                == Some(existing_submission_id.as_str())
        }) {
            fail("An application already exists for this form submission");
            return;
        }
    }
    let application = Application {
        id: new_id(),
        period_id: period.id.clone(),
        applicant_user_id,
        full_name,
        email,
        phone: field_str("phone").filter(|value| value.len() <= MAX_SHORT),
        program_id,
        level,
        form_submission_id: submission_id.clone(),
        status: "draft".to_string(),
        total_score: None,
        fee_amount: period.registration_fee_amount,
        fee_currency: period.fee_currency.clone(),
        fee_invoice_status: "none".to_string(),
        fee_invoice_id: None,
        student_id: None,
        source: "form".to_string(),
        created_at: now(),
        updated_at: now(),
    };
    state.ensure_checklist(&application);
    state.applications.insert(application.id.clone(), application.clone());
    state.emit(
        "admissions.application_created",
        json!({
            "application_id": application.id,
            "period_id": application.period_id,
            "applicant_user_id": application.applicant_user_id,
            "source": "form",
        }),
    );
    if submission_id.is_some() {
        state.emit(
            "admissions.application_received",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "applicant_user_id": application.applicant_user_id,
                "form_submission_id": submission_id,
            }),
        );
    }
    hook_ctx.add_result(json!({ "created": true, "id": application.id }));
}

/// Handle `forms.submission.received` ({form_id, submission_id,
/// slug}). The event carries no field values, so it cannot create an
/// application on its own. When a draft application already exists
/// for the submission (created by the mapping handler) nothing more
/// is needed; when a period links this form id and no application
/// exists yet for the submission, a placeholder draft is recorded
/// with the submission id so staff can complete it. The placeholder
/// carries no applicant data and cannot be submitted until the
/// required fields are filled in.
fn on_forms_submission_received(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let form_id = hook_ctx
        .data
        .get("form_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let submission_id = hook_ctx
        .data
        .get("submission_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if form_id.is_empty() || submission_id.is_empty() {
        return;
    }
    if state.applications.iter().any(|entry| {
        entry
            .value()
            .form_submission_id
            .as_deref()
            == Some(submission_id)
    }) {
        return;
    }
    let Some(period) = state.periods.iter().find(|entry| {
        entry
            .value()
            .registration_form_id
            .as_deref()
            == Some(form_id)
    }) else {
        return;
    };
    let period = period.value().clone();
    if !state.period_accepts_submissions(&period) {
        return;
    }
    let application = Application {
        id: new_id(),
        period_id: period.id.clone(),
        applicant_user_id: String::new(),
        full_name: String::new(),
        email: String::new(),
        phone: None,
        program_id: String::new(),
        level: String::new(),
        form_submission_id: Some(submission_id.to_string()),
        status: "draft".to_string(),
        total_score: None,
        fee_amount: period.registration_fee_amount,
        fee_currency: period.fee_currency.clone(),
        fee_invoice_status: "none".to_string(),
        fee_invoice_id: None,
        student_id: None,
        source: "form".to_string(),
        created_at: now(),
        updated_at: now(),
    };
    state.ensure_checklist(&application);
    state.applications.insert(application.id.clone(), application.clone());
    state.emit(
        "admissions.application_received",
        json!({
            "application_id": application.id,
            "period_id": application.period_id,
            "applicant_user_id": application.applicant_user_id,
            "form_submission_id": submission_id,
        }),
    );
}

/// Handle `document.uploaded` (the full document record). When the
/// document belongs to this module (`module_id` is `admissions`) and
/// its `name` equals a pending checklist item's `document_key` for an
/// application owned by `owner_id`, the item is marked `submitted`
/// and linked to the document. Uploads that match nothing are
/// ignored.
fn on_document_uploaded(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let data = hook_ctx.data.clone();
    if data.get("module_id").and_then(|value| value.as_str()) != Some("admissions") {
        return;
    }
    let document_id = match data.get("id").and_then(|value| value.as_str()) {
        Some(value) => value.to_string(),
        None => return,
    };
    let document_name = match data.get("name").and_then(|value| value.as_str()) {
        Some(value) => value.to_string(),
        None => return,
    };
    let owner_id = match data.get("owner_id").and_then(|value| value.as_str()) {
        Some(value) => value.to_string(),
        None => return,
    };
    let mut matched: Vec<(String, String)> = Vec::new();
    for entry in state.checklist.iter() {
        let item = entry.value();
        if item.status != "pending" || item.document_key != document_name {
            continue;
        }
        let Some(application) = state
            .applications
            .get(&item.application_id)
            .map(|value| value.value().clone())
        else {
            continue;
        };
        if application.applicant_user_id == owner_id {
            matched.push((item.id.clone(), application.id.clone()));
        }
    }
    for (item_id, application_id) in matched {
        if let Some(mut item) = state.checklist.get_mut(&item_id) {
            item.status = "submitted".to_string();
            item.document_id = Some(document_id.clone());
            item.updated_at = now();
        }
        state.emit(
            "admissions.document_attached",
            json!({
                "application_id": application_id,
                "checklist_item_id": item_id,
                "document_id": document_id,
            }),
        );
    }
}

/// Handle `document.deleted` ({id, ...}). Any checklist item
/// pointing at the deleted document goes back to `pending` with its
/// link cleared, so the applicant can upload a replacement.
fn on_document_deleted(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let document_id = hook_ctx
        .data
        .get("id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if document_id.is_empty() {
        return;
    }
    for mut entry in state.checklist.iter_mut() {
        let item = entry.value_mut();
        if item.document_id.as_deref() == Some(document_id) {
            item.document_id = None;
            if item.status == "submitted" || item.status == "verified" {
                item.status = "pending".to_string();
            }
            item.verified_by = None;
            item.verified_at = None;
            item.updated_at = now();
        }
    }
}

/// Handle `invoice.paid` ({invoice, method}). When the paid invoice
/// carries an `order_id` equal to an application id, the
/// application's fee is marked paid. Billing must set `order_id` to
/// the application id when fulfilling `billing.invoice.requested`
/// for admissions (see the module header, convergence point 2).
fn on_invoice_paid(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let invoice = hook_ctx.data.get("invoice").cloned().unwrap_or(Value::Null);
    let order_id = invoice
        .get("order_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if order_id.is_empty() {
        return;
    }
    let invoice_id = invoice
        .get("id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let Some(mut application) = state.applications.get_mut(order_id) else {
        return;
    };
    if application.fee_invoice_status == "paid" {
        return;
    }
    application.fee_invoice_status = "paid".to_string();
    if let Some(invoice_id) = invoice_id.clone() {
        application.fee_invoice_id = Some(invoice_id.clone());
    }
    application.updated_at = now();
    let application_id = application.id.clone();
    let period_id = application.period_id.clone();
    drop(application);
    state.emit(
        "admissions.fee_paid",
        json!({
            "application_id": application_id,
            "period_id": period_id,
            "invoice_id": invoice_id,
        }),
    );
}

/// Handle `academic.student.created` ({application_id, student_id}).
/// Links the student record created by the academic system to the
/// enrolled application. Applications are never turned into student
/// records here.
fn on_academic_student_created(state: Arc<AdmissionsState>, hook_ctx: &mut HookContext) {
    let application_id = hook_ctx
        .data
        .get("application_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    let student_id = hook_ctx
        .data
        .get("student_id")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    if application_id.is_empty() || student_id.is_empty() {
        return;
    }
    if let Some(mut application) = state.applications.get_mut(application_id) {
        application.student_id = Some(student_id.to_string());
        if application.status == "accepted" {
            application.status = "enrolled".to_string();
        }
        application.updated_at = now();
    }
}

// ---------------------------------------------------------------------------
// Handlers: periods
// ---------------------------------------------------------------------------

async fn list_periods(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AdmissionPeriod>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AdmissionPeriod> = state
            .periods
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|period| {
                matches_search(
                    &format!("{} {} {}", period.name, period.academic_year, period.term),
                    &params,
                ) && params
                    .filters
                    .get("status")
                    .is_none_or(|status| period.status == *status)
                    && params
                        .filters
                        .get("academic_year")
                        .is_none_or(|year| period.academic_year == *year)
                    && params
                        .filters
                        .get("term")
                        .is_none_or(|term| period.term == *term)
            })
            .collect();
        items.sort_by(|a, b| {
            b.open_date
                .cmp(&a.open_date)
                .then_with(|| a.name.cmp(&b.name))
        });
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    body: web::Json<CreatePeriod>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let input = body.into_inner();
        let academic_year = require_text(&input.academic_year, "academic_year", MAX_SHORT)?;
        let term = require_text(&input.term, "term", MAX_SHORT)?;
        let name = match input.name {
            Some(name) if !name.trim().is_empty() => require_text(&name, "name", MAX_SHORT)?,
            _ => format!("{academic_year} {term}"),
        };
        let open_date = parse_date(&input.open_date, "open_date")?;
        let close_date = parse_date(&input.close_date, "close_date")?;
        if close_date < open_date {
            return Err(UnifiedError::bad_request(
                "close_date must not be before open_date",
            ));
        }
        if state.periods.iter().any(|entry| {
            let period = entry.value();
            period.academic_year == academic_year && period.term == term
        }) {
            return Err(UnifiedError::conflict(format!(
                "A period already exists for academic year \"{academic_year}\" term \"{term}\""
            )));
        }
        let registration_fee_amount = match input.registration_fee_amount {
            Some(amount) => Some(parse_money(amount, "registration_fee_amount")?),
            None => None,
        };
        let fee_currency = match input.fee_currency {
            Some(currency) => valid_currency(&currency)?,
            None => state.setting(KEY_FEE_CURRENCY, SEED_FEE_CURRENCY),
        };
        let registration_form_id = match input.registration_form_id {
            Some(form_id) if !form_id.trim().is_empty() => {
                Some(require_id(&form_id, "registration_form_id")?)
            }
            _ => None,
        };
        let period = AdmissionPeriod {
            id: new_id(),
            academic_year,
            term,
            name,
            open_date,
            close_date,
            status: "draft".to_string(),
            registration_fee_amount,
            fee_currency,
            registration_form_id,
            created_at: now(),
            updated_at: now(),
        };
        state.periods.insert(period.id.clone(), period.clone());
        state.emit(
            "admissions.period_created",
            json!({
                "period_id": period.id,
                "academic_year": period.academic_year,
                "term": period.term,
            }),
        );
        Ok(period)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        state.period(path.as_str())
    })();
    json::respond(result, debug)
}

async fn update_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePeriod>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let input = body.into_inner();
        let period_id = path.as_str().to_string();
        let mut period = state.period(&period_id)?;
        if period.status != "draft" && period.status != "closed" {
            return Err(UnifiedError::conflict(
                "Only draft or closed periods can be updated; open periods must be closed first",
            ));
        }
        if let Some(academic_year) = input.academic_year {
            period.academic_year = require_text(&academic_year, "academic_year", MAX_SHORT)?;
        }
        if let Some(term) = input.term {
            period.term = require_text(&term, "term", MAX_SHORT)?;
        }
        if let Some(name) = input.name {
            period.name = require_text(&name, "name", MAX_SHORT)?;
        }
        let mut open_date = period.open_date;
        let mut close_date = period.close_date;
        if let Some(raw) = input.open_date {
            open_date = parse_date(&raw, "open_date")?;
        }
        if let Some(raw) = input.close_date {
            close_date = parse_date(&raw, "close_date")?;
        }
        if close_date < open_date {
            return Err(UnifiedError::bad_request(
                "close_date must not be before open_date",
            ));
        }
        period.open_date = open_date;
        period.close_date = close_date;
        if let Some(amount) = input.registration_fee_amount {
            period.registration_fee_amount = match amount {
                Some(value) => Some(parse_money(value, "registration_fee_amount")?),
                None => None,
            };
        }
        if let Some(currency) = input.fee_currency {
            period.fee_currency = valid_currency(&currency)?;
        }
        if let Some(form_id) = input.registration_form_id {
            period.registration_form_id = match form_id {
                Some(value) if !value.trim().is_empty() => {
                    Some(require_id(&value, "registration_form_id")?)
                }
                _ => None,
            };
        }
        if let Some(status) = input.status {
            if status != "archived" {
                return Err(UnifiedError::bad_request(
                    "status can only be changed to archived here; use the open and close endpoints for the other transitions",
                ));
            }
            if period.status != "closed" {
                return Err(UnifiedError::conflict(
                    "Only a closed period can be archived",
                ));
            }
            period.status = status;
        }
        if state.periods.iter().any(|entry| {
            let other = entry.value();
            other.id != period.id
                && other.academic_year == period.academic_year
                && other.term == period.term
        }) {
            return Err(UnifiedError::conflict(format!(
                "A period already exists for academic year \"{}\" term \"{}\"",
                period.academic_year, period.term
            )));
        }
        period.updated_at = now();
        state.periods.insert(period.id.clone(), period.clone());
        Ok(period)
    })();
    json::respond(result, debug)
}

async fn delete_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft period can be deleted",
            ));
        }
        if state
            .applications
            .iter()
            .any(|entry| entry.value().period_id == period_id)
        {
            return Err(UnifiedError::conflict(
                "Period still has applications; archive it instead",
            ));
        }
        state
            .quotas
            .retain(|_, quota| quota.period_id != period_id);
        state
            .stages
            .retain(|_, stage| stage.period_id != period_id);
        state
            .required_documents
            .retain(|_, document| document.period_id != period_id);
        state
            .periods
            .remove(period_id.as_str())
            .map(|_| json!({ "deleted": true, "id": period_id }))
            .ok_or_else(|| UnifiedError::not_found("Admission period not found"))
    })();
    json::respond(result, debug)
}

async fn open_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let mut period = state.period(&period_id)?;
        if period.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft period can be opened",
            ));
        }
        if period.close_date < today() {
            return Err(UnifiedError::conflict(
                "close_date is already in the past; extend it before opening",
            ));
        }
        period.status = "open".to_string();
        period.updated_at = now();
        state.periods.insert(period.id.clone(), period.clone());
        state.emit(
            "admissions.period_opened",
            json!({ "period_id": period.id }),
        );
        Ok(period)
    })();
    json::respond(result, debug)
}

async fn close_period(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionPeriod> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let mut period = state.period(&period_id)?;
        if period.status != "open" {
            return Err(UnifiedError::conflict(
                "Only an open period can be closed",
            ));
        }
        period.status = "closed".to_string();
        period.updated_at = now();
        state.periods.insert(period.id.clone(), period.clone());
        state.emit(
            "admissions.period_closed",
            json!({ "period_id": period.id }),
        );
        Ok(period)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: quotas
// ---------------------------------------------------------------------------

async fn list_quotas(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PeriodQuota>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let mut items: Vec<PeriodQuota> = state
            .quotas
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|quota| {
                quota.period_id == period_id
                    && matches_search(&format!("{} {}", quota.program_id, quota.level), &params)
                    && params
                        .filters
                        .get("program_id")
                        .is_none_or(|program| quota.program_id == *program)
                    && params
                        .filters
                        .get("level")
                        .is_none_or(|level| quota.level == *level)
            })
            .collect();
        items.sort_by(|a, b| {
            a.program_id
                .cmp(&b.program_id)
                .then_with(|| a.level.cmp(&b.level))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_quota(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuota>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PeriodQuota> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Quotas cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let program_id = require_id(&input.program_id, "program_id")?;
        let level = require_text(&input.level, "level", MAX_SHORT)?;
        if input.seats == 0 {
            return Err(UnifiedError::bad_request("seats must be at least 1"));
        }
        if state.quotas.iter().any(|entry| {
            let quota = entry.value();
            quota.period_id == period_id
                && quota.program_id == program_id
                && quota.level == level
        }) {
            return Err(UnifiedError::conflict(format!(
                "A quota already exists for program \"{program_id}\" level \"{level}\" in this period",
            )));
        }
        let quota = PeriodQuota {
            id: new_id(),
            period_id,
            program_id,
            level,
            seats: input.seats,
            created_at: now(),
            updated_at: now(),
        };
        state.quotas.insert(quota.id.clone(), quota.clone());
        Ok(quota)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_quota(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateQuota>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PeriodQuota> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, quota_id) = path.into_inner();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Quotas cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let mut quota = state
            .quotas
            .get(&quota_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quota not found"))?;
        if quota.period_id != period_id {
            return Err(UnifiedError::not_found("Quota not found"));
        }
        if let Some(seats) = input.seats {
            if seats == 0 {
                return Err(UnifiedError::bad_request("seats must be at least 1"));
            }
            let taken = state.accepted_count(&period_id, &quota.program_id, &quota.level);
            if seats < taken {
                return Err(UnifiedError::conflict(format!(
                    "seats cannot drop below the {taken} already accepted for this program and level",
                )));
            }
            quota.seats = seats;
        }
        quota.updated_at = now();
        state.quotas.insert(quota.id.clone(), quota.clone());
        Ok(quota)
    })();
    json::respond(result, debug)
}

async fn delete_quota(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, quota_id) = path.into_inner();
        let quota = state
            .quotas
            .get(&quota_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quota not found"))?;
        if quota.period_id != period_id {
            return Err(UnifiedError::not_found("Quota not found"));
        }
        if state.accepted_count(&period_id, &quota.program_id, &quota.level) > 0 {
            return Err(UnifiedError::conflict(
                "Quota still has accepted applications; lower the seats instead of deleting it",
            ));
        }
        state
            .quotas
            .remove(quota_id.as_str())
            .map(|_| json!({ "deleted": true, "id": quota_id }))
            .ok_or_else(|| UnifiedError::not_found("Quota not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: stage definitions
// ---------------------------------------------------------------------------

async fn list_stages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StageDefinition>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let stages = state.stages_of(&period_id);
        let items: Vec<StageDefinition> = stages
            .into_iter()
            .filter(|stage| {
                matches_search(&format!("{} {}", stage.stage_key, stage.name), &params)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_stage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateStage>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StageDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Stages cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let stage_key = slugify(&input.stage_key);
        if stage_key.is_empty() {
            return Err(UnifiedError::bad_request(
                "stage_key must contain at least one letter or digit",
            ));
        }
        let name = require_text(&input.name, "name", MAX_SHORT)?;
        let weight = match input.weight {
            Some(weight) => {
                if !weight.is_finite() || weight < 0.0 || weight > 1000.0 {
                    return Err(UnifiedError::bad_request(
                        "weight must be between 0 and 1000",
                    ));
                }
                round4(weight)
            }
            None => 1.0,
        };
        let scale_max = state.score_scale_max();
        let pass_score = match input.pass_score {
            Some(score) => {
                if !score.is_finite() || score < 0.0 || score > scale_max {
                    return Err(UnifiedError::bad_request(format!(
                        "pass_score must be between 0 and {scale_max}"
                    )));
                }
                Some(round4(score))
            }
            None => None,
        };
        if state.stages.iter().any(|entry| {
            let stage = entry.value();
            stage.period_id == period_id && stage.stage_key == stage_key
        }) {
            return Err(UnifiedError::conflict(format!(
                "A stage with key \"{stage_key}\" already exists in this period",
            )));
        }
        let position = match input.position {
            Some(position) => position,
            None => state
                .stages_of(&period_id)
                .iter()
                .map(|stage| stage.position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(1),
        };
        let stage = StageDefinition {
            id: new_id(),
            period_id,
            stage_key,
            name,
            position,
            weight,
            pass_score,
            required: input.required.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.stages.insert(stage.id.clone(), stage.clone());
        Ok(stage)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_stage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateStage>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StageDefinition> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, stage_id) = path.into_inner();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Stages cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let mut stage = state
            .stages
            .get(&stage_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))?;
        if stage.period_id != period_id {
            return Err(UnifiedError::not_found("Stage not found"));
        }
        if let Some(name) = input.name {
            stage.name = require_text(&name, "name", MAX_SHORT)?;
        }
        if let Some(position) = input.position {
            stage.position = position;
        }
        if let Some(weight) = input.weight {
            if !weight.is_finite() || weight < 0.0 || weight > 1000.0 {
                return Err(UnifiedError::bad_request(
                    "weight must be between 0 and 1000",
                ));
            }
            stage.weight = round4(weight);
        }
        if let Some(pass_score) = input.pass_score {
            let scale_max = state.score_scale_max();
            stage.pass_score = match pass_score {
                Some(score) => {
                    if !score.is_finite() || score < 0.0 || score > scale_max {
                        return Err(UnifiedError::bad_request(format!(
                            "pass_score must be between 0 and {scale_max}"
                        )));
                    }
                    Some(round4(score))
                }
                None => None,
            };
        }
        if let Some(required) = input.required {
            stage.required = required;
        }
        stage.updated_at = now();
        state.stages.insert(stage.id.clone(), stage.clone());
        // Collect the ids first: refreshing totals mutates the
        // applications map, which must not happen while iterating it.
        let application_ids: Vec<String> = state
            .applications
            .iter()
            .filter(|entry| entry.value().period_id == period_id)
            .map(|entry| entry.value().id.clone())
            .collect();
        for application_id in application_ids {
            state.refresh_total(&application_id);
        }
        Ok(stage)
    })();
    json::respond(result, debug)
}

async fn delete_stage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, stage_id) = path.into_inner();
        let stage = state
            .stages
            .get(&stage_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))?;
        if stage.period_id != period_id {
            return Err(UnifiedError::not_found("Stage not found"));
        }
        if state
            .scores
            .iter()
            .any(|entry| entry.value().stage_id == stage_id)
        {
            return Err(UnifiedError::conflict(
                "Stage still has recorded scores; keep it and set its weight to 0 instead",
            ));
        }
        state
            .stages
            .remove(stage_id.as_str())
            .map(|_| json!({ "deleted": true, "id": stage_id }))
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: required documents
// ---------------------------------------------------------------------------

async fn list_required_documents(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<RequiredDocument>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let documents = state.required_documents_of(&period_id);
        let items: Vec<RequiredDocument> = documents
            .into_iter()
            .filter(|document| {
                matches_search(&format!("{} {}", document.document_key, document.title), &params)
                    && params
                        .filters
                        .get("required")
                        .is_none_or(|required| {
                            document.required.to_string() == *required
                        })
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_required_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRequiredDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RequiredDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Required documents cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let document_key = slugify(&input.document_key);
        if document_key.is_empty() {
            return Err(UnifiedError::bad_request(
                "document_key must contain at least one letter or digit",
            ));
        }
        let title = require_text(&input.title, "title", MAX_SHORT)?;
        let description = optional_text(&input.description, "description", MAX_TEXT)?;
        if state.required_documents.iter().any(|entry| {
            let document = entry.value();
            document.period_id == period_id && document.document_key == document_key
        }) {
            return Err(UnifiedError::conflict(format!(
                "A required document with key \"{document_key}\" already exists in this period",
            )));
        }
        let document = RequiredDocument {
            id: new_id(),
            period_id,
            document_key,
            title,
            description,
            required: input.required.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state
            .required_documents
            .insert(document.id.clone(), document.clone());
        Ok(document)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_required_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateRequiredDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RequiredDocument> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, document_id) = path.into_inner();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Required documents cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let mut document = state
            .required_documents
            .get(&document_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Required document not found"))?;
        if document.period_id != period_id {
            return Err(UnifiedError::not_found("Required document not found"));
        }
        if let Some(title) = input.title {
            document.title = require_text(&title, "title", MAX_SHORT)?;
        }
        if let Some(description) = input.description {
            document.description = match description {
                Some(value) => Some(require_text(&value, "description", MAX_TEXT)?),
                None => None,
            };
        }
        if let Some(required) = input.required {
            document.required = required;
        }
        document.updated_at = now();
        state
            .required_documents
            .insert(document.id.clone(), document.clone());
        Ok(document)
    })();
    json::respond(result, debug)
}

async fn delete_required_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, document_id) = path.into_inner();
        let document = state
            .required_documents
            .get(&document_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Required document not found"))?;
        if document.period_id != period_id {
            return Err(UnifiedError::not_found("Required document not found"));
        }
        if state.checklist.iter().any(|entry| {
            entry.value().required_document_id == document_id
                && matches!(entry.value().status.as_str(), "submitted" | "verified")
        }) {
            return Err(UnifiedError::conflict(
                "Required document still has submitted or verified checklist items; keep it and mark it not required instead",
            ));
        }
        state
            .checklist
            .retain(|_, item| item.required_document_id != document_id);
        state
            .required_documents
            .remove(document_id.as_str())
            .map(|_| json!({ "deleted": true, "id": document_id }))
            .ok_or_else(|| UnifiedError::not_found("Required document not found"))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: period summary report
// ---------------------------------------------------------------------------

async fn period_summary(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        let applications: Vec<Application> = state
            .applications
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|application| application.period_id == period_id)
            .collect();
        let mut by_status: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for application in &applications {
            *by_status.entry(application.status.clone()).or_insert(0) += 1;
        }
        let mut quota_usage: Vec<Value> = Vec::new();
        for quota in state.quotas.iter() {
            let quota = quota.value();
            if quota.period_id != period_id {
                continue;
            }
            let accepted = state.accepted_count(&period_id, &quota.program_id, &quota.level);
            quota_usage.push(json!({
                "program_id": quota.program_id,
                "level": quota.level,
                "seats": quota.seats,
                "accepted": accepted,
                "free": quota.seats.saturating_sub(accepted),
            }));
        }
        let waiting = state
            .waitlist
            .iter()
            .filter(|entry| {
                entry.value().period_id == period_id && entry.value().status == "waiting"
            })
            .count() as u64;
        let published_results = state
            .results
            .iter()
            .filter(|entry| entry.value().period_id == period_id && entry.value().published)
            .count() as u64;
        let checklist_items: Vec<ChecklistItem> = state
            .checklist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.period_id == period_id)
            .collect();
        let required_items = checklist_items.iter().filter(|item| item.required).count() as u64;
        let verified_items = checklist_items
            .iter()
            .filter(|item| item.required && item.status == "verified")
            .count() as u64;
        Ok(json!({
            "period": period,
            "application_count": applications.len() as u64,
            "by_status": by_status,
            "quota_usage": quota_usage,
            "waitlist_waiting": waiting,
            "results_published": published_results,
            "required_checklist_items": required_items,
            "verified_checklist_items": verified_items,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: applications
// ---------------------------------------------------------------------------

async fn list_applications(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Application>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_READ_ALL);
        let mut items: Vec<Application> = state
            .applications
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|application| {
                (read_all || application.applicant_user_id == caller.actor)
                    && matches_search(
                        &format!("{} {} {}", application.full_name, application.email, application.program_id),
                        &params,
                    )
                    && params
                        .filters
                        .get("period_id")
                        .is_none_or(|period| application.period_id == *period)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| application.status == *status)
                    && params
                        .filters
                        .get("program_id")
                        .is_none_or(|program| application.program_id == *program)
                    && params
                        .filters
                        .get("level")
                        .is_none_or(|level| application.level == *level)
                    && params
                        .filters
                        .get("applicant_user_id")
                        .is_none_or(|user| application.applicant_user_id == *user)
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    body: web::Json<CreateApplication>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let caller = caller(&req)?;
        let input = body.into_inner();
        let applicant_user_id = match input.applicant_user_id {
            Some(user_id) if !user_id.trim().is_empty() => {
                let user_id = require_id(&user_id, "applicant_user_id")?;
                if user_id != caller.actor
                    && !check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)
                {
                    return Err(UnifiedError::forbidden(
                        "Only admissions managers may create an application for another user",
                    ));
                }
                user_id
            }
            _ => {
                require_permission(&ctx, &caller.role, PERM_ADMISSIONS_APPLY)?;
                caller.actor.clone()
            }
        };
        let period = state.period(&input.period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Applications cannot be created for an archived period",
            ));
        }
        let full_name = require_text(&input.full_name, "full_name", MAX_TEXT)?;
        let email = valid_email(&input.email)?;
        let phone = optional_text(&input.phone, "phone", MAX_SHORT)?;
        let program_id = require_id(&input.program_id, "program_id")?;
        let level = require_text(&input.level, "level", MAX_SHORT)?;
        let form_submission_id = match input.form_submission_id {
            Some(value) if !value.trim().is_empty() => {
                Some(require_id(&value, "form_submission_id")?)
            }
            _ => None,
        };
        if state.applications.iter().any(|entry| {
            let application = entry.value();
            application.period_id == period.id
                && application.applicant_user_id == applicant_user_id
                && application.status != "withdrawn"
        }) {
            return Err(UnifiedError::conflict(
                "An active application already exists for this applicant in this period",
            ));
        }
        if let Some(submission_id) = &form_submission_id {
            if state.applications.iter().any(|entry| {
                entry
                    .value()
                    .form_submission_id
                    .as_deref()
                    == Some(submission_id.as_str())
            }) {
                return Err(UnifiedError::conflict(
                    "An application already exists for this form submission",
                ));
            }
        }
        let application = Application {
            id: new_id(),
            period_id: period.id.clone(),
            applicant_user_id,
            full_name,
            email,
            phone,
            program_id,
            level,
            form_submission_id,
            status: "draft".to_string(),
            total_score: None,
            fee_amount: period.registration_fee_amount,
            fee_currency: period.fee_currency.clone(),
            fee_invoice_status: "none".to_string(),
            fee_invoice_id: None,
            student_id: None,
            source: "manual".to_string(),
            created_at: now(),
            updated_at: now(),
        };
        state.ensure_checklist(&application);
        state.applications.insert(application.id.clone(), application.clone());
        state.emit(
            "admissions.application_created",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "applicant_user_id": application.applicant_user_id,
                "source": "manual",
            }),
        );
        Ok(application)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        load_application_for_read(&state, &ctx, &caller, path.as_str())
    })();
    json::respond(result, debug)
}

async fn update_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateApplication>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let input = body.into_inner();
        let application_id = path.as_str().to_string();
        let mut application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may update this application",
            ));
        }
        if application.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft application can be updated",
            ));
        }
        if let Some(full_name) = input.full_name {
            application.full_name = require_text(&full_name, "full_name", MAX_TEXT)?;
        }
        if let Some(email) = input.email {
            application.email = valid_email(&email)?;
        }
        if let Some(phone) = input.phone {
            application.phone = match phone {
                Some(value) => Some(require_text(&value, "phone", MAX_SHORT)?),
                None => None,
            };
        }
        if let Some(program_id) = input.program_id {
            application.program_id = require_id(&program_id, "program_id")?;
        }
        if let Some(level) = input.level {
            application.level = require_text(&level, "level", MAX_SHORT)?;
        }
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        Ok(application)
    })();
    json::respond(result, debug)
}

async fn delete_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let application = state.application(&application_id)?;
        if !matches!(application.status.as_str(), "draft" | "withdrawn" | "rejected") {
            return Err(UnifiedError::conflict(
                "Only a draft, withdrawn, or rejected application can be deleted",
            ));
        }
        state
            .scores
            .retain(|_, score| score.application_id != application_id);
        state
            .checklist
            .retain(|_, item| item.application_id != application_id);
        state
            .waitlist
            .retain(|_, entry| entry.application_id != application_id);
        state
            .results
            .retain(|_, result| result.application_id != application_id);
        state
            .applications
            .remove(application_id.as_str())
            .map(|_| json!({ "deleted": true, "id": application_id }))
            .ok_or_else(|| UnifiedError::not_found("Application not found"))
    })();
    json::respond(result, debug)
}

async fn submit_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let mut application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may submit this application",
            ));
        }
        if application.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft application can be submitted",
            ));
        }
        if application.full_name.trim().is_empty()
            || application.email.trim().is_empty()
            || application.program_id.trim().is_empty()
            || application.level.trim().is_empty()
            || application.applicant_user_id.trim().is_empty()
        {
            return Err(UnifiedError::bad_request(
                "full_name, email, program_id, level, and applicant_user_id must be filled in before submitting",
            ));
        }
        let period = state.period(&application.period_id)?;
        if !state.period_accepts_submissions(&period) {
            return Err(UnifiedError::conflict(
                "Admission period is not open for submissions",
            ));
        }
        application.status = "submitted".to_string();
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        state.emit(
            "admissions.application_submitted",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
            }),
        );
        Ok(application)
    })();
    json::respond(result, debug)
}

async fn withdraw_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let mut application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may withdraw this application",
            ));
        }
        if !matches!(
            application.status.as_str(),
            "submitted" | "under_review" | "waitlisted" | "accepted"
        ) {
            return Err(UnifiedError::conflict(format!(
                "An application in status \"{}\" cannot be withdrawn",
                application.status
            )));
        }
        let freed_seat = application.status == "accepted";
        let period_id = application.period_id.clone();
        application.status = "withdrawn".to_string();
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        for mut entry in state.waitlist.iter_mut() {
            if entry.value().application_id == application_id
                && entry.value().status == "waiting"
            {
                entry.status = "expired".to_string();
                entry.updated_at = now();
            }
        }
        state.emit(
            "admissions.application_withdrawn",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
            }),
        );
        let mut promoted: Option<Application> = None;
        if freed_seat {
            promoted = state.promote_top_waitlisted(&period_id);
        }
        Ok(json!({
            "application": application,
            "waitlist_promoted": promoted,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: scores
// ---------------------------------------------------------------------------

async fn list_scores(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ApplicationScore>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let params = query.into_inner();
        let scores = state.scores_of(&application.id);
        let items: Vec<ApplicationScore> = scores
            .into_iter()
            .filter(|score| {
                matches_search(&score.stage_key, &params)
                    && params
                        .filters
                        .get("stage_id")
                        .is_none_or(|stage| score.stage_id == *stage)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn create_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScore>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApplicationScore> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_SCORE)?;
        let application_id = path.as_str().to_string();
        let application = state.application(&application_id)?;
        if matches!(application.status.as_str(), "withdrawn" | "enrolled") {
            return Err(UnifiedError::conflict(format!(
                "Scores cannot be recorded for an application in status \"{}\"",
                application.status
            )));
        }
        let input = body.into_inner();
        let stage = state
            .stages
            .get(&input.stage_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))?;
        if stage.period_id != application.period_id {
            return Err(UnifiedError::not_found("Stage not found"));
        }
        let scale_max = state.score_scale_max();
        if !input.score.is_finite() || input.score < 0.0 || input.score > scale_max {
            return Err(UnifiedError::bad_request(format!(
                "score must be between 0 and {scale_max}"
            )));
        }
        if state.scores.iter().any(|entry| {
            let score = entry.value();
            score.application_id == application_id && score.stage_id == stage.id
        }) {
            return Err(UnifiedError::conflict(format!(
                "A score already exists for stage \"{}\"; update it instead",
                stage.stage_key
            )));
        }
        let note = optional_text(&input.note, "note", MAX_TEXT)?;
        let score = ApplicationScore {
            id: new_id(),
            application_id: application.id.clone(),
            period_id: application.period_id.clone(),
            stage_id: stage.id.clone(),
            stage_key: stage.stage_key.clone(),
            score: round4(input.score),
            scored_by: caller.actor.clone(),
            note,
            created_at: now(),
            updated_at: now(),
        };
        state.scores.insert(score.id.clone(), score.clone());
        let total = state.refresh_total(&application.id);
        if application.status == "submitted" {
            if let Some(mut record) = state.applications.get_mut(&application.id) {
                record.status = "under_review".to_string();
                record.updated_at = now();
            }
        }
        state.emit(
            "admissions.score_recorded",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "stage_id": stage.id,
                "score": score.score,
                "total_score": total,
            }),
        );
        Ok(score)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateScore>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApplicationScore> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_SCORE)?;
        let (application_id, score_id) = path.into_inner();
        let application = state.application(&application_id)?;
        let input = body.into_inner();
        let mut score = state
            .scores
            .get(&score_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Score not found"))?;
        if score.application_id != application_id {
            return Err(UnifiedError::not_found("Score not found"));
        }
        if let Some(value) = input.score {
            let scale_max = state.score_scale_max();
            if !value.is_finite() || value < 0.0 || value > scale_max {
                return Err(UnifiedError::bad_request(format!(
                    "score must be between 0 and {scale_max}"
                )));
            }
            score.score = round4(value);
        }
        if let Some(note) = input.note {
            score.note = match note {
                Some(value) => Some(require_text(&value, "note", MAX_TEXT)?),
                None => None,
            };
        }
        score.scored_by = caller.actor.clone();
        score.updated_at = now();
        state.scores.insert(score.id.clone(), score.clone());
        let total = state.refresh_total(&application.id);
        state.emit(
            "admissions.score_recorded",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "stage_id": score.stage_id,
                "score": score.score,
                "total_score": total,
            }),
        );
        Ok(score)
    })();
    json::respond(result, debug)
}

async fn delete_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_SCORE)?;
        let (application_id, score_id) = path.into_inner();
        let application = state.application(&application_id)?;
        let score = state
            .scores
            .get(&score_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Score not found"))?;
        if score.application_id != application_id {
            return Err(UnifiedError::not_found("Score not found"));
        }
        state.scores.remove(score_id.as_str());
        state.refresh_total(&application.id);
        Ok(json!({ "deleted": true, "id": score_id }))
    })();
    json::respond(result, debug)
}

async fn get_score_total(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScoreTotal> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        Ok(state.compute_total(&application.id))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: document checklist
// ---------------------------------------------------------------------------

async fn get_checklist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ChecklistItem>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        state.ensure_checklist(&application);
        let params = query.into_inner();
        let items = state.checklist_of(&application.id);
        let items: Vec<ChecklistItem> = items
            .into_iter()
            .filter(|item| {
                matches_search(&format!("{} {}", item.document_key, item.title), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| item.status == *status)
                    && params
                        .filters
                        .get("required")
                        .is_none_or(|required| item.required.to_string() == *required)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn attach_checklist_document(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<AttachDocument>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let (application_id, item_id) = path.into_inner();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may attach a document",
            ));
        }
        let input = body.into_inner();
        let document_id = require_id(&input.document_id, "document_id")?;
        let mut item = state
            .checklist
            .get(&item_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.application_id != application_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        if item.status == "verified" {
            return Err(UnifiedError::conflict(
                "Checklist item is already verified",
            ));
        }
        item.document_id = Some(document_id.clone());
        item.status = "submitted".to_string();
        item.rejection_reason = None;
        item.updated_at = now();
        state.checklist.insert(item.id.clone(), item.clone());
        state.emit(
            "admissions.document_attached",
            json!({
                "application_id": application.id,
                "checklist_item_id": item.id,
                "document_id": document_id,
            }),
        );
        Ok(item)
    })();
    json::respond(result, debug)
}

async fn verify_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (application_id, item_id) = path.into_inner();
        let mut item = state
            .checklist
            .get(&item_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.application_id != application_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        if item.status != "submitted" {
            return Err(UnifiedError::conflict(
                "Only a submitted checklist item can be verified",
            ));
        }
        item.status = "verified".to_string();
        item.verified_by = Some(caller.actor.clone());
        item.verified_at = Some(now());
        item.rejection_reason = None;
        item.updated_at = now();
        state.checklist.insert(item.id.clone(), item.clone());
        state.emit(
            "admissions.document_verified",
            json!({
                "application_id": application_id,
                "checklist_item_id": item.id,
            }),
        );
        Ok(item)
    })();
    json::respond(result, debug)
}

async fn reject_checklist_item(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<RejectChecklistItem>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ChecklistItem> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (application_id, item_id) = path.into_inner();
        let input = body.into_inner();
        let reason = require_text(&input.reason, "reason", MAX_TEXT)?;
        let mut item = state
            .checklist
            .get(&item_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Checklist item not found"))?;
        if item.application_id != application_id {
            return Err(UnifiedError::not_found("Checklist item not found"));
        }
        if item.status != "submitted" {
            return Err(UnifiedError::conflict(
                "Only a submitted checklist item can be rejected",
            ));
        }
        item.status = "rejected".to_string();
        item.rejection_reason = Some(reason);
        item.updated_at = now();
        state.checklist.insert(item.id.clone(), item.clone());
        Ok(item)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: fee, result, decision, enrolment
// ---------------------------------------------------------------------------

async fn request_fee_invoice(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<FeeAction>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let mut application = state.application(&application_id)?;
        let input = body.into_inner();
        match input.action.as_str() {
            "request" => {
                let amount = match application.fee_amount {
                    Some(amount) => amount,
                    None => {
                        return Err(UnifiedError::conflict(
                            "This application has no registration fee configured",
                        ));
                    }
                };
                if application.fee_invoice_status == "paid" {
                    return Err(UnifiedError::conflict("The fee is already paid"));
                }
                if application.fee_invoice_status == "waived" {
                    return Err(UnifiedError::conflict("The fee was waived"));
                }
                application.fee_invoice_status = "requested".to_string();
                application.updated_at = now();
                state
                    .applications
                    .insert(application.id.clone(), application.clone());
                // The reference lets billing echo the application back:
                // billing sets the created invoice's `order_id` to the
                // application id, so `invoice.paid` can mark it paid.
                let reference = format!("admissions:{}", application.id);
                state.emit(
                    "admissions.fee_invoice_requested",
                    json!({
                        "application_id": application.id,
                        "period_id": application.period_id,
                        "amount": amount,
                        "currency": application.fee_currency,
                        "reference": reference,
                    }),
                );
                state.emit(
                    "billing.invoice.requested",
                    json!({
                        "application_id": application.id,
                        "period_id": application.period_id,
                        "customer_user_id": application.applicant_user_id,
                        "customer_name": application.full_name,
                        "customer_email": application.email,
                        "amount": amount,
                        "currency": application.fee_currency,
                        "description": format!("Registration fee for application {}", application.id),
                        "reference": reference,
                    }),
                );
                state.request_notification(
                    &application.applicant_user_id,
                    "Registration fee invoice requested",
                    "Your registration fee invoice was requested. Please complete the payment to continue enrolment.",
                    "admissions.fee_invoice_requested",
                );
            }
            "waive" => {
                if application.fee_invoice_status == "paid" {
                    return Err(UnifiedError::conflict("The fee is already paid"));
                }
                application.fee_invoice_status = "waived".to_string();
                application.updated_at = now();
                state
                    .applications
                    .insert(application.id.clone(), application.clone());
            }
            "mark_paid" => {
                let invoice_id = match input.invoice_id {
                    Some(value) if !value.trim().is_empty() => {
                        require_id(&value, "invoice_id")?
                    }
                    _ => {
                        return Err(UnifiedError::bad_request(
                            "invoice_id is required for action mark_paid",
                        ));
                    }
                };
                application.fee_invoice_status = "paid".to_string();
                application.fee_invoice_id = Some(invoice_id.clone());
                application.updated_at = now();
                state
                    .applications
                    .insert(application.id.clone(), application.clone());
                state.emit(
                    "admissions.fee_paid",
                    json!({
                        "application_id": application.id,
                        "period_id": application.period_id,
                        "invoice_id": invoice_id,
                    }),
                );
            }
            _ => {
                return Err(UnifiedError::bad_request(
                    "action must be one of: request, waive, mark_paid",
                ));
            }
        }
        Ok(application)
    })();
    json::respond(result, debug)
}

async fn get_application_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionResult> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let result = state
            .results
            .iter()
            .find(|entry| entry.value().application_id == application_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
        // An applicant sees their own result only once it is
        // published (and only when the visibility default allows it);
        // staff with read-all always see it. Unpublished results are
        // hidden from applicants as 404, so the decision cannot be
        // probed early.
        let is_staff = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_READ_ALL);
        if !is_staff {
            if !result.published || !state.results_visible_to_applicants() {
                return Err(UnifiedError::not_found("Result not found"));
            }
        }
        Ok(result)
    })();
    json::respond(result, debug)
}

async fn decide_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DecideApplication>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let mut application = state.application(&application_id)?;
        let input = body.into_inner();
        let decisions = state.vocabulary(KEY_DECISIONS, &["accepted", "rejected", "waitlisted"]);
        if !decisions.iter().any(|decision| decision == &input.decision) {
            return Err(UnifiedError::bad_request(format!(
                "decision must be one of: {}",
                decisions.join(", ")
            )));
        }
        if !matches!(
            application.status.as_str(),
            "submitted" | "under_review" | "waitlisted" | "accepted" | "rejected"
        ) {
            return Err(UnifiedError::conflict(format!(
                "An application in status \"{}\" cannot be decided",
                application.status
            )));
        }
        let note = optional_text(&input.note, "note", MAX_TEXT)?;
        let mut waitlist_entry: Option<WaitlistEntry> = None;
        match input.decision.as_str() {
            "accepted" => {
                let total = state.compute_total(&application.id);
                if !total.all_required_stages_passed {
                    return Err(UnifiedError::conflict(
                        "Not every required selection stage has a passing score yet",
                    ));
                }
                if !state.seat_available(&application)? {
                    return Err(UnifiedError::conflict(
                        "No seat is free for this program and level; place the application on the waitlist instead",
                    ));
                }
            }
            "waitlisted" => {
                if state.waitlist.iter().any(|entry| {
                    entry.value().application_id == application_id
                        && entry.value().status == "waiting"
                }) {
                    return Err(UnifiedError::conflict(
                        "Application is already on the waitlist",
                    ));
                }
            }
            _ => {}
        }
        match input.decision.as_str() {
            "accepted" => {
                application.status = "accepted".to_string();
            }
            "rejected" => {
                application.status = "rejected".to_string();
                // A rejected application frees no seat, but a
                // previously waitlisted one leaves the waitlist.
                for mut entry in state.waitlist.iter_mut() {
                    if entry.value().application_id == application_id
                        && entry.value().status == "waiting"
                    {
                        entry.status = "expired".to_string();
                        entry.updated_at = now();
                    }
                }
            }
            "waitlisted" => {
                application.status = "waitlisted".to_string();
                let position = state
                    .waitlist
                    .iter()
                    .filter(|entry| {
                        entry.value().period_id == application.period_id
                            && entry.value().status == "waiting"
                    })
                    .count() as u32
                    + 1;
                let entry = WaitlistEntry {
                    id: new_id(),
                    period_id: application.period_id.clone(),
                    application_id: application.id.clone(),
                    position,
                    status: "waiting".to_string(),
                    promoted_at: None,
                    created_at: now(),
                    updated_at: now(),
                };
                state.waitlist.insert(entry.id.clone(), entry.clone());
                waitlist_entry = Some(entry);
            }
            _ => {}
        }
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        let result = state.upsert_result(
            &application,
            &input.decision,
            &caller.actor,
            note,
        );
        state.emit(
            "admissions.decision_made",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "decision": input.decision,
            }),
        );
        Ok(json!({
            "application": application,
            "result": result,
            "waitlist_entry": waitlist_entry,
        }))
    })();
    json::respond(result, debug)
}

async fn enroll_application(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Application> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let application_id = path.as_str().to_string();
        let mut application = state.application(&application_id)?;
        if application.status != "accepted" {
            return Err(UnifiedError::conflict(
                "Only an accepted application can be enrolled",
            ));
        }
        if application.fee_amount.is_some()
            && !matches!(
                application.fee_invoice_status.as_str(),
                "paid" | "waived"
            )
        {
            return Err(UnifiedError::conflict(
                "The registration fee must be paid or waived before enrolment",
            ));
        }
        if !state.checklist_complete(&application.id) {
            return Err(UnifiedError::conflict(
                "Not every required document is verified yet",
            ));
        }
        let period = state.period(&application.period_id)?;
        application.status = "enrolled".to_string();
        application.updated_at = now();
        state
            .applications
            .insert(application.id.clone(), application.clone());
        // The academic system owns the student record: this event
        // asks it to create one from the admitted application. The
        // `academic.student.created` answer links the student id
        // back onto this application. `enrollment_year` is the
        // leading year of the period's free-text academic year
        // (for example "2026/2027" -> 2026); the academic module
        // derives it the same way when the value is absent.
        let enrollment_year = period
            .academic_year
            .trim()
            .chars()
            .take(4)
            .collect::<String>()
            .parse::<i32>()
            .ok()
            .filter(|year| (1990..=2100).contains(year));
        let payload = json!({
            "application_id": application.id,
            "period_id": application.period_id,
            "applicant_user_id": application.applicant_user_id,
            "full_name": application.full_name,
            "email": application.email,
            "phone": application.phone,
            "program_id": application.program_id,
            "level": application.level,
            "academic_year": period.academic_year,
            "term": period.term,
            "enrollment_year": enrollment_year,
        });
        state.emit("academic.student.create_requested", payload.clone());
        state.emit("admissions.enrollment_requested", payload);
        Ok(application)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: waitlist
// ---------------------------------------------------------------------------

async fn list_waitlist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WaitlistEntry>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let entries = state.waiting_entries(&period_id);
        let items: Vec<WaitlistEntry> = entries
            .into_iter()
            .filter(|entry| {
                params
                    .filters
                    .get("status")
                    .is_none_or(|status| entry.status == *status)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn promote_waitlist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PromoteWaitlist>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let input = body.into_inner();
        let entries = state.waiting_entries(&period_id);
        let entry = match input.application_id {
            Some(application_id) => {
                let application_id = require_id(&application_id, "application_id")?;
                entries
                    .into_iter()
                    .find(|entry| entry.application_id == application_id)
                    .ok_or_else(|| {
                        UnifiedError::not_found(
                            "No waiting waitlist entry for this application in this period",
                        )
                    })?
            }
            None => entries
                .into_iter()
                .next()
                .ok_or_else(|| UnifiedError::not_found("Waitlist is empty"))?,
        };
        let application = state.application(&entry.application_id)?;
        if !state.seat_available(&application)? {
            return Err(UnifiedError::conflict(
                "No seat is free for this program and level yet",
            ));
        }
        let promoted = state.promote_waitlist_entry(&entry, &application);
        Ok(json!({ "application": promoted }))
    })();
    json::respond(result, debug)
}

async fn decline_waitlist_entry(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let entry_id = path.as_str().to_string();
        let entry = state
            .waitlist
            .get(&entry_id)
            .map(|value| value.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Waitlist entry not found"))?;
        let application = load_application_for_read(&state, &ctx, &caller, &entry.application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may decline a waitlist entry",
            ));
        }
        if entry.status != "waiting" {
            return Err(UnifiedError::conflict(format!(
                "A waitlist entry in status \"{}\" cannot be declined",
                entry.status
            )));
        }
        if let Some(mut record) = state.waitlist.get_mut(&entry_id) {
            record.status = "declined".to_string();
            record.updated_at = now();
        }
        if let Some(mut record) = state.applications.get_mut(&application.id) {
            if record.status == "waitlisted" {
                record.status = "rejected".to_string();
                record.updated_at = now();
            }
        }
        state.upsert_result(&application, "rejected", &caller.actor, None);
        Ok(json!({ "declined": true, "id": entry_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: results
// ---------------------------------------------------------------------------

async fn list_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AdmissionResult>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_READ_ALL);
        let mut items: Vec<AdmissionResult> = state
            .results
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|result| {
                result.period_id == period_id
                    && (read_all || result.published)
                    && params
                        .filters
                        .get("decision")
                        .is_none_or(|decision| result.decision == *decision)
                    && params
                        .filters
                        .get("published")
                        .is_none_or(|published| result.published.to_string() == *published)
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn publish_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status != "closed" {
            return Err(UnifiedError::conflict(
                "Results can only be published for a closed period",
            ));
        }
        let mut published: Vec<AdmissionResult> = Vec::new();
        for mut entry in state.results.iter_mut() {
            let result = entry.value_mut();
            if result.period_id == period_id && !result.published {
                result.published = true;
                result.published_at = Some(now());
                result.updated_at = now();
                published.push(result.clone());
            }
        }
        for result in &published {
            if let Some(application) = state
                .applications
                .get(&result.application_id)
                .map(|value| value.value().clone())
            {
                state.notify_result(&application, &result.decision);
            }
        }
        let count = published.len() as u64;
        state.emit(
            "admissions.results_published",
            json!({ "period_id": period_id, "result_count": count }),
        );
        Ok(json!({ "published": count, "period_id": period_id }))
    })();
    json::respond(result, debug)
}

async fn unpublish_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let mut unpublished: u64 = 0;
        for mut entry in state.results.iter_mut() {
            let result = entry.value_mut();
            if result.period_id == period_id && result.published {
                result.published = false;
                result.published_at = None;
                result.updated_at = now();
                unpublished += 1;
            }
        }
        state.emit(
            "admissions.results_unpublished",
            json!({ "period_id": period_id }),
        );
        Ok(json!({ "unpublished": unpublished, "period_id": period_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

async fn list_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AdmissionSetting>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<AdmissionSetting> = state
            .settings
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|setting| matches_search(&format!("{} {}", setting.key, setting.value), &params))
            .collect();
        items.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn update_setting(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    body: web::Json<UpdateSetting>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AdmissionSetting> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let input = body.into_inner();
        let key = require_text(&input.key, "key", MAX_SETTING_KEY)?;
        if !key
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '.' || ch == '_')
        {
            return Err(UnifiedError::bad_request(
                "key must use only lowercase letters, digits, '.' and '_'",
            ));
        }
        let value = require_text(&input.value, "value", MAX_SETTING_VALUE)?;
        // Vocabulary settings must stay non-empty lists; the waitlist
        // order must name a known policy; the visibility flag must be
        // a boolean; the score scale must be a positive number.
        match key.as_str() {
            KEY_PERIOD_STATUSES | KEY_APPLICATION_STATUSES | KEY_CHECKLIST_STATUSES
            | KEY_WAITLIST_STATUSES | KEY_DECISIONS => {
                let parts: Vec<&str> = value
                    .split(',')
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect();
                if parts.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "a vocabulary setting must list at least one value",
                    ));
                }
            }
            KEY_WAITLIST_ORDER => {
                if !WAITLIST_ORDERS.contains(&value.trim()) {
                    return Err(UnifiedError::bad_request(format!(
                        "waitlist order must be one of: {}",
                        WAITLIST_ORDERS.join(", ")
                    )));
                }
            }
            KEY_RESULT_VISIBLE => {
                if !matches!(value.trim().to_lowercase().as_str(), "true" | "false") {
                    return Err(UnifiedError::bad_request(
                        "result visibility must be true or false",
                    ));
                }
            }
            KEY_SCORE_SCALE_MAX => {
                let parsed: f64 = value.trim().parse().map_err(|_| {
                    UnifiedError::bad_request("score scale max must be a number")
                })?;
                if !parsed.is_finite() || parsed <= 0.0 || parsed > 1000.0 {
                    return Err(UnifiedError::bad_request(
                        "score scale max must be between 0 and 1000",
                    ));
                }
            }
            KEY_FEE_CURRENCY => {
                valid_currency(&value)?;
            }
            _ => {}
        }
        let description = match input.description {
            Some(description) => require_text(&description, "description", MAX_TEXT)?,
            None => state
                .settings
                .get(&key)
                .map(|entry| entry.value().description.clone())
                .unwrap_or_default(),
        };
        let setting = AdmissionSetting {
            key: key.clone(),
            value,
            description,
            updated_at: now(),
        };
        state.settings.insert(key, setting.clone());
        Ok(setting)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------

impl Module for AdmissionsModule {
    fn id(&self) -> &'static str {
        "admissions"
    }

    fn name(&self) -> &'static str {
        "Admissions"
    }

    fn description(&self) -> &'static str {
        "New-student registration: admission periods with per-program quotas, online registration through the Forms module, admin-defined selection stages with weighted scoring, result announcement with per-applicant visibility, registration fees through Billing, a document checklist through the Documents module, an ordered waitlist with promotion, and enrolment handoff to the academic system."
    }

    /// Admissions is sold as a product: a customer buys a module
    /// entitlement for it (see `crate::modules::licensing`).
    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn requires(&self) -> &[&'static str] {
        &["forms", "documents", "billing", "notifications"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["forms", "documents", "billing", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Arc::clone(&ctx.hooks);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "forms.mapping.requested",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_forms_mapping_requested(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "forms.submission.received",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_forms_submission_received(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "document.uploaded",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_document_uploaded(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "document.deleted",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_document_deleted(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "invoice.paid",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_invoice_paid(state.clone(), hook_ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            "academic.student.created",
            "admissions",
            Arc::new(move |hook_ctx: &mut HookContext| {
                on_academic_student_created(state.clone(), hook_ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/admissions")
                // Literal segments first: they must win over `/{id}`.
                .route("/settings", web::get().to(list_settings))
                .route("/settings", web::put().to(update_setting))
                .route("/periods", web::get().to(list_periods))
                .route("/periods", web::post().to(create_period))
                .route("/periods/{id}", web::get().to(get_period))
                .route("/periods/{id}", web::patch().to(update_period))
                .route("/periods/{id}", web::delete().to(delete_period))
                .route("/periods/{id}/open", web::post().to(open_period))
                .route("/periods/{id}/close", web::post().to(close_period))
                .route("/periods/{id}/summary", web::get().to(period_summary))
                .route("/periods/{id}/quotas", web::get().to(list_quotas))
                .route("/periods/{id}/quotas", web::post().to(create_quota))
                .route(
                    "/periods/{id}/quotas/{quota_id}",
                    web::patch().to(update_quota),
                )
                .route(
                    "/periods/{id}/quotas/{quota_id}",
                    web::delete().to(delete_quota),
                )
                .route("/periods/{id}/stages", web::get().to(list_stages))
                .route("/periods/{id}/stages", web::post().to(create_stage))
                .route(
                    "/periods/{id}/stages/{stage_id}",
                    web::patch().to(update_stage),
                )
                .route(
                    "/periods/{id}/stages/{stage_id}",
                    web::delete().to(delete_stage),
                )
                .route(
                    "/periods/{id}/required-documents",
                    web::get().to(list_required_documents),
                )
                .route(
                    "/periods/{id}/required-documents",
                    web::post().to(create_required_document),
                )
                .route(
                    "/periods/{id}/required-documents/{document_id}",
                    web::patch().to(update_required_document),
                )
                .route(
                    "/periods/{id}/required-documents/{document_id}",
                    web::delete().to(delete_required_document),
                )
                .route("/periods/{id}/waitlist", web::get().to(list_waitlist))
                .route(
                    "/periods/{id}/waitlist/promote",
                    web::post().to(promote_waitlist),
                )
                .route("/periods/{id}/results", web::get().to(list_results))
                .route(
                    "/periods/{id}/results/publish",
                    web::post().to(publish_results),
                )
                .route(
                    "/periods/{id}/results/unpublish",
                    web::post().to(unpublish_results),
                )
                .route("/applications", web::get().to(list_applications))
                .route("/applications", web::post().to(create_application))
                .route("/applications/{id}", web::get().to(get_application))
                .route("/applications/{id}", web::patch().to(update_application))
                .route("/applications/{id}", web::delete().to(delete_application))
                .route(
                    "/applications/{id}/submit",
                    web::post().to(submit_application),
                )
                .route(
                    "/applications/{id}/withdraw",
                    web::post().to(withdraw_application),
                )
                .route("/applications/{id}/scores", web::get().to(list_scores))
                .route("/applications/{id}/scores", web::post().to(create_score))
                .route(
                    "/applications/{id}/scores/total",
                    web::get().to(get_score_total),
                )
                .route(
                    "/applications/{id}/scores/{score_id}",
                    web::patch().to(update_score),
                )
                .route(
                    "/applications/{id}/scores/{score_id}",
                    web::delete().to(delete_score),
                )
                .route(
                    "/applications/{id}/checklist",
                    web::get().to(get_checklist),
                )
                .route(
                    "/applications/{id}/checklist/{item_id}/attach",
                    web::post().to(attach_checklist_document),
                )
                .route(
                    "/applications/{id}/checklist/{item_id}/verify",
                    web::post().to(verify_checklist_item),
                )
                .route(
                    "/applications/{id}/checklist/{item_id}/reject",
                    web::post().to(reject_checklist_item),
                )
                .route(
                    "/applications/{id}/request-fee-invoice",
                    web::post().to(request_fee_invoice),
                )
                .route(
                    "/applications/{id}/result",
                    web::get().to(get_application_result),
                )
                .route(
                    "/applications/{id}/decide",
                    web::post().to(decide_application),
                )
                .route(
                    "/applications/{id}/enroll",
                    web::post().to(enroll_application),
                )
                .route(
                    "/waitlist/{entry_id}/decline",
                    web::post().to(decline_waitlist_entry),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/admissions/settings", owner: "admissions", description: "List admin-editable admissions settings (vocabularies and defaults)." },
            RouteInfo { method: "PUT", path: "/api/v1/admissions/settings", owner: "admissions", description: "Update one admissions setting (admissions:manage; vocabularies validated)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods", owner: "admissions", description: "List admission periods (paginated, filter by status, academic_year, term)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods", owner: "admissions", description: "Create an admission period (draft; admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}", owner: "admissions", description: "Get one admission period." },
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}", owner: "admissions", description: "Update a draft or closed period (archive only from closed; admissions:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}", owner: "admissions", description: "Delete a draft period with no applications (admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/open", owner: "admissions", description: "Open a draft period for submissions (admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/close", owner: "admissions", description: "Close an open period (admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/summary", owner: "admissions", description: "Period summary: counts by status, quota usage, waitlist, results, checklist completion." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/quotas", owner: "admissions", description: "List per-program, per-level seat quotas of a period (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/quotas", owner: "admissions", description: "Create a seat quota for one program and level (admissions:manage)." },
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}/quotas/{quota_id}", owner: "admissions", description: "Update a quota's seats (never below already-accepted; admissions:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}/quotas/{quota_id}", owner: "admissions", description: "Delete a quota with no accepted applications (admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/stages", owner: "admissions", description: "List selection stage definitions of a period (paginated, definition order)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/stages", owner: "admissions", description: "Create a selection stage (key, weight, pass score, required; admissions:manage)." },
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}/stages/{stage_id}", owner: "admissions", description: "Update a selection stage; totals are recomputed (admissions:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}/stages/{stage_id}", owner: "admissions", description: "Delete a stage with no recorded scores (admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/required-documents", owner: "admissions", description: "List required documents of a period (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/required-documents", owner: "admissions", description: "Define a required document for a period (admissions:manage)." },
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/periods/{id}/required-documents/{document_id}", owner: "admissions", description: "Update a required document (admissions:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/periods/{id}/required-documents/{document_id}", owner: "admissions", description: "Delete a required document with no submitted or verified checklist items (admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/waitlist", owner: "admissions", description: "List the waiting waitlist of a period in promotion order (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/waitlist/promote", owner: "admissions", description: "Promote the top (or a named) waiting entry to accepted when a seat is free (admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/periods/{id}/results", owner: "admissions", description: "List results of a period (applicants see only published ones)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/results/publish", owner: "admissions", description: "Publish all results of a closed period and notify applicants (admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/periods/{id}/results/unpublish", owner: "admissions", description: "Unpublish all results of a period (admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications", owner: "admissions", description: "List applications (applicants see only their own; paginated, filter by period_id, status, program_id, level)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications", owner: "admissions", description: "Create an application (self with admissions:apply, or for another user with admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}", owner: "admissions", description: "Get one application (owner or admissions:read_all)." },
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/applications/{id}", owner: "admissions", description: "Update a draft application (owner or admissions:manage)." },
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/applications/{id}", owner: "admissions", description: "Delete a draft, withdrawn, or rejected application (admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/submit", owner: "admissions", description: "Submit a draft application while the period is open (owner or admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/withdraw", owner: "admissions", description: "Withdraw an application; a freed seat auto-promotes the top waitlisted applicant (owner or admissions:manage)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/scores", owner: "admissions", description: "List stage scores of an application (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/scores", owner: "admissions", description: "Record a stage score; the weighted total is recomputed (admissions:score)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/scores/total", owner: "admissions", description: "Weighted total score with per-stage breakdown and required-stage pass state." },
            RouteInfo { method: "PATCH", path: "/api/v1/admissions/applications/{id}/scores/{score_id}", owner: "admissions", description: "Correct a stage score; the weighted total is recomputed (admissions:score)." },
            RouteInfo { method: "DELETE", path: "/api/v1/admissions/applications/{id}/scores/{score_id}", owner: "admissions", description: "Delete a stage score; the weighted total is recomputed (admissions:score)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/checklist", owner: "admissions", description: "List the document checklist of an application (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/checklist/{item_id}/attach", owner: "admissions", description: "Attach a documents-module document to a checklist item (owner or admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/checklist/{item_id}/verify", owner: "admissions", description: "Verify a submitted checklist item (admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/checklist/{item_id}/reject", owner: "admissions", description: "Reject a submitted checklist item with a reason (admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/request-fee-invoice", owner: "admissions", description: "Request, waive, or manually mark paid the registration fee invoice (admissions:manage; emits billing.invoice.requested)." },
            RouteInfo { method: "GET", path: "/api/v1/admissions/applications/{id}/result", owner: "admissions", description: "Get one application's result (owner only once published; staff always)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/decide", owner: "admissions", description: "Decide an application (accepted requires passing stages and a free seat; waitlisted joins the waitlist; admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/applications/{id}/enroll", owner: "admissions", description: "Enrol an accepted application (fee paid or waived, documents verified; emits academic.student.create_requested; admissions:manage)." },
            RouteInfo { method: "POST", path: "/api/v1/admissions/waitlist/{entry_id}/decline", owner: "admissions", description: "Decline a waitlist entry (owner or admissions:manage)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AdmissionsModule::new()));
}
