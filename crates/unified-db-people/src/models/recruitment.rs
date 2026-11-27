// Diesel models for the recruitment domain of the unified-db-people bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// Recruitment (migration 000051_recruitment).

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = jobs)]
pub struct Job {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub department_id: Option<String>,
    pub location: Option<String>,
    pub job_type: String,
    pub salary_min: Option<PgNumeric>,
    pub salary_max: Option<PgNumeric>,
    pub currency: String,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = jobs)]
pub struct NewJob {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub department_id: Option<String>,
    pub location: Option<String>,
    pub salary_min: Option<PgNumeric>,
    pub salary_max: Option<PgNumeric>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = applicants)]
pub struct Applicant {
    pub id: Uuid,
    pub job_id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub resume_storage_target: Option<String>,
    pub resume_storage_key: Option<String>,
    pub consent_contact: bool,
    pub consent_at: Option<DateTime<Utc>>,
    pub stage: String,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = applicants)]
pub struct NewApplicant {
    pub id: Uuid,
    pub job_id: Uuid,
    pub name: String,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub resume_storage_target: Option<String>,
    pub resume_storage_key: Option<String>,
    pub consent_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = applicant_stage_history)]
pub struct ApplicantStageHistory {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub from_stage: Option<String>,
    pub to_stage: String,
    pub moved_by: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = applicant_stage_history)]
pub struct NewApplicantStageHistory {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub from_stage: Option<String>,
    pub to_stage: String,
    pub moved_by: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = interviews)]
pub struct Interview {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub scheduled_at: DateTime<Utc>,
    pub type_: String,
    pub interviewers: Value,
    pub notes: Option<String>,
    pub outcome: Option<String>,
    pub outcome_notes: Option<String>,
    pub status: String,
    pub scheduler_status: String,
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = interviews)]
pub struct NewInterview {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub scheduled_at: DateTime<Utc>,
    pub notes: Option<String>,
    pub outcome: Option<String>,
    pub outcome_notes: Option<String>,
    pub scheduler_job_id: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = evaluations)]
pub struct Evaluation {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub interviewer_id: String,
    pub interview_id: Option<Uuid>,
    pub scores: Value,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = evaluations)]
pub struct NewEvaluation {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub interviewer_id: String,
    pub interview_id: Option<Uuid>,
    pub scores: Value,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = offers)]
pub struct Offer {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub salary: PgNumeric,
    pub currency: String,
    pub start_date: Option<NaiveDate>,
    pub terms: Option<String>,
    pub status: String,
    pub created_by: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = offers)]
pub struct NewOffer {
    pub id: Uuid,
    pub applicant_id: Uuid,
    pub job_id: Uuid,
    pub salary: PgNumeric,
    pub start_date: Option<NaiveDate>,
    pub terms: Option<String>,
    pub created_by: String,
    pub sent_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
}
