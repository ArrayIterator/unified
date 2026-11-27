//! exams - inputs.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;



#[derive(Debug, Clone, Deserialize)]
// Input payloads
pub struct CreateBankInput {
    pub title: String,
    pub description: Option<String>,
    pub category: String,
    pub company_id: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateBankInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct QuestionOptionInput {
    pub id: Option<String>,
    pub text: String,
    pub position: Option<u32>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateQuestionInput {
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub options: Option<Vec<QuestionOptionInput>>,
    pub correct_answer: Option<Value>,
    pub answer_key: Option<String>,
    pub marks: Option<f64>,
    pub status: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateQuestionInput {
    pub question_type: Option<String>,
    pub category: Option<String>,
    pub difficulty: Option<String>,
    pub text: Option<String>,
    pub options: Option<Vec<QuestionOptionInput>>,
    pub correct_answer: Option<Value>,
    pub answer_key: Option<Option<String>>,
    pub marks: Option<f64>,
    pub status: Option<String>,
    pub reason: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct ImportQuestionsInput {
    pub questions: Vec<CreateQuestionInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateExamInput {
    pub title: String,
    pub description: Option<String>,
    pub category: Option<String>,
    pub bank_ids: Option<Vec<String>>,
    pub course_id: Option<String>,
    pub academic_enrollment_id: Option<String>,
    pub academic_component_id: Option<String>,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub duration_minutes: Option<i64>,
    pub pass_mark: Option<f64>,
    pub attempts_allowed: Option<u32>,
    /// Price in integer minor units (0 = free). When set above 0 the
    /// exam must also be marked paid with `is_paid`.
    pub price_minor: Option<i64>,
    /// Whether the exam is paid (standalone; no course required).
    pub is_paid: Option<bool>,
    pub settings: Option<ExamSettingsInput>,
    pub company_id: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct ExamSettingsInput {
    pub show_results: Option<String>,
    pub allow_review: Option<bool>,
    pub calculator_allowed: Option<bool>,
    pub notes_allowed: Option<bool>,
    pub randomise_questions: Option<bool>,
    pub randomise_options: Option<bool>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateExamInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub bank_ids: Option<Vec<String>>,
    pub course_id: Option<Option<String>>,
    pub academic_enrollment_id: Option<Option<String>>,
    pub academic_component_id: Option<Option<String>>,
    pub opens_at: Option<Option<DateTime<Utc>>>,
    pub closes_at: Option<Option<DateTime<Utc>>>,
    pub duration_minutes: Option<i64>,
    pub pass_mark: Option<f64>,
    pub attempts_allowed: Option<u32>,
    pub price_minor: Option<i64>,
    pub is_paid: Option<bool>,
    pub settings: Option<ExamSettingsInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct AttachQuestionInput {
    pub question_id: String,
    pub question_version: Option<u32>,
    pub position: Option<u32>,
    pub section: Option<String>,
    pub marks: Option<f64>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct AutosaveInput {
    pub answers: Value,
}


#[derive(Debug, Clone, Deserialize)]
pub struct ProctorSignalInput {
    pub signal_type: String,
    pub detail: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct CreateRubricInput {
    pub title: String,
    pub description: Option<String>,
    pub criteria: Vec<RubricCriterionInput>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct RubricCriterionInput {
    pub id: Option<String>,
    pub title: String,
    pub description: Option<String>,
    pub max_marks: f64,
}


#[derive(Debug, Clone, Deserialize)]
pub struct UpdateRubricInput {
    pub title: Option<String>,
    pub description: Option<String>,
    pub criteria: Option<Vec<RubricCriterionInput>>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct GradeItemInput {
    pub question_id: String,
    pub rubric_id: Option<String>,
    pub criterion_scores: Option<HashMap<String, f64>>,
    pub marks_awarded: f64,
    pub comment: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct GradeAttemptInput {
    pub grades: Vec<GradeItemInput>,
    pub grader_id: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct OverrideInput {
    pub score: f64,
    pub reason: String,
}


#[derive(Debug, Clone, Deserialize)]
pub struct DisputeInput {
    pub reason: String,
}


#[derive(Debug, Clone, Deserialize)]
pub struct SecondGradeInput {
    pub marks_awarded: f64,
    pub criterion_scores: Option<HashMap<String, f64>>,
    pub comment: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct AiDraftQuestionsInput {
    pub topic: String,
    pub count: Option<u32>,
    pub difficulty: Option<String>,
    pub question_type: Option<String>,
    pub bank_id: Option<String>,
    pub category: Option<String>,
}


#[derive(Debug, Clone, Deserialize)]
pub struct AiAnalyseResultsInput {
    pub exam_id: String,
}
