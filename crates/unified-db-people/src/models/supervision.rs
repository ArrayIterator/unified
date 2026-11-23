// Diesel models for the supervision domain of the unified-db-people bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, NaiveDate, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Supervision (migration 000063_supervision; batch 5 notes §d, field
// lists expanded into full structs). Money structs carry no serde
// derives.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_assignments)]
pub struct SupervisionAssignment {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub user_id: String,
    pub role: String,
    pub status: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_assignments)]
pub struct NewSupervisionAssignment {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub user_id: String,
    pub role: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: Option<DateTime<Utc>>,
    pub notes: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_inspection_templates)]
pub struct SupervisionInspectionTemplate {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub category: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_inspection_templates)]
pub struct NewSupervisionInspectionTemplate {
    pub id: Uuid,
    pub project_id: Option<String>,
    pub company_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_template_items)]
pub struct SupervisionTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub position: i32,
    pub required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_template_items)]
pub struct NewSupervisionTemplateItem {
    pub id: Uuid,
    pub template_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub position: i32,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_inspections)]
pub struct SupervisionInspection {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub location: Option<String>,
    pub inspector_id: String,
    pub status: String,
    pub scheduled_for: DateTime<Utc>,
    pub conducted_at: Option<DateTime<Utc>>,
    pub verdict: Option<String>,
    pub summary: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_inspections)]
pub struct NewSupervisionInspection {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub template_id: Option<Uuid>,
    pub title: String,
    pub location: Option<String>,
    pub inspector_id: String,
    pub scheduled_for: DateTime<Utc>,
    pub conducted_at: Option<DateTime<Utc>>,
    pub verdict: Option<String>,
    pub summary: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_inspection_items)]
pub struct SupervisionInspectionItem {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub template_item_id: Option<Uuid>,
    pub title: String,
    pub result: String,
    pub notes: Option<String>,
    pub follow_up_requested: bool,
    pub follow_up_task_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_inspection_items)]
pub struct NewSupervisionInspectionItem {
    pub id: Uuid,
    pub inspection_id: Uuid,
    pub template_item_id: Option<Uuid>,
    pub title: String,
    pub notes: Option<String>,
    pub follow_up_task_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = supervision_progress_reports)]
pub struct SupervisionProgressReport {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub milestone_id: Option<String>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub reported_percent_complete: PgNumeric,
    pub planned_percent_complete: Option<PgNumeric>,
    pub project_percent_complete: Option<PgNumeric>,
    pub source: String,
    pub status: String,
    pub notes: Option<String>,
    pub reported_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = supervision_progress_reports)]
pub struct NewSupervisionProgressReport {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub milestone_id: Option<String>,
    pub title: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub reported_percent_complete: PgNumeric,
    pub planned_percent_complete: Option<PgNumeric>,
    pub project_percent_complete: Option<PgNumeric>,
    pub notes: Option<String>,
    pub reported_by: String,
    pub reviewed_by: Option<String>,
    pub reviewed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_defects)]
pub struct SupervisionDefect {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub inspection_id: Option<Uuid>,
    pub task_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub severity: String,
    pub status: String,
    pub reported_by: String,
    pub assigned_to: Option<String>,
    pub follow_up_requested: bool,
    pub follow_up_task_id: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_defects)]
pub struct NewSupervisionDefect {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub inspection_id: Option<Uuid>,
    pub task_id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub location: Option<String>,
    pub reported_by: String,
    pub assigned_to: Option<String>,
    pub follow_up_task_id: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_approvals)]
pub struct SupervisionApproval {
    pub id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub approver_id: String,
    pub decision: String,
    pub comment: Option<String>,
    pub decided_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_approvals)]
pub struct NewSupervisionApproval {
    pub id: Uuid,
    pub target_kind: String,
    pub target_id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub approver_id: String,
    pub decision: String,
    pub comment: Option<String>,
    pub decided_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_daily_logs)]
pub struct SupervisionDailyLog {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub log_date: NaiveDate,
    pub supervisor_id: String,
    pub weather: Option<String>,
    pub workforce_count: i64,
    pub work_done: Option<String>,
    pub materials_received: Option<String>,
    pub incidents_on_site: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_daily_logs)]
pub struct NewSupervisionDailyLog {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub log_date: NaiveDate,
    pub supervisor_id: String,
    pub weather: Option<String>,
    pub work_done: Option<String>,
    pub materials_received: Option<String>,
    pub incidents_on_site: Option<String>,
    pub signed_by: Option<String>,
    pub signed_at: Option<DateTime<Utc>>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = supervision_photos)]
pub struct SupervisionPhoto {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub caption: Option<String>,
    pub uploaded_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = supervision_photos)]
pub struct NewSupervisionPhoto {
    pub id: Uuid,
    pub project_id: String,
    pub company_id: Option<Uuid>,
    pub subject_kind: String,
    pub subject_id: Uuid,
    pub storage_target: String,
    pub storage_key: String,
    pub content_sha256: String,
    pub content_type: String,
    pub size_bytes: i64,
    pub caption: Option<String>,
    pub uploaded_by: String,
}
