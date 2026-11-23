// Diesel models for the exams domain of the unified-db-education bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Online exams (migration 000095_exams).
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_question_banks)]
pub struct ExamQuestionBank {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_question_banks)]
pub struct NewExamQuestionBank {
    pub id: Uuid,
    pub title: String,
    pub owner_id: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_questions)]
pub struct ExamQuestion {
    pub id: Uuid,
    pub bank_id: Uuid,
    pub version: i32,
    pub previous_version_id: Option<Uuid>,
    pub superseded_by: Option<Uuid>,
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub options: Value,
    pub correct_answer: Option<Value>,
    pub answer_key: Option<String>,
    pub marks: f64,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_questions)]
pub struct NewExamQuestion {
    pub id: Uuid,
    pub bank_id: Uuid,
    pub previous_version_id: Option<Uuid>,
    pub superseded_by: Option<Uuid>,
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub correct_answer: Option<Value>,
    pub answer_key: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_question_versions)]
pub struct ExamQuestionVersion {
    pub id: Uuid,
    pub question_id: Uuid,
    pub version: i32,
    pub snapshot: Value,
    pub reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_question_versions)]
pub struct NewExamQuestionVersion {
    pub id: Uuid,
    pub question_id: Uuid,
    pub version: i32,
    pub reason: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
// NOTE: the path must stay fully qualified to the TABLE module.
// `crate::schema::exams` is the schema file-module (schema/exams.rs),
// not the table: the `pub use exams::*;` re-export in schema.rs
// cannot surface the inner `exams` table over the file-module of
// the same name, so the table's columns live at
// `crate::schema::exams::exams`. Pointing `table_name` at the
// file-module (or the bare name `exams`) makes every field fail
// with E0425 "cannot find type ... in module `exams`".
#[diesel(table_name = crate::schema::exams::exams)]
pub struct Exam {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub bank_ids: Vec<Uuid>,
    pub course_id: Option<String>,
    pub academic_enrollment_id: Option<String>,
    pub academic_component_id: Option<String>,
    pub status: String,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub duration_minutes: i32,
    pub pass_mark: f64,
    pub attempts_allowed: i32,
    pub price_minor: i64,
    pub is_paid: bool,
    pub settings: Value,
    pub total_marks: f64,
    pub created_by: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = crate::schema::exams::exams)]
pub struct NewExam {
    pub id: Uuid,
    pub title: String,
    pub course_id: Option<String>,
    pub academic_enrollment_id: Option<String>,
    pub academic_component_id: Option<String>,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub created_by: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_question_links)]
pub struct ExamQuestionLink {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub question_version: i32,
    pub position: i32,
    pub section: Option<String>,
    pub marks: f64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_question_links)]
pub struct NewExamQuestionLink {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub section: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_rubrics)]
pub struct ExamRubric {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub title: String,
    pub description: String,
    pub criteria: Value,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_rubrics)]
pub struct NewExamRubric {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub title: String,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_attempts)]
pub struct ExamAttempt {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub attempt_number: i32,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub duration_seconds: i64,
    pub seed: i64,
    pub question_order: Value,
    pub option_orders: Value,
    pub answers: Value,
    pub autosaved_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub max_score: f64,
    pub passed: Option<bool>,
    pub graded_at: Option<DateTime<Utc>>,
    pub result_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_attempts)]
pub struct NewExamAttempt {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub expires_at: DateTime<Utc>,
    pub duration_seconds: i64,
    pub seed: i64,
    pub autosaved_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub passed: Option<bool>,
    pub graded_at: Option<DateTime<Utc>>,
    pub result_id: Option<Uuid>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_grades)]
pub struct ExamGrade {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub grader_id: Option<String>,
    pub rubric_id: Option<Uuid>,
    pub criterion_scores: Value,
    pub marks_awarded: Option<f64>,
    pub max_marks: f64,
    pub status: String,
    pub comment: Option<String>,
    pub second_grader_id: Option<String>,
    pub audit: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_grades)]
pub struct NewExamGrade {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub question_id: Uuid,
    pub grader_id: Option<String>,
    pub rubric_id: Option<Uuid>,
    pub marks_awarded: Option<f64>,
    pub comment: Option<String>,
    pub second_grader_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_proctor_signals)]
pub struct ExamProctorSignal {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub signal_type: String,
    pub detail: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_proctor_signals)]
pub struct NewExamProctorSignal {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub signal_type: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_results)]
pub struct ExamResult {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub score: f64,
    pub max_score: f64,
    pub percentage: f64,
    pub passed: bool,
    pub status: String,
    pub per_question: Value,
    pub graded_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_results)]
pub struct NewExamResult {
    pub id: Uuid,
    pub attempt_id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub published_at: Option<DateTime<Utc>>,
}

// Paid certification exams (migration 000108_paid_certification_exams).
#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = exam_activations)]
pub struct ExamActivation {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub order_id: Option<String>,
    pub status: String,
    pub activated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = exam_activations)]
pub struct NewExamActivation {
    pub id: Uuid,
    pub exam_id: Uuid,
    pub user_id: String,
    pub order_id: Option<String>,
}
