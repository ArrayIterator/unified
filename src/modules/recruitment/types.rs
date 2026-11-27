//! Recruitment - Shared types: request and response bodies, views, and state shapes.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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
use super::{constants::*, state::*, helpers::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_evaluations::*, handlers_offers::*};

// Types

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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
/// reference to the Departments module -- its existence is not
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
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
/// of the applicant -- see `applicant_view` / `aggregate_scores`.
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
    pub(crate) fn as_str(&self) -> &'static str {
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

// Request bodies

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn applicant_stage_as_str() {
        assert_eq!(ApplicantStage::Applied.as_str(), "applied");
        assert_eq!(ApplicantStage::Screening.as_str(), "screening");
        assert_eq!(ApplicantStage::Interview.as_str(), "interview");
        assert_eq!(ApplicantStage::Offer.as_str(), "offer");
        assert_eq!(ApplicantStage::Hired.as_str(), "hired");
        assert_eq!(ApplicantStage::Rejected.as_str(), "rejected");
    }

    #[test]
    fn job_status_as_str_and_default() {
        assert_eq!(JobStatus::Draft.as_str(), "draft");
        assert_eq!(JobStatus::Published.as_str(), "published");
        assert_eq!(JobStatus::Closed.as_str(), "closed");
        assert!(matches!(JobStatus::default(), JobStatus::Draft));
        assert!(matches!(ApplicantStage::default(), ApplicantStage::Applied));
    }

    #[test]
    fn offer_status_as_str() {
        assert_eq!(OfferStatus::Draft.as_str(), "draft");
        assert_eq!(OfferStatus::Sent.as_str(), "sent");
        assert_eq!(OfferStatus::Accepted.as_str(), "accepted");
        assert_eq!(OfferStatus::Declined.as_str(), "declined");
        assert_eq!(OfferStatus::Expired.as_str(), "expired");
        assert!(matches!(OfferStatus::default(), OfferStatus::Draft));
    }
}
