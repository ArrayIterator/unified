// Diesel models for the curriculum domain of the unified-db-education bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::data_types::PgNumeric;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Curriculum (migration 000092_curriculum).
// Structs carrying NUMERIC columns skip serde derives (PgNumeric
// has no serde implementation), per the repo header rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_frameworks)]
pub struct CurriculumFramework {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub description: String,
    pub country: String,
    pub is_active: bool,
    pub is_built_in: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_frameworks)]
pub struct NewCurriculumFramework {
    pub id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_levels)]
pub struct CurriculumLevel {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub code: String,
    pub name: String,
    pub grade_count: i32,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_levels)]
pub struct NewCurriculumLevel {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_subjects)]
pub struct CurriculumSubject {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub name: String,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_subjects)]
pub struct NewCurriculumSubject {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_core_competencies)]
pub struct CurriculumCoreCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub title: String,
    pub description: String,
    pub grade_year: i32,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_core_competencies)]
pub struct NewCurriculumCoreCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub level_id: Uuid,
    pub code: String,
    pub title: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_basic_competencies)]
pub struct CurriculumBasicCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub core_competency_id: Uuid,
    pub code: String,
    pub title: String,
    pub description: String,
    pub grade_year: i32,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_basic_competencies)]
pub struct NewCurriculumBasicCompetency {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub core_competency_id: Uuid,
    pub code: String,
    pub title: String,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = curriculum_structures)]
pub struct CurriculumStructure {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub subject_id: Uuid,
    pub hours_per_week: PgNumeric,
    pub sort_order: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = curriculum_structures)]
pub struct NewCurriculumStructure {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub subject_id: Uuid,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_assessment_domains)]
pub struct CurriculumAssessmentDomain {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub parent_domain_id: Option<Uuid>,
    pub description: String,
    pub sort_order: i32,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_assessment_domains)]
pub struct NewCurriculumAssessmentDomain {
    pub id: Uuid,
    pub code: String,
    pub name: String,
    pub parent_domain_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = curriculum_grade_descriptors)]
pub struct CurriculumGradeDescriptor {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub domain_id: Option<Uuid>,
    pub min_score: PgNumeric,
    pub max_score: PgNumeric,
    pub grade_label: String,
    pub descriptor: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = curriculum_grade_descriptors)]
pub struct NewCurriculumGradeDescriptor {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub domain_id: Option<Uuid>,
    pub min_score: PgNumeric,
    pub max_score: PgNumeric,
    pub grade_label: String,
    pub descriptor: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_lesson_plans)]
pub struct CurriculumLessonPlan {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub core_competency_id: Option<Uuid>,
    pub basic_competency_id: Option<Uuid>,
    pub title: String,
    pub objectives: Vec<String>,
    pub materials: Vec<String>,
    pub activities: Vec<String>,
    pub assessment_notes: String,
    pub time_minutes: i32,
    pub grade_year: i32,
    pub semester: i32,
    pub teacher_id: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_lesson_plans)]
pub struct NewCurriculumLessonPlan {
    pub id: Uuid,
    pub framework_id: Uuid,
    pub subject_id: Uuid,
    pub core_competency_id: Option<Uuid>,
    pub basic_competency_id: Option<Uuid>,
    pub title: String,
    pub assessment_notes: String,
    pub teacher_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_report_cards)]
pub struct CurriculumReportCard {
    pub id: Uuid,
    pub student_ref: String,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub semester: i32,
    pub academic_year: String,
    pub status: String,
    pub issued_at: Option<DateTime<Utc>>,
    pub issued_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_report_cards)]
pub struct NewCurriculumReportCard {
    pub id: Uuid,
    pub student_ref: String,
    pub framework_id: Uuid,
    pub level_id: Uuid,
    pub grade_year: i32,
    pub semester: i32,
    pub academic_year: String,
    pub issued_at: Option<DateTime<Utc>>,
    pub issued_by: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable)]
#[diesel(table_name = curriculum_report_card_entries)]
pub struct CurriculumReportCardEntry {
    pub id: Uuid,
    pub report_card_id: Uuid,
    pub subject_id: Uuid,
    pub domain_id: Uuid,
    pub score: Option<PgNumeric>,
    pub grade_label: Option<String>,
    pub descriptor: Option<String>,
    pub notes: String,
    pub academic_grade_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable)]
#[diesel(table_name = curriculum_report_card_entries)]
pub struct NewCurriculumReportCardEntry {
    pub id: Uuid,
    pub report_card_id: Uuid,
    pub subject_id: Uuid,
    pub domain_id: Uuid,
    pub score: Option<PgNumeric>,
    pub grade_label: Option<String>,
    pub descriptor: Option<String>,
    pub academic_grade_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_interventions)]
pub struct CurriculumIntervention {
    pub id: Uuid,
    pub student_ref: String,
    pub basic_competency_id: Uuid,
    pub kind: String,
    pub status: String,
    pub teacher_id: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub notes: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_interventions)]
pub struct NewCurriculumIntervention {
    pub id: Uuid,
    pub student_ref: String,
    pub basic_competency_id: Uuid,
    pub kind: String,
    pub teacher_id: String,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = curriculum_settings)]
pub struct CurriculumSetting {
    pub key: String,
    pub value: Value,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = curriculum_settings)]
pub struct NewCurriculumSetting {
    pub key: String,
    pub value: Value,
}
