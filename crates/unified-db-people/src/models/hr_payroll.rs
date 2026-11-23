// Diesel models for the hr_payroll domain of the unified-db-people bounded context.
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

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_payroll_runs)]
pub struct HrPayrollRun {
    pub id: Uuid,
    pub period: String,
    pub currency: String,
    pub status: String,
    pub total_gross: PgNumeric,
    pub total_net: PgNumeric,
    pub total_employees: i32,
    pub finalized_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_payroll_runs)]
pub struct NewHrPayrollRun {
    pub id: Uuid,
    pub period: String,
    pub currency: String,
    pub status: String,
    pub total_gross: PgNumeric,
    pub total_net: PgNumeric,
    pub total_employees: i32,
    pub finalized_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = hr_payroll_items)]
pub struct HrPayrollItem {
    pub id: Uuid,
    pub payroll_run_id: Uuid,
    pub employee_id: Uuid,
    pub gross: PgNumeric,
    pub adjustments: PgNumeric,
    pub deductions: PgNumeric,
    pub net: PgNumeric,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = hr_payroll_items)]
pub struct NewHrPayrollItem {
    pub id: Uuid,
    pub payroll_run_id: Uuid,
    pub employee_id: Uuid,
    pub gross: PgNumeric,
    pub adjustments: PgNumeric,
    pub deductions: PgNumeric,
    pub net: PgNumeric,
    pub notes: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_review_cycles)]
pub struct HrReviewCycle {
    pub id: Uuid,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_review_cycles)]
pub struct NewHrReviewCycle {
    pub id: Uuid,
    pub name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_reviews)]
pub struct HrReview {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub employee_id: Uuid,
    pub reviewer_id: String,
    pub rating: Option<i16>,
    pub goals: Value,
    pub reviewer_notes: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_reviews)]
pub struct NewHrReview {
    pub id: Uuid,
    pub cycle_id: Uuid,
    pub employee_id: Uuid,
    pub reviewer_id: String,
    pub rating: Option<i16>,
    pub goals: Value,
    pub reviewer_notes: String,
    pub status: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_checklists)]
pub struct HrChecklist {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub kind: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_checklists)]
pub struct NewHrChecklist {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub kind: String,
    pub status: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_checklist_items)]
pub struct HrChecklistItem {
    pub id: Uuid,
    pub checklist_id: Uuid,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_checklist_items)]
pub struct NewHrChecklistItem {
    pub id: Uuid,
    pub checklist_id: Uuid,
    pub title: String,
    pub completed: bool,
    pub completed_by: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = hr_documents)]
pub struct HrDocument {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub title: String,
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = hr_documents)]
pub struct NewHrDocument {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub title: String,
    pub document_type: String,
    pub storage_file_id: String,
    pub mime_type: Option<String>,
    pub size_bytes: Option<i64>,
    pub uploaded_by: String,
}
