// Diesel models for the admissions domain of the unified-db-education bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// Admissions (migration 000093_admissions).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_periods)]
pub struct AdmissionPeriod {
    pub id: Uuid,
    pub academic_year: String,
    pub term: String,
    pub name: String,
    pub open_date: NaiveDate,
    pub close_date: NaiveDate,
    pub status: String,
    pub registration_fee_amount: Option<PgNumeric>,
    pub fee_currency: String,
    pub registration_form_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_periods)]
pub struct NewAdmissionPeriod {
    pub id: Uuid,
    pub academic_year: String,
    pub term: String,
    pub name: String,
    pub open_date: NaiveDate,
    pub close_date: NaiveDate,
    pub registration_fee_amount: Option<PgNumeric>,
    pub registration_form_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_period_quotas)]
pub struct AdmissionPeriodQuota {
    pub id: Uuid,
    pub period_id: Uuid,
    pub program_id: String,
    pub level: String,
    pub seats: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_period_quotas)]
pub struct NewAdmissionPeriodQuota {
    pub id: Uuid,
    pub period_id: Uuid,
    pub program_id: String,
    pub level: String,
    pub seats: i32,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_stage_definitions)]
pub struct AdmissionStageDefinition {
    pub id: Uuid,
    pub period_id: Uuid,
    pub stage_key: String,
    pub name: String,
    pub position: i32,
    pub weight: PgNumeric,
    pub pass_score: Option<PgNumeric>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_stage_definitions)]
pub struct NewAdmissionStageDefinition {
    pub id: Uuid,
    pub period_id: Uuid,
    pub stage_key: String,
    pub name: String,
    pub pass_score: Option<PgNumeric>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_required_documents)]
pub struct AdmissionRequiredDocument {
    pub id: Uuid,
    pub period_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub description: Option<String>,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_required_documents)]
pub struct NewAdmissionRequiredDocument {
    pub id: Uuid,
    pub period_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_applications)]
pub struct AdmissionApplication {
    pub id: Uuid,
    pub period_id: Uuid,
    pub applicant_user_id: String,
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub program_id: String,
    pub level: String,
    pub form_submission_id: Option<String>,
    pub status: String,
    pub total_score: Option<PgNumeric>,
    pub fee_amount: Option<PgNumeric>,
    pub fee_currency: String,
    pub fee_invoice_status: String,
    pub fee_invoice_id: Option<String>,
    pub student_id: Option<String>,
    pub source: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_applications)]
pub struct NewAdmissionApplication {
    pub id: Uuid,
    pub period_id: Uuid,
    pub applicant_user_id: String,
    pub full_name: String,
    pub email: String,
    pub phone: Option<String>,
    pub program_id: String,
    pub level: String,
    pub form_submission_id: Option<String>,
    pub total_score: Option<PgNumeric>,
    pub fee_amount: Option<PgNumeric>,
    pub fee_invoice_id: Option<String>,
    pub student_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = admission_application_scores)]
pub struct AdmissionApplicationScore {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub stage_id: Uuid,
    pub stage_key: String,
    pub score: PgNumeric,
    pub scored_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = admission_application_scores)]
pub struct NewAdmissionApplicationScore {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub stage_id: Uuid,
    pub stage_key: String,
    pub score: PgNumeric,
    pub scored_by: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_checklist_items)]
pub struct AdmissionChecklistItem {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub required_document_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub required: bool,
    pub status: String,
    pub document_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_checklist_items)]
pub struct NewAdmissionChecklistItem {
    pub id: Uuid,
    pub application_id: Uuid,
    pub period_id: Uuid,
    pub required_document_id: Uuid,
    pub document_key: String,
    pub title: String,
    pub document_id: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_waitlist_entries)]
pub struct AdmissionWaitlistEntry {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub position: i32,
    pub status: String,
    pub promoted_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_waitlist_entries)]
pub struct NewAdmissionWaitlistEntry {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub position: i32,
    pub promoted_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_results)]
pub struct AdmissionResult {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub decision: String,
    pub published: bool,
    pub published_at: Option<DateTime<Utc>>,
    pub decided_by: String,
    pub note: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_results)]
pub struct NewAdmissionResult {
    pub id: Uuid,
    pub period_id: Uuid,
    pub application_id: Uuid,
    pub decision: String,
    pub published_at: Option<DateTime<Utc>>,
    pub decided_by: String,
    pub note: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = admission_settings)]
pub struct AdmissionSetting {
    pub key: String,
    pub value: String,
    pub description: String,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = admission_settings)]
pub struct NewAdmissionSetting {
    pub key: String,
    pub value: String,
}
