// admissions module — Domain types.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

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
    /// Stable key used to match uploads (the slug of the uploaded
    /// file name's stem must equal this key when the file is
    /// uploaded through the documents module with `module_id`
    /// `admissions`).
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
