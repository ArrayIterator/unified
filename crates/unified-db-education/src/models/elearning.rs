// Diesel models for the elearning domain of the unified-db-education bounded context.
//
// Split out of `src/models.rs` (see `src/lib.rs`); the structs below are
// byte-identical to the originals, only regrouped.

use crate::schema::*;
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

// E-learning (migration 000079_elearning). Course certificates
// are `ElearningCertificate` / `NewElearningCertificate` below
// (table `elearning_certificates`, renamed from `certificates`
// during the final inspection: migration 000005 already owns
// that name for SSL certificates).

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = courses)]
pub struct Course {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: String,
    pub thumbnail_ref: Option<String>,
    pub instructor_id: String,
    pub status: String,
    pub price_minor: i64,
    pub currency: String,
    pub certificate_enabled: bool,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = courses)]
pub struct NewCourse {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: String,
    pub thumbnail_ref: Option<String>,
    pub instructor_id: String,
    pub status: String,
    pub company_id: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = course_modules)]
pub struct CourseModule {
    pub id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = course_modules)]
pub struct NewCourseModule {
    pub id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lessons)]
pub struct Lesson {
    pub id: Uuid,
    pub module_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub lesson_type: String,
    pub content_ref: Option<String>,
    pub body: Option<String>,
    pub position: i64,
    pub required: bool,
    pub available_from: Option<DateTime<Utc>>,
    pub requires_previous: bool,
    pub duration_minutes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lessons)]
pub struct NewLesson {
    pub id: Uuid,
    pub module_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub lesson_type: String,
    pub content_ref: Option<String>,
    pub body: Option<String>,
    pub available_from: Option<DateTime<Utc>>,
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = enrollments)]
pub struct Enrollment {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub price_paid_minor: i64,
    pub currency: String,
    pub order_id: Option<String>,
    pub learner_name: Option<String>,
    pub enrolled_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = enrollments)]
pub struct NewEnrollment {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub order_id: Option<String>,
    pub learner_name: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = lesson_progress)]
pub struct LessonProgress {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub completed: bool,
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = lesson_progress)]
pub struct NewLessonProgress {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quizzes)]
pub struct Quiz {
    pub id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub pass_mark: i64,
    pub max_attempts: Option<i64>,
    pub allow_review: bool,
    pub time_limit_minutes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quizzes)]
pub struct NewQuiz {
    pub id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub max_attempts: Option<i64>,
    pub time_limit_minutes: Option<i64>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quiz_questions)]
pub struct QuizQuestion {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub question: String,
    pub options: Value,
    pub correct_index: i64,
    pub points: i64,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quiz_questions)]
pub struct NewQuizQuestion {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub question: String,
    pub options: Value,
    pub correct_index: i64,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = quiz_attempts)]
pub struct QuizAttempt {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub lesson_id: Uuid,
    pub enrollment_id: Uuid,
    pub user_id: String,
    pub answers: Value,
    pub score: i64,
    pub max_score: i64,
    pub percent: i64,
    pub passed: bool,
    pub submitted_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = quiz_attempts)]
pub struct NewQuizAttempt {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub lesson_id: Uuid,
    pub enrollment_id: Uuid,
    pub user_id: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = elearning_certificates)]
pub struct ElearningCertificate {
    pub id: Uuid,
    pub code: String,
    pub course_id: Uuid,
    pub user_id: String,
    pub learner_name: String,
    pub course_title: String,
    pub issuer_name: String,
    pub issued_at: DateTime<Utc>,
    pub content_sha256: String,
    pub status: String,
    pub anchor_status: String,
    pub anchor_tx: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = elearning_certificates)]
pub struct NewElearningCertificate {
    pub id: Uuid,
    pub code: String,
    pub course_id: Uuid,
    pub user_id: String,
    pub learner_name: String,
    pub course_title: String,
    pub issuer_name: String,
    pub issued_at: DateTime<Utc>,
    pub content_sha256: String,
    pub status: String,
    pub anchor_status: String,
    pub anchor_tx: Option<String>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = reviews)]
pub struct Review {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub rating: i64,
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = reviews)]
pub struct NewReview {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub rating: i64,
    pub comment: Option<String>,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = learning_paths)]
pub struct LearningPath {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = learning_paths)]
pub struct NewLearningPath {
    pub id: Uuid,
    pub title: String,
    pub description: Option<String>,
    pub created_by: String,
}

#[derive(Debug, Clone, Queryable, Selectable, Serialize)]
#[diesel(table_name = learning_path_courses)]
pub struct LearningPathCourse {
    pub id: Uuid,
    pub path_id: Uuid,
    pub course_id: Uuid,
    pub position: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Insertable, Deserialize)]
#[diesel(table_name = learning_path_courses)]
pub struct NewLearningPathCourse {
    pub id: Uuid,
    pub path_id: Uuid,
    pub course_id: Uuid,
}
