// Elearning — Types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One course. `price_minor` is the price in the smallest currency
/// unit (cents); 0 means free. `thumbnail_ref` is a reference into
/// the shared documents store — a documents document id, or a
/// `/files/public/{key}` (or `/files/private/{key}`) URL from the
/// media picker; the documents module owns the bytes.
/// `certificate_enabled` decides whether completion issues a
/// certificate. `status` is `draft`, `published`, or `archived`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Course {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_ref: Option<String>,
    pub instructor_id: String,
    pub status: String,
    pub price_minor: i64,
    pub currency: String,
    pub certificate_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One module inside a course. `position` orders modules inside
/// their course (0-based, unique per course).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CourseModule {
    pub id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One lesson inside a module. `lesson_type` comes from the live
/// settings list. `content_ref` is a storage reference for video
/// and file lessons; `body` holds the text of text lessons.
/// `available_from` is the drip date (None = available at once);
/// `requires_previous` is the progress drip (the previous lesson
/// of the course must be complete first). `required` lessons count
/// toward course completion; optional lessons do not.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lesson {
    pub id: Uuid,
    pub module_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    pub lesson_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub position: i64,
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub available_from: Option<DateTime<Utc>>,
    pub requires_previous: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_minutes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One enrollment of one user in one course. At most one active
/// (non-cancelled) enrollment exists per (course, user); a paid
/// course starts as `pending_payment` and becomes `active` on
/// `order.paid`. `price_paid_minor` snapshots the price paid.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Enrollment {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub status: String,
    pub price_paid_minor: i64,
    pub currency: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_id: Option<String>,
    /// Display name recorded at enrollment, printed on certificates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learner_name: Option<String>,
    pub enrolled_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One learner's completion of one lesson. The row is created on
/// first completion; un-completing is not supported (completion
/// is an audited fact, not a toggle).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonProgress {
    pub id: Uuid,
    pub enrollment_id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub completed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One quiz attached to one lesson. `pass_mark` is a percent
/// (0-100). `max_attempts` None means unlimited (or the settings
/// default when the quiz was created without one). `allow_review`
/// decides whether a learner may see the correct answers after
/// submitting.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Quiz {
    pub id: Uuid,
    pub lesson_id: Uuid,
    pub course_id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub pass_mark: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<i64>,
    pub allow_review: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_limit_minutes: Option<i64>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One multiple-choice question. `options` holds 2 or more
/// choices; `correct_index` is the index of the correct one.
/// `points` is the weight of the question in grading.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuizQuestion {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub question: String,
    pub options: Vec<String>,
    pub correct_index: usize,
    pub points: i64,
    pub position: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One submitted answer inside an attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptAnswer {
    pub question_id: Uuid,
    pub selected_index: usize,
}

/// One quiz attempt. Auto-graded at submission: `score` /
/// `max_score` and `percent`; `passed` is `percent >= pass_mark`.
/// `started_at` is when the learner first fetched the quiz
/// questions: the quiz time limit is measured from it, not from
/// submission. Correct answers are never stored in the serialized attempt —
/// they are attached on read only when the quiz allows review
/// (see `QuizAttemptView`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuizAttempt {
    pub id: Uuid,
    pub quiz_id: Uuid,
    pub lesson_id: Uuid,
    pub enrollment_id: Uuid,
    pub user_id: String,
    pub answers: Vec<AttemptAnswer>,
    pub score: i64,
    pub max_score: i64,
    pub percent: i64,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<DateTime<Utc>>,
    pub submitted_at: DateTime<Utc>,
}

/// Read view of an attempt. `review` is present only when the
/// quiz allows review; it carries per-question correctness and
/// the correct option index.
#[derive(Debug, Clone, Serialize)]
pub struct QuizAttemptView {
    #[serde(flatten)]
    pub attempt: QuizAttempt,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<Vec<AttemptReviewItem>>,
}

/// One reviewed answer: whether the learner was right, and the
/// correct option index (review only). `selected_index` is None
/// for a question the learner left unanswered.
#[derive(Debug, Clone, Serialize)]
pub struct AttemptReviewItem {
    pub question_id: Uuid,
    pub selected_index: Option<usize>,
    pub correct: bool,
    pub correct_index: usize,
    pub points_earned: i64,
    pub points: i64,
}

/// One certificate issued on course completion. `code` is the
/// public verifiable code; `content_sha256` is the SHA-256
/// fingerprint of the canonical certificate payload (learner,
/// course, date, issuer, code) — the value handed to the
/// anchoring flow. `status` is `valid` or `revoked`.
/// `anchor_status` is `none`, `requested`, `confirmed`, or
/// `failed`; anchoring itself lives in the Web3 module.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Certificate {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub anchor_tx: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One course review. One row per (course, user) at most; the
/// author may edit it. `rating` is 1-5.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Review {
    pub id: Uuid,
    pub course_id: Uuid,
    pub user_id: String,
    pub rating: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One learning path: an ordered sequence of courses.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningPath {
    pub id: Uuid,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One course inside a learning path, with its position.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearningPathCourse {
    pub id: Uuid,
    pub path_id: Uuid,
    pub course_id: Uuid,
    pub position: i64,
    pub created_at: DateTime<Utc>,
}

/// Per-course progress summary, computed on read from lesson
/// progress rows (no stored percentage is ever trusted).
#[derive(Debug, Clone, Serialize)]
pub struct CourseProgress {
    pub course_id: Uuid,
    pub user_id: String,
    pub enrollment_id: Uuid,
    pub enrollment_status: String,
    pub total_required: usize,
    pub completed_required: usize,
    pub progress_percent: i64,
    pub completed: bool,
}

/// Per-path progress summary, computed on read.
#[derive(Debug, Clone, Serialize)]
pub struct PathProgress {
    pub path_id: Uuid,
    pub user_id: String,
    pub total_courses: usize,
    pub completed_courses: usize,
    pub progress_percent: i64,
    pub completed: bool,
    pub courses: Vec<PathCourseProgress>,
}

/// Progress of one course inside a path.
#[derive(Debug, Clone, Serialize)]
pub struct PathCourseProgress {
    pub course_id: Uuid,
    pub position: i64,
    pub enrolled: bool,
    pub completed: bool,
    pub progress_percent: i64,
}
