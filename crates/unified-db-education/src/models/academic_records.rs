// Diesel models for the academic_records domain of the unified-db-education bounded context.
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

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_schedules)]
pub struct AcademicSchedule {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub lecturer_id: Uuid,
    pub room_id: Uuid,
    pub semester_id: Uuid,
    pub day_of_week: i32,
    pub start_time: String,
    pub end_time: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_schedules)]
pub struct NewAcademicSchedule {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub lecturer_id: Uuid,
    pub room_id: Uuid,
    pub semester_id: Uuid,
    pub day_of_week: i32,
    pub start_time: String,
    pub end_time: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_enrollments)]
pub struct AcademicEnrollment {
    pub id: Uuid,
    pub student_id: Uuid,
    pub subject_id: Uuid,
    pub semester_id: Uuid,
    pub credits: PgNumeric,
    pub status: String,
    pub advisor_id: Option<Uuid>,
    pub rejection_reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_enrollments)]
pub struct NewAcademicEnrollment {
    pub id: Uuid,
    pub student_id: Uuid,
    pub subject_id: Uuid,
    pub semester_id: Uuid,
    pub credits: PgNumeric,
    pub advisor_id: Option<Uuid>,
    pub rejection_reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_attendance)]
pub struct AcademicAttendance {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub student_id: Uuid,
    pub date: NaiveDate,
    pub status: String,
    pub source: String,
    pub entry_id: Option<String>,
    pub minutes: Option<i64>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_attendance)]
pub struct NewAcademicAttendance {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub student_id: Uuid,
    pub date: NaiveDate,
    pub entry_id: Option<String>,
    pub minutes: Option<i64>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_grade_components)]
pub struct AcademicGradeComponent {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub name: String,
    pub kind: String,
    pub weight_percent: PgNumeric,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_grade_components)]
pub struct NewAcademicGradeComponent {
    pub id: Uuid,
    pub subject_id: Uuid,
    pub name: String,
    pub kind: String,
    pub weight_percent: PgNumeric,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_grades)]
pub struct AcademicGrade {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Uuid,
    pub score: PgNumeric,
    pub graded_by: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_grades)]
pub struct NewAcademicGrade {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Uuid,
    pub score: PgNumeric,
    pub graded_by: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_grade_appeals)]
pub struct AcademicGradeAppeal {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Option<Uuid>,
    pub student_id: Uuid,
    pub reason: String,
    pub status: String,
    pub reviewed_by: Option<Uuid>,
    pub decision: Option<String>,
    pub corrected_score: Option<PgNumeric>,
    pub resolution_notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_grade_appeals)]
pub struct NewAcademicGradeAppeal {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub component_id: Option<Uuid>,
    pub student_id: Uuid,
    pub reason: String,
    pub reviewed_by: Option<Uuid>,
    pub decision: Option<String>,
    pub corrected_score: Option<PgNumeric>,
    pub resolution_notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_transcripts)]
pub struct AcademicTranscript {
    pub id: Uuid,
    pub student_id: Uuid,
    pub issued_by: String,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub lines: Value,
    pub print_job_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_transcripts)]
pub struct NewAcademicTranscript {
    pub id: Uuid,
    pub student_id: Uuid,
    pub issued_by: String,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub print_job_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_theses)]
pub struct AcademicThesis {
    pub id: Uuid,
    pub student_id: Uuid,
    pub title: String,
    pub advisor_id: Uuid,
    pub co_advisor_id: Option<Uuid>,
    pub status: String,
    pub proposal_date: Option<NaiveDate>,
    pub defense_date: Option<NaiveDate>,
    pub defense_result: Option<String>,
    pub notes: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_theses)]
pub struct NewAcademicThesis {
    pub id: Uuid,
    pub student_id: Uuid,
    pub title: String,
    pub advisor_id: Uuid,
    pub co_advisor_id: Option<Uuid>,
    pub proposal_date: Option<NaiveDate>,
    pub defense_date: Option<NaiveDate>,
    pub defense_result: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = academic_standing_records)]
pub struct AcademicStandingRecord {
    pub id: Uuid,
    pub student_id: Uuid,
    pub semester_id: Uuid,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub standing: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = academic_standing_records)]
pub struct NewAcademicStandingRecord {
    pub id: Uuid,
    pub student_id: Uuid,
    pub semester_id: Uuid,
    pub gpa: PgNumeric,
    pub total_credits: PgNumeric,
    pub standing: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_definitions)]
pub struct AcademicDefinition {
    pub id: Uuid,
    pub kind: String,
    pub key: String,
    pub label: String,
    pub config: Value,
    pub active: bool,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_definitions)]
pub struct NewAcademicDefinition {
    pub id: Uuid,
    pub kind: String,
    pub key: String,
    pub label: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = academic_settings)]
pub struct AcademicSetting {
    pub key: String,
    pub value: Value,
    pub description: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = academic_settings)]
pub struct NewAcademicSetting {
    pub key: String,
    pub value: Value,
    pub description: Option<String>,
}
