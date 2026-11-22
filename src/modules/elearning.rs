// E-learning module — courses, modules, lessons, enrollments,
// quizzes, progress, certificates, reviews, and learning paths.
//
// Routes live under `/api/v1/elearning`:
//
//   Courses:
//     GET/POST /courses — list (published only for learners) / create.
//     GET/PATCH/DELETE /courses/{id}.
//     POST /courses/{id}/publish — draft -> published (manage or owner).
//     POST /courses/{id}/archive — published/draft -> archived.
//   Course modules:
//     GET/POST /courses/{id}/modules — list / add a module.
//     GET/PATCH/DELETE /modules/{id}.
//   Lessons:
//     GET/POST /modules/{id}/lessons — list / add a lesson.
//     GET/PATCH/DELETE /lessons/{id}.
//   Enrollment:
//     POST /courses/{id}/enroll — enroll (free: active at once;
//          paid: pending_payment plus a purchase event for the
//          store/billing flow; activation happens on `order.paid`).
//     GET  /enrollments — enrollments (own only unless manager).
//     GET  /enrollments/{id}.
//     POST /enrollments/{id}/cancel.
//     GET  /my/enrollments — the caller's enrollments.
//   Progress:
//     POST /lessons/{id}/complete — mark a lesson complete.
//     GET  /courses/{id}/progress — the caller's course progress.
//     GET  /courses/{id}/progress/{user_id} — one learner's progress
//          (own or manager/instructor).
//     GET  /my/progress — the caller's progress across courses.
//   Quizzes:
//     GET/POST /lessons/{id}/quiz — the quiz of a lesson / define it.
//     PATCH /lessons/{id}/quiz — update the quiz.
//     GET/POST /lessons/{id}/quiz/questions — list / add a question.
//     PATCH/DELETE /questions/{id}.
//     POST /lessons/{id}/quiz/attempts — submit an attempt (auto-graded).
//     GET  /quiz-attempts — attempts (own only unless manager).
//     GET  /quiz-attempts/{id}.
//   Certificates:
//     GET  /certificates — certificates (own only unless manager).
//     GET  /certificates/{id}.
//     GET  /certificates/verify/{code} — PUBLIC verification endpoint
//          (no caller headers; verification must be public).
//     POST /certificates/{id}/revoke — revoke (manager).
//     POST /certificates/{id}/anchor — request tamper-evidence
//          anchoring through the Web3 flow (events only).
//   Reviews:
//     GET/POST /courses/{id}/reviews — list / leave a review.
//     GET/PATCH/DELETE /reviews/{id}.
//   Learning paths:
//     GET/POST /learning-paths — list / create a path.
//     GET/PATCH/DELETE /learning-paths/{id}.
//     POST /learning-paths/{id}/courses — append a course.
//     DELETE /learning-paths/{id}/courses/{course_id} — remove one.
//     GET  /learning-paths/{id}/progress — the caller's path progress.
//     GET  /my/learning-paths — paths the caller is progressing in.
//   Settings:
//     GET/PUT /settings — the configurable defaults (manager).
//
// HONESTY — READ THIS FIRST. What is real in this module:
// in-memory course / module / lesson / enrollment / progress /
// quiz / attempt / certificate / review / learning-path
// management; free and paid enrollment (paid enrollment never
// calls the store directly — it emits a purchase event and waits
// for `order.paid`); drip content by date and by previous-lesson
// completion; multiple-choice quizzes with a per-quiz pass mark,
// attempt limits, auto-grading, attempt history, and answer
// review only when the quiz allows it; per-lesson completion and
// course progress percentages; course completion when every
// required lesson is done; certificates issued on completion with
// a verifiable code, a public verification endpoint, and a
// SHA-256 content fingerprint; learning paths with ordered
// courses and per-user path progress; instructor ownership
// (instructors manage their own courses). What is NOT built here:
// durable storage (the DashMap stores are authoritative;
// migration `2026-11-22-000076_elearning` is the persistence
// target); payment capture itself (the store/billing modules own
// it — this module only requests a purchase and reacts to the
// paid/refunded events); media storage or playback (lesson video
// and file content are storage references only — the storage
// module owns the bytes); on-chain anchoring itself (the Web3
// module owns it — this module only emits `web3.anchor.requested`
// and listens for the confirmed/failed outcome); and email/push
// delivery (the notifications module owns delivery — this module
// only emits `notification.requested`).
//
// CONFIG-DRIVEN — READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed course levels, categories, languages,
// and lesson types, the default pass mark, the default attempt
// limit, the default currency, the certificate issuer name, and
// the anchoring switch all live in `ElearningSettings`, editable
// by an administrator through `PUT /settings`. The values in
// `ElearningSettings::default` are CONFIGURABLE DEFAULTS only —
// an admin change applies to every validation and default that
// reads them, and no code path consults a baked-in list instead.
// Lesson types, levels, categories, and languages are validated
// against the live settings, never against a constant list.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the events module. Capability checks use
// the `role.check` hook and fail closed (no answer means not allowed).
// The certificate verification endpoint is the single exception:
// it is intentionally public (no headers, no capability check),
// because anyone holding a certificate code must be able to
// verify it without an account.
//
// Events emitted on the hook bus:
//   - `elearning.course.created`, `elearning.course.updated`,
//     `elearning.course.deleted`, `elearning.course.published`,
//     `elearning.course.archived`
//   - `elearning.module.created`, `elearning.module.updated`,
//     `elearning.module.deleted`
//   - `elearning.lesson.created`, `elearning.lesson.updated`,
//     `elearning.lesson.deleted`, `elearning.lesson.completed`
//   - `elearning.enrollment.created`,
//     `elearning.enrollment.purchase_requested` (paid enrollment:
//     `{"course_id", "user_id", "amount_minor", "currency",
//     "module": "elearning", "item_type": "course_enrollment",
//     "enrollment_id"}` — no store listener creates an order
//     from this request yet: a course is not a store product, so
//     the buyer checks out with the course id on the checkout
//     item and the paid order activates the enrollment),
//     `elearning.enrollment.activated`,
//     `elearning.enrollment.cancelled`,
//     `elearning.enrollment.suspended`,
//     `elearning.enrollment.completed`
//   - `elearning.quiz.created`, `elearning.quiz.updated`,
//     `elearning.quiz.question.created`,
//     `elearning.quiz.question.updated`,
//     `elearning.quiz.question.deleted`,
//     `elearning.quiz.attempt.submitted` (graded result, no
//     correct answers inside), `elearning.quiz.attempt.passed`,
//     `elearning.quiz.attempt.failed`
//   - `elearning.certificate.issued`
//     (`{"certificate_id", "code", "course_id", "user_id",
//     "content_sha256"}`), `elearning.certificate.revoked`,
//     `elearning.certificate.anchor.requested`
//   - `elearning.review.created`, `elearning.review.updated`,
//     `elearning.review.deleted`
//   - `elearning.path.created`, `elearning.path.updated`,
//     `elearning.path.deleted`, `elearning.path.course.added`,
//     `elearning.path.course.removed`,
//     `elearning.path.completed`
//   - `web3.anchor.requested` (certificate anchoring, only when
//     anchoring is enabled in settings:
//     `{"record_type": "elearning.certificate", "record_id",
//     "content_sha256", "network_id"}` — the Web3 module owns the
//     anchor itself)
//   - `notification.requested` (enrollment activated, course
//     completed, certificate issued, in the notifications
//     module's payload shape).
// Events this module listens to (all read defensively — a missing
// field or an unknown course is skipped, never an error):
//   - `elearning.course.requested` — answers `{exists, course_id,
//     title, status}` for the academic module's course links.
//   - `order.paid`, `store.order.completed` — a paid order whose
//     payload names an e-learning course activates the matching
//     pending enrollment (`course_id` plus `module` =
//     `"elearning"`, or an `items` entry carrying `course_id`;
//     serialized store items carry `module_id`, which is accepted
//     too).
//   - `store.order.refunded`, `store.order.cancelled` — suspends
//     the matching active enrollment.
//   - `web3.anchor.confirmed`, `web3.anchor.failed` — updates the
//     anchor status of the named certificate (`record_id`).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
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

/// Maximum lengths enforced on user supplied text.
const MAX_TITLE_LEN: usize = 200;
const MAX_DESCRIPTION_LEN: usize = 5000;
const MAX_TEXT_LEN: usize = 20000;
const MAX_SLUG_LEN: usize = 64;
const MAX_REF_LEN: usize = 500;
const MAX_COMMENT_LEN: usize = 2000;
const MAX_QUESTIONS_PER_QUIZ: usize = 200;
const MAX_OPTIONS_PER_QUESTION: usize = 10;

/// Capability names checked through the `role.check` hook.
const PERM_ELEARNING_VIEW: &str = "elearning.view";
const PERM_ELEARNING_MANAGE: &str = "elearning.manage";
const PERM_ELEARNING_INSTRUCT: &str = "elearning.instruct";

/// Event names this module listens to.
const ORDER_PAID: &str = "order.paid";
const STORE_ORDER_COMPLETED: &str = "store.order.completed";
const STORE_ORDER_REFUNDED: &str = "store.order.refunded";
const STORE_ORDER_CANCELLED: &str = "store.order.cancelled";
const ANCHOR_CONFIRMED: &str = "web3.anchor.confirmed";
const ANCHOR_FAILED: &str = "web3.anchor.failed";

/// Course lifecycle statuses. Validated against the live settings
/// only through the transition table in `valid_course_transition`;
/// the strings themselves are the stored status values.
const STATUS_DRAFT: &str = "draft";
const STATUS_PUBLISHED: &str = "published";
const STATUS_ARCHIVED: &str = "archived";

/// Enrollment statuses.
const ENROLL_ACTIVE: &str = "active";
const ENROLL_PENDING_PAYMENT: &str = "pending_payment";
const ENROLL_COMPLETED: &str = "completed";
const ENROLL_CANCELLED: &str = "cancelled";
const ENROLL_SUSPENDED: &str = "suspended";

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; every
/// validation and default in this file reads the live value, so
/// changing a list here changes what the module accepts without
/// any code change. The values below are CONFIGURABLE DEFAULTS.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElearningSettings {
    /// Allowed course levels (for example beginner / intermediate).
    pub levels: Vec<String>,
    /// Allowed course categories.
    pub categories: Vec<String>,
    /// Allowed course languages, as language tags.
    pub languages: Vec<String>,
    /// Allowed lesson types.
    pub lesson_types: Vec<String>,
    /// Pass mark in percent (0-100) used when a quiz does not set one.
    pub default_pass_mark: i64,
    /// Attempt limit used when a quiz does not set one; None means
    /// unlimited attempts.
    pub default_max_attempts: Option<i64>,
    /// Default currency for paid courses (ISO code).
    pub default_currency: String,
    /// Name printed on certificates as the issuer.
    pub certificate_issuer_name: String,
    /// Whether issued certificates also request tamper-evidence
    /// anchoring through the Web3 flow (events only).
    pub certificate_anchoring_enabled: bool,
    /// Anchoring network identifier handed to the Web3 flow.
    pub anchoring_network_id: String,
}

impl Default for ElearningSettings {
    fn default() -> Self {
        Self {
            levels: vec![
                "beginner".to_string(),
                "intermediate".to_string(),
                "advanced".to_string(),
                "all-levels".to_string(),
            ],
            categories: vec![
                "business".to_string(),
                "technology".to_string(),
                "design".to_string(),
                "language".to_string(),
                "health".to_string(),
                "finance".to_string(),
            ],
            languages: vec!["en".to_string()],
            lesson_types: vec![
                "video".to_string(),
                "text".to_string(),
                "file".to_string(),
                "quiz".to_string(),
            ],
            default_pass_mark: 70,
            default_max_attempts: None,
            default_currency: "USD".to_string(),
            certificate_issuer_name: "Unified Academy".to_string(),
            certificate_anchoring_enabled: false,
            anchoring_network_id: "mainnet".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// One course. `price_minor` is the price in the smallest currency
/// unit (cents); 0 means free. `thumbnail_ref` is a storage
/// reference only — the storage module owns the bytes.
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
/// Correct answers are never stored in the serialized attempt —
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
/// correct option index (review only).
#[derive(Debug, Clone, Serialize)]
pub struct AttemptReviewItem {
    pub question_id: Uuid,
    pub selected_index: usize,
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

// ---------------------------------------------------------------------------
// Request bodies
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateCourseRequest {
    pub title: String,
    pub description: String,
    pub category: String,
    pub level: String,
    pub language: Option<String>,
    pub thumbnail_ref: Option<String>,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub certificate_enabled: Option<bool>,
    pub company_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateCourseRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    pub category: Option<String>,
    pub level: Option<String>,
    pub language: Option<String>,
    pub thumbnail_ref: Option<Option<String>>,
    pub price_minor: Option<i64>,
    pub currency: Option<String>,
    pub certificate_enabled: Option<bool>,
    pub company_id: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateModuleRequest {
    pub title: String,
    pub description: Option<String>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateModuleRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLessonRequest {
    pub title: String,
    pub lesson_type: String,
    pub content_ref: Option<String>,
    pub body: Option<String>,
    pub position: Option<i64>,
    pub required: Option<bool>,
    pub available_from: Option<DateTime<Utc>>,
    pub requires_previous: Option<bool>,
    pub duration_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateLessonRequest {
    pub title: Option<String>,
    pub lesson_type: Option<String>,
    pub content_ref: Option<Option<String>>,
    pub body: Option<Option<String>>,
    pub position: Option<i64>,
    pub required: Option<bool>,
    pub available_from: Option<Option<DateTime<Utc>>>,
    pub requires_previous: Option<bool>,
    pub duration_minutes: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct EnrollRequest {
    /// Display name recorded on the certificate when one is issued.
    pub learner_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuizRequest {
    pub title: String,
    pub description: Option<String>,
    pub pass_mark: Option<i64>,
    pub max_attempts: Option<Option<i64>>,
    pub allow_review: Option<bool>,
    pub time_limit_minutes: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuizRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
    pub pass_mark: Option<i64>,
    pub max_attempts: Option<Option<i64>>,
    pub allow_review: Option<bool>,
    pub time_limit_minutes: Option<Option<i64>>,
}

#[derive(Debug, Deserialize)]
pub struct CreateQuestionRequest {
    pub question: String,
    pub options: Vec<String>,
    pub correct_index: usize,
    pub points: Option<i64>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateQuestionRequest {
    pub question: Option<String>,
    pub options: Option<Vec<String>>,
    pub correct_index: Option<usize>,
    pub points: Option<i64>,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct SubmitAttemptRequest {
    pub answers: Vec<AttemptAnswer>,
}

#[derive(Debug, Deserialize)]
pub struct CreateReviewRequest {
    pub rating: i64,
    pub comment: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateReviewRequest {
    pub rating: Option<i64>,
    pub comment: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct CreatePathRequest {
    pub title: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdatePathRequest {
    pub title: Option<String>,
    pub description: Option<Option<String>>,
}

#[derive(Debug, Deserialize)]
pub struct AddPathCourseRequest {
    pub course_id: String,
    pub position: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateSettingsRequest {
    pub levels: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
    pub languages: Option<Vec<String>>,
    pub lesson_types: Option<Vec<String>>,
    pub default_pass_mark: Option<i64>,
    pub default_max_attempts: Option<Option<i64>>,
    pub default_currency: Option<String>,
    pub certificate_issuer_name: Option<String>,
    pub certificate_anchoring_enabled: Option<bool>,
    pub anchoring_network_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for e-learning. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000076_elearning`; until that migration is
/// executed, e-learning data does not survive a restart.
#[derive(Clone)]
pub struct ElearningState {
    pub courses: Arc<DashMap<Uuid, Course>>,
    pub modules: Arc<DashMap<Uuid, CourseModule>>,
    pub lessons: Arc<DashMap<Uuid, Lesson>>,
    pub enrollments: Arc<DashMap<Uuid, Enrollment>>,
    pub progress: Arc<DashMap<Uuid, LessonProgress>>,
    pub quizzes: Arc<DashMap<Uuid, Quiz>>,
    pub questions: Arc<DashMap<Uuid, QuizQuestion>>,
    pub attempts: Arc<DashMap<Uuid, QuizAttempt>>,
    pub certificates: Arc<DashMap<Uuid, Certificate>>,
    pub reviews: Arc<DashMap<Uuid, Review>>,
    pub paths: Arc<DashMap<Uuid, LearningPath>>,
    pub path_courses: Arc<DashMap<Uuid, LearningPathCourse>>,
    pub settings: Arc<RwLock<ElearningSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ElearningState {
    fn default() -> Self {
        Self {
            courses: Arc::new(DashMap::new()),
            modules: Arc::new(DashMap::new()),
            lessons: Arc::new(DashMap::new()),
            enrollments: Arc::new(DashMap::new()),
            progress: Arc::new(DashMap::new()),
            quizzes: Arc::new(DashMap::new()),
            questions: Arc::new(DashMap::new()),
            attempts: Arc::new(DashMap::new()),
            certificates: Arc::new(DashMap::new()),
            reviews: Arc::new(DashMap::new()),
            paths: Arc::new(DashMap::new()),
            path_courses: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ElearningSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ElearningState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> ElearningSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "elearning",
                "category": "elearning",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the events module.
struct Caller {
    actor: String,
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller may manage content: the manage capability
/// or the instructor capability.
fn can_teach(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_ELEARNING_MANAGE)
        || check_capability(ctx, role, PERM_ELEARNING_INSTRUCT)
}

fn require_teach(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_teach(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_ELEARNING_MANAGE}\" or \"{PERM_ELEARNING_INSTRUCT}\""
        )))
    }
}

/// Instructors may only change their own courses; managers may
/// change any course.
fn require_course_write(
    ctx: &AppContext,
    caller: &Caller,
    course: &Course,
) -> UnifiedResult<()> {
    if check_capability(ctx, &caller.role, PERM_ELEARNING_MANAGE) {
        return Ok(());
    }
    if check_capability(ctx, &caller.role, PERM_ELEARNING_INSTRUCT)
        && course.instructor_id == caller.actor
    {
        return Ok(());
    }
    Err(UnifiedError::forbidden(
        "Only a manager or the course instructor may change this course",
    ))
}

fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )))
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one — never a constant in this file.
fn require_in_list(value: &str, field: &str, allowed: &[String]) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

fn require_currency(value: &str) -> UnifiedResult<String> {
    let checked = require_text(value, "currency", 3)?;
    if checked.len() != 3 || !checked.chars().all(|c| c.is_ascii_alphabetic()) {
        return Err(UnifiedError::bad_request(
            "currency must be a 3-letter ISO code",
        ));
    }
    Ok(checked.to_ascii_uppercase())
}

fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

/// Allowed course status transitions. Anything not listed is
/// rejected with a 422 so a course can never jump backwards.
fn valid_course_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (STATUS_DRAFT, STATUS_PUBLISHED)
            | (STATUS_DRAFT, STATUS_ARCHIVED)
            | (STATUS_PUBLISHED, STATUS_ARCHIVED)
            | (STATUS_ARCHIVED, STATUS_DRAFT)
    )
}

// --- Loaders ---------------------------------------------------------------

fn load_course(state: &ElearningState, raw_id: &str) -> UnifiedResult<Course> {
    let id = parse_uuid(raw_id, "course")?;
    state
        .courses
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Course not found: {id}")))
}

fn load_module(state: &ElearningState, raw_id: &str) -> UnifiedResult<CourseModule> {
    let id = parse_uuid(raw_id, "module")?;
    state
        .modules
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Module not found: {id}")))
}

fn load_lesson(state: &ElearningState, raw_id: &str) -> UnifiedResult<Lesson> {
    let id = parse_uuid(raw_id, "lesson")?;
    state
        .lessons
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Lesson not found: {id}")))
}

fn load_enrollment(state: &ElearningState, raw_id: &str) -> UnifiedResult<Enrollment> {
    let id = parse_uuid(raw_id, "enrollment")?;
    state
        .enrollments
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Enrollment not found: {id}")))
}

fn load_quiz_for_lesson(state: &ElearningState, lesson_id: Uuid) -> UnifiedResult<Quiz> {
    state
        .quizzes
        .iter()
        .map(|entry| entry.value().clone())
        .find(|quiz| quiz.lesson_id == lesson_id)
        .ok_or_else(|| UnifiedError::not_found("No quiz is defined for this lesson"))
}

fn load_question(state: &ElearningState, raw_id: &str) -> UnifiedResult<QuizQuestion> {
    let id = parse_uuid(raw_id, "question")?;
    state
        .questions
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Question not found: {id}")))
}

fn load_attempt(state: &ElearningState, raw_id: &str) -> UnifiedResult<QuizAttempt> {
    let id = parse_uuid(raw_id, "attempt")?;
    state
        .attempts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Attempt not found: {id}")))
}

fn load_certificate(state: &ElearningState, raw_id: &str) -> UnifiedResult<Certificate> {
    let id = parse_uuid(raw_id, "certificate")?;
    state
        .certificates
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Certificate not found: {id}")))
}

fn load_review(state: &ElearningState, raw_id: &str) -> UnifiedResult<Review> {
    let id = parse_uuid(raw_id, "review")?;
    state
        .reviews
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Review not found: {id}")))
}

fn load_path(state: &ElearningState, raw_id: &str) -> UnifiedResult<LearningPath> {
    let id = parse_uuid(raw_id, "learning path")?;
    state
        .paths
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Learning path not found: {id}")))
}

/// The caller's live (not cancelled) enrollment in a course, if any.
fn find_enrollment(state: &ElearningState, course_id: Uuid, user_id: &str) -> Option<Enrollment> {
    state
        .enrollments
        .iter()
        .map(|entry| entry.value().clone())
        .find(|row| {
            row.course_id == course_id
                && row.user_id == user_id
                && row.status != ENROLL_CANCELLED
        })
}

/// All lessons of a course in course order (module position,
/// then lesson position).
fn ordered_lessons(state: &ElearningState, course_id: Uuid) -> Vec<Lesson> {
    let module_positions: HashMap<Uuid, i64> = state
        .modules
        .iter()
        .map(|entry| (entry.value().id, entry.value().position))
        .collect();
    let mut lessons: Vec<Lesson> = state
        .lessons
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|lesson| lesson.course_id == course_id)
        .collect();
    lessons.sort_by_key(|lesson| {
        (
            module_positions.get(&lesson.module_id).copied().unwrap_or(i64::MAX),
            lesson.position,
        )
    });
    lessons
}

/// Whether a lesson is currently available to a learner under the
/// drip rules: the drip date must have passed, and when the lesson
/// requires the previous one, that lesson must be complete.
fn lesson_available(
    state: &ElearningState,
    lesson: &Lesson,
    enrollment: &Enrollment,
) -> UnifiedResult<bool> {
    if let Some(from) = lesson.available_from {
        if Utc::now() < from {
            return Ok(false);
        }
    }
    if lesson.requires_previous {
        let lessons = ordered_lessons(state, lesson.course_id);
        if let Some(index) = lessons.iter().position(|row| row.id == lesson.id) {
            if index > 0 {
                let previous = &lessons[index - 1];
                let done = state.progress.iter().any(|entry| {
                    let row = entry.value();
                    row.enrollment_id == enrollment.id
                        && row.lesson_id == previous.id
                        && row.completed
                });
                if !done {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

/// Whether the lesson's quiz (when one exists) has a passed
/// attempt for this enrollment. Quiz lessons cannot be completed
/// by hand until the quiz is passed.
fn quiz_passed(state: &ElearningState, lesson_id: Uuid, enrollment_id: Uuid) -> bool {
    let Some(quiz) = state
        .quizzes
        .iter()
        .map(|entry| entry.value().clone())
        .find(|quiz| quiz.lesson_id == lesson_id)
    else {
        return false;
    };
    state.attempts.iter().any(|entry| {
        let row = entry.value();
        row.quiz_id == quiz.id && row.enrollment_id == enrollment_id && row.passed
    })
}

/// Progress of one enrollment, computed on read from lesson
/// progress rows. A course with no required lessons is complete
/// as soon as the learner enrolls (progress 100).
fn course_progress(state: &ElearningState, enrollment: &Enrollment) -> CourseProgress {
    let lessons = ordered_lessons(state, enrollment.course_id);
    let required: Vec<&Lesson> = lessons.iter().filter(|lesson| lesson.required).collect();
    let total_required = required.len();
    let completed_required = required
        .iter()
        .filter(|lesson| {
            state.progress.iter().any(|entry| {
                let row = entry.value();
                row.enrollment_id == enrollment.id
                    && row.lesson_id == lesson.id
                    && row.completed
            })
        })
        .count();
    let percent = if total_required == 0 {
        100
    } else {
        ((completed_required as i64 * 100) / total_required as i64).clamp(0, 100)
    };
    CourseProgress {
        course_id: enrollment.course_id,
        user_id: enrollment.user_id.clone(),
        enrollment_id: enrollment.id,
        enrollment_status: enrollment.status.clone(),
        total_required,
        completed_required,
        progress_percent: percent,
        completed: total_required == 0 || completed_required == total_required,
    }
}

/// A random verifiable certificate code. Only the code is ever
/// returned to the learner; nothing secret is derived from it.
fn new_certificate_code() -> String {
    let mut rng = rand::rng();
    let groups: Vec<String> = (0..3)
        .map(|_| {
            (&mut rng)
                .sample_iter(&Alphanumeric)
                .take(4)
                .map(char::from)
                .collect::<String>()
                .to_ascii_uppercase()
        })
        .collect();
    format!("ELRN-{}", groups.join("-"))
}

/// SHA-256 fingerprint of the canonical certificate payload. This
/// is the value handed to the anchoring flow — never the payload
/// itself, so no personal data leaves the application.
fn certificate_fingerprint(
    code: &str,
    course_id: Uuid,
    user_id: &str,
    learner_name: &str,
    course_title: &str,
    issuer_name: &str,
    issued_at: DateTime<Utc>,
) -> String {
    let canonical = format!(
        "elearning-certificate|{code}|{course_id}|{user_id}|{learner_name}|{course_title}|{issuer_name}|{}",
        issued_at.to_rfc3339()
    );
    let digest = Sha256::digest(canonical.as_bytes());
    format!("{digest:x}")
}

/// Issue a certificate for a completed enrollment, unless a valid
/// one already exists for the same (course, user). Emits
/// `elearning.certificate.issued` and, when anchoring is enabled
/// in settings, `web3.anchor.requested` plus
/// `elearning.certificate.anchor.requested`. Anchoring is events
/// only — this module never talks to a chain.
fn issue_certificate(
    state: &ElearningState,
    enrollment: &Enrollment,
    course: &Course,
    learner_name: &str,
) -> Option<Certificate> {
    if !course.certificate_enabled {
        return None;
    }
    if state.certificates.iter().any(|entry| {
        let row = entry.value();
        row.course_id == course.id && row.user_id == enrollment.user_id && row.status == "valid"
    }) {
        return None;
    }
    let settings = state.settings();
    let now = Utc::now();
    let code = new_certificate_code();
    let fingerprint = certificate_fingerprint(
        &code,
        course.id,
        &enrollment.user_id,
        learner_name,
        &course.title,
        &settings.certificate_issuer_name,
        now,
    );
    let certificate = Certificate {
        id: Uuid::now_v7(),
        code: code.clone(),
        course_id: course.id,
        user_id: enrollment.user_id.clone(),
        learner_name: learner_name.to_string(),
        course_title: course.title.clone(),
        issuer_name: settings.certificate_issuer_name.clone(),
        issued_at: now,
        content_sha256: fingerprint.clone(),
        status: "valid".to_string(),
        anchor_status: if settings.certificate_anchoring_enabled {
            "requested".to_string()
        } else {
            "none".to_string()
        },
        anchor_tx: None,
        revoked_at: None,
        created_at: now,
        updated_at: now,
    };
    state
        .certificates
        .insert(certificate.id, certificate.clone());
    state.emit(
        "elearning.certificate.issued",
        json!({
            "certificate_id": certificate.id.to_string(),
            "code": code,
            "course_id": course.id.to_string(),
            "user_id": enrollment.user_id,
            "content_sha256": fingerprint,
        }),
    );
    if settings.certificate_anchoring_enabled {
        state.emit(
            "web3.anchor.requested",
            json!({
                "record_type": "elearning.certificate",
                "record_id": certificate.id.to_string(),
                "content_sha256": fingerprint,
                "network_id": settings.anchoring_network_id,
            }),
        );
        state.emit(
            "elearning.certificate.anchor.requested",
            json!({
                "certificate_id": certificate.id.to_string(),
                "code": code,
                "content_sha256": fingerprint,
            }),
        );
    }
    state.notify(
        "elearning.certificate.issued",
        &enrollment.user_id,
        &format!("Certificate issued: {}", course.title),
        &format!(
            "Your certificate for \"{}\" is ready. Verification code: {code}.",
            course.title
        ),
        Some(format!("/api/v1/elearning/certificates/verify/{code}")),
    );
    Some(certificate)
}

/// After a lesson completion, finish the enrollment (and issue a
/// certificate) when every required lesson is done.
fn maybe_complete_course(state: &ElearningState, enrollment: &Enrollment) {
    let progress = course_progress(state, enrollment);
    if !progress.completed || enrollment.status != ENROLL_ACTIVE {
        return;
    }
    let Some(mut row) = state.enrollments.get_mut(&enrollment.id) else {
        return;
    };
    if row.status != ENROLL_ACTIVE {
        return;
    }
    let now = Utc::now();
    row.status = ENROLL_COMPLETED.to_string();
    row.completed_at = Some(now);
    row.updated_at = now;
    let updated = row.clone();
    drop(row);
    let Some(course) = state.courses.get(&enrollment.course_id).map(|entry| entry.value().clone()) else {
        return;
    };
    state.emit(
        "elearning.enrollment.completed",
        json!({
            "enrollment_id": updated.id.to_string(),
            "course_id": updated.course_id.to_string(),
            "user_id": updated.user_id,
        }),
    );
    state.notify(
        "elearning.enrollment.completed",
        &updated.user_id,
        &format!("Course completed: {}", course.title),
        &format!("You completed \"{}\".", course.title),
        Some(format!("/api/v1/elearning/courses/{}/progress", course.id)),
    );
    let learner_name = updated
        .learner_name
        .clone()
        .unwrap_or_else(|| updated.user_id.clone());
    issue_certificate(state, &updated, &course, &learner_name);
}

// ---------------------------------------------------------------------------
// Hook listeners (store / Web3 events; defensive reads only)
// ---------------------------------------------------------------------------

/// Extract the e-learning course id from a store order payload:
/// either a top-level `course_id` with `module` = `"elearning"`,
/// or an `items` entry carrying `course_id`. Both spellings of
/// the module marker are accepted: serialized store order items
/// carry `module_id`, never `module`, so matching on `module`
/// alone would never see a real store order.
fn order_course_id(data: &Value) -> Option<(Uuid, Option<String>)> {
    let module = opt_string(data, "module").or_else(|| opt_string(data, "module_id"));
    if module.as_deref() == Some("elearning") {
        if let Some(raw) = opt_string(data, "course_id") {
            if let Ok(id) = Uuid::parse_str(&raw) {
                return Some((id, opt_string(data, "order_id").or_else(|| opt_string(data, "id"))));
            }
        }
    }
    if let Some(items) = data.get("items").and_then(|value| value.as_array()) {
        for item in items {
            let item_module =
                opt_string(item, "module").or_else(|| opt_string(item, "module_id"));
            if item_module.as_deref() == Some("elearning") {
                if let Some(raw) = opt_string(item, "course_id") {
                    if let Ok(id) = Uuid::parse_str(&raw) {
                        return Some((id, opt_string(data, "order_id").or_else(|| opt_string(data, "id"))));
                    }
                }
            }
        }
    }
    None
}

/// Answer `elearning.course.requested` (`{course_id}` — the
/// academic module asks whether a course exists before linking a
/// subject to it): add `{exists, course_id, title, status}` for a
/// known course, or `{exists: false, course_id}` when the id is
/// missing, malformed, or unknown, so the caller can record the
/// link unverified instead of waiting on a silent no-answer.
fn on_course_requested(state: ElearningState, ctx: &mut HookContext) {
    let course_id = ctx
        .get("course_id")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    match load_course(&state, &course_id) {
        Ok(course) => {
            ctx.add_result(json!({
                "exists": true,
                "course_id": course.id.to_string(),
                "title": course.title,
                "status": course.status,
            }));
        }
        Err(_) => {
            ctx.add_result(json!({
                "exists": false,
                "course_id": course_id,
            }));
        }
    }
}

/// Activate the pending enrollment that matches a paid order.
/// The buyer is read from `user_id` (falling back to
/// `customer_id`); a missing buyer or course id is skipped.
fn on_order_paid(state: ElearningState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some((course_id, order_id)) = order_course_id(&data) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no e-learning course_id" }));
        return;
    };
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    let Some(mut enrollment) = state
        .enrollments
        .iter_mut()
        .find(|entry| {
            entry.value().course_id == course_id
                && entry.value().user_id == user_id
                && entry.value().status == ENROLL_PENDING_PAYMENT
        })
    else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "no pending enrollment matches this order" }));
        return;
    };
    enrollment.status = ENROLL_ACTIVE.to_string();
    if let Some(order) = order_id {
        enrollment.order_id = Some(order);
    }
    enrollment.updated_at = Utc::now();
    let updated = enrollment.clone();
    drop(enrollment);
    state.emit(
        "elearning.enrollment.activated",
        json!({
            "enrollment_id": updated.id.to_string(),
            "course_id": updated.course_id.to_string(),
            "user_id": updated.user_id,
        }),
    );
    state.notify(
        "elearning.enrollment.activated",
        &updated.user_id,
        "Enrollment activated",
        "Your paid enrollment is now active.",
        Some(format!("/api/v1/elearning/courses/{}/progress", updated.course_id)),
    );
    ctx.add_result(json!({ "elearning": "activated", "enrollment_id": updated.id.to_string() }));
}

/// Suspend the active enrollment that matches a refunded or
/// cancelled order.
fn on_order_refunded_or_cancelled(state: ElearningState, ctx: &mut HookContext) {
    let data = ctx.data.clone();
    let Some((course_id, _)) = order_course_id(&data) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no e-learning course_id" }));
        return;
    };
    let Some(user_id) = opt_string(&data, "user_id").or_else(|| opt_string(&data, "customer_id")) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "order event carries no user_id or customer_id" }));
        return;
    };
    let Some(mut enrollment) = state.enrollments.iter_mut().find(|entry| {
        entry.value().course_id == course_id
            && entry.value().user_id == user_id
            && entry.value().status == ENROLL_ACTIVE
    }) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "no active enrollment matches this order" }));
        return;
    };
    enrollment.status = ENROLL_SUSPENDED.to_string();
    enrollment.updated_at = Utc::now();
    let updated = enrollment.clone();
    drop(enrollment);
    state.emit(
        "elearning.enrollment.suspended",
        json!({
            "enrollment_id": updated.id.to_string(),
            "course_id": updated.course_id.to_string(),
            "user_id": updated.user_id,
        }),
    );
    ctx.add_result(json!({ "elearning": "suspended", "enrollment_id": updated.id.to_string() }));
}

/// Record the anchoring outcome for the named certificate
/// (`record_id` is the certificate id).
fn on_anchor_outcome(state: ElearningState, ctx: &mut HookContext, confirmed: bool) {
    let data = ctx.data.clone();
    let Some(raw) = opt_string(&data, "record_id") else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "anchor event carries no record_id" }));
        return;
    };
    let Ok(id) = Uuid::parse_str(&raw) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "anchor record_id is not a UUID" }));
        return;
    };
    let Some(mut certificate) = state.certificates.get_mut(&id) else {
        ctx.add_result(json!({ "elearning": "skipped", "reason": "no certificate matches this anchor record" }));
        return;
    };
    certificate.anchor_status = if confirmed { "confirmed".to_string() } else { "failed".to_string() };
    if let Some(tx) = opt_string(&data, "tx_hash") {
        certificate.anchor_tx = Some(tx);
    }
    certificate.updated_at = Utc::now();
    ctx.add_result(json!({ "elearning": "anchor_updated", "certificate_id": id.to_string(), "anchor_status": certificate.anchor_status }));
}

// ---------------------------------------------------------------------------
// Course handlers
// ---------------------------------------------------------------------------

async fn list_courses(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let category_filter = params.filters.get("category").cloned();
        let level_filter = params.filters.get("level").cloned();
        let language_filter = params.filters.get("language").cloned();
        let status_filter = params.filters.get("status").cloned();
        let instructor_filter = params.filters.get("instructor_id").cloned();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let mut items: Vec<Course> = state
            .courses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|course| {
                // Learners only ever see published courses; managers
                // and instructors see every status.
                if !is_manager && !can_teach(&ctx, &caller.role) && course.status != STATUS_PUBLISHED {
                    return false;
                }
                if let Some(category) = &category_filter {
                    if &course.category != category {
                        return false;
                    }
                }
                if let Some(level) = &level_filter {
                    if &course.level != level {
                        return false;
                    }
                }
                if let Some(language) = &language_filter {
                    if &course.language != language {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &course.status != status {
                        return false;
                    }
                }
                if let Some(instructor) = &instructor_filter {
                    if &course.instructor_id != instructor {
                        return false;
                    }
                }
                if let Some(needle) = &search {
                    let haystack = format!("{} {}", course.title, course.description).to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|course| course.title.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    body: web::Json<CreateCourseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = require_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let category = require_in_list(&input.category, "category", &settings.categories)?;
        let level = require_in_list(&input.level, "level", &settings.levels)?;
        let language = match &input.language {
            Some(raw) => require_in_list(raw, "language", &settings.languages)?,
            None => settings.languages.first().cloned().unwrap_or_else(|| "en".to_string()),
        };
        let price_minor = input.price_minor.unwrap_or(0);
        if price_minor < 0 || price_minor > 1_000_000_000 {
            return Err(UnifiedError::bad_request(
                "price_minor must be between 0 and 1000000000",
            ));
        }
        let currency = match &input.currency {
            Some(raw) => require_currency(raw)?,
            None => settings.default_currency.clone(),
        };
        let thumbnail_ref = optional_text(&input.thumbnail_ref, "thumbnail_ref", MAX_REF_LEN)?;
        let company_id = optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?;
        let now = Utc::now();
        let course = Course {
            id: Uuid::now_v7(),
            title,
            description,
            category,
            level,
            language,
            thumbnail_ref,
            instructor_id: caller.actor.clone(),
            status: STATUS_DRAFT.to_string(),
            price_minor,
            currency,
            certificate_enabled: input.certificate_enabled.unwrap_or(true),
            company_id,
            created_at: now,
            updated_at: now,
        };
        state.courses.insert(course.id, course.clone());
        state.emit(
            "elearning.course.created",
            json!({
                "course_id": course.id.to_string(),
                "title": course.title,
                "instructor_id": course.instructor_id,
                "status": course.status,
            }),
        );
        Ok(course)
    })();
    match result {
        Ok(course) => created(course),
        Err(error) => err(error, debug),
    }
}

async fn get_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        if course.status != STATUS_PUBLISHED
            && !check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE)
            && course.instructor_id != caller.actor
        {
            return Err(UnifiedError::not_found("Course not found"));
        }
        Ok(course)
    })();
    match result {
        Ok(course) => ok(course),
        Err(error) => err(error, debug),
    }
}

async fn update_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCourseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let caller = caller(&req)?;
        let mut course = load_course(&state, &path.into_inner())?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let settings = state.settings();
        if let Some(title) = &input.title {
            course.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            course.description = require_text(description, "description", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(category) = &input.category {
            course.category = require_in_list(category, "category", &settings.categories)?;
        }
        if let Some(level) = &input.level {
            course.level = require_in_list(level, "level", &settings.levels)?;
        }
        if let Some(language) = &input.language {
            course.language = require_in_list(language, "language", &settings.languages)?;
        }
        if let Some(thumbnail_ref) = &input.thumbnail_ref {
            course.thumbnail_ref = match thumbnail_ref {
                Some(raw) => Some(require_text(raw, "thumbnail_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(price) = input.price_minor {
            if price < 0 || price > 1_000_000_000 {
                return Err(UnifiedError::bad_request(
                    "price_minor must be between 0 and 1000000000",
                ));
            }
            if state.enrollments.iter().any(|entry| {
                entry.value().course_id == course.id
                    && entry.value().status == ENROLL_PENDING_PAYMENT
            }) {
                return Err(UnifiedError::conflict(
                    "Course price cannot change while a paid enrollment is pending payment",
                ));
            }
            course.price_minor = price;
        }
        if let Some(currency) = &input.currency {
            course.currency = require_currency(currency)?;
        }
        if let Some(enabled) = input.certificate_enabled {
            course.certificate_enabled = enabled;
        }
        if let Some(company_id) = &input.company_id {
            course.company_id = match company_id {
                Some(raw) => Some(require_text(raw, "company_id", MAX_SLUG_LEN)?),
                None => None,
            };
        }
        course.updated_at = Utc::now();
        state.courses.insert(course.id, course.clone());
        state.emit(
            "elearning.course.updated",
            json!({ "course_id": course.id.to_string(), "title": course.title }),
        );
        Ok(course)
    })();
    match result {
        Ok(course) => ok(course),
        Err(error) => err(error, debug),
    }
}

async fn delete_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let course = load_course(&state, &path.into_inner())?;
        require_course_write(&ctx, &caller, &course)?;
        if state.enrollments.iter().any(|entry| {
            entry.value().course_id == course.id
                && entry.value().status != ENROLL_CANCELLED
        }) {
            return Err(UnifiedError::conflict(
                "Course cannot be deleted while enrollments exist; archive it instead",
            ));
        }
        let module_ids: Vec<Uuid> = state
            .modules
            .iter()
            .filter(|entry| entry.value().course_id == course.id)
            .map(|entry| *entry.key())
            .collect();
        for module_id in &module_ids {
            let lesson_ids: Vec<Uuid> = state
                .lessons
                .iter()
                .filter(|entry| entry.value().module_id == *module_id)
                .map(|entry| *entry.key())
                .collect();
            for lesson_id in lesson_ids {
                delete_lesson_rows(&state, lesson_id);
            }
            state.modules.remove(module_id);
        }
        state.reviews.retain(|_, row| row.course_id != course.id);
        state.path_courses.retain(|_, row| row.course_id != course.id);
        state.courses.remove(&course.id);
        state.emit(
            "elearning.course.deleted",
            json!({ "course_id": course.id.to_string(), "title": course.title }),
        );
        Ok(json!({ "deleted": course.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Remove a lesson and everything attached to it (its quiz,
/// questions, attempts, and progress rows).
fn delete_lesson_rows(state: &ElearningState, lesson_id: Uuid) {
    let quiz_ids: Vec<Uuid> = state
        .quizzes
        .iter()
        .filter(|entry| entry.value().lesson_id == lesson_id)
        .map(|entry| *entry.key())
        .collect();
    for quiz_id in quiz_ids {
        state.questions.retain(|_, row| row.quiz_id != quiz_id);
        state.attempts.retain(|_, row| row.quiz_id != quiz_id);
        state.quizzes.remove(&quiz_id);
    }
    state.progress.retain(|_, row| row.lesson_id != lesson_id);
    state.lessons.remove(&lesson_id);
}

async fn transition_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    target: &str,
    event: &str,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Course> = (|| {
        let caller = caller(&req)?;
        let mut course = load_course(&state, &path.into_inner())?;
        require_course_write(&ctx, &caller, &course)?;
        if !valid_course_transition(&course.status, target) {
            return Err(UnifiedError::unprocessable(format!(
                "Cannot move course from {} to {target}",
                course.status
            )));
        }
        if target == STATUS_PUBLISHED {
            let has_lesson = state
                .lessons
                .iter()
                .any(|entry| entry.value().course_id == course.id);
            if !has_lesson {
                return Err(UnifiedError::unprocessable(
                    "Cannot publish a course with no lessons",
                ));
            }
        }
        course.status = target.to_string();
        course.updated_at = Utc::now();
        state.courses.insert(course.id, course.clone());
        state.emit(
            event,
            json!({ "course_id": course.id.to_string(), "status": course.status }),
        );
        Ok(course)
    })();
    match result {
        Ok(course) => ok(course),
        Err(error) => err(error, debug),
    }
}

async fn publish_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_course(ctx, state, req, path, STATUS_PUBLISHED, "elearning.course.published").await
}

async fn archive_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    transition_course(ctx, state, req, path, STATUS_ARCHIVED, "elearning.course.archived").await
}

// ---------------------------------------------------------------------------
// Module and lesson handlers
// ---------------------------------------------------------------------------

async fn list_course_modules(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        let params = query.into_inner();
        let mut items: Vec<CourseModule> = state
            .modules
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.course_id == course.id)
            .collect();
        items.sort_by_key(|row| row.position);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_course_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseModule> = (|| {
        let caller = caller(&req)?;
        let course = load_course(&state, &path.into_inner())?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .modules
                .iter()
                .filter(|entry| entry.value().course_id == course.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let now = Utc::now();
        let row = CourseModule {
            id: Uuid::now_v7(),
            course_id: course.id,
            title,
            description,
            position,
            created_at: now,
            updated_at: now,
        };
        state.modules.insert(row.id, row.clone());
        state.emit(
            "elearning.module.created",
            json!({ "module_id": row.id.to_string(), "course_id": course.id.to_string(), "title": row.title }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn get_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseModule> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        load_module(&state, &path.into_inner())
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn update_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateModuleRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseModule> = (|| {
        let caller = caller(&req)?;
        let mut row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            row.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            row.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("position must be >= 0"));
            }
            row.position = position;
        }
        row.updated_at = Utc::now();
        state.modules.insert(row.id, row.clone());
        state.emit(
            "elearning.module.updated",
            json!({ "module_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn delete_module(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let lesson_ids: Vec<Uuid> = state
            .lessons
            .iter()
            .filter(|entry| entry.value().module_id == row.id)
            .map(|entry| *entry.key())
            .collect();
        for lesson_id in lesson_ids {
            delete_lesson_rows(&state, lesson_id);
        }
        state.modules.remove(&row.id);
        state.emit(
            "elearning.module.deleted",
            json!({ "module_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(json!({ "deleted": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_module_lessons(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_module(&state, &path.into_inner())?;
        let params = query.into_inner();
        let type_filter = params.filters.get("lesson_type").cloned();
        let mut items: Vec<Lesson> = state
            .lessons
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|lesson| {
                if lesson.module_id != row.id {
                    return false;
                }
                if let Some(kind) = &type_filter {
                    if &lesson.lesson_type != kind {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|lesson| lesson.position);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateLessonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lesson> = (|| {
        let caller = caller(&req)?;
        let row = load_module(&state, &path.into_inner())?;
        let course = load_course(&state, &row.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let lesson_type = require_in_list(&input.lesson_type, "lesson_type", &settings.lesson_types)?;
        // Content rules per type: video and file lessons need a
        // storage reference, text lessons need a body, quiz lessons
        // need neither (their quiz is defined separately).
        let content_ref = optional_text(&input.content_ref, "content_ref", MAX_REF_LEN)?;
        let body_text = optional_text(&input.body, "body", MAX_TEXT_LEN)?;
        match lesson_type.as_str() {
            "video" | "file" => {
                if content_ref.is_none() {
                    return Err(UnifiedError::bad_request(format!(
                        "{lesson_type} lessons require content_ref (a storage reference)"
                    )));
                }
            }
            "text" => {
                if body_text.as_deref().map(str::is_empty).unwrap_or(true) {
                    return Err(UnifiedError::bad_request(
                        "text lessons require body",
                    ));
                }
            }
            _ => {}
        }
        if let Some(minutes) = input.duration_minutes {
            if minutes < 0 || minutes > 100_000 {
                return Err(UnifiedError::bad_request(
                    "duration_minutes must be between 0 and 100000",
                ));
            }
        }
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .lessons
                .iter()
                .filter(|entry| entry.value().module_id == row.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let now = Utc::now();
        let lesson = Lesson {
            id: Uuid::now_v7(),
            module_id: row.id,
            course_id: course.id,
            title,
            lesson_type,
            content_ref,
            body: body_text,
            position,
            required: input.required.unwrap_or(true),
            available_from: input.available_from,
            requires_previous: input.requires_previous.unwrap_or(false),
            duration_minutes: input.duration_minutes,
            created_at: now,
            updated_at: now,
        };
        state.lessons.insert(lesson.id, lesson.clone());
        state.emit(
            "elearning.lesson.created",
            json!({ "lesson_id": lesson.id.to_string(), "course_id": course.id.to_string(), "lesson_type": lesson.lesson_type }),
        );
        Ok(lesson)
    })();
    match result {
        Ok(lesson) => created(lesson),
        Err(error) => err(error, debug),
    }
}

async fn get_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lesson> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        load_lesson(&state, &path.into_inner())
    })();
    match result {
        Ok(lesson) => ok(lesson),
        Err(error) => err(error, debug),
    }
}

async fn update_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateLessonRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Lesson> = (|| {
        let caller = caller(&req)?;
        let mut lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let settings = state.settings();
        if let Some(title) = &input.title {
            lesson.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(kind) = &input.lesson_type {
            lesson.lesson_type = require_in_list(kind, "lesson_type", &settings.lesson_types)?;
        }
        if let Some(content_ref) = &input.content_ref {
            lesson.content_ref = match content_ref {
                Some(raw) => Some(require_text(raw, "content_ref", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(body_text) = &input.body {
            lesson.body = match body_text {
                Some(raw) => Some(require_text(raw, "body", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("position must be >= 0"));
            }
            lesson.position = position;
        }
        if let Some(required) = input.required {
            lesson.required = required;
        }
        if let Some(available_from) = &input.available_from {
            lesson.available_from = *available_from;
        }
        if let Some(requires_previous) = input.requires_previous {
            lesson.requires_previous = requires_previous;
        }
        if let Some(duration) = &input.duration_minutes {
            if let Some(minutes) = duration {
                if *minutes < 0 || *minutes > 100_000 {
                    return Err(UnifiedError::bad_request(
                        "duration_minutes must be between 0 and 100000",
                    ));
                }
            }
            lesson.duration_minutes = *duration;
        }
        // Re-check the content rules after the edit.
        match lesson.lesson_type.as_str() {
            "video" | "file" => {
                if lesson.content_ref.is_none() {
                    return Err(UnifiedError::bad_request(format!(
                        "{} lessons require content_ref (a storage reference)",
                        lesson.lesson_type
                    )));
                }
            }
            "text" => {
                if lesson.body.as_deref().map(str::is_empty).unwrap_or(true) {
                    return Err(UnifiedError::bad_request("text lessons require body"));
                }
            }
            _ => {}
        }
        lesson.updated_at = Utc::now();
        state.lessons.insert(lesson.id, lesson.clone());
        state.emit(
            "elearning.lesson.updated",
            json!({ "lesson_id": lesson.id.to_string(), "course_id": lesson.course_id.to_string() }),
        );
        Ok(lesson)
    })();
    match result {
        Ok(lesson) => ok(lesson),
        Err(error) => err(error, debug),
    }
}

async fn delete_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        delete_lesson_rows(&state, lesson.id);
        state.emit(
            "elearning.lesson.deleted",
            json!({ "lesson_id": lesson.id.to_string(), "course_id": lesson.course_id.to_string() }),
        );
        Ok(json!({ "deleted": lesson.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Enrollment and progress handlers
// ---------------------------------------------------------------------------

async fn enroll_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<EnrollRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        if course.status != STATUS_PUBLISHED {
            return Err(UnifiedError::unprocessable(
                "Only published courses accept enrollments",
            ));
        }
        if let Some(existing) = find_enrollment(&state, course.id, &caller.actor) {
            if existing.status == ENROLL_ACTIVE || existing.status == ENROLL_COMPLETED {
                return Err(UnifiedError::conflict(
                    "Already enrolled in this course",
                ));
            }
            if existing.status == ENROLL_PENDING_PAYMENT {
                return Err(UnifiedError::conflict(
                    "A paid enrollment is already pending payment for this course",
                ));
            }
            if existing.status == ENROLL_SUSPENDED {
                return Err(UnifiedError::conflict(
                    "Enrollment is suspended; contact support to reactivate it",
                ));
            }
        }
        let input = body.into_inner();
        let learner_name = optional_text(&input.learner_name, "learner_name", MAX_TITLE_LEN)?;
        let now = Utc::now();
        let paid = course.price_minor > 0;
        let enrollment = Enrollment {
            id: Uuid::now_v7(),
            course_id: course.id,
            user_id: caller.actor.clone(),
            status: if paid {
                ENROLL_PENDING_PAYMENT.to_string()
            } else {
                ENROLL_ACTIVE.to_string()
            },
            price_paid_minor: course.price_minor,
            currency: course.currency.clone(),
            order_id: None,
            learner_name,
            enrolled_at: now,
            completed_at: None,
            created_at: now,
            updated_at: now,
        };
        state.enrollments.insert(enrollment.id, enrollment.clone());
        state.emit(
            "elearning.enrollment.created",
            json!({
                "enrollment_id": enrollment.id.to_string(),
                "course_id": course.id.to_string(),
                "user_id": enrollment.user_id,
                "status": enrollment.status,
            }),
        );
        if paid {
            // Paid enrollment never calls the store directly: it
            // emits a purchase request and waits for `order.paid`.
            state.emit(
                "elearning.enrollment.purchase_requested",
                json!({
                    "enrollment_id": enrollment.id.to_string(),
                    "course_id": course.id.to_string(),
                    "user_id": enrollment.user_id,
                    "amount_minor": course.price_minor,
                    "currency": course.currency,
                    "module": "elearning",
                    "item_type": "course_enrollment",
                }),
            );
        } else {
            state.notify(
                "elearning.enrollment.activated",
                &enrollment.user_id,
                &format!("Enrolled: {}", course.title),
                &format!("You are enrolled in \"{}\".", course.title),
                Some(format!("/api/v1/elearning/courses/{}/progress", course.id)),
            );
        }
        Ok(enrollment)
    })();
    match result {
        Ok(enrollment) => created(enrollment),
        Err(error) => err(error, debug),
    }
}

async fn list_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let course_filter = params.filters.get("course_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(course_id) = &course_filter {
                    if row.course_id.to_string() != *course_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.enrolled_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_enrollment(&state, &path.into_inner())?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may view this enrollment",
            ));
        }
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn cancel_enrollment(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Enrollment> = (|| {
        let caller = caller(&req)?;
        let mut row = load_enrollment(&state, &path.into_inner())?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may cancel this enrollment",
            ));
        }
        if row.status == ENROLL_COMPLETED {
            return Err(UnifiedError::conflict(
                "A completed enrollment cannot be cancelled",
            ));
        }
        if row.status == ENROLL_CANCELLED {
            return Err(UnifiedError::conflict("Enrollment is already cancelled"));
        }
        row.status = ENROLL_CANCELLED.to_string();
        row.updated_at = Utc::now();
        state.enrollments.insert(row.id, row.clone());
        state.emit(
            "elearning.enrollment.cancelled",
            json!({
                "enrollment_id": row.id.to_string(),
                "course_id": row.course_id.to_string(),
                "user_id": row.user_id,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn list_my_enrollments(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Enrollment> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.user_id != caller.actor {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.enrolled_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn complete_lesson(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LessonProgress> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let Some(enrollment) = find_enrollment(&state, lesson.course_id, &caller.actor) else {
            return Err(UnifiedError::forbidden(
                "Enroll in this course before completing lessons",
            ));
        };
        if enrollment.status != ENROLL_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Enrollment is {}, not active",
                enrollment.status
            )));
        }
        if !lesson_available(&state, &lesson, &enrollment)? {
            return Err(UnifiedError::unprocessable(
                "Lesson is not available yet (drip schedule or previous lesson incomplete)",
            ));
        }
        // A lesson with a quiz is completed by passing the quiz,
        // never by hand.
        let has_quiz = state
            .quizzes
            .iter()
            .any(|entry| entry.value().lesson_id == lesson.id);
        if has_quiz && !quiz_passed(&state, lesson.id, enrollment.id) {
            return Err(UnifiedError::unprocessable(
                "Pass the lesson quiz before completing this lesson",
            ));
        }
        if let Some(existing) = state.progress.iter().find(|entry| {
            let row = entry.value();
            row.enrollment_id == enrollment.id && row.lesson_id == lesson.id && row.completed
        }) {
            return Ok(existing.value().clone());
        }
        let now = Utc::now();
        let row = LessonProgress {
            id: Uuid::now_v7(),
            enrollment_id: enrollment.id,
            lesson_id: lesson.id,
            course_id: lesson.course_id,
            user_id: caller.actor.clone(),
            completed: true,
            completed_at: Some(now),
            created_at: now,
            updated_at: now,
        };
        state.progress.insert(row.id, row.clone());
        state.emit(
            "elearning.lesson.completed",
            json!({
                "lesson_id": lesson.id.to_string(),
                "course_id": lesson.course_id.to_string(),
                "user_id": caller.actor,
                "enrollment_id": enrollment.id.to_string(),
            }),
        );
        maybe_complete_course(&state, &enrollment);
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn get_course_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseProgress> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        let Some(enrollment) = find_enrollment(&state, course.id, &caller.actor) else {
            return Err(UnifiedError::not_found("No enrollment in this course"));
        };
        Ok(course_progress(&state, &enrollment))
    })();
    match result {
        Ok(progress) => ok(progress),
        Err(error) => err(error, debug),
    }
}

async fn get_user_course_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CourseProgress> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let (course_id, user_id) = path.into_inner();
        let course = load_course(&state, &course_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let is_instructor = check_capability(&ctx, &caller.role, PERM_ELEARNING_INSTRUCT)
            && course.instructor_id == caller.actor;
        if !is_manager && !is_instructor && user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner, the course instructor, or a manager may view this progress",
            ));
        }
        let Some(enrollment) = find_enrollment(&state, course.id, &user_id) else {
            return Err(UnifiedError::not_found("No enrollment in this course"));
        };
        Ok(course_progress(&state, &enrollment))
    })();
    match result {
        Ok(progress) => ok(progress),
        Err(error) => err(error, debug),
    }
}

async fn list_my_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<CourseProgress> = state
            .enrollments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.user_id == caller.actor && row.status != ENROLL_CANCELLED)
            .map(|row| course_progress(&state, &row))
            .collect();
        items.sort_by_key(|row| row.course_id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Quiz handlers
// ---------------------------------------------------------------------------

async fn get_lesson_quiz(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Quiz> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        load_quiz_for_lesson(&state, lesson.id)
    })();
    match result {
        Ok(quiz) => ok(quiz),
        Err(error) => err(error, debug),
    }
}

async fn create_lesson_quiz(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuizRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Quiz> = (|| {
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        if state
            .quizzes
            .iter()
            .any(|entry| entry.value().lesson_id == lesson.id)
        {
            return Err(UnifiedError::conflict(
                "This lesson already has a quiz; update it instead",
            ));
        }
        let input = body.into_inner();
        let settings = state.settings();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let pass_mark = input.pass_mark.unwrap_or(settings.default_pass_mark);
        if pass_mark < 0 || pass_mark > 100 {
            return Err(UnifiedError::bad_request("pass_mark must be between 0 and 100"));
        }
        let max_attempts = match input.max_attempts {
            Some(value) => value,
            None => settings.default_max_attempts,
        };
        if let Some(limit) = max_attempts {
            if limit < 1 || limit > 1000 {
                return Err(UnifiedError::bad_request(
                    "max_attempts must be between 1 and 1000",
                ));
            }
        }
        if let Some(minutes) = input.time_limit_minutes {
            if minutes < 1 || minutes > 100_000 {
                return Err(UnifiedError::bad_request(
                    "time_limit_minutes must be between 1 and 100000",
                ));
            }
        }
        let now = Utc::now();
        let quiz = Quiz {
            id: Uuid::now_v7(),
            lesson_id: lesson.id,
            course_id: lesson.course_id,
            title,
            description,
            pass_mark,
            max_attempts,
            allow_review: input.allow_review.unwrap_or(false),
            time_limit_minutes: input.time_limit_minutes,
            created_at: now,
            updated_at: now,
        };
        state.quizzes.insert(quiz.id, quiz.clone());
        state.emit(
            "elearning.quiz.created",
            json!({ "quiz_id": quiz.id.to_string(), "lesson_id": lesson.id.to_string(), "course_id": lesson.course_id.to_string() }),
        );
        Ok(quiz)
    })();
    match result {
        Ok(quiz) => created(quiz),
        Err(error) => err(error, debug),
    }
}

async fn update_lesson_quiz(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateQuizRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Quiz> = (|| {
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let mut quiz = load_quiz_for_lesson(&state, lesson.id)?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            quiz.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            quiz.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(pass_mark) = input.pass_mark {
            if pass_mark < 0 || pass_mark > 100 {
                return Err(UnifiedError::bad_request("pass_mark must be between 0 and 100"));
            }
            quiz.pass_mark = pass_mark;
        }
        if let Some(max_attempts) = input.max_attempts {
            if let Some(limit) = max_attempts {
                if limit < 1 || limit > 1000 {
                    return Err(UnifiedError::bad_request(
                        "max_attempts must be between 1 and 1000",
                    ));
                }
            }
            quiz.max_attempts = max_attempts;
        }
        if let Some(allow_review) = input.allow_review {
            quiz.allow_review = allow_review;
        }
        if let Some(limit) = &input.time_limit_minutes {
            if let Some(minutes) = limit {
                if *minutes < 1 || *minutes > 100_000 {
                    return Err(UnifiedError::bad_request(
                        "time_limit_minutes must be between 1 and 100000",
                    ));
                }
            }
            quiz.time_limit_minutes = *limit;
        }
        quiz.updated_at = Utc::now();
        state.quizzes.insert(quiz.id, quiz.clone());
        state.emit(
            "elearning.quiz.updated",
            json!({ "quiz_id": quiz.id.to_string(), "lesson_id": lesson.id.to_string() }),
        );
        Ok(quiz)
    })();
    match result {
        Ok(quiz) => ok(quiz),
        Err(error) => err(error, debug),
    }
}

fn validate_question_input(
    question: &str,
    options: &[String],
    correct_index: usize,
    points: Option<i64>,
) -> UnifiedResult<(String, Vec<String>, i64)> {
    let checked_question = require_text(question, "question", MAX_TEXT_LEN)?;
    if options.len() < 2 || options.len() > MAX_OPTIONS_PER_QUESTION {
        return Err(UnifiedError::bad_request(format!(
            "options must hold between 2 and {MAX_OPTIONS_PER_QUESTION} choices"
        )));
    }
    let mut checked_options = Vec::with_capacity(options.len());
    for option in options {
        checked_options.push(require_text(option, "option", MAX_TEXT_LEN)?);
    }
    if correct_index >= checked_options.len() {
        return Err(UnifiedError::bad_request(
            "correct_index is outside the options list",
        ));
    }
    let checked_points = points.unwrap_or(1);
    if checked_points < 1 || checked_points > 1000 {
        return Err(UnifiedError::bad_request("points must be between 1 and 1000"));
    }
    Ok((checked_question, checked_options, checked_points))
}

async fn list_quiz_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let quiz = load_quiz_for_lesson(&state, lesson.id)?;
        let params = query.into_inner();
        // Learners taking the quiz must never see the correct
        // answers: the question list hides `correct_index` unless
        // the caller may teach this course.
        let course = load_course(&state, &lesson.course_id.to_string())?;
        let may_teach = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE)
            || (check_capability(&ctx, &caller.role, PERM_ELEARNING_INSTRUCT)
                && course.instructor_id == caller.actor);
        let mut items: Vec<Value> = state
            .questions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.quiz_id == quiz.id)
            .collect::<Vec<QuizQuestion>>()
            .into_iter()
            .map(|row| {
                if may_teach {
                    json!(row)
                } else {
                    json!({
                        "id": row.id,
                        "quiz_id": row.quiz_id,
                        "question": row.question,
                        "options": row.options,
                        "points": row.points,
                        "position": row.position,
                        "created_at": row.created_at,
                        "updated_at": row.updated_at,
                    })
                }
            })
            .collect();
        items.sort_by_key(|row| {
            row.get("position").and_then(|value| value.as_i64()).unwrap_or(0)
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_quiz_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizQuestion> = (|| {
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let quiz = load_quiz_for_lesson(&state, lesson.id)?;
        let count = state
            .questions
            .iter()
            .filter(|entry| entry.value().quiz_id == quiz.id)
            .count();
        if count >= MAX_QUESTIONS_PER_QUIZ {
            return Err(UnifiedError::bad_request(format!(
                "A quiz holds at most {MAX_QUESTIONS_PER_QUIZ} questions"
            )));
        }
        let input = body.into_inner();
        let (question, options, points) =
            validate_question_input(&input.question, &input.options, input.correct_index, input.points)?;
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .questions
                .iter()
                .filter(|entry| entry.value().quiz_id == quiz.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let now = Utc::now();
        let row = QuizQuestion {
            id: Uuid::now_v7(),
            quiz_id: quiz.id,
            question,
            options,
            correct_index: input.correct_index,
            points,
            position,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(row.id, row.clone());
        state.emit(
            "elearning.quiz.question.created",
            json!({ "question_id": row.id.to_string(), "quiz_id": quiz.id.to_string() }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn update_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizQuestion> = (|| {
        let caller = caller(&req)?;
        let mut row = load_question(&state, &path.into_inner())?;
        let quiz = state
            .quizzes
            .get(&row.quiz_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quiz not found"))?;
        let course = load_course(&state, &quiz.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        let input = body.into_inner();
        let question = input.question.as_deref().unwrap_or(&row.question);
        let options = input.options.as_deref().unwrap_or(&row.options);
        let correct_index = input.correct_index.unwrap_or(row.correct_index);
        let points = input.points.or(Some(row.points));
        let (checked_question, checked_options, checked_points) =
            validate_question_input(question, options, correct_index, points)?;
        row.question = checked_question;
        row.options = checked_options;
        row.correct_index = correct_index;
        row.points = checked_points;
        if let Some(position) = input.position {
            if position < 0 {
                return Err(UnifiedError::bad_request("position must be >= 0"));
            }
            row.position = position;
        }
        row.updated_at = Utc::now();
        state.questions.insert(row.id, row.clone());
        state.emit(
            "elearning.quiz.question.updated",
            json!({ "question_id": row.id.to_string(), "quiz_id": row.quiz_id.to_string() }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn delete_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let row = load_question(&state, &path.into_inner())?;
        let quiz = state
            .quizzes
            .get(&row.quiz_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quiz not found"))?;
        let course = load_course(&state, &quiz.course_id.to_string())?;
        require_course_write(&ctx, &caller, &course)?;
        if state
            .attempts
            .iter()
            .any(|entry| entry.value().quiz_id == quiz.id)
        {
            return Err(UnifiedError::conflict(
                "Questions cannot be deleted once attempts exist; edit the question instead",
            ));
        }
        state.questions.remove(&row.id);
        state.emit(
            "elearning.quiz.question.deleted",
            json!({ "question_id": row.id.to_string(), "quiz_id": row.quiz_id.to_string() }),
        );
        Ok(json!({ "deleted": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn submit_quiz_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitAttemptRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizAttemptView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let quiz = load_quiz_for_lesson(&state, lesson.id)?;
        let Some(enrollment) = find_enrollment(&state, lesson.course_id, &caller.actor) else {
            return Err(UnifiedError::forbidden(
                "Enroll in this course before taking its quizzes",
            ));
        };
        if enrollment.status != ENROLL_ACTIVE {
            return Err(UnifiedError::conflict(format!(
                "Enrollment is {}, not active",
                enrollment.status
            )));
        }
        if !lesson_available(&state, &lesson, &enrollment)? {
            return Err(UnifiedError::unprocessable(
                "Lesson is not available yet (drip schedule or previous lesson incomplete)",
            ));
        }
        if let Some(limit) = quiz.max_attempts {
            let used = state
                .attempts
                .iter()
                .filter(|entry| {
                    entry.value().quiz_id == quiz.id && entry.value().user_id == caller.actor
                })
                .count() as i64;
            if used >= limit {
                return Err(UnifiedError::conflict(format!(
                    "Attempt limit reached ({limit} attempts)",
                )));
            }
        }
        let questions: Vec<QuizQuestion> = state
            .questions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.quiz_id == quiz.id)
            .collect();
        if questions.is_empty() {
            return Err(UnifiedError::unprocessable(
                "This quiz has no questions yet",
            ));
        }
        let input = body.into_inner();
        if input.answers.is_empty() || input.answers.len() > questions.len() {
            return Err(UnifiedError::bad_request(
                "answers must name at least one question and at most one answer per question",
            ));
        }
        let by_id: HashMap<Uuid, &QuizQuestion> =
            questions.iter().map(|row| (row.id, row)).collect();
        let mut seen: Vec<Uuid> = Vec::new();
        let mut score: i64 = 0;
        let mut max_score: i64 = 0;
        let mut review_items: Vec<AttemptReviewItem> = Vec::new();
        for answer in &input.answers {
            let Some(question) = by_id.get(&answer.question_id) else {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown question: {}",
                    answer.question_id
                )));
            };
            if seen.contains(&answer.question_id) {
                return Err(UnifiedError::bad_request(
                    "Each question may be answered at most once per attempt",
                ));
            }
            seen.push(answer.question_id);
            if answer.selected_index >= question.options.len() {
                return Err(UnifiedError::bad_request(format!(
                    "selected_index is outside the options of question {}",
                    answer.question_id
                )));
            }
            max_score += question.points;
            let correct = answer.selected_index == question.correct_index;
            let earned = if correct { question.points } else { 0 };
            score += earned;
            review_items.push(AttemptReviewItem {
                question_id: question.id,
                selected_index: answer.selected_index,
                correct,
                correct_index: question.correct_index,
                points_earned: earned,
                points: question.points,
            });
        }
        // Unanswered questions count as wrong: they add to the
        // maximum but not to the score.
        for question in &questions {
            if !seen.contains(&question.id) {
                max_score += question.points;
                review_items.push(AttemptReviewItem {
                    question_id: question.id,
                    selected_index: usize::MAX,
                    correct: false,
                    correct_index: question.correct_index,
                    points_earned: 0,
                    points: question.points,
                });
            }
        }
        let percent = if max_score == 0 {
            0
        } else {
            ((score * 100) / max_score).clamp(0, 100)
        };
        let passed = percent >= quiz.pass_mark;
        let attempt = QuizAttempt {
            id: Uuid::now_v7(),
            quiz_id: quiz.id,
            lesson_id: lesson.id,
            enrollment_id: enrollment.id,
            user_id: caller.actor.clone(),
            answers: input.answers,
            score,
            max_score,
            percent,
            passed,
            submitted_at: Utc::now(),
        };
        state.attempts.insert(attempt.id, attempt.clone());
        state.emit(
            "elearning.quiz.attempt.submitted",
            json!({
                "attempt_id": attempt.id.to_string(),
                "quiz_id": quiz.id.to_string(),
                "lesson_id": lesson.id.to_string(),
                "user_id": attempt.user_id,
                "score": score,
                "max_score": max_score,
                "percent": percent,
                "passed": passed,
            }),
        );
        state.emit(
            if passed {
                "elearning.quiz.attempt.passed"
            } else {
                "elearning.quiz.attempt.failed"
            },
            json!({
                "attempt_id": attempt.id.to_string(),
                "quiz_id": quiz.id.to_string(),
                "user_id": attempt.user_id,
                "percent": percent,
            }),
        );
        let view = QuizAttemptView {
            attempt,
            review: if quiz.allow_review {
                Some(review_items)
            } else {
                None
            },
        };
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

fn attempt_view(state: &ElearningState, attempt: QuizAttempt) -> QuizAttemptView {
    let quiz = state
        .quizzes
        .get(&attempt.quiz_id)
        .map(|entry| entry.value().clone());
    let review = match quiz {
        Some(quiz) if quiz.allow_review => {
            let items: Vec<AttemptReviewItem> = attempt
                .answers
                .iter()
                .filter_map(|answer| {
                    let question = state.questions.get(&answer.question_id)?;
                    let correct = answer.selected_index == question.correct_index;
                    Some(AttemptReviewItem {
                        question_id: question.id,
                        selected_index: answer.selected_index,
                        correct,
                        correct_index: question.correct_index,
                        points_earned: if correct { question.points } else { 0 },
                        points: question.points,
                    })
                })
                .collect();
            Some(items)
        }
        _ => None,
    };
    QuizAttemptView { attempt, review }
}

async fn list_quiz_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let quiz_filter = params.filters.get("quiz_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let passed_filter = params
            .filters
            .get("passed")
            .and_then(|raw| raw.parse::<bool>().ok());
        let mut items: Vec<QuizAttemptView> = state
            .attempts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(quiz_id) = &quiz_filter {
                    if row.quiz_id.to_string() != *quiz_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if let Some(passed) = passed_filter {
                    if row.passed != passed {
                        return false;
                    }
                }
                true
            })
            .map(|row| attempt_view(&state, row))
            .collect();
        items.sort_by_key(|view| view.attempt.submitted_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_quiz_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizAttemptView> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let attempt = load_attempt(&state, &path.into_inner())?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && attempt.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may view this attempt",
            ));
        }
        Ok(attempt_view(&state, attempt))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Certificate handlers
// ---------------------------------------------------------------------------

async fn list_certificates(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        let course_filter = params.filters.get("course_id").cloned();
        let user_filter = params.filters.get("user_id").cloned();
        let status_filter = params.filters.get("status").cloned();
        let anchor_filter = params.filters.get("anchor_status").cloned();
        let mut items: Vec<Certificate> = state
            .certificates
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if !is_manager && row.user_id != caller.actor {
                    return false;
                }
                if let Some(course_id) = &course_filter {
                    if row.course_id.to_string() != *course_id {
                        return false;
                    }
                }
                if let Some(user_id) = &user_filter {
                    if &row.user_id != user_id {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if &row.status != status {
                        return false;
                    }
                }
                if let Some(anchor) = &anchor_filter {
                    if &row.anchor_status != anchor {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.issued_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn get_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Certificate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_certificate(&state, &path.into_inner())?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the learner or a manager may view this certificate",
            ));
        }
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

/// Public certificate verification: no caller headers and no
/// capability check, by design — anyone holding a code must be
/// able to verify it. The code is compared in constant time
/// against stored codes; an unknown or revoked code answers 404
/// without saying which of the two it was.
async fn verify_certificate(
    state: web::Data<ElearningState>,
    path: web::Path<String>,
) -> HttpResponse {
    let code = path.into_inner();
    let found = state.certificates.iter().find(|entry| {
        let row = entry.value();
        row.status == "valid" && constant_time_eq(&row.code, &code)
    });
    match found {
        Some(entry) => ok(entry.value().clone()),
        None => err(
            UnifiedError::not_found("Certificate not found or revoked"),
            false,
        ),
    }
}

/// Constant-time string comparison so verification timing does
/// not leak how much of a code matched.
fn constant_time_eq(a: &str, b: &str) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.len() != b_bytes.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a_bytes.iter().zip(b_bytes.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

async fn revoke_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Certificate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_MANAGE)?;
        let mut row = load_certificate(&state, &path.into_inner())?;
        if row.status == "revoked" {
            return Err(UnifiedError::conflict("Certificate is already revoked"));
        }
        row.status = "revoked".to_string();
        row.revoked_at = Some(Utc::now());
        row.updated_at = Utc::now();
        state.certificates.insert(row.id, row.clone());
        state.emit(
            "elearning.certificate.revoked",
            json!({
                "certificate_id": row.id.to_string(),
                "code": row.code,
                "course_id": row.course_id.to_string(),
                "user_id": row.user_id,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn anchor_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Certificate> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_MANAGE)?;
        let mut row = load_certificate(&state, &path.into_inner())?;
        if row.status != "valid" {
            return Err(UnifiedError::conflict(
                "Only a valid certificate can be anchored",
            ));
        }
        if row.anchor_status == "confirmed" {
            return Err(UnifiedError::conflict(
                "Certificate is already anchored and confirmed",
            ));
        }
        let settings = state.settings();
        row.anchor_status = "requested".to_string();
        row.updated_at = Utc::now();
        state.certificates.insert(row.id, row.clone());
        // Events only: the Web3 module owns the anchor itself.
        state.emit(
            "web3.anchor.requested",
            json!({
                "record_type": "elearning.certificate",
                "record_id": row.id.to_string(),
                "content_sha256": row.content_sha256,
                "network_id": settings.anchoring_network_id,
            }),
        );
        state.emit(
            "elearning.certificate.anchor.requested",
            json!({
                "certificate_id": row.id.to_string(),
                "code": row.code,
                "content_sha256": row.content_sha256,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Review handlers
// ---------------------------------------------------------------------------

fn course_rating_summary(state: &ElearningState, course_id: Uuid) -> Value {
    let ratings: Vec<i64> = state
        .reviews
        .iter()
        .filter(|entry| entry.value().course_id == course_id)
        .map(|entry| entry.value().rating)
        .collect();
    let count = ratings.len() as i64;
    let average = if count == 0 {
        0.0
    } else {
        ratings.iter().sum::<i64>() as f64 / count as f64
    };
    json!({ "course_id": course_id.to_string(), "review_count": count, "average_rating": average })
}

async fn list_course_reviews(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        let params = query.into_inner();
        let rating_filter = params
            .filters
            .get("rating")
            .and_then(|raw| raw.parse::<i64>().ok());
        let mut items: Vec<Review> = state
            .reviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.course_id != course.id {
                    return false;
                }
                if let Some(rating) = rating_filter {
                    if row.rating != rating {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.created_at);
        let page = paginate(items, &params);
        let mut data = page.to_json();
        data["rating_summary"] = course_rating_summary(&state, course.id);
        Ok(data)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_course_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let course = load_course(&state, &path.into_inner())?;
        // Reviews are for enrolled learners only, one per user.
        let Some(_) = find_enrollment(&state, course.id, &caller.actor) else {
            return Err(UnifiedError::forbidden(
                "Only enrolled learners may review this course",
            ));
        };
        if state.reviews.iter().any(|entry| {
            entry.value().course_id == course.id && entry.value().user_id == caller.actor
        }) {
            return Err(UnifiedError::conflict(
                "You already reviewed this course; edit your review instead",
            ));
        }
        let input = body.into_inner();
        if input.rating < 1 || input.rating > 5 {
            return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
        }
        let comment = optional_text(&input.comment, "comment", MAX_COMMENT_LEN)?;
        let now = Utc::now();
        let row = Review {
            id: Uuid::now_v7(),
            course_id: course.id,
            user_id: caller.actor.clone(),
            rating: input.rating,
            comment,
            created_at: now,
            updated_at: now,
        };
        state.reviews.insert(row.id, row.clone());
        state.emit(
            "elearning.review.created",
            json!({
                "review_id": row.id.to_string(),
                "course_id": course.id.to_string(),
                "user_id": row.user_id,
                "rating": row.rating,
            }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn get_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        load_review(&state, &path.into_inner())
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn update_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateReviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Review> = (|| {
        let caller = caller(&req)?;
        let mut row = load_review(&state, &path.into_inner())?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the author or a manager may edit this review",
            ));
        }
        let input = body.into_inner();
        if let Some(rating) = input.rating {
            if rating < 1 || rating > 5 {
                return Err(UnifiedError::bad_request("rating must be between 1 and 5"));
            }
            row.rating = rating;
        }
        if let Some(comment) = &input.comment {
            row.comment = match comment {
                Some(raw) => Some(require_text(raw, "comment", MAX_COMMENT_LEN)?),
                None => None,
            };
        }
        row.updated_at = Utc::now();
        state.reviews.insert(row.id, row.clone());
        state.emit(
            "elearning.review.updated",
            json!({ "review_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn delete_review(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        let row = load_review(&state, &path.into_inner())?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE);
        if !is_manager && row.user_id != caller.actor {
            return Err(UnifiedError::forbidden(
                "Only the author or a manager may delete this review",
            ));
        }
        state.reviews.remove(&row.id);
        state.emit(
            "elearning.review.deleted",
            json!({ "review_id": row.id.to_string(), "course_id": row.course_id.to_string() }),
        );
        Ok(json!({ "deleted": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Learning path handlers
// ---------------------------------------------------------------------------

async fn list_paths(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let search = params.search.clone().map(|value| value.to_lowercase());
        let mut items: Vec<LearningPath> = state
            .paths
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if let Some(needle) = &search {
                    let haystack = format!(
                        "{} {}",
                        row.title,
                        row.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(needle) {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|row| row.title.clone());
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn create_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    body: web::Json<CreatePathRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LearningPath> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let row = LearningPath {
            id: Uuid::now_v7(),
            title,
            description,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.paths.insert(row.id, row.clone());
        state.emit(
            "elearning.path.created",
            json!({ "path_id": row.id.to_string(), "title": row.title }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => created(row),
        Err(error) => err(error, debug),
    }
}

async fn get_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_path(&state, &path.into_inner())?;
        let mut courses: Vec<LearningPathCourse> = state
            .path_courses
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.path_id == row.id)
            .collect();
        courses.sort_by_key(|item| item.position);
        Ok(json!({ "path": row, "courses": courses }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn update_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePathRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LearningPath> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let mut row = load_path(&state, &path.into_inner())?;
        let input = body.into_inner();
        if let Some(title) = &input.title {
            row.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            row.description = match description {
                Some(raw) => Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        row.updated_at = Utc::now();
        state.paths.insert(row.id, row.clone());
        state.emit(
            "elearning.path.updated",
            json!({ "path_id": row.id.to_string(), "title": row.title }),
        );
        Ok(row)
    })();
    match result {
        Ok(row) => ok(row),
        Err(error) => err(error, debug),
    }
}

async fn delete_path(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let row = load_path(&state, &path.into_inner())?;
        state.path_courses.retain(|_, item| item.path_id != row.id);
        state.paths.remove(&row.id);
        state.emit(
            "elearning.path.deleted",
            json!({ "path_id": row.id.to_string(), "title": row.title }),
        );
        Ok(json!({ "deleted": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn add_path_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AddPathCourseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<LearningPathCourse> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let row = load_path(&state, &path.into_inner())?;
        let input = body.into_inner();
        let course = load_course(&state, &input.course_id)?;
        if state.path_courses.iter().any(|entry| {
            entry.value().path_id == row.id && entry.value().course_id == course.id
        }) {
            return Err(UnifiedError::conflict(
                "Course is already in this learning path",
            ));
        }
        let position = match input.position {
            Some(value) => {
                if value < 0 {
                    return Err(UnifiedError::bad_request("position must be >= 0"));
                }
                value
            }
            None => state
                .path_courses
                .iter()
                .filter(|entry| entry.value().path_id == row.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0),
        };
        let item = LearningPathCourse {
            id: Uuid::now_v7(),
            path_id: row.id,
            course_id: course.id,
            position,
            created_at: Utc::now(),
        };
        state.path_courses.insert(item.id, item.clone());
        state.emit(
            "elearning.path.course.added",
            json!({
                "path_id": row.id.to_string(),
                "course_id": course.id.to_string(),
                "position": position,
            }),
        );
        Ok(item)
    })();
    match result {
        Ok(item) => created(item),
        Err(error) => err(error, debug),
    }
}

async fn remove_path_course(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_teach(&ctx, &caller.role)?;
        let (path_id, course_id) = path.into_inner();
        let row = load_path(&state, &path_id)?;
        let course = load_course(&state, &course_id)?;
        let Some(item) = state
            .path_courses
            .iter()
            .find(|entry| entry.value().path_id == row.id && entry.value().course_id == course.id)
            .map(|entry| entry.value().clone())
        else {
            return Err(UnifiedError::not_found(
                "Course is not in this learning path",
            ));
        };
        state.path_courses.remove(&item.id);
        state.emit(
            "elearning.path.course.removed",
            json!({
                "path_id": row.id.to_string(),
                "course_id": course.id.to_string(),
            }),
        );
        Ok(json!({ "deleted": item.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

fn compute_path_progress(state: &ElearningState, path: &LearningPath, user_id: &str) -> PathProgress {
    let mut items: Vec<LearningPathCourse> = state
        .path_courses
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|item| item.path_id == path.id)
        .collect();
    items.sort_by_key(|item| item.position);
    let total = items.len();
    let mut completed_count = 0usize;
    let mut courses: Vec<PathCourseProgress> = Vec::with_capacity(total);
    for item in items {
        let enrollment = find_enrollment(state, item.course_id, user_id);
        let (enrolled, completed, percent) = match enrollment {
            Some(row) => {
                let progress = course_progress(state, &row);
                (true, progress.completed, progress.progress_percent)
            }
            None => (false, false, 0),
        };
        if completed {
            completed_count += 1;
        }
        courses.push(PathCourseProgress {
            course_id: item.course_id,
            position: item.position,
            enrolled,
            completed,
            progress_percent: percent,
        });
    }
    let percent = if total == 0 {
        100
    } else {
        ((completed_count as i64 * 100) / total as i64).clamp(0, 100)
    };
    PathProgress {
        path_id: path.id,
        user_id: user_id.to_string(),
        total_courses: total,
        completed_courses: completed_count,
        progress_percent: percent,
        completed: total > 0 && completed_count == total,
        courses,
    }
}

async fn get_path_progress(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let row = load_path(&state, &path.into_inner())?;
        let progress = compute_path_progress(&state, &row, &caller.actor);
        // Reading progress never emits completion: completion is
        // announced by the course-completion flow, not by a read.
        Ok(json!(progress))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

async fn list_my_paths(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let params = query.into_inner();
        let enrolled_course_ids: Vec<Uuid> = state
            .enrollments
            .iter()
            .filter(|entry| {
                entry.value().user_id == caller.actor
                    && entry.value().status != ENROLL_CANCELLED
            })
            .map(|entry| entry.value().course_id)
            .collect();
        let mut items: Vec<PathProgress> = state
            .paths
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|path| {
                state.path_courses.iter().any(|item| {
                    item.value().path_id == path.id
                        && enrolled_course_ids.contains(&item.value().course_id)
                })
            })
            .map(|path| compute_path_progress(&state, &path, &caller.actor))
            .collect();
        items.sort_by_key(|row| row.path_id);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Settings handlers
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ElearningSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        Ok(state.settings())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    body: web::Json<UpdateSettingsRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ElearningSettings> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        let mut validate_list = |values: &Option<Vec<String>>, field: &str| -> UnifiedResult<Option<Vec<String>>> {
            match values {
                Some(list) => {
                    if list.is_empty() {
                        return Err(UnifiedError::bad_request(format!(
                            "{field} must not be empty"
                        )));
                    }
                    let mut checked = Vec::with_capacity(list.len());
                    for value in list {
                        checked.push(require_text(value, field, MAX_SLUG_LEN)?);
                    }
                    Ok(Some(checked))
                }
                None => Ok(None),
            }
        };
        if let Some(levels) = validate_list(&input.levels, "levels")? {
            settings.levels = levels;
        }
        if let Some(categories) = validate_list(&input.categories, "categories")? {
            settings.categories = categories;
        }
        if let Some(languages) = validate_list(&input.languages, "languages")? {
            settings.languages = languages;
        }
        if let Some(lesson_types) = validate_list(&input.lesson_types, "lesson_types")? {
            settings.lesson_types = lesson_types;
        }
        if let Some(pass_mark) = input.default_pass_mark {
            if pass_mark < 0 || pass_mark > 100 {
                return Err(UnifiedError::bad_request(
                    "default_pass_mark must be between 0 and 100",
                ));
            }
            settings.default_pass_mark = pass_mark;
        }
        if let Some(max_attempts) = input.default_max_attempts {
            if let Some(limit) = max_attempts {
                if limit < 1 || limit > 1000 {
                    return Err(UnifiedError::bad_request(
                        "default_max_attempts must be between 1 and 1000",
                    ));
                }
            }
            settings.default_max_attempts = max_attempts;
        }
        if let Some(currency) = &input.default_currency {
            settings.default_currency = require_currency(currency)?;
        }
        if let Some(issuer) = &input.certificate_issuer_name {
            settings.certificate_issuer_name = require_text(issuer, "certificate_issuer_name", MAX_TITLE_LEN)?;
        }
        if let Some(enabled) = input.certificate_anchoring_enabled {
            settings.certificate_anchoring_enabled = enabled;
        }
        if let Some(network) = &input.anchoring_network_id {
            settings.anchoring_network_id = require_text(network, "anchoring_network_id", MAX_SLUG_LEN)?;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        state.emit(
            "elearning.settings.updated",
            json!({
                "levels": settings.levels,
                "categories": settings.categories,
                "certificate_anchoring_enabled": settings.certificate_anchoring_enabled,
            }),
        );
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------
// AI capabilities (AI Capabilities Contract, see `crate::modules::ai`).
//
// Declared here, registered once in `init`, grounded through the
// `elearning.ai.<capability>_requested` hooks, and run through the
// AI module's gated runner. Drafts are returned inline and are
// never stored, published, or attached to a lesson by this module:
// creating quiz questions stays on the existing human endpoints.
// ---------------------------------------------------------------------------

/// AI capabilities this module exposes through the AI module's
/// contract. Both are non-PII: lesson content is course material,
/// redacted before it reaches the model; no learner data is ever
/// grounded (no enrollments, attempts, or scores).
pub const AI_CAPABILITIES: &[ai::AiCapability] = &[
    ai::AiCapability {
        id: "draft_quiz",
        module_id: "elearning",
        name: "Draft quiz questions",
        description: "Draft quiz questions for a lesson, grounded in the lesson content and the lesson's existing questions (so drafts do not repeat them). The output is returned inline; it is never stored by this module.",
        input_fields: &["lesson_id", "count", "difficulty"],
        allows_pii: false,
        use_case: "elearning.draft_quiz",
    },
    ai::AiCapability {
        id: "summarize_lesson",
        module_id: "elearning",
        name: "Summarize lesson",
        description: "Summarize one lesson, grounded in the lesson content. The output is returned inline; it is never stored by this module.",
        input_fields: &["lesson_id"],
        allows_pii: false,
        use_case: "elearning.summarize_lesson",
    },
];

/// Grounding for one lesson: its title, type, text body (capped),
/// and its course. Video / file lessons have no body text, so they
/// ground on title and course only. Unknown lessons ground nothing
/// (the endpoint 404s on them before delegating).
fn lesson_grounding(state: &ElearningState, lesson_id: &str) -> Value {
    let Some(lesson) = Uuid::parse_str(lesson_id).ok().and_then(|id| {
        state
            .lessons
            .get(&id)
            .map(|entry| entry.value().clone())
    }) else {
        return json!({});
    };
    let course = state
        .courses
        .get(&lesson.course_id)
        .map(|entry| entry.value().clone());
    let body = lesson.body.as_deref().unwrap_or("");
    let body: String = if body.chars().count() > 8000 {
        body.chars().take(8000).collect()
    } else {
        body.to_string()
    };
    json!({
        "lesson": {
            "title": lesson.title,
            "lesson_type": lesson.lesson_type,
            "body": body,
        },
        "course": course.map(|course| json!({
            "title": course.title,
            "description": course.description,
        })),
    })
}

/// Grounding for a quiz draft: the lesson grounding plus the
/// lesson's existing questions (truncated, at most five), so the
/// model drafts new questions instead of repeating existing ones.
fn quiz_grounding(state: &ElearningState, lesson_id: &str) -> Value {
    let mut grounding = lesson_grounding(state, lesson_id);
    let Some(lesson_uuid) = Uuid::parse_str(lesson_id).ok() else {
        return grounding;
    };
    let quiz_ids: Vec<Uuid> = state
        .quizzes
        .iter()
        .filter(|entry| entry.value().lesson_id == lesson_uuid)
        .map(|entry| *entry.key())
        .collect();
    let mut existing: Vec<Value> = Vec::new();
    for question in state.questions.iter().map(|entry| entry.value().clone()) {
        if quiz_ids.contains(&question.quiz_id) {
            let text: String = question.question.chars().take(300).collect();
            existing.push(json!({ "question": text }));
            if existing.len() >= 5 {
                break;
            }
        }
    }
    if let Value::Object(map) = &mut grounding {
        map.insert(
            "existing_questions".to_string(),
            Value::Array(existing),
        );
    }
    grounding
}

/// Run one registered e-learning AI capability:
/// `POST /api/v1/elearning/ai/{capability}` with body
/// `{"input": {...}}`. Requires `ai:use` plus the module's own
/// `elearning.view`; the named lesson must exist (404 otherwise).
/// Delegates to the AI module's gated runner.
async fn ai_capability(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ai::CapabilityCallInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let capability = path.into_inner();
    let input = body.into_inner().input;
    let caller = match caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW) {
        return json::error_response(&error, debug);
    }
    if capability == "draft_quiz" || capability == "summarize_lesson" {
        let raw = input
            .get("lesson_id")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        let lesson_id = match Uuid::parse_str(raw) {
            Ok(id) => id,
            Err(_) => {
                return json::error_response(
                    &UnifiedError::bad_request("input.lesson_id must be a valid id"),
                    debug,
                )
            }
        };
        if !state.lessons.contains_key(&lesson_id) {
            return json::error_response(
                &UnifiedError::not_found(format!("Lesson not found: {lesson_id}")),
                debug,
            );
        }
    }
    let ai_caller = match ai::caller(&req) {
        Ok(caller) => caller,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = ai::require_permission(&ctx, &ai_caller.role, ai::PERM_AI_USE) {
        return json::error_response(&error, debug);
    }
    let company_id = ai::resolve_capability_company(&ctx, &req, &ai_caller);
    match ai::run_ai_capability(
        &ctx,
        &ai_caller,
        company_id,
        "elearning",
        &capability,
        input,
    )
    .await
    {
        Ok(data) => json::success_response(data, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct ElearningModule {
    state: ElearningState,
}

impl ElearningModule {
    pub fn new() -> Self {
        Self {
            state: ElearningState::default(),
        }
    }
}

impl Default for ElearningModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for ElearningModule {
    fn id(&self) -> &'static str {
        "elearning"
    }

    fn name(&self) -> &'static str {
        "E-Learning"
    }

    fn description(&self) -> &'static str {
        "E-learning: courses with modules and drip lessons (video, text, file, quiz), free and paid enrollment through store/billing events, multiple-choice quizzes with pass marks, attempt limits, auto-grading and review, per-lesson and per-course progress, verifiable certificates with tamper-evidence anchoring events, instructor ownership, course reviews, and ordered learning paths. All lists, levels, categories, languages, and defaults are admin-configurable settings, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["store", "storage", "billing", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["store", "storage", "billing", "roles"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("productivity")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        ai::register_ai_capabilities("elearning", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            "elearning.course.requested",
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_course_requested(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ORDER_PAID,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_COMPLETED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_paid(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_REFUNDED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_refunded_or_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            STORE_ORDER_CANCELLED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_order_refunded_or_cancelled(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ANCHOR_CONFIRMED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_anchor_outcome(state.clone(), ctx, true)
            }),
        );
        let state = self.state.clone();
        bus.register(
            ANCHOR_FAILED,
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                on_anchor_outcome(state.clone(), ctx, false)
            }),
        );
        // Ground the quiz / lesson AI capabilities with the lesson
        // content (and, for quiz drafts, the existing questions).
        // No learner data is ever grounded: no enrollments,
        // attempts, or scores. Tenant scoping: this store is
        // in-memory with no tenant partition yet; when batch 17
        // persistence lands, these handlers must only ground
        // lessons of the calling tenant, keyed by the caller
        // company id in the payload.
        let state = self.state.clone();
        bus.register(
            "elearning.ai.draft_quiz_requested",
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                let lesson_id = ctx
                    .get("input")
                    .and_then(|input| input.get("lesson_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.add_result(quiz_grounding(&state, lesson_id));
            }),
        );
        let state = self.state.clone();
        bus.register(
            "elearning.ai.summarize_lesson_requested",
            "elearning",
            Arc::new(move |ctx: &mut HookContext| {
                let lesson_id = ctx
                    .get("input")
                    .and_then(|input| input.get("lesson_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("");
                ctx.add_result(lesson_grounding(&state, lesson_id));
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/elearning")
                .app_data(state)
                .route("/courses", web::get().to(list_courses))
                .route("/courses", web::post().to(create_course))
                .route("/courses/{id}", web::get().to(get_course))
                .route("/courses/{id}", web::patch().to(update_course))
                .route("/courses/{id}", web::delete().to(delete_course))
                .route("/courses/{id}/publish", web::post().to(publish_course))
                .route("/courses/{id}/archive", web::post().to(archive_course))
                .route("/courses/{id}/modules", web::get().to(list_course_modules))
                .route("/courses/{id}/modules", web::post().to(create_course_module))
                .route("/modules/{id}", web::get().to(get_module))
                .route("/modules/{id}", web::patch().to(update_module))
                .route("/modules/{id}", web::delete().to(delete_module))
                .route("/modules/{id}/lessons", web::get().to(list_module_lessons))
                .route("/modules/{id}/lessons", web::post().to(create_lesson))
                .route("/lessons/{id}", web::get().to(get_lesson))
                .route("/lessons/{id}", web::patch().to(update_lesson))
                .route("/lessons/{id}", web::delete().to(delete_lesson))
                .route("/courses/{id}/enroll", web::post().to(enroll_course))
                .route("/enrollments", web::get().to(list_enrollments))
                .route("/enrollments/{id}", web::get().to(get_enrollment))
                .route("/enrollments/{id}/cancel", web::post().to(cancel_enrollment))
                .route("/my/enrollments", web::get().to(list_my_enrollments))
                .route("/lessons/{id}/complete", web::post().to(complete_lesson))
                .route("/courses/{id}/progress", web::get().to(get_course_progress))
                .route(
                    "/courses/{id}/progress/{user_id}",
                    web::get().to(get_user_course_progress),
                )
                .route("/my/progress", web::get().to(list_my_progress))
                .route("/lessons/{id}/quiz", web::get().to(get_lesson_quiz))
                .route("/lessons/{id}/quiz", web::post().to(create_lesson_quiz))
                .route("/lessons/{id}/quiz", web::patch().to(update_lesson_quiz))
                .route(
                    "/lessons/{id}/quiz/questions",
                    web::get().to(list_quiz_questions),
                )
                .route(
                    "/lessons/{id}/quiz/questions",
                    web::post().to(create_quiz_question),
                )
                .route("/questions/{id}", web::patch().to(update_question))
                .route("/questions/{id}", web::delete().to(delete_question))
                .route(
                    "/lessons/{id}/quiz/attempts",
                    web::post().to(submit_quiz_attempt),
                )
                .route("/quiz-attempts", web::get().to(list_quiz_attempts))
                .route("/quiz-attempts/{id}", web::get().to(get_quiz_attempt))
                .route("/certificates", web::get().to(list_certificates))
                .route("/certificates/verify/{code}", web::get().to(verify_certificate))
                .route("/certificates/{id}", web::get().to(get_certificate))
                .route(
                    "/certificates/{id}/revoke",
                    web::post().to(revoke_certificate),
                )
                .route(
                    "/certificates/{id}/anchor",
                    web::post().to(anchor_certificate),
                )
                .route("/courses/{id}/reviews", web::get().to(list_course_reviews))
                .route("/courses/{id}/reviews", web::post().to(create_course_review))
                .route("/reviews/{id}", web::get().to(get_review))
                .route("/reviews/{id}", web::patch().to(update_review))
                .route("/reviews/{id}", web::delete().to(delete_review))
                .route("/learning-paths", web::get().to(list_paths))
                .route("/learning-paths", web::post().to(create_path))
                .route("/learning-paths/{id}", web::get().to(get_path))
                .route("/learning-paths/{id}", web::patch().to(update_path))
                .route("/learning-paths/{id}", web::delete().to(delete_path))
                .route(
                    "/learning-paths/{id}/courses",
                    web::post().to(add_path_course),
                )
                .route(
                    "/learning-paths/{id}/courses/{course_id}",
                    web::delete().to(remove_path_course),
                )
                .route(
                    "/learning-paths/{id}/progress",
                    web::get().to(get_path_progress),
                )
                .route("/my/learning-paths", web::get().to(list_my_paths))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/ai/{capability}", web::post().to(ai_capability)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses", owner: "elearning", description: "List courses (paginated, filter by category, level, language, status, instructor_id; learners see published only)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses", owner: "elearning", description: "Create a course in draft (manager or instructor; category, level, language validated against settings)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}", owner: "elearning", description: "Get one course (drafts only for managers and the instructor)." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/courses/{id}", owner: "elearning", description: "Update a course (manager or its instructor; price locked while a paid enrollment is pending)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/courses/{id}", owner: "elearning", description: "Delete a course with no enrollments, with its modules, lessons, quizzes, and reviews (manager or its instructor)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/publish", owner: "elearning", description: "Publish a draft course (manager or its instructor; requires at least one lesson)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/archive", owner: "elearning", description: "Archive a published or draft course (manager or its instructor)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/modules", owner: "elearning", description: "List the modules of a course (paginated, ordered by position)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/modules", owner: "elearning", description: "Add a module to a course (manager or its instructor)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/modules/{id}", owner: "elearning", description: "Get one course module." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/modules/{id}", owner: "elearning", description: "Update a course module (manager or its instructor)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/modules/{id}", owner: "elearning", description: "Delete a module with its lessons, quizzes, attempts, and progress (manager or its instructor)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/modules/{id}/lessons", owner: "elearning", description: "List the lessons of a module (paginated, filter by lesson_type, ordered by position)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/modules/{id}/lessons", owner: "elearning", description: "Add a lesson (video/file need a storage reference, text needs a body; drip by date or previous lesson)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/lessons/{id}", owner: "elearning", description: "Get one lesson." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/lessons/{id}", owner: "elearning", description: "Update a lesson (manager or its instructor)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/lessons/{id}", owner: "elearning", description: "Delete a lesson with its quiz, questions, attempts, and progress (manager or its instructor)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/enroll", owner: "elearning", description: "Enroll in a published course (free: active at once; paid: pending_payment plus a purchase event for store/billing)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/enrollments", owner: "elearning", description: "List enrollments (own only unless manager; filter by course_id, user_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/enrollments/{id}", owner: "elearning", description: "Get one enrollment (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/enrollments/{id}/cancel", owner: "elearning", description: "Cancel an enrollment (own or manager; completed enrollments cannot be cancelled)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/my/enrollments", owner: "elearning", description: "The caller's enrollments (paginated, filter by status)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/complete", owner: "elearning", description: "Mark a lesson complete (enrolled and active only; drip rules apply; quiz lessons need a passed attempt)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/progress", owner: "elearning", description: "The caller's progress in a course (required lessons, percent, completion)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/progress/{user_id}", owner: "elearning", description: "One learner's progress in a course (own, instructor, or manager)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/my/progress", owner: "elearning", description: "The caller's progress across enrolled courses (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/lessons/{id}/quiz", owner: "elearning", description: "Get the quiz of a lesson." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/quiz", owner: "elearning", description: "Define the quiz of a lesson (manager or its instructor; pass mark and attempt limit default from settings)." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/lessons/{id}/quiz", owner: "elearning", description: "Update a lesson quiz (manager or its instructor)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/lessons/{id}/quiz/questions", owner: "elearning", description: "List quiz questions (paginated; correct answers hidden from learners)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/quiz/questions", owner: "elearning", description: "Add a multiple-choice question (manager or its instructor)." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/questions/{id}", owner: "elearning", description: "Update a quiz question (manager or its instructor)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/questions/{id}", owner: "elearning", description: "Delete a quiz question (manager or its instructor; blocked once attempts exist)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/lessons/{id}/quiz/attempts", owner: "elearning", description: "Submit a quiz attempt (auto-graded; attempt limit enforced; review only when the quiz allows it)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/quiz-attempts", owner: "elearning", description: "List quiz attempts (own only unless manager; filter by quiz_id, user_id, passed)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/quiz-attempts/{id}", owner: "elearning", description: "Get one quiz attempt with review when allowed (own only unless manager)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/certificates", owner: "elearning", description: "List certificates (own only unless manager; filter by course_id, user_id, status, anchor_status)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/certificates/verify/{code}", owner: "elearning", description: "PUBLIC certificate verification by code (no authentication; revoked or unknown codes answer 404)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/certificates/{id}", owner: "elearning", description: "Get one certificate (own only unless manager)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/certificates/{id}/revoke", owner: "elearning", description: "Revoke a certificate (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/certificates/{id}/anchor", owner: "elearning", description: "Request tamper-evidence anchoring for a certificate through the Web3 flow (manager; events only)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/courses/{id}/reviews", owner: "elearning", description: "List course reviews with a rating summary (paginated, filter by rating)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/courses/{id}/reviews", owner: "elearning", description: "Leave a course review (enrolled learners only, one per user, rating 1-5)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/reviews/{id}", owner: "elearning", description: "Get one review." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/reviews/{id}", owner: "elearning", description: "Edit a review (author or manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/reviews/{id}", owner: "elearning", description: "Delete a review (author or manager)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/learning-paths", owner: "elearning", description: "List learning paths (paginated, searchable)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/learning-paths", owner: "elearning", description: "Create a learning path (manager or instructor)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/learning-paths/{id}", owner: "elearning", description: "Get one learning path with its ordered courses." },
            RouteInfo { method: "PATCH", path: "/api/v1/elearning/learning-paths/{id}", owner: "elearning", description: "Update a learning path (manager or instructor)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/learning-paths/{id}", owner: "elearning", description: "Delete a learning path with its course links (manager or instructor)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/learning-paths/{id}/courses", owner: "elearning", description: "Append a course to a learning path (manager or instructor)." },
            RouteInfo { method: "DELETE", path: "/api/v1/elearning/learning-paths/{id}/courses/{course_id}", owner: "elearning", description: "Remove a course from a learning path (manager or instructor)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/learning-paths/{id}/progress", owner: "elearning", description: "The caller's progress in a learning path (per-course progress, percent, completion)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/my/learning-paths", owner: "elearning", description: "Learning paths the caller is progressing in (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/elearning/settings", owner: "elearning", description: "Get the configurable e-learning settings (levels, categories, languages, lesson types, defaults)." },
            RouteInfo { method: "PUT", path: "/api/v1/elearning/settings", owner: "elearning", description: "Update the configurable e-learning settings (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/elearning/ai/{capability}", owner: "elearning", description: "Run one registered e-learning AI capability (draft_quiz, summarize_lesson; requires ai:use plus elearning.view)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ElearningModule::new()));
}
