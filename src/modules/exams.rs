// Online Exams module - question banks, timed exams, server-side
// attempts, auto and manual grading with rubrics, results, and
// analytics.
//
// Routes live under `/api/v1/exams`:
//
//   Question banks:
//     GET/POST /question-banks - list (paginated, filter by category,
//          status, owner_id) / create.
//     GET/PATCH/DELETE /question-banks/{id}.
//     GET/POST /question-banks/{id}/questions - list / add a question.
//     POST /question-banks/{id}/questions/import - import questions
//          from JSON (creates draft questions only).
//     GET  /question-banks/{id}/questions/export - export questions
//          as JSON (correct answers included only for managers).
//   Questions:
//     GET/PATCH/DELETE /questions/{id}.
//     GET  /questions/{id}/versions - version history of a question.
//   Exams:
//     GET/POST /exams - list (paginated, filter by status, category,
//          bank_id, course_id) / create.
//     GET/PATCH/DELETE /exams/{id}.
//     POST /exams/{id}/publish - draft -> published.
//     POST /exams/{id}/archive - published/draft -> archived.
//     POST /exams/{id}/close - published -> closed.
//     POST /exams/{id}/duplicate - copy an exam as a new draft.
//     GET/POST /exams/{id}/questions - the exam's question set /
//          attach a question (by bank question id, pinned version).
//     DELETE /exams/{id}/questions/{question_id} - detach one.
//   Attempts (taking an exam):
//     POST /exams/{id}/attempts - start an attempt (server clock
//          decides the deadline; randomised order is seeded per
//          attempt and the seed is recorded).
//     GET  /attempts - list attempts (own only unless manager or
//          grader; filter by exam_id, user_id, status).
//     GET  /attempts/{id} - one attempt (answers shown without
//          correct answers; correct answers are never returned here).
//     GET  /my/attempts - the caller's attempts.
//     POST /attempts/{id}/heartbeat - server remaining time; an
//          expired attempt is marked expired by the server.
//     POST /attempts/{id}/autosave - persist draft answers.
//     POST /attempts/{id}/submit - submit for grading.
//     POST /attempts/{id}/expire - mark an in-progress attempt
//          expired (manager; the server also expires on heartbeat).
//     POST /attempts/{id}/proctor-signal - record a proctoring
//          signal (tab switch, window blur, navigation away).
//     GET  /attempts/{id}/proctor-signals - signals of an attempt.
//   Rubrics and manual grading:
//     GET/POST /exams/{id}/rubrics - list / create a rubric.
//     GET/PATCH/DELETE /rubrics/{id}.
//     GET  /grading-queue - attempts waiting for manual grading
//          (grader or manager; filter by exam_id).
//     GET  /attempts/{id}/grading - the manual-grading view of one
//          attempt (essay answers plus rubric criteria).
//     POST /attempts/{id}/grade - submit manual grades for the
//          essay answers of an attempt.
//     POST /attempts/{id}/regrade - reopen a graded attempt for
//          grading again (manager; audited).
//     POST /attempts/{id}/override - override the final score
//          (manager; audited, original score kept).
//     POST /grades/{id}/dispute - flag a manual grade as disputed.
//     POST /grades/{id}/second-grade - second grader's grade for a
//          disputed grade (replaces the disputed grade; audited).
//   Results and analytics:
//     GET  /exams/{id}/results - results of an exam (manager or
//          grader; filter by passed, status).
//     GET  /results/{id} - one result (own only unless manager or
//          grader).
//     GET  /my/results - the caller's results.
//     POST /exams/{id}/results/publish - publish draft results.
//     POST /results/{id}/certificate - request a certificate for a
//          passed, published result (events only).
//     GET  /exams/{id}/analytics - pass rate, score distribution,
//          per-question statistics.
//     GET  /exams/{id}/question-stats - per-question statistics
//          (attempted %, correct %, discrimination index).
//   AI (smart module):
//     POST /ai/draft-questions - draft questions with AI help
//          (manager; drafts only, never auto-published).
//     POST /ai/analyse-results - analyse an exam's results with AI
//          help (manager or grader; anonymised statistics only).
//   Settings:
//     GET/PUT /settings - the configurable defaults (manager).
//
// HONESTY - READ THIS FIRST. What is real in this module:
// in-memory question-bank / question / exam / attempt / grade /
// rubric / proctor-signal / result management; question versioning
// (editing a question that already has attempts creates a new
// version and old attempts keep their pinned version); timed exams
// with scheduled open/close windows, seeded randomised question
// and option order, per-question marks, pass marks, and attempt
// limits; server-side attempt timers (start, heartbeat, autosave,
// submit, resume after disconnect - the server clock decides
// expiry, never the client clock); auto-grading for multiple-choice,
// true/false, and short-answer questions (exact match plus an
// optional case-insensitive match); a manual-grading queue for
// essays with rubrics, grader assignment, dispute plus second
// grader, regrade, and audited score overrides; result publication
// (draft -> published); per-question statistics including a
// discrimination index, score distribution, and per-exam pass
// rates; certificate requests and academic grade flow as events
// only. What is NOT built here: durable storage (the DashMap
// stores are authoritative; migration `2026-11-22-000090_exams`
// is the persistence target); camera or video proctoring - tab
// switch / window blur / navigation away are recorded only as
// proctoring SIGNALS in the attempt log, and this module never
// claims camera proctoring exists; payment capture (exams never
// charge directly - the store/billing modules own it); email or
// push delivery (the notifications module owns delivery - this
// module only emits `notification.requested`); certificate
// issuance itself (the e-learning certificate flow owns it -
// this module only emits `exams.certificate.requested`); and
// academic records (grades live in the academic module - this
// module only emits `academic.grade.recorded` when an exam is
// linked to an academic enrollment and component).
//
// AI RULE - READ THIS FIRST. AI-drafted questions are stored as
// DRAFT records only. They are never auto-published: a human must
// review and publish every AI-drafted question. AI analysis of
// results uses anonymised per-question statistics only - raw
// student answers and student identities are never sent to the AI
// helper.
//
// CONFIG-DRIVEN - READ THIS FIRST. Nothing behavioural is
// hard-coded: the allowed question types, difficulty levels, and
// question categories, the default pass mark, the default exam
// duration, the default attempts allowed, the default result
// visibility, the score-distribution bucket count, and the
// proctoring signal types all live in `ExamsSettings`, editable
// by an administrator through `PUT /settings`. The values in
// `ExamsSettings::default` are CONFIGURABLE DEFAULTS only - an
// admin change applies to every validation and default that reads
// them, and no code path consults a baked-in list instead. The
// standard constants that remain hard-coded are the status
// vocabularies (exam / attempt / grade / result statuses), the
// per-question mark bounds, and the field length limits - all
// standard constants, listed in the hard-coded disclosure at the
// end of this file's report.
//
// TENANT SCOPING — READ THIS FIRST (integration round, 2026-11-22).
// Every tenant-data handler in this module resolves the tenant
// FIRST via `tenancy::TenantContext::from_http_request` (see
// `require_tenant` below): no resolved tenant context means the
// request is rejected (401 missing context / 403 invalid tenant
// id), before caller identity, capability checks, store access,
// or any emit. No handler falls back to `public` and no tenant
// id, schema name, or package id is hard-coded — all are resolved
// at request time from the tenant context, per
// `crate::core::tenancy::TENANT_SCOPE_CONTRACT`. This module has
// no entitlement check of its own yet: the entitlements module
// answers `entitlements.check_requested`, but no grant records
// exist for this module and handlers do not reliably carry a
// company id, so a blocking entitlement gate would reject all
// traffic today; wiring it (grant per tenant + company id from
// the tenant context) is OPEN for the integrator. Likewise the
// stores are still in-memory DashMaps with no `DbPool`, so
// `with_tenant_connection` has no call site here yet and the maps
// are not partitioned by tenant — the guard rejects unresolved
// tenants, but full per-tenant isolation lands with the Diesel
// writers (migration `2026-11-22-000090_exams`) and a `tenant_id`
// on every record. Hook listeners (`on_ai_*_grounding`,
// `on_course_deleted`) take no `HttpRequest` and read event
// payloads defensively; they carry no tenant scope until event
// payloads carry `tenant_id`.
//
// Actor identity: until the auth module provides an extractor, the
// caller is identified by the `X-User-Id` header (required) and
// `X-Role` carries the role slug the capability check runs
// against, exactly like the e-learning module. Capability checks
// use the `role.check` hook and fail closed (no answer means not
// allowed). `X-Company-Id` is optional and is forwarded to the AI
// helper when present.
//
// Events emitted on the hook bus:
//   - `exams.question.created`, `exams.question.updated`
//   - `exams.questions.import_requested`
//     (`{"bank_id", "imported", "skipped", "format": "json"}` - the
//     import/export module may consume it; the import itself is
//     performed by this module and the event is a record of it)
//   - `exams.exam.created`, `exams.exam.published`,
//     `exams.exam.started`, `exams.exam.closed`
//   - `exams.attempt.started`, `exams.attempt.autosaved`,
//     `exams.attempt.submitted`, `exams.attempt.expired`
//   - `exams.attempt.proctor_signal`
//   - `exams.attempt.graded`
//     (`{"attempt_id", "exam_id", "user_id", "score", "max_score",
//     "percentage", "passed", "result_id"}`)
//   - `exams.result.published`
//   - `exams.certificate.requested`
//     (`{"attempt_id", "exam_id", "user_id", "result_id", "score",
//     "passed", "module": "exams"}` - the e-learning certificate
//     flow listens to it; this module never issues certificates)
//   - `academic.grade.recorded`
//     (`{"grade_id", "enrollment_id", "component_id", "score"}` -
//     exactly the payload the academic module records when it
//     creates a grade itself; emitted only when the exam is linked
//     to an academic enrollment and component, on grading
//     completion; `grade_id` is this module's result id and
//     `score` is the percentage score)
//   - `notification.requested` (grading completed, result
//     published, certificate requested, in the notifications
//     module's payload shape).
// Events this module listens to (all read defensively - a missing
// field or an unknown record is skipped, never an error):
//   - `exams.ai.draft_questions_requested`,
//     `exams.ai.analyse_results_requested` - AI grounding hooks:
//     this module answers with grounding context through
//     `ctx.add_result` (question-bank topic counts for drafting;
//     the exam's anonymised per-question statistics for analysis;
//     never raw student answers or student identities).
//   - `elearning.course.deleted` - exams linked to a deleted
//     course keep their link but are marked unlinked in the
//     response (the course itself is never deleted here).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use crate::core::pagination::{PageResult, PaginationParams};
use crate::core::tenancy;
use crate::modules::ai::{register_ai_capabilities, run_ai_capability, AiCapability};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
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
const MAX_QUESTIONS_PER_EXAM: usize = 200;
const MAX_OPTIONS_PER_QUESTION: usize = 10;
const MAX_ANSWERS_PER_ATTEMPT: usize = 500;
const MAX_CRITERIA_PER_RUBRIC: usize = 20;

/// Capability names checked through the `role.check` hook.
const PERM_EXAMS_VIEW: &str = "exams.view";
const PERM_EXAMS_MANAGE: &str = "exams.manage";
const PERM_EXAMS_TAKE: &str = "exams.take";
const PERM_EXAMS_GRADE: &str = "exams.grade";

/// Event names this module emits or listens to.
const EVT_QUESTION_CREATED: &str = "exams.question.created";
const EVT_QUESTION_UPDATED: &str = "exams.question.updated";
const EVT_QUESTIONS_IMPORT: &str = "exams.questions.import_requested";
const EVT_EXAM_CREATED: &str = "exams.exam.created";
const EVT_EXAM_PUBLISHED: &str = "exams.exam.published";
const EVT_EXAM_STARTED: &str = "exams.exam.started";
const EVT_EXAM_CLOSED: &str = "exams.exam.closed";
const EVT_ATTEMPT_STARTED: &str = "exams.attempt.started";
const EVT_ATTEMPT_AUTOSAVED: &str = "exams.attempt.autosaved";
const EVT_ATTEMPT_SUBMITTED: &str = "exams.attempt.submitted";
const EVT_ATTEMPT_EXPIRED: &str = "exams.attempt.expired";
const EVT_PROCTOR_SIGNAL: &str = "exams.attempt.proctor_signal";
const EVT_ATTEMPT_GRADED: &str = "exams.attempt.graded";
const EVT_RESULT_PUBLISHED: &str = "exams.result.published";
const EVT_CERTIFICATE_REQUESTED: &str = "exams.certificate.requested";
const EVT_ACADEMIC_GRADE: &str = "academic.grade.recorded";
const EVT_AI_DRAFT_GROUNDING: &str = "exams.ai.draft_questions_requested";
const EVT_AI_ANALYSE_GROUNDING: &str = "exams.ai.analyse_results_requested";
const EVT_COURSE_DELETED: &str = "elearning.course.deleted";

/// Exam lifecycle statuses.
const EXAM_DRAFT: &str = "draft";
const EXAM_PUBLISHED: &str = "published";
const EXAM_ARCHIVED: &str = "archived";
const EXAM_CLOSED: &str = "closed";

/// Attempt lifecycle statuses.
const ATTEMPT_IN_PROGRESS: &str = "in_progress";
const ATTEMPT_SUBMITTED: &str = "submitted";
const ATTEMPT_EXPIRED: &str = "expired";
const ATTEMPT_GRADED: &str = "graded";

/// Manual grade statuses.
const GRADE_PENDING: &str = "pending";
const GRADE_GRADED: &str = "graded";
const GRADE_DISPUTED: &str = "disputed";
const GRADE_SECOND_GRADING: &str = "second_grading";

/// Result publication statuses.
const RESULT_DRAFT: &str = "draft";
const RESULT_PUBLISHED: &str = "published";

/// AI capabilities this module exposes through the AI module's
/// contract. Both are non-PII: drafting grounds on topic counts
/// and analysis grounds on anonymised statistics only.
pub const AI_CAPABILITIES: &[AiCapability] = &[
    AiCapability {
        id: "draft_questions",
        module_id: "exams",
        name: "Draft exam questions",
        description: "Draft multiple-choice, true/false, short-answer, or essay questions for a topic, difficulty, and question type. Drafts are stored as draft records only and must be reviewed and published by a human.",
        input_fields: &["topic", "count", "difficulty", "question_type"],
        allows_pii: false,
        use_case: "exams.draft_questions",
    },
    AiCapability {
        id: "analyse_results",
        module_id: "exams",
        name: "Analyse exam results",
        description: "Analyse an exam's anonymised per-question statistics and score distribution to surface weak questions and cohort trends. Never receives raw student answers or student identities.",
        input_fields: &["exam_id"],
        allows_pii: false,
        use_case: "exams.analyse_results",
    },
];

// ---------------------------------------------------------------------------
// Configurable settings (admin-editable; defaults are defaults only)
// ---------------------------------------------------------------------------

/// All behaviour-shaping lists and defaults of this module. An
/// administrator edits these through `PUT /settings`; every
/// validation and default in this file reads the live settings,
/// never a constant list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamsSettings {
    /// Allowed question types (for example multiple_choice_single,
    /// multiple_choice_multiple, true_false, short_answer, essay).
    pub question_types: Vec<String>,
    /// Allowed difficulty levels (for example easy, medium, hard).
    pub difficulty_levels: Vec<String>,
    /// Allowed question categories.
    pub categories: Vec<String>,
    /// Allowed proctoring signal types.
    pub proctor_signal_types: Vec<String>,
    /// Default pass mark as a percentage (0-100).
    pub default_pass_mark: f64,
    /// Default exam duration in minutes.
    pub default_duration_minutes: i64,
    /// Default number of attempts allowed per student.
    pub default_attempts_allowed: u32,
    /// Default result visibility after grading.
    pub default_show_results: String,
    /// Number of buckets in the score distribution.
    pub distribution_buckets: u32,
    /// Whether short answers match case-insensitively by default.
    pub short_answer_case_insensitive: bool,
    /// Whether certificates are requested on pass by default.
    pub certificate_on_pass: bool,
    /// Maximum questions an exam may hold.
    pub max_questions_per_exam: usize,
    /// Maximum options a question may hold.
    pub max_options_per_question: usize,
}

impl Default for ExamsSettings {
    fn default() -> Self {
        Self {
            question_types: vec![
                "multiple_choice_single".to_string(),
                "multiple_choice_multiple".to_string(),
                "true_false".to_string(),
                "short_answer".to_string(),
                "essay".to_string(),
            ],
            difficulty_levels: vec![
                "easy".to_string(),
                "medium".to_string(),
                "hard".to_string(),
            ],
            categories: vec![
                "general".to_string(),
                "mathematics".to_string(),
                "language".to_string(),
                "science".to_string(),
                "history".to_string(),
                "technology".to_string(),
            ],
            proctor_signal_types: vec![
                "tab_switch".to_string(),
                "window_blur".to_string(),
                "navigation_away".to_string(),
                "copy_attempt".to_string(),
                "paste_attempt".to_string(),
                "fullscreen_exit".to_string(),
                "idle_timeout".to_string(),
                "other".to_string(),
            ],
            default_pass_mark: 60.0,
            default_duration_minutes: 60,
            default_attempts_allowed: 1,
            default_show_results: "after_grading".to_string(),
            distribution_buckets: 10,
            short_answer_case_insensitive: true,
            certificate_on_pass: true,
            max_questions_per_exam: MAX_QUESTIONS_PER_EXAM,
            max_options_per_question: MAX_OPTIONS_PER_QUESTION,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateExamsSettings {
    pub question_types: Option<Vec<String>>,
    pub difficulty_levels: Option<Vec<String>>,
    pub categories: Option<Vec<String>>,
    pub proctor_signal_types: Option<Vec<String>>,
    pub default_pass_mark: Option<f64>,
    pub default_duration_minutes: Option<i64>,
    pub default_attempts_allowed: Option<u32>,
    pub default_show_results: Option<String>,
    pub distribution_buckets: Option<u32>,
    pub short_answer_case_insensitive: Option<bool>,
    pub certificate_on_pass: Option<bool>,
    pub max_questions_per_exam: Option<usize>,
    pub max_options_per_question: Option<usize>,
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionBank {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: String,
    pub owner_id: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionOption {
    pub id: String,
    pub text: String,
    pub position: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Question {
    pub id: String,
    pub bank_id: String,
    pub version: u32,
    pub previous_version_id: Option<String>,
    pub superseded_by: Option<String>,
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub options: Vec<QuestionOption>,
    /// Correct answer, stored ONLY on the question row: for
    /// multiple-choice single, the option id; for multiple-choice
    /// multiple, the list of option ids; for true/false, a boolean;
    /// for short answer, the accepted answer text (or list of
    /// accepted texts); for essays, null (graded by a human).
    pub correct_answer: Value,
    /// Answer key / explanation shown to graders and, when the exam
    /// allows review, to students after grading.
    pub answer_key: Option<String>,
    pub marks: f64,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Public question view for exam takers: correct answers and the
/// answer key are stripped. Never serialize a `Question` directly
/// to a taker.
#[derive(Debug, Clone, Serialize)]
pub struct QuestionPublicView {
    pub id: String,
    pub bank_id: String,
    pub version: u32,
    pub question_type: String,
    pub category: String,
    pub difficulty: String,
    pub text: String,
    pub options: Vec<QuestionOption>,
    pub marks: f64,
}

impl From<&Question> for QuestionPublicView {
    fn from(question: &Question) -> Self {
        Self {
            id: question.id.clone(),
            bank_id: question.bank_id.clone(),
            version: question.version,
            question_type: question.question_type.clone(),
            category: question.category.clone(),
            difficulty: question.difficulty.clone(),
            text: question.text.clone(),
            options: question.options.clone(),
            marks: question.marks,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuestionVersion {
    pub id: String,
    pub question_id: String,
    pub version: u32,
    pub snapshot: Value,
    pub reason: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamSettings {
    /// When results become visible: "immediately" or "after_grading".
    pub show_results: String,
    pub allow_review: bool,
    pub calculator_allowed: bool,
    pub notes_allowed: bool,
    pub randomise_questions: bool,
    pub randomise_options: bool,
}

impl Default for ExamSettings {
    fn default() -> Self {
        Self {
            show_results: "after_grading".to_string(),
            allow_review: true,
            calculator_allowed: false,
            notes_allowed: false,
            randomise_questions: true,
            randomise_options: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Exam {
    pub id: String,
    pub title: String,
    pub description: String,
    pub category: String,
    pub bank_ids: Vec<String>,
    /// Linked e-learning course id, when the exam belongs to a course.
    pub course_id: Option<String>,
    /// Linked academic enrollment id for the academic grade flow.
    pub academic_enrollment_id: Option<String>,
    /// Linked academic grade component id for the academic grade flow.
    pub academic_component_id: Option<String>,
    pub status: String,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub duration_minutes: i64,
    pub pass_mark: f64,
    pub attempts_allowed: u32,
    pub settings: ExamSettings,
    pub total_marks: f64,
    pub created_by: String,
    pub company_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamQuestion {
    pub id: String,
    pub exam_id: String,
    pub question_id: String,
    /// Pinned question version: attempts always grade against this
    /// version, even when the question is edited later.
    pub question_version: u32,
    pub position: u32,
    pub section: Option<String>,
    pub marks: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamAttempt {
    pub id: String,
    pub exam_id: String,
    pub user_id: String,
    pub attempt_number: u32,
    pub status: String,
    pub started_at: DateTime<Utc>,
    /// Server-computed deadline. The server clock decides expiry,
    /// never the client clock.
    pub expires_at: DateTime<Utc>,
    pub duration_seconds: i64,
    /// Seed used for the randomised question and option order.
    pub seed: u64,
    /// Question ids in the order this attempt presents them.
    pub question_order: Vec<String>,
    /// Per-question option order: question id -> option ids.
    pub option_orders: HashMap<String, Vec<String>>,
    /// Draft answers: question id -> answer value. Correct answers
    /// are never stored here; grading reads the pinned question.
    pub answers: Value,
    pub autosaved_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub max_score: f64,
    pub passed: Option<bool>,
    pub graded_at: Option<DateTime<Utc>>,
    pub result_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AttemptView {
    pub id: String,
    pub exam_id: String,
    pub user_id: String,
    pub attempt_number: u32,
    pub status: String,
    pub started_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Seconds remaining according to the server clock.
    pub remaining_seconds: i64,
    pub seed: u64,
    pub question_order: Vec<String>,
    pub option_orders: HashMap<String, Vec<String>>,
    pub answers: Value,
    pub autosaved_at: Option<DateTime<Utc>>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub score: Option<f64>,
    pub max_score: f64,
    pub passed: Option<bool>,
    pub graded_at: Option<DateTime<Utc>>,
    pub result_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RubricCriterion {
    pub id: String,
    pub title: String,
    pub description: String,
    pub max_marks: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rubric {
    pub id: String,
    pub exam_id: String,
    pub title: String,
    pub description: String,
    pub criteria: Vec<RubricCriterion>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamGrade {
    pub id: String,
    pub attempt_id: String,
    pub exam_id: String,
    pub question_id: String,
    pub grader_id: Option<String>,
    pub rubric_id: Option<String>,
    pub criterion_scores: HashMap<String, f64>,
    pub marks_awarded: Option<f64>,
    pub max_marks: f64,
    pub status: String,
    pub comment: Option<String>,
    pub second_grader_id: Option<String>,
    pub audit: Vec<AuditEntry>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProctorSignal {
    pub id: String,
    pub attempt_id: String,
    pub exam_id: String,
    pub user_id: String,
    pub signal_type: String,
    pub detail: Option<String>,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamResult {
    pub id: String,
    pub attempt_id: String,
    pub exam_id: String,
    pub user_id: String,
    pub score: f64,
    pub max_score: f64,
    pub percentage: f64,
    pub passed: bool,
    pub status: String,
    /// Per-question outcome: question id -> {awarded, max, correct}.
    pub per_question: Value,
    pub graded_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct ExamsState {
    pub banks: Arc<DashMap<String, QuestionBank>>,
    pub questions: Arc<DashMap<String, Question>>,
    pub versions: Arc<DashMap<String, QuestionVersion>>,
    pub exams: Arc<DashMap<String, Exam>>,
    pub exam_questions: Arc<DashMap<String, ExamQuestion>>,
    pub attempts: Arc<DashMap<String, ExamAttempt>>,
    pub grades: Arc<DashMap<String, ExamGrade>>,
    pub rubrics: Arc<DashMap<String, Rubric>>,
    pub proctor_signals: Arc<DashMap<String, ProctorSignal>>,
    pub results: Arc<DashMap<String, ExamResult>>,
    pub settings: Arc<RwLock<ExamsSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

pub type ExamsStateRef = Arc<ExamsState>;

impl ExamsState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    fn settings(&self) -> ExamsSettings {
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
                "module": "exams",
                "category": "exams",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    fn attempt_view(&self, attempt: &ExamAttempt) -> AttemptView {
        let remaining = (attempt.expires_at - Utc::now()).num_seconds().max(0);
        AttemptView {
            id: attempt.id.clone(),
            exam_id: attempt.exam_id.clone(),
            user_id: attempt.user_id.clone(),
            attempt_number: attempt.attempt_number,
            status: attempt.status.clone(),
            started_at: attempt.started_at,
            expires_at: attempt.expires_at,
            remaining_seconds: if attempt.status == ATTEMPT_IN_PROGRESS {
                remaining
            } else {
                0
            },
            seed: attempt.seed,
            question_order: attempt.question_order.clone(),
            option_orders: attempt.option_orders.clone(),
            answers: attempt.answers.clone(),
            autosaved_at: attempt.autosaved_at,
            submitted_at: attempt.submitted_at,
            score: attempt.score,
            max_score: attempt.max_score,
            passed: attempt.passed,
            graded_at: attempt.graded_at,
            result_id: attempt.result_id.clone(),
        }
    }
}

pub struct ExamsModule {
    state: ExamsStateRef,
}

impl ExamsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(ExamsState::default()),
        }
    }
}

impl Default for ExamsModule {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: `X-User-Id` (required) plus `X-Role` (required
/// for capability checks), exactly like the e-learning module.
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

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `crate::core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Diesel persistence is not wired yet (the DashMap stores are
/// authoritative; migration `2026-11-22-000090_exams` is the
/// persistence target), so there is no `DbPool` in this module
/// and `with_tenant_connection` has no call site here yet — when
/// the Diesel writers land, all data access goes through
/// `tenancy::with_tenant_connection(pool, tenant.tenant_id, ...)`.
/// Until then the guard rejects unresolved tenants, but the
/// in-memory stores are not yet partitioned by tenant: records
/// carry no `tenant_id` yet, so per-tenant isolation of the maps
/// themselves is still OPEN for the integrator (see the module
/// header convergence note).
fn require_tenant(req: &HttpRequest) -> UnifiedResult<tenancy::TenantContext> {
    tenancy::TenantContext::from_http_request(req)
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

fn require_marks(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 1000.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a number between 0 and 1000"
        )));
    }
    Ok(value)
}

fn require_percentage(value: f64, field: &str) -> UnifiedResult<f64> {
    if !value.is_finite() || value < 0.0 || value > 100.0 {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be a number between 0 and 100"
        )));
    }
    Ok(value)
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one - never a constant in this file.
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

/// Validate a list of strings against a settings list.
fn require_list(values: &[String], field: &str, allowed: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(require_in_list(value, field, allowed)?);
    }
    Ok(out)
}

/// Paginate an already sorted vector into a `PageResult`.
fn page_of<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, search: &Option<String>) -> bool {
    match search {
        Some(term) => haystack.to_lowercase().contains(&term.to_lowercase()),
        None => true,
    }
}

/// Validate question options and the correct answer for a question
/// type. The allowed question types come from the live settings.
fn validate_question_payload(
    settings: &ExamsSettings,
    question_type: &str,
    options: &[QuestionOptionInput],
    correct_answer: &Value,
) -> UnifiedResult<Vec<QuestionOption>> {
    if !settings.question_types.iter().any(|t| t == question_type) {
        return Err(UnifiedError::bad_request(format!(
            "question_type \"{question_type}\" is not allowed; allowed values: {}",
            settings.question_types.join(", ")
        )));
    }
    if options.len() > settings.max_options_per_question {
        return Err(UnifiedError::bad_request(format!(
            "A question may have at most {} options",
            settings.max_options_per_question
        )));
    }
    let mut seen_texts: Vec<String> = Vec::new();
    let mut out: Vec<QuestionOption> = Vec::with_capacity(options.len());
    for (index, option) in options.iter().enumerate() {
        let text = require_text(&option.text, "option text", MAX_TEXT_LEN)?;
        if seen_texts.iter().any(|t| t == &text) {
            return Err(UnifiedError::bad_request(
                "Question options must not repeat the same text",
            ));
        }
        seen_texts.push(text.clone());
        out.push(QuestionOption {
            id: option.id.clone().unwrap_or_else(new_id),
            text,
            position: option.position.unwrap_or(index as u32),
        });
    }
    match question_type {
        "multiple_choice_single" => {
            if out.len() < 2 {
                return Err(UnifiedError::bad_request(
                    "A single-answer multiple-choice question needs at least 2 options",
                ));
            }
            let selected = correct_answer
                .as_str()
                .ok_or_else(|| UnifiedError::bad_request(
                    "correct_answer must be the id of the correct option",
                ))?;
            if !out.iter().any(|o| o.id == selected) {
                return Err(UnifiedError::bad_request(
                    "correct_answer must be the id of one of the options",
                ));
            }
        }
        "multiple_choice_multiple" => {
            if out.len() < 2 {
                return Err(UnifiedError::bad_request(
                    "A multiple-answer multiple-choice question needs at least 2 options",
                ));
            }
            let selected = correct_answer
                .as_array()
                .ok_or_else(|| UnifiedError::bad_request(
                    "correct_answer must be a list of correct option ids",
                ))?;
            if selected.is_empty() {
                return Err(UnifiedError::bad_request(
                    "correct_answer must list at least one correct option",
                ));
            }
            for entry in selected {
                let id = entry.as_str().ok_or_else(|| UnifiedError::bad_request(
                    "correct_answer entries must be option ids",
                ))?;
                if !out.iter().any(|o| o.id == id) {
                    return Err(UnifiedError::bad_request(
                        "correct_answer must only list ids of the options",
                    ));
                }
            }
        }
        "true_false" => {
            if !correct_answer.is_boolean() {
                return Err(UnifiedError::bad_request(
                    "correct_answer must be true or false for a true/false question",
                ));
            }
        }
        "short_answer" => {
            let accepted = match correct_answer {
                Value::String(text) => vec![text.clone()],
                Value::Array(items) => {
                    let mut texts = Vec::new();
                    for item in items {
                        texts.push(item.as_str().ok_or_else(|| UnifiedError::bad_request(
                            "correct_answer entries must be answer texts",
                        ))?.to_string());
                    }
                    texts
                }
                _ => {
                    return Err(UnifiedError::bad_request(
                        "correct_answer must be the accepted answer text (or a list of accepted texts)",
                    ));
                }
            };
            if accepted.iter().any(|text| text.trim().is_empty()) {
                return Err(UnifiedError::bad_request(
                    "correct_answer must not contain an empty answer",
                ));
            }
        }
        "essay" => {
            if !correct_answer.is_null() {
                return Err(UnifiedError::bad_request(
                    "correct_answer must be null for an essay question; essays are graded by a human",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported question_type \"{question_type}\""
            )));
        }
    }
    Ok(out)
}

/// Score one objective answer against the pinned question. Returns
/// the marks awarded (0 when wrong or unanswered). Essays return
/// `None`: they need a human grader.
fn auto_score(
    question: &Question,
    answer: &Value,
    case_insensitive: bool,
) -> Option<f64> {
    match question.question_type.as_str() {
        "multiple_choice_single" => {
            let selected = answer.as_str()?;
            let correct = question.correct_answer.as_str()?;
            if selected == correct {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        "multiple_choice_multiple" => {
            let selected = answer.as_array()?;
            let correct = question.correct_answer.as_array()?;
            let mut selected_ids: Vec<&str> = selected
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            selected_ids.sort_unstable();
            selected_ids.dedup();
            let mut correct_ids: Vec<&str> = correct
                .iter()
                .filter_map(|v| v.as_str())
                .collect();
            correct_ids.sort_unstable();
            correct_ids.dedup();
            if selected_ids == correct_ids {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        "true_false" => {
            let selected = answer.as_bool()?;
            let correct = question.correct_answer.as_bool()?;
            if selected == correct {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        "short_answer" => {
            let given = answer.as_str()?.trim();
            if given.is_empty() {
                return Some(0.0);
            }
            let accepted: Vec<String> = match &question.correct_answer {
                Value::String(text) => vec![text.clone()],
                Value::Array(items) => items
                    .iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect(),
                _ => return Some(0.0),
            };
            let matches = accepted.iter().any(|text| {
                if case_insensitive {
                    text.trim().eq_ignore_ascii_case(given)
                } else {
                    text.trim() == given
                }
            });
            if matches {
                Some(question.marks)
            } else {
                Some(0.0)
            }
        }
        _ => None,
    }
}

/// Does this exam already have attempts? Used to decide whether
/// editing a question must create a new version.
fn question_has_attempts(state: &ExamsState, question_id: &str) -> bool {
    state.attempts.iter().any(|entry| {
        let attempt = entry.value();
        attempt.question_order.iter().any(|id| id == question_id)
    })
}

/// Snapshot a question for its version history.
fn snapshot_question(question: &Question) -> Value {
    json!({
        "id": question.id,
        "bank_id": question.bank_id,
        "version": question.version,
        "question_type": question.question_type,
        "category": question.category,
        "difficulty": question.difficulty,
        "text": question.text,
        "options": question.options,
        "correct_answer": question.correct_answer,
        "answer_key": question.answer_key,
        "marks": question.marks,
        "status": question.status,
    })
}

fn exam_total_marks(state: &ExamsState, exam_id: &str) -> f64 {
    state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == exam_id)
        .map(|entry| entry.value().marks)
        .sum()
}

fn recompute_exam_total(state: &ExamsState, exam_id: &str) {
    if let Some(mut exam) = state.exams.get_mut(exam_id) {
        exam.total_marks = exam_total_marks(state, exam_id);
        exam.updated_at = now();
    }
}

/// Expire an in-progress attempt whose server deadline has passed.
/// Returns true when the attempt was expired by this call.
fn expire_if_due(state: &ExamsState, attempt_id: &str) -> bool {
    let due = state
        .attempts
        .get(attempt_id)
        .map(|entry| {
            entry.value().status == ATTEMPT_IN_PROGRESS && entry.value().expires_at <= Utc::now()
        })
        .unwrap_or(false);
    if !due {
        return false;
    }
    if let Some(mut attempt) = state.attempts.get_mut(attempt_id) {
        attempt.status = ATTEMPT_EXPIRED.to_string();
        attempt.submitted_at = Some(now());
        let exam_id = attempt.exam_id.clone();
        let user_id = attempt.user_id.clone();
        drop(attempt);
        state.emit(
            EVT_ATTEMPT_EXPIRED,
            json!({
                "attempt_id": attempt_id,
                "exam_id": exam_id,
                "user_id": user_id,
                "expired_by": "server",
            }),
        );
        return true;
    }
    false
}

/// Auto-grade the objective answers of a submitted attempt, create
/// pending manual grades for essay answers, and finish the attempt
/// when nothing needs a human grader. Returns the attempt's current
/// objective score and whether manual grading is still pending.
fn auto_grade_attempt(state: &ExamsState, attempt: &mut ExamAttempt) -> (f64, bool) {
    let settings = state.settings();
    let mut objective_score = 0.0;
    let mut needs_manual = false;
    let mut per_question = serde_json::Map::new();
    let exam_questions: Vec<ExamQuestion> = state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == attempt.exam_id)
        .map(|entry| entry.value().clone())
        .collect();
    for link in &exam_questions {
        let question = match state.questions.get(&link.question_id) {
            Some(entry) => entry.value().clone(),
            None => continue,
        };
        let answer = attempt
            .answers
            .get(&link.question_id)
            .cloned()
            .unwrap_or(Value::Null);
        match auto_score(&question, &answer, settings.short_answer_case_insensitive) {
            Some(awarded) => {
                objective_score += awarded;
                per_question.insert(
                    link.question_id.clone(),
                    json!({
                        "awarded": awarded,
                        "max": link.marks,
                        "correct": awarded >= link.marks,
                        "graded_by": "auto",
                    }),
                );
            }
            None => {
                needs_manual = true;
                per_question.insert(
                    link.question_id.clone(),
                    json!({
                        "awarded": null,
                        "max": link.marks,
                        "correct": null,
                        "graded_by": "manual",
                    }),
                );
                let already = state.grades.iter().any(|entry| {
                    entry.value().attempt_id == attempt.id
                        && entry.value().question_id == link.question_id
                });
                if !already {
                    let grade = ExamGrade {
                        id: new_id(),
                        attempt_id: attempt.id.clone(),
                        exam_id: attempt.exam_id.clone(),
                        question_id: link.question_id.clone(),
                        grader_id: None,
                        rubric_id: None,
                        criterion_scores: HashMap::new(),
                        marks_awarded: None,
                        max_marks: link.marks,
                        status: GRADE_PENDING.to_string(),
                        comment: None,
                        second_grader_id: None,
                        audit: vec![AuditEntry {
                            at: now(),
                            actor: "system".to_string(),
                            action: "manual_grade_created".to_string(),
                            detail: "Essay answer queued for manual grading".to_string(),
                        }],
                        created_at: now(),
                        updated_at: now(),
                    };
                    state.grades.insert(grade.id.clone(), grade);
                }
            }
        }
    }
    attempt.score = Some(objective_score);
    (objective_score, needs_manual)
}

/// Finish grading for an attempt: build the result, decide pass or
/// fail, emit the grading, certificate, and academic events, and
/// notify the student. Called once all manual grades are in.
fn finalize_grading(state: &ExamsState, attempt_id: &str) -> UnifiedResult<ExamResult> {
    let mut attempt = state
        .attempts
        .get(attempt_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
    let exam = state
        .exams
        .get(&attempt.exam_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
    let mut total = 0.0;
    let mut per_question = serde_json::Map::new();
    let exam_questions: Vec<ExamQuestion> = state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == attempt.exam_id)
        .map(|entry| entry.value().clone())
        .collect();
    for link in &exam_questions {
        let question = match state.questions.get(&link.question_id) {
            Some(entry) => entry.value().clone(),
            None => continue,
        };
        let answer = attempt
            .answers
            .get(&link.question_id)
            .cloned()
            .unwrap_or(Value::Null);
        if let Some(awarded) =
            auto_score(&question, &answer, state.settings().short_answer_case_insensitive)
        {
            // Objective scores are recomputed from the stored answers
            // every time, so regrades and second grades never lose
            // them when the attempt score is cleared on reopen.
            total += awarded;
            per_question.insert(
                link.question_id.clone(),
                json!({
                    "awarded": awarded,
                    "max": link.marks,
                    "correct": awarded >= link.marks,
                    "graded_by": "auto",
                }),
            );
        }
    }
    for entry in state.grades.iter() {
        let grade = entry.value();
        if grade.attempt_id != attempt.id {
            continue;
        }
        let awarded = grade.marks_awarded.unwrap_or(0.0);
        if grade.status == GRADE_GRADED {
            total += awarded;
            per_question.insert(
                grade.question_id.clone(),
                json!({
                    "awarded": awarded,
                    "max": grade.max_marks,
                    "correct": awarded >= grade.max_marks,
                    "graded_by": grade.grader_id.clone(),
                }),
            );
        }
    }
    let max_score = if attempt.max_score > 0.0 {
        attempt.max_score
    } else {
        exam.total_marks
    };
    let percentage = if max_score > 0.0 {
        total / max_score * 100.0
    } else {
        0.0
    };
    let passed = percentage >= exam.pass_mark;
    let result = ExamResult {
        id: new_id(),
        attempt_id: attempt.id.clone(),
        exam_id: attempt.exam_id.clone(),
        user_id: attempt.user_id.clone(),
        score: total,
        max_score,
        percentage,
        passed,
        status: RESULT_DRAFT.to_string(),
        per_question: Value::Object(per_question),
        graded_at: now(),
        published_at: None,
        created_at: now(),
        updated_at: now(),
    };
    attempt.score = Some(total);
    attempt.passed = Some(passed);
    attempt.status = ATTEMPT_GRADED.to_string();
    attempt.graded_at = Some(now());
    attempt.result_id = Some(result.id.clone());
    state.results.insert(result.id.clone(), result.clone());
    state.attempts.insert(attempt.id.clone(), attempt.clone());
    state.emit(
        EVT_ATTEMPT_GRADED,
        json!({
            "attempt_id": attempt.id,
            "exam_id": attempt.exam_id,
            "user_id": attempt.user_id,
            "score": total,
            "max_score": max_score,
            "percentage": percentage,
            "passed": passed,
            "result_id": result.id,
        }),
    );
    // Grades live in the academic record, never duplicated here:
    // emit the academic event with exactly the payload the academic
    // module records itself, only when the exam is linked.
    if let (Some(enrollment_id), Some(component_id)) = (
        exam.academic_enrollment_id.clone(),
        exam.academic_component_id.clone(),
    ) {
        state.emit(
            EVT_ACADEMIC_GRADE,
            json!({
                "grade_id": result.id,
                "enrollment_id": enrollment_id,
                "component_id": component_id,
                "score": percentage,
            }),
        );
    }
    if passed && state.settings().certificate_on_pass {
        state.emit(
            EVT_CERTIFICATE_REQUESTED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "result_id": result.id,
                "score": total,
                "passed": passed,
                "module": "exams",
            }),
        );
    }
    state.notify(
        "exams.graded",
        &attempt.user_id,
        "Your exam has been graded",
        &format!(
            "Exam \"{}\" graded: {:.1}% - {}.",
            exam.title,
            percentage,
            if passed { "passed" } else { "not passed" }
        ),
        Some(format!("/api/v1/exams/results/{}", result.id)),
    );
    Ok(result)
}

/// Per-question statistics for an exam, computed from graded
/// attempts only. No student identity or raw answer is included.
fn question_statistics(state: &ExamsState, exam_id: &str) -> Value {
    let graded: Vec<ExamAttempt> = state
        .attempts
        .iter()
        .filter(|entry| {
            entry.value().exam_id == exam_id && entry.value().status == ATTEMPT_GRADED
        })
        .map(|entry| entry.value().clone())
        .collect();
    let total_attempts = graded.len();
    let mut stats: Vec<Value> = Vec::new();
    let links: Vec<ExamQuestion> = state
        .exam_questions
        .iter()
        .filter(|entry| entry.value().exam_id == exam_id)
        .map(|entry| entry.value().clone())
        .collect();
    // Discrimination index: top 27% of attempts by score versus
    // bottom 27%, per question.
    let mut ranked: Vec<&ExamAttempt> = graded.iter().collect();
    ranked.sort_by(|a, b| {
        b.score
            .unwrap_or(0.0)
            .partial_cmp(&a.score.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let group_size = ((total_attempts as f64 * 0.27).ceil() as usize).max(1);
    let top: Vec<&ExamAttempt> = ranked.iter().take(group_size).cloned().collect();
    let bottom: Vec<&ExamAttempt> = ranked
        .iter()
        .rev()
        .take(group_size)
        .cloned()
        .collect();
    for link in &links {
        let question = match state.questions.get(&link.question_id) {
            Some(entry) => entry.value().clone(),
            None => continue,
        };
        let mut attempted = 0usize;
        let mut correct = 0usize;
        let mut top_correct = 0usize;
        let mut top_attempted = 0usize;
        let mut bottom_correct = 0usize;
        let mut bottom_attempted = 0usize;
        let case_insensitive = state.settings().short_answer_case_insensitive;
        for attempt in &graded {
            let answer = attempt.answers.get(&link.question_id);
            if let Some(answer) = answer {
                if !answer.is_null() {
                    attempted += 1;
                    if let Some(awarded) = auto_score(&question, answer, case_insensitive) {
                        if awarded >= link.marks {
                            correct += 1;
                        }
                    }
                }
            }
        }
        for attempt in &top {
            let answer = attempt.answers.get(&link.question_id);
            if let Some(answer) = answer {
                if !answer.is_null() {
                    top_attempted += 1;
                    if let Some(awarded) = auto_score(&question, answer, case_insensitive) {
                        if awarded >= link.marks {
                            top_correct += 1;
                        }
                    }
                }
            }
        }
        for attempt in &bottom {
            let answer = attempt.answers.get(&link.question_id);
            if let Some(answer) = answer {
                if !answer.is_null() {
                    bottom_attempted += 1;
                    if let Some(awarded) = auto_score(&question, answer, case_insensitive) {
                        if awarded >= link.marks {
                            bottom_correct += 1;
                        }
                    }
                }
            }
        }
        let attempted_pct = if total_attempts > 0 {
            attempted as f64 / total_attempts as f64 * 100.0
        } else {
            0.0
        };
        let correct_pct = if attempted > 0 {
            correct as f64 / attempted as f64 * 100.0
        } else {
            0.0
        };
        let top_rate = if top_attempted > 0 {
            top_correct as f64 / top_attempted as f64
        } else {
            0.0
        };
        let bottom_rate = if bottom_attempted > 0 {
            bottom_correct as f64 / bottom_attempted as f64
        } else {
            0.0
        };
        stats.push(json!({
            "question_id": link.question_id,
            "question_type": question.question_type,
            "difficulty": question.difficulty,
            "marks": link.marks,
            "attempted_count": attempted,
            "attempted_pct": attempted_pct,
            "correct_count": correct,
            "correct_pct": correct_pct,
            "discrimination_index": top_rate - bottom_rate,
        }));
    }
    json!({
        "exam_id": exam_id,
        "graded_attempts": total_attempts,
        "questions": stats,
    })
}

// ---------------------------------------------------------------------------
// Input payloads
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
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

// ---------------------------------------------------------------------------
// Handlers: question banks
// ---------------------------------------------------------------------------

async fn list_banks(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<QuestionBank>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<QuestionBank> = state
            .banks
            .iter()
            .filter(|entry| {
                let bank = entry.value();
                (params
                    .filters
                    .get("category")
                    .map(|v| &bank.category == v)
                    .unwrap_or(true))
                    && (params
                        .filters
                        .get("owner_id")
                        .map(|v| &bank.owner_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("company_id")
                        .map(|v| bank.company_id.as_deref() == Some(v.as_str()))
                        .unwrap_or(true))
                    && matches_search(&format!("{} {}", bank.title, bank.description), &params.search)
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<CreateBankInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuestionBank> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let description = body
            .description
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(UnifiedError::bad_request(
                "description must be at most 5000 characters",
            ));
        }
        let category = require_in_list(&body.category, "category", &settings.categories)?;
        let company_id = optional_text(&body.company_id, "company_id", MAX_SLUG_LEN)?;
        let bank = QuestionBank {
            id: new_id(),
            title,
            description,
            category,
            owner_id: caller.actor,
            company_id,
            created_at: now(),
            updated_at: now(),
        };
        state.banks.insert(bank.id.clone(), bank.clone());
        Ok(bank)
    })();
    json::respond(result, debug)
}

async fn get_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuestionBank> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))
    })();
    json::respond(result, debug)
}

async fn update_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateBankInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuestionBank> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let mut bank = state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
        if let Some(title) = &body.title {
            bank.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &body.description {
            if description.len() > MAX_DESCRIPTION_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 5000 characters",
                ));
            }
            bank.description = description.trim().to_string();
        }
        if let Some(category) = &body.category {
            bank.category = require_in_list(category, "category", &settings.categories)?;
        }
        bank.updated_at = now();
        state.banks.insert(bank.id.clone(), bank.clone());
        Ok(bank)
    })();
    json::respond(result, debug)
}

async fn delete_bank(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let bank = state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
        if state
            .questions
            .iter()
            .any(|entry| entry.value().bank_id == bank.id)
        {
            return Err(UnifiedError::conflict(
                "Question bank still holds questions; delete or move them first",
            ));
        }
        state.banks.remove(&bank.id);
        Ok(json!({ "deleted": true, "id": bank.id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: questions
// ---------------------------------------------------------------------------

fn question_admin_view(question: &Question) -> Value {
    json!(question)
}

async fn list_bank_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let bank = state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .questions
            .iter()
            .filter(|entry| {
                let question = entry.value();
                question.bank_id == bank.id
                    && question.superseded_by.is_none()
                    && (params
                        .filters
                        .get("question_type")
                        .map(|v| &question.question_type == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("difficulty")
                        .map(|v| &question.difficulty == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("category")
                        .map(|v| &question.category == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &question.status == v)
                        .unwrap_or(true))
                    && matches_search(&question.text, &params.search)
            })
            .map(|entry| {
                let question = entry.value();
                if is_manager {
                    question_admin_view(question)
                } else {
                    json!(QuestionPublicView::from(question))
                }
            })
            .collect();
        items.sort_by(|a, b| {
            let a_text = a.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let b_text = b.get("text").and_then(|v| v.as_str()).unwrap_or("");
            b_text.cmp(a_text)
        });
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuestionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Question> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let bank = state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
        let question = build_question(&state, &settings, &bank.id, &caller.actor, body.into_inner())?;
        let created = question.clone();
        state.questions.insert(question.id.clone(), question);
        state.emit(
            EVT_QUESTION_CREATED,
            json!({
                "question_id": created.id,
                "bank_id": created.bank_id,
                "question_type": created.question_type,
                "version": created.version,
            }),
        );
        Ok(created)
    })();
    json::respond(result, debug)
}

fn build_question(
    _state: &ExamsState,
    settings: &ExamsSettings,
    bank_id: &str,
    actor: &str,
    input: CreateQuestionInput,
) -> UnifiedResult<Question> {
    let question_type = require_in_list(&input.question_type, "question_type", &settings.question_types)?;
    let category = require_in_list(&input.category, "category", &settings.categories)?;
    let difficulty = require_in_list(&input.difficulty, "difficulty", &settings.difficulty_levels)?;
    let text = require_text(&input.text, "text", MAX_TEXT_LEN)?;
    let marks = require_marks(input.marks.unwrap_or(1.0), "marks")?;
    let status = match input.status.as_deref() {
        Some("published") => "published".to_string(),
        _ => "draft".to_string(),
    };
    let options = validate_question_payload(
        settings,
        &question_type,
        &input.options.unwrap_or_default(),
        &input.correct_answer.clone().unwrap_or(Value::Null),
    )?;
    let answer_key = optional_text(&input.answer_key, "answer_key", MAX_TEXT_LEN)?;
    Ok(Question {
        id: new_id(),
        bank_id: bank_id.to_string(),
        version: 1,
        previous_version_id: None,
        superseded_by: None,
        question_type,
        category,
        difficulty,
        text,
        options,
        correct_answer: input.correct_answer.unwrap_or(Value::Null),
        answer_key,
        marks,
        status,
        created_by: actor.to_string(),
        created_at: now(),
        updated_at: now(),
    })
}

async fn get_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let question = state
            .questions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question not found"))?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        if is_manager {
            Ok(question_admin_view(&question))
        } else {
            Ok(json!(QuestionPublicView::from(&question)))
        }
    })();
    json::respond(result, debug)
}

async fn update_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateQuestionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Question> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let existing = state
            .questions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question not found"))?;
        // Editing a question that already has attempts creates a new
        // version; old attempts keep their pinned version.
        let must_version = question_has_attempts(&state, &existing.id);
        let mut next = existing.clone();
        if let Some(question_type) = &input.question_type {
            next.question_type =
                require_in_list(question_type, "question_type", &settings.question_types)?;
        }
        if let Some(category) = &input.category {
            next.category = require_in_list(category, "category", &settings.categories)?;
        }
        if let Some(difficulty) = &input.difficulty {
            next.difficulty =
                require_in_list(difficulty, "difficulty", &settings.difficulty_levels)?;
        }
        if let Some(text) = &input.text {
            next.text = require_text(text, "text", MAX_TEXT_LEN)?;
        }
        if let Some(marks) = input.marks {
            next.marks = require_marks(marks, "marks")?;
        }
        if let Some(status) = &input.status {
            next.status = match status.as_str() {
                "published" => "published".to_string(),
                _ => "draft".to_string(),
            };
        }
        if let Some(answer_key) = &input.answer_key {
            next.answer_key = match answer_key {
                Some(raw) => Some(require_text(raw, "answer_key", MAX_TEXT_LEN)?),
                None => None,
            };
        }
        if let Some(options) = &input.options {
            let correct = input
                .correct_answer
                .clone()
                .unwrap_or_else(|| existing.correct_answer.clone());
            next.options = validate_question_payload(
                &settings,
                &next.question_type,
                options,
                &correct,
            )?;
            next.correct_answer = correct;
        } else if let Some(correct) = input.correct_answer.clone() {
            let option_inputs: Vec<QuestionOptionInput> = next
                .options
                .iter()
                .map(|o| QuestionOptionInput {
                    id: Some(o.id.clone()),
                    text: o.text.clone(),
                    position: Some(o.position),
                })
                .collect();
            next.options = validate_question_payload(
                &settings,
                &next.question_type,
                &option_inputs,
                &correct,
            )?;
            next.correct_answer = correct;
        }
        if must_version {
            let version = QuestionVersion {
                id: new_id(),
                question_id: existing.id.clone(),
                version: existing.version,
                snapshot: snapshot_question(&existing),
                reason: input.reason.clone(),
                created_by: caller.actor.clone(),
                created_at: now(),
            };
            state.versions.insert(version.id.clone(), version);
            let mut old = existing.clone();
            old.superseded_by = Some(new_id());
            next.id = old.superseded_by.clone().unwrap_or_else(new_id);
            next.version = existing.version + 1;
            next.previous_version_id = Some(existing.id.clone());
            next.created_at = now();
            next.updated_at = now();
            state.questions.insert(old.id.clone(), old);
            state.questions.insert(next.id.clone(), next.clone());
        } else {
            next.updated_at = now();
            state.questions.insert(next.id.clone(), next.clone());
        }
        state.emit(
            EVT_QUESTION_UPDATED,
            json!({
                "question_id": next.id,
                "bank_id": next.bank_id,
                "question_type": next.question_type,
                "version": next.version,
                "new_version_created": must_version,
            }),
        );
        Ok(next)
    })();
    json::respond(result, debug)
}

async fn delete_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let question = state
            .questions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question not found"))?;
        if question_has_attempts(&state, &question.id) {
            return Err(UnifiedError::conflict(
                "Question already has attempts; it cannot be deleted, only superseded by editing it",
            ));
        }
        if state
            .exam_questions
            .iter()
            .any(|entry| entry.value().question_id == question.id)
        {
            return Err(UnifiedError::conflict(
                "Question is attached to an exam; detach it first",
            ));
        }
        state.questions.remove(&question.id);
        Ok(json!({ "deleted": true, "id": question.id }))
    })();
    json::respond(result, debug)
}

async fn list_question_versions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<QuestionVersion>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let question = state
            .questions
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question not found"))?;
        let params = query.into_inner();
        let mut items: Vec<QuestionVersion> = state
            .versions
            .iter()
            .filter(|entry| entry.value().question_id == question.id)
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by_key(|v| v.version);
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn import_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ImportQuestionsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let bank = state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
        if body.questions.len() > MAX_QUESTIONS_PER_EXAM {
            return Err(UnifiedError::bad_request(format!(
                "An import may hold at most {} questions",
                MAX_QUESTIONS_PER_EXAM
            )));
        }
        let mut imported = 0usize;
        let mut skipped = 0usize;
        let mut errors: Vec<String> = Vec::new();
        for input in &body.questions {
            let mut draft = input.clone();
            draft.status = Some("draft".to_string());
            match build_question(&state, &settings, &bank.id, &caller.actor, draft) {
                Ok(question) => {
                    let id = question.id.clone();
                    state.questions.insert(id, question);
                    imported += 1;
                }
                Err(err) => {
                    skipped += 1;
                    errors.push(err.to_string());
                }
            }
        }
        state.emit(
            EVT_QUESTIONS_IMPORT,
            json!({
                "bank_id": bank.id,
                "imported": imported,
                "skipped": skipped,
                "format": "json",
            }),
        );
        Ok(json!({
            "bank_id": bank.id,
            "imported": imported,
            "skipped": skipped,
            "errors": errors,
        }))
    })();
    json::respond(result, debug)
}

async fn export_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let bank = state
            .banks
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question bank not found"))?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .questions
            .iter()
            .filter(|entry| {
                entry.value().bank_id == bank.id && entry.value().superseded_by.is_none()
            })
            .map(|entry| {
                let question = entry.value();
                if is_manager {
                    question_admin_view(question)
                } else {
                    json!(QuestionPublicView::from(question))
                }
            })
            .collect();
        items.sort_by(|a, b| {
            let a_text = a.get("text").and_then(|v| v.as_str()).unwrap_or("");
            let b_text = b.get("text").and_then(|v| v.as_str()).unwrap_or("");
            a_text.cmp(b_text)
        });
        let page = page_of(items, &params);
        Ok(json!({
            "bank_id": bank.id,
            "format": "json",
            "exported_at": now(),
            "items": page.items,
            "pagination": page.pagination,
        }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: exams
// ---------------------------------------------------------------------------

fn exam_settings_from(input: &Option<ExamSettingsInput>, defaults: &ExamsSettings) -> UnifiedResult<ExamSettings> {
    let mut settings = ExamSettings::default();
    if let Some(input) = input {
        if let Some(show_results) = &input.show_results {
            let checked = require_text(show_results, "show_results", MAX_SLUG_LEN)?;
            if checked != "immediately" && checked != "after_grading" {
                return Err(UnifiedError::bad_request(
                    "show_results must be \"immediately\" or \"after_grading\"",
                ));
            }
            settings.show_results = checked;
        }
        if let Some(value) = input.allow_review {
            settings.allow_review = value;
        }
        if let Some(value) = input.calculator_allowed {
            settings.calculator_allowed = value;
        }
        if let Some(value) = input.notes_allowed {
            settings.notes_allowed = value;
        }
        if let Some(value) = input.randomise_questions {
            settings.randomise_questions = value;
        }
        if let Some(value) = input.randomise_options {
            settings.randomise_options = value;
        }
    }
    let _ = defaults;
    Ok(settings)
}

fn valid_exam_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (EXAM_DRAFT, EXAM_PUBLISHED)
            | (EXAM_PUBLISHED, EXAM_ARCHIVED)
            | (EXAM_DRAFT, EXAM_ARCHIVED)
            | (EXAM_PUBLISHED, EXAM_CLOSED)
            | (EXAM_ARCHIVED, EXAM_DRAFT)
    )
}

async fn list_exams(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Exam>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let params = query.into_inner();
        let mut items: Vec<Exam> = state
            .exams
            .iter()
            .filter(|entry| {
                let exam = entry.value();
                (is_manager || exam.status == EXAM_PUBLISHED || exam.status == EXAM_CLOSED)
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &exam.status == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("category")
                        .map(|v| &exam.category == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("course_id")
                        .map(|v| exam.course_id.as_deref() == Some(v.as_str()))
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("bank_id")
                        .map(|v| exam.bank_ids.iter().any(|id| id == v))
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("created_by")
                        .map(|v| &exam.created_by == v)
                        .unwrap_or(true))
                    && matches_search(&format!("{} {}", exam.title, exam.description), &params.search)
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<CreateExamInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = input.description.as_deref().unwrap_or("").trim().to_string();
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(UnifiedError::bad_request(
                "description must be at most 5000 characters",
            ));
        }
        let category = match &input.category {
            Some(raw) => require_in_list(raw, "category", &settings.categories)?,
            None => "general".to_string(),
        };
        let bank_ids = input.bank_ids.unwrap_or_default();
        for bank_id in &bank_ids {
            if !state.banks.contains_key(bank_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Question bank not found: {bank_id}"
                )));
            }
        }
        if let (Some(opens), Some(closes)) = (input.opens_at, input.closes_at) {
            if closes <= opens {
                return Err(UnifiedError::bad_request(
                    "closes_at must be after opens_at",
                ));
            }
        }
        let duration_minutes = input
            .duration_minutes
            .unwrap_or(settings.default_duration_minutes);
        if duration_minutes < 1 || duration_minutes > 10080 {
            return Err(UnifiedError::bad_request(
                "duration_minutes must be between 1 and 10080",
            ));
        }
        let pass_mark = require_percentage(
            input.pass_mark.unwrap_or(settings.default_pass_mark),
            "pass_mark",
        )?;
        let exam_settings = exam_settings_from(&input.settings, &settings)?;
        let exam = Exam {
            id: new_id(),
            title,
            description,
            category,
            bank_ids,
            course_id: optional_text(&input.course_id, "course_id", MAX_REF_LEN)?,
            academic_enrollment_id: optional_text(
                &input.academic_enrollment_id,
                "academic_enrollment_id",
                MAX_REF_LEN,
            )?,
            academic_component_id: optional_text(
                &input.academic_component_id,
                "academic_component_id",
                MAX_REF_LEN,
            )?,
            status: EXAM_DRAFT.to_string(),
            opens_at: input.opens_at,
            closes_at: input.closes_at,
            duration_minutes,
            pass_mark,
            attempts_allowed: input
                .attempts_allowed
                .unwrap_or(settings.default_attempts_allowed)
                .max(1),
            settings: exam_settings,
            total_marks: 0.0,
            created_by: caller.actor,
            company_id: optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?,
            created_at: now(),
            updated_at: now(),
        };
        state.exams.insert(exam.id.clone(), exam.clone());
        state.emit(
            EVT_EXAM_CREATED,
            json!({
                "exam_id": exam.id,
                "title": exam.title,
                "status": exam.status,
                "course_id": exam.course_id,
            }),
        );
        Ok(exam)
    })();
    json::respond(result, debug)
}

async fn get_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        if !is_manager && exam.status == EXAM_DRAFT {
            return Err(UnifiedError::not_found("Exam not found"));
        }
        Ok(exam)
    })();
    json::respond(result, debug)
}

async fn update_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateExamInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let mut exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        if exam.status == EXAM_CLOSED || exam.status == EXAM_ARCHIVED {
            return Err(UnifiedError::conflict(
                "A closed or archived exam cannot be edited; duplicate it instead",
            ));
        }
        if let Some(title) = &input.title {
            exam.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &input.description {
            if description.len() > MAX_DESCRIPTION_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 5000 characters",
                ));
            }
            exam.description = description.trim().to_string();
        }
        if let Some(category) = &input.category {
            exam.category = require_in_list(category, "category", &settings.categories)?;
        }
        if let Some(bank_ids) = &input.bank_ids {
            for bank_id in bank_ids {
                if !state.banks.contains_key(bank_id) {
                    return Err(UnifiedError::bad_request(format!(
                        "Question bank not found: {bank_id}"
                    )));
                }
            }
            exam.bank_ids = bank_ids.clone();
        }
        if let Some(course_id) = &input.course_id {
            exam.course_id = match course_id {
                Some(raw) => Some(require_text(raw, "course_id", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(enrollment_id) = &input.academic_enrollment_id {
            exam.academic_enrollment_id = match enrollment_id {
                Some(raw) => Some(require_text(raw, "academic_enrollment_id", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(component_id) = &input.academic_component_id {
            exam.academic_component_id = match component_id {
                Some(raw) => Some(require_text(raw, "academic_component_id", MAX_REF_LEN)?),
                None => None,
            };
        }
        if let Some(opens_at) = input.opens_at {
            exam.opens_at = opens_at;
        }
        if let Some(closes_at) = input.closes_at {
            exam.closes_at = closes_at;
        }
        if let (Some(opens), Some(closes)) = (exam.opens_at, exam.closes_at) {
            if closes <= opens {
                return Err(UnifiedError::bad_request(
                    "closes_at must be after opens_at",
                ));
            }
        }
        if let Some(duration) = input.duration_minutes {
            if duration < 1 || duration > 10080 {
                return Err(UnifiedError::bad_request(
                    "duration_minutes must be between 1 and 10080",
                ));
            }
            exam.duration_minutes = duration;
        }
        if let Some(pass_mark) = input.pass_mark {
            exam.pass_mark = require_percentage(pass_mark, "pass_mark")?;
        }
        if let Some(attempts) = input.attempts_allowed {
            exam.attempts_allowed = attempts.max(1);
        }
        if let Some(settings_input) = &input.settings {
            exam.settings = exam_settings_from(&Some(settings_input.clone()), &settings)?;
        }
        exam.updated_at = now();
        state.exams.insert(exam.id.clone(), exam.clone());
        Ok(exam)
    })();
    json::respond(result, debug)
}

async fn delete_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        if state
            .attempts
            .iter()
            .any(|entry| entry.value().exam_id == exam.id)
        {
            return Err(UnifiedError::conflict(
                "Exam already has attempts; archive or close it instead of deleting",
            ));
        }
        let link_ids: Vec<String> = state
            .exam_questions
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .map(|entry| entry.key().clone())
            .collect();
        for id in link_ids {
            state.exam_questions.remove(&id);
        }
        state.exams.remove(&exam.id);
        Ok(json!({ "deleted": true, "id": exam.id }))
    })();
    json::respond(result, debug)
}

fn change_exam_status(
    state: &ExamsState,
    caller: &Caller,
    exam_id: &str,
    target: &str,
    event: &str,
) -> UnifiedResult<Exam> {
    let mut exam = state
        .exams
        .get(exam_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
    if !valid_exam_transition(&exam.status, target) {
        return Err(UnifiedError::conflict(format!(
            "Exam cannot move from {} to {target}",
            exam.status
        )));
    }
    if target == EXAM_PUBLISHED {
        let question_count = state
            .exam_questions
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .count();
        if question_count == 0 {
            return Err(UnifiedError::conflict(
                "An exam needs at least one question before it can be published",
            ));
        }
    }
    let from = exam.status.clone();
    exam.status = target.to_string();
    exam.updated_at = now();
    state.exams.insert(exam.id.clone(), exam.clone());
    state.emit(
        event,
        json!({
            "exam_id": exam.id,
            "from_status": from,
            "to_status": target,
            "actor": caller.actor,
        }),
    );
    Ok(exam)
}

async fn publish_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        change_exam_status(&state, &caller, path.as_str(), EXAM_PUBLISHED, EVT_EXAM_PUBLISHED)
    })();
    json::respond(result, debug)
}

async fn archive_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        change_exam_status(&state, &caller, path.as_str(), EXAM_ARCHIVED, EVT_EXAM_CLOSED)
    })();
    json::respond(result, debug)
}

async fn close_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        change_exam_status(&state, &caller, path.as_str(), EXAM_CLOSED, EVT_EXAM_CLOSED)
    })();
    json::respond(result, debug)
}

async fn duplicate_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let source = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let mut copy = source.clone();
        copy.id = new_id();
        copy.title = format!("{} (copy)", source.title);
        copy.status = EXAM_DRAFT.to_string();
        copy.opens_at = None;
        copy.closes_at = None;
        copy.created_at = now();
        copy.updated_at = now();
        state.exams.insert(copy.id.clone(), copy.clone());
        let links: Vec<ExamQuestion> = state
            .exam_questions
            .iter()
            .filter(|entry| entry.value().exam_id == source.id)
            .map(|entry| entry.value().clone())
            .collect();
        for link in links {
            let mut copied = link.clone();
            copied.id = new_id();
            copied.exam_id = copy.id.clone();
            state.exam_questions.insert(copied.id.clone(), copied);
        }
        recompute_exam_total(&state, &copy.id);
        let copy = state
            .exams
            .get(&copy.id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::internal("Duplicated exam was not stored"))?;
        state.emit(
            EVT_EXAM_CREATED,
            json!({
                "exam_id": copy.id,
                "title": copy.title,
                "status": copy.status,
                "duplicated_from": source.id,
            }),
        );
        Ok(copy)
    })();
    json::respond(result, debug)
}

async fn list_exam_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .exam_questions
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .map(|entry| {
                let link = entry.value().clone();
                let question = state
                    .questions
                    .get(&link.question_id)
                    .map(|q| q.value().clone());
                let question_view = match question {
                    Some(question) => {
                        if is_manager {
                            question_admin_view(&question)
                        } else {
                            json!(QuestionPublicView::from(&question))
                        }
                    }
                    None => Value::Null,
                };
                json!({
                    "link_id": link.id,
                    "exam_id": link.exam_id,
                    "question_id": link.question_id,
                    "question_version": link.question_version,
                    "position": link.position,
                    "section": link.section,
                    "marks": link.marks,
                    "question": question_view,
                })
            })
            .collect();
        items.sort_by_key(|item| {
            item.get("position").and_then(|v| v.as_u64()).unwrap_or(0)
        });
        if params.order.as_deref() == Some("desc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn attach_exam_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AttachQuestionInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamQuestion> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        if exam.status == EXAM_CLOSED || exam.status == EXAM_ARCHIVED {
            return Err(UnifiedError::conflict(
                "Questions cannot be attached to a closed or archived exam",
            ));
        }
        let question = state
            .questions
            .get(&input.question_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Question not found"))?;
        if question.superseded_by.is_some() {
            return Err(UnifiedError::bad_request(
                "A superseded question version cannot be attached; attach the current version",
            ));
        }
        let count = state
            .exam_questions
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .count();
        if count >= settings.max_questions_per_exam {
            return Err(UnifiedError::bad_request(format!(
                "An exam may hold at most {} questions",
                settings.max_questions_per_exam
            )));
        }
        if state.exam_questions.iter().any(|entry| {
            entry.value().exam_id == exam.id && entry.value().question_id == question.id
        }) {
            return Err(UnifiedError::conflict(
                "Question is already attached to this exam",
            ));
        }
        let position = input.position.unwrap_or_else(|| {
            state
                .exam_questions
                .iter()
                .filter(|entry| entry.value().exam_id == exam.id)
                .map(|entry| entry.value().position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(0)
        });
        let marks = require_marks(input.marks.unwrap_or(question.marks), "marks")?;
        let link = ExamQuestion {
            id: new_id(),
            exam_id: exam.id.clone(),
            question_id: question.id.clone(),
            question_version: input.question_version.unwrap_or(question.version),
            position,
            section: optional_text(&input.section, "section", MAX_SLUG_LEN)?,
            marks,
        };
        state.exam_questions.insert(link.id.clone(), link.clone());
        recompute_exam_total(&state, &exam.id);
        Ok(link)
    })();
    json::respond(result, debug)
}

async fn detach_exam_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let (exam_id, question_id) = path.into_inner();
        let exam = state
            .exams
            .get(&exam_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        if state
            .attempts
            .iter()
            .any(|entry| entry.value().exam_id == exam.id)
        {
            return Err(UnifiedError::conflict(
                "Questions cannot be detached once attempts exist",
            ));
        }
        let link_id = state
            .exam_questions
            .iter()
            .find(|entry| {
                entry.value().exam_id == exam.id && entry.value().question_id == question_id
            })
            .map(|entry| entry.key().clone())
            .ok_or_else(|| UnifiedError::not_found("Question is not attached to this exam"))?;
        state.exam_questions.remove(&link_id);
        recompute_exam_total(&state, &exam.id);
        Ok(json!({ "deleted": true, "exam_id": exam.id, "question_id": question_id }))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: attempts (taking an exam)
// ---------------------------------------------------------------------------

fn exam_window_open(exam: &Exam, at: DateTime<Utc>) -> UnifiedResult<()> {
    if exam.status != EXAM_PUBLISHED {
        return Err(UnifiedError::conflict(
            "Exam is not published; it cannot be taken",
        ));
    }
    if let Some(opens) = exam.opens_at {
        if at < opens {
            return Err(UnifiedError::conflict("Exam has not opened yet"));
        }
    }
    if let Some(closes) = exam.closes_at {
        if at > closes {
            return Err(UnifiedError::conflict("Exam has closed"));
        }
    }
    Ok(())
}

fn visible_attempt(
    _state: &ExamsState,
    ctx: &AppContext,
    caller: &Caller,
    attempt: &ExamAttempt,
) -> UnifiedResult<()> {
    let is_manager = check_capability(ctx, &caller.role, PERM_EXAMS_MANAGE);
    let is_grader = check_capability(ctx, &caller.role, PERM_EXAMS_GRADE);
    if is_manager || is_grader || attempt.user_id == caller.actor {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Attempt not found"))
    }
}

async fn start_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let started_at = now();
        exam_window_open(&exam, started_at)?;
        // Resume: an in-progress attempt for this student continues
        // with its remaining server time instead of starting over.
        let existing = state
            .attempts
            .iter()
            .find(|entry| {
                entry.value().exam_id == exam.id
                    && entry.value().user_id == caller.actor
                    && entry.value().status == ATTEMPT_IN_PROGRESS
            })
            .map(|entry| entry.value().clone());
        if let Some(mut attempt) = existing {
            if expire_if_due(&state, &attempt.id) {
                attempt = state
                    .attempts
                    .get(&attempt.id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::internal("Attempt was not stored"))?;
                return Ok(state.attempt_view(&attempt));
            }
            return Ok(state.attempt_view(&attempt));
        }
        let used = state
            .attempts
            .iter()
            .filter(|entry| {
                entry.value().exam_id == exam.id && entry.value().user_id == caller.actor
            })
            .count() as u32;
        if used >= exam.attempts_allowed {
            return Err(UnifiedError::conflict(
                "No attempts remain for this exam",
            ));
        }
        let mut links: Vec<ExamQuestion> = state
            .exam_questions
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .map(|entry| entry.value().clone())
            .collect();
        links.sort_by_key(|link| link.position);
        if links.is_empty() {
            return Err(UnifiedError::conflict(
                "Exam has no questions attached",
            ));
        }
        // Seeded randomisation: the seed is recorded on the attempt
        // so the order is reproducible for review and grading.
        let seed: u64 = {
            let mut rng = rand::rng();
            rng.random()
        };
        let mut rng = StdRng::seed_from_u64(seed);
        let mut question_order: Vec<String> =
            links.iter().map(|link| link.question_id.clone()).collect();
        if exam.settings.randomise_questions {
            for i in (1..question_order.len()).rev() {
                let j = rng.random_range(0..=i);
                question_order.swap(i, j);
            }
        }
        let mut option_orders: HashMap<String, Vec<String>> = HashMap::new();
        for link in &links {
            let question = match state.questions.get(&link.question_id) {
                Some(entry) => entry.value().clone(),
                None => continue,
            };
            let mut option_ids: Vec<String> =
                question.options.iter().map(|o| o.id.clone()).collect();
            if exam.settings.randomise_options && question.question_type.starts_with("multiple_choice") {
                for i in (1..option_ids.len()).rev() {
                    let j = rng.random_range(0..=i);
                    option_ids.swap(i, j);
                }
            }
            option_orders.insert(link.question_id.clone(), option_ids);
        }
        let duration_seconds = exam.duration_minutes * 60;
        let attempt = ExamAttempt {
            id: new_id(),
            exam_id: exam.id.clone(),
            user_id: caller.actor.clone(),
            attempt_number: used + 1,
            status: ATTEMPT_IN_PROGRESS.to_string(),
            started_at,
            expires_at: started_at + chrono::Duration::seconds(duration_seconds),
            duration_seconds,
            seed,
            question_order,
            option_orders,
            answers: Value::Object(serde_json::Map::new()),
            autosaved_at: None,
            submitted_at: None,
            score: None,
            max_score: exam.total_marks,
            passed: None,
            graded_at: None,
            result_id: None,
        };
        state.attempts.insert(attempt.id.clone(), attempt.clone());
        state.emit(
            EVT_ATTEMPT_STARTED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": exam.id,
                "user_id": attempt.user_id,
                "attempt_number": attempt.attempt_number,
                "seed": attempt.seed,
                "expires_at": attempt.expires_at,
            }),
        );
        state.emit(
            EVT_EXAM_STARTED,
            json!({
                "exam_id": exam.id,
                "attempt_id": attempt.id,
                "user_id": attempt.user_id,
            }),
        );
        Ok(state.attempt_view(&attempt))
    })();
    json::respond(result, debug)
}

async fn list_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttemptView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        let params = query.into_inner();
        let mut items: Vec<AttemptView> = state
            .attempts
            .iter()
            .filter(|entry| {
                let attempt = entry.value();
                (is_manager
                    || is_grader
                    || attempt.user_id == caller.actor)
                    && (params
                        .filters
                        .get("exam_id")
                        .map(|v| &attempt.exam_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("user_id")
                        .map(|v| &attempt.user_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &attempt.status == v)
                        .unwrap_or(true))
            })
            .map(|entry| state.attempt_view(entry.value()))
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        expire_if_due(&state, path.as_str());
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        visible_attempt(&state, &ctx, &caller, &attempt)?;
        Ok(state.attempt_view(&attempt))
    })();
    json::respond(result, debug)
}

async fn list_my_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttemptView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<AttemptView> = state
            .attempts
            .iter()
            .filter(|entry| {
                let attempt = entry.value();
                attempt.user_id == caller.actor
                    && (params
                        .filters
                        .get("exam_id")
                        .map(|v| &attempt.exam_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &attempt.status == v)
                        .unwrap_or(true))
            })
            .map(|entry| state.attempt_view(entry.value()))
            .collect();
        items.sort_by(|a, b| b.started_at.cmp(&a.started_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn heartbeat_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        // The server decides expiry: an attempt past its deadline is
        // expired here, whatever the client clock says.
        expire_if_due(&state, path.as_str());
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.user_id != caller.actor {
            return Err(UnifiedError::not_found("Attempt not found"));
        }
        if attempt.status != ATTEMPT_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and can no longer send heartbeats",
                attempt.status
            )));
        }
        Ok(state.attempt_view(&attempt))
    })();
    json::respond(result, debug)
}

fn validate_answers(
    state: &ExamsState,
    attempt: &ExamAttempt,
    answers: &Value,
) -> UnifiedResult<Value> {
    let object = answers
        .as_object()
        .ok_or_else(|| UnifiedError::bad_request("answers must be a JSON object keyed by question id"))?;
    if object.len() > MAX_ANSWERS_PER_ATTEMPT {
        return Err(UnifiedError::bad_request(format!(
            "An attempt may hold at most {MAX_ANSWERS_PER_ATTEMPT} answers"
        )));
    }
    let allowed: Vec<String> = attempt.question_order.clone();
    let mut merged = attempt
        .answers
        .as_object()
        .cloned()
        .unwrap_or_default();
    for (question_id, answer) in object {
        if !allowed.iter().any(|id| id == question_id) {
            return Err(UnifiedError::bad_request(format!(
                "Answer for unknown question: {question_id}"
            )));
        }
        if answer.is_null() {
            merged.remove(question_id);
        } else {
            let question = state
                .questions
                .get(question_id)
                .map(|entry| entry.value().clone());
            if let Some(question) = question {
                match question.question_type.as_str() {
                    "multiple_choice_single" => {
                        if !answer.is_string() {
                            return Err(UnifiedError::bad_request(
                                "A single-answer question takes one option id",
                            ));
                        }
                    }
                    "multiple_choice_multiple" => {
                        if !answer.is_array() {
                            return Err(UnifiedError::bad_request(
                                "A multiple-answer question takes a list of option ids",
                            ));
                        }
                    }
                    "true_false" => {
                        if !answer.is_boolean() {
                            return Err(UnifiedError::bad_request(
                                "A true/false question takes true or false",
                            ));
                        }
                    }
                    "short_answer" | "essay" => {
                        if let Some(text) = answer.as_str() {
                            if text.len() > MAX_TEXT_LEN {
                                return Err(UnifiedError::bad_request(
                                    "An answer must be at most 20000 characters",
                                ));
                            }
                        } else {
                            return Err(UnifiedError::bad_request(
                                "A text question takes a text answer",
                            ));
                        }
                    }
                    _ => {}
                }
            }
            merged.insert(question_id.clone(), answer.clone());
        }
    }
    Ok(Value::Object(merged))
}

async fn autosave_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AutosaveInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        expire_if_due(&state, path.as_str());
        let mut attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.user_id != caller.actor {
            return Err(UnifiedError::not_found("Attempt not found"));
        }
        if attempt.status != ATTEMPT_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and can no longer be autosaved",
                attempt.status
            )));
        }
        attempt.answers = validate_answers(&state, &attempt, &body.answers)?;
        attempt.autosaved_at = Some(now());
        state.attempts.insert(attempt.id.clone(), attempt.clone());
        state.emit(
            EVT_ATTEMPT_AUTOSAVED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "autosaved_at": attempt.autosaved_at,
            }),
        );
        Ok(state.attempt_view(&attempt))
    })();
    json::respond(result, debug)
}

async fn submit_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AutosaveInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        expire_if_due(&state, path.as_str());
        let mut attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.user_id != caller.actor {
            return Err(UnifiedError::not_found("Attempt not found"));
        }
        if attempt.status != ATTEMPT_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and cannot be submitted",
                attempt.status
            )));
        }
        attempt.answers = validate_answers(&state, &attempt, &body.answers)?;
        attempt.submitted_at = Some(now());
        attempt.status = ATTEMPT_SUBMITTED.to_string();
        let (objective_score, needs_manual) = auto_grade_attempt(&state, &mut attempt);
        state.attempts.insert(attempt.id.clone(), attempt.clone());
        state.emit(
            EVT_ATTEMPT_SUBMITTED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "objective_score": objective_score,
                "needs_manual_grading": needs_manual,
            }),
        );
        if needs_manual {
            Ok(json!({
                "attempt": state.attempt_view(&attempt),
                "grading": "pending_manual",
                "objective_score": objective_score,
            }))
        } else {
            let result = finalize_grading(&state, &attempt.id)?;
            let attempt = state
                .attempts
                .get(&attempt.id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::internal("Attempt was not stored"))?;
            Ok(json!({
                "attempt": state.attempt_view(&attempt),
                "grading": "complete",
                "result": result,
            }))
        }
    })();
    json::respond(result, debug)
}

async fn expire_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.status != ATTEMPT_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and cannot be expired",
                attempt.status
            )));
        }
        if let Some(mut attempt) = state.attempts.get_mut(path.as_str()) {
            attempt.status = ATTEMPT_EXPIRED.to_string();
            attempt.submitted_at = Some(now());
        }
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::internal("Attempt was not stored"))?;
        state.emit(
            EVT_ATTEMPT_EXPIRED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "expired_by": caller.actor,
            }),
        );
        Ok(state.attempt_view(&attempt))
    })();
    json::respond(result, debug)
}

async fn record_proctor_signal(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ProctorSignalInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProctorSignal> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        let settings = state.settings();
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.user_id != caller.actor {
            return Err(UnifiedError::not_found("Attempt not found"));
        }
        if attempt.status != ATTEMPT_IN_PROGRESS {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and accepts no more proctoring signals",
                attempt.status
            )));
        }
        let signal_type =
            require_in_list(&body.signal_type, "signal_type", &settings.proctor_signal_types)?;
        let detail = optional_text(&body.detail, "detail", MAX_COMMENT_LEN)?;
        let signal = ProctorSignal {
            id: new_id(),
            attempt_id: attempt.id.clone(),
            exam_id: attempt.exam_id.clone(),
            user_id: attempt.user_id.clone(),
            signal_type,
            detail,
            recorded_at: now(),
        };
        state
            .proctor_signals
            .insert(signal.id.clone(), signal.clone());
        // A signal is a record only: it never auto-fails, auto-expires,
        // or auto-flags an attempt. A human reviews the log.
        state.emit(
            EVT_PROCTOR_SIGNAL,
            json!({
                "signal_id": signal.id,
                "attempt_id": signal.attempt_id,
                "exam_id": signal.exam_id,
                "user_id": signal.user_id,
                "signal_type": signal.signal_type,
            }),
        );
        Ok(signal)
    })();
    json::respond(result, debug)
}

async fn list_proctor_signals(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ProctorSignal>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        visible_attempt(&state, &ctx, &caller, &attempt)?;
        let params = query.into_inner();
        let mut items: Vec<ProctorSignal> = state
            .proctor_signals
            .iter()
            .filter(|entry| {
                entry.value().attempt_id == attempt.id
                    && (params
                        .filters
                        .get("signal_type")
                        .map(|v| &entry.value().signal_type == v)
                        .unwrap_or(true))
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.recorded_at.cmp(&a.recorded_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: rubrics and manual grading
// ---------------------------------------------------------------------------

fn validate_rubric_criteria(
    criteria: &[RubricCriterionInput],
) -> UnifiedResult<Vec<RubricCriterion>> {
    if criteria.is_empty() {
        return Err(UnifiedError::bad_request(
            "A rubric needs at least one criterion",
        ));
    }
    if criteria.len() > MAX_CRITERIA_PER_RUBRIC {
        return Err(UnifiedError::bad_request(format!(
            "A rubric may hold at most {MAX_CRITERIA_PER_RUBRIC} criteria"
        )));
    }
    let mut out = Vec::with_capacity(criteria.len());
    for criterion in criteria {
        let title = require_text(&criterion.title, "criterion title", MAX_TITLE_LEN)?;
        let description = criterion
            .description
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(UnifiedError::bad_request(
                "criterion description must be at most 5000 characters",
            ));
        }
        let max_marks = require_marks(criterion.max_marks, "criterion max_marks")?;
        out.push(RubricCriterion {
            id: criterion.id.clone().unwrap_or_else(new_id),
            title,
            description,
            max_marks,
        });
    }
    Ok(out)
}

async fn list_rubrics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Rubric>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let params = query.into_inner();
        let mut items: Vec<Rubric> = state
            .rubrics
            .iter()
            .filter(|entry| {
                entry.value().exam_id == exam.id
                    && matches_search(&entry.value().title, &params.search)
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn create_rubric(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateRubricInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Rubric> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let title = require_text(&body.title, "title", MAX_TITLE_LEN)?;
        let description = body
            .description
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        if description.len() > MAX_DESCRIPTION_LEN {
            return Err(UnifiedError::bad_request(
                "description must be at most 5000 characters",
            ));
        }
        let criteria = validate_rubric_criteria(&body.criteria)?;
        let rubric = Rubric {
            id: new_id(),
            exam_id: exam.id,
            title,
            description,
            criteria,
            created_by: caller.actor,
            created_at: now(),
            updated_at: now(),
        };
        state.rubrics.insert(rubric.id.clone(), rubric.clone());
        Ok(rubric)
    })();
    json::respond(result, debug)
}

async fn get_rubric(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Rubric> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        state
            .rubrics
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Rubric not found"))
    })();
    json::respond(result, debug)
}

async fn update_rubric(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateRubricInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Rubric> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let mut rubric = state
            .rubrics
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Rubric not found"))?;
        if state.grades.iter().any(|entry| {
            entry.value().rubric_id.as_deref() == Some(rubric.id.as_str())
                && entry.value().status == GRADE_GRADED
        }) {
            return Err(UnifiedError::conflict(
                "Rubric already has graded answers; create a new rubric instead",
            ));
        }
        if let Some(title) = &body.title {
            rubric.title = require_text(title, "title", MAX_TITLE_LEN)?;
        }
        if let Some(description) = &body.description {
            if description.len() > MAX_DESCRIPTION_LEN {
                return Err(UnifiedError::bad_request(
                    "description must be at most 5000 characters",
                ));
            }
            rubric.description = description.trim().to_string();
        }
        if let Some(criteria) = &body.criteria {
            rubric.criteria = validate_rubric_criteria(criteria)?;
        }
        rubric.updated_at = now();
        state.rubrics.insert(rubric.id.clone(), rubric.clone());
        Ok(rubric)
    })();
    json::respond(result, debug)
}

async fn delete_rubric(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let rubric = state
            .rubrics
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Rubric not found"))?;
        if state
            .grades
            .iter()
            .any(|entry| entry.value().rubric_id.as_deref() == Some(rubric.id.as_str()))
        {
            return Err(UnifiedError::conflict(
                "Rubric is referenced by manual grades and cannot be deleted",
            ));
        }
        state.rubrics.remove(&rubric.id);
        Ok(json!({ "deleted": true, "id": rubric.id }))
    })();
    json::respond(result, debug)
}

async fn list_grading_queue(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ExamGrade>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let params = query.into_inner();
        let mut items: Vec<ExamGrade> = state
            .grades
            .iter()
            .filter(|entry| {
                let grade = entry.value();
                grade.status == GRADE_PENDING
                    || grade.status == GRADE_DISPUTED
                    || grade.status == GRADE_SECOND_GRADING
                    || grade.grader_id.as_deref() == Some(caller.actor.as_str())
                        && grade.status == GRADE_GRADED
            })
            .filter(|entry| {
                let grade = entry.value();
                params
                    .filters
                    .get("exam_id")
                    .map(|v| &grade.exam_id == v)
                    .unwrap_or(true)
                    && params
                        .filters
                        .get("attempt_id")
                        .map(|v| &grade.attempt_id == v)
                        .unwrap_or(true)
                    && params
                        .filters
                        .get("status")
                        .map(|v| &grade.status == v)
                        .unwrap_or(true)
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_attempt_grading(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.status != ATTEMPT_SUBMITTED && attempt.status != ATTEMPT_GRADED {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and is not awaiting grading",
                attempt.status
            )));
        }
        let grades: Vec<ExamGrade> = state
            .grades
            .iter()
            .filter(|entry| entry.value().attempt_id == attempt.id)
            .map(|entry| entry.value().clone())
            .collect();
        let mut essay_answers: Vec<Value> = Vec::new();
        for grade in &grades {
            let question = state
                .questions
                .get(&grade.question_id)
                .map(|entry| entry.value().clone());
            let answer = attempt.answers.get(&grade.question_id).cloned();
            essay_answers.push(json!({
                "question_id": grade.question_id,
                "question": question.map(|q| json!({
                    "text": q.text,
                    "marks": q.marks,
                    "answer_key": q.answer_key,
                })),
                "answer": answer,
                "grade": grade,
            }));
        }
        let rubrics: Vec<Rubric> = state
            .rubrics
            .iter()
            .filter(|entry| entry.value().exam_id == attempt.exam_id)
            .map(|entry| entry.value().clone())
            .collect();
        Ok(json!({
            "attempt": state.attempt_view(&attempt),
            "essay_answers": essay_answers,
            "rubrics": rubrics,
        }))
    })();
    json::respond(result, debug)
}

async fn grade_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<GradeAttemptInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let input = body.into_inner();
        let grader = input.grader_id.clone().unwrap_or_else(|| caller.actor.clone());
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.status != ATTEMPT_SUBMITTED {
            return Err(UnifiedError::conflict(format!(
                "Attempt is {} and cannot be graded now",
                attempt.status
            )));
        }
        if input.grades.is_empty() {
            return Err(UnifiedError::bad_request("At least one grade is required"));
        }
        for item in &input.grades {
            let grade_id = state
                .grades
                .iter()
                .find(|entry| {
                    entry.value().attempt_id == attempt.id
                        && entry.value().question_id == item.question_id
                })
                .map(|entry| entry.key().clone())
                .ok_or_else(|| UnifiedError::bad_request(format!(
                    "Question {} has no manual grade waiting for this attempt",
                    item.question_id
                )))?;
            let mut grade = state
                .grades
                .get(&grade_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
            if grade.status != GRADE_PENDING && grade.status != GRADE_SECOND_GRADING {
                return Err(UnifiedError::conflict(format!(
                    "Grade for question {} is {} and cannot be graded again; use regrade",
                    item.question_id, grade.status
                )));
            }
            let marks = require_marks(item.marks_awarded, "marks_awarded")?;
            if marks > grade.max_marks {
                return Err(UnifiedError::bad_request(
                    "marks_awarded must not exceed the question's maximum marks",
                ));
            }
            if let Some(rubric_id) = &item.rubric_id {
                let rubric = state
                    .rubrics
                    .get(rubric_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::bad_request("Rubric not found"))?;
                if rubric.exam_id != attempt.exam_id {
                    return Err(UnifiedError::bad_request(
                        "Rubric belongs to a different exam",
                    ));
                }
                grade.rubric_id = Some(rubric.id.clone());
                let mut criterion_scores: HashMap<String, f64> = HashMap::new();
                let supplied = item.criterion_scores.clone().unwrap_or_default();
                let mut rubric_total = 0.0;
                for criterion in &rubric.criteria {
                    let scored = supplied.get(&criterion.id).copied().unwrap_or(0.0);
                    if !(0.0..=criterion.max_marks).contains(&scored) {
                        return Err(UnifiedError::bad_request(format!(
                            "Criterion \"{}\" score must be between 0 and {}",
                            criterion.title, criterion.max_marks
                        )));
                    }
                    rubric_total += scored;
                    criterion_scores.insert(criterion.id.clone(), scored);
                }
                if (rubric_total - marks).abs() > 0.001 {
                    return Err(UnifiedError::bad_request(
                        "marks_awarded must equal the sum of the rubric criterion scores",
                    ));
                }
                grade.criterion_scores = criterion_scores;
            }
            grade.marks_awarded = Some(marks);
            grade.grader_id = Some(grader.clone());
            grade.status = GRADE_GRADED.to_string();
            grade.comment = optional_text(&item.comment, "comment", MAX_COMMENT_LEN)?;
            grade.audit.push(AuditEntry {
                at: now(),
                actor: caller.actor.clone(),
                action: "manual_grade_submitted".to_string(),
                detail: format!("Awarded {marks} of {} marks", grade.max_marks),
            });
            grade.updated_at = now();
            state.grades.insert(grade.id.clone(), grade);
        }
        let pending = state.grades.iter().any(|entry| {
            entry.value().attempt_id == attempt.id
                && (entry.value().status == GRADE_PENDING
                    || entry.value().status == GRADE_SECOND_GRADING)
        });
        if pending {
            let attempt = state
                .attempts
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::internal("Attempt was not stored"))?;
            Ok(json!({
                "attempt": state.attempt_view(&attempt),
                "grading": "partial",
            }))
        } else {
            let result = finalize_grading(&state, &attempt.id)?;
            let attempt = state
                .attempts
                .get(path.as_str())
                .map(|entry| entry.value().clone())
                .ok_or_else(|| UnifiedError::internal("Attempt was not stored"))?;
            Ok(json!({
                "attempt": state.attempt_view(&attempt),
                "grading": "complete",
                "result": result,
            }))
        }
    })();
    json::respond(result, debug)
}

async fn regrade_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.status != ATTEMPT_GRADED {
            return Err(UnifiedError::conflict(
                "Only a graded attempt can be reopened for regrading",
            ));
        }
        let grade_ids: Vec<String> = state
            .grades
            .iter()
            .filter(|entry| entry.value().attempt_id == attempt.id)
            .map(|entry| entry.key().clone())
            .collect();
        for grade_id in grade_ids {
            if let Some(mut grade) = state.grades.get_mut(&grade_id) {
                grade.status = GRADE_PENDING.to_string();
                grade.marks_awarded = None;
                grade.audit.push(AuditEntry {
                    at: now(),
                    actor: caller.actor.clone(),
                    action: "regrade_opened".to_string(),
                    detail: "Manual grade reopened for regrading".to_string(),
                });
                grade.updated_at = now();
            }
        }
        if let Some(mut attempt) = state.attempts.get_mut(path.as_str()) {
            attempt.status = ATTEMPT_SUBMITTED.to_string();
            attempt.score = None;
            attempt.passed = None;
            attempt.graded_at = None;
            if let Some(result_id) = attempt.result_id.take() {
                state.results.remove(&result_id);
            }
        }
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::internal("Attempt was not stored"))?;
        state.emit(
            EVT_ATTEMPT_GRADED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "regrade": true,
                "actor": caller.actor,
            }),
        );
        Ok(json!({
            "attempt": state.attempt_view(&attempt),
            "grading": "reopened",
        }))
    })();
    json::respond(result, debug)
}

async fn override_attempt_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<OverrideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let reason = require_text(&body.reason, "reason", MAX_COMMENT_LEN)?;
        let attempt = state
            .attempts
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        if attempt.status != ATTEMPT_GRADED {
            return Err(UnifiedError::conflict(
                "Only a graded attempt can have its score overridden",
            ));
        }
        let result_id = attempt
            .result_id
            .clone()
            .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
        let mut result = state
            .results
            .get(&result_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
        let new_score = require_marks(body.score, "score")?;
        if new_score > result.max_score {
            return Err(UnifiedError::bad_request(
                "score must not exceed the exam's maximum score",
            ));
        }
        let original_score = result.score;
        let original_passed = result.passed;
        result.score = new_score;
        result.percentage = if result.max_score > 0.0 {
            new_score / result.max_score * 100.0
        } else {
            0.0
        };
        let exam = state
            .exams
            .get(&attempt.exam_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        result.passed = result.percentage >= exam.pass_mark;
        result.updated_at = now();
        state.results.insert(result.id.clone(), result.clone());
        if let Some(mut attempt) = state.attempts.get_mut(path.as_str()) {
            attempt.score = Some(new_score);
            attempt.passed = Some(result.passed);
        }
        state.emit(
            EVT_ATTEMPT_GRADED,
            json!({
                "attempt_id": attempt.id,
                "exam_id": attempt.exam_id,
                "user_id": attempt.user_id,
                "score_override": true,
                "original_score": original_score,
                "original_passed": original_passed,
                "new_score": new_score,
                "passed": result.passed,
                "reason": reason,
                "actor": caller.actor,
                "result_id": result.id,
            }),
        );
        Ok(result)
    })();
    json::respond(result, debug)
}

async fn dispute_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<DisputeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamGrade> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let reason = require_text(&body.reason, "reason", MAX_COMMENT_LEN)?;
        let mut grade = state
            .grades
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        if grade.status != GRADE_GRADED {
            return Err(UnifiedError::conflict(
                "Only a graded manual grade can be disputed",
            ));
        }
        grade.status = GRADE_DISPUTED.to_string();
        grade.audit.push(AuditEntry {
            at: now(),
            actor: caller.actor.clone(),
            action: "grade_disputed".to_string(),
            detail: reason,
        });
        grade.updated_at = now();
        state.grades.insert(grade.id.clone(), grade.clone());
        Ok(grade)
    })();
    json::respond(result, debug)
}

async fn second_grade(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SecondGradeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let mut grade = state
            .grades
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Grade not found"))?;
        if grade.status != GRADE_DISPUTED && grade.status != GRADE_SECOND_GRADING {
            return Err(UnifiedError::conflict(
                "Only a disputed grade can receive a second grade",
            ));
        }
        if grade.grader_id.as_deref() == Some(caller.actor.as_str()) {
            return Err(UnifiedError::conflict(
                "The second grader must be different from the first grader",
            ));
        }
        let marks = require_marks(body.marks_awarded, "marks_awarded")?;
        if marks > grade.max_marks {
            return Err(UnifiedError::bad_request(
                "marks_awarded must not exceed the question's maximum marks",
            ));
        }
        if let Some(scores) = &body.criterion_scores {
            for (criterion_id, scored) in scores {
                if !(*scored).is_finite() || *scored < 0.0 {
                    return Err(UnifiedError::bad_request(format!(
                        "Criterion {criterion_id} score must be a non-negative number"
                    )));
                }
            }
            grade.criterion_scores = scores.clone();
        }
        grade.second_grader_id = Some(caller.actor.clone());
        grade.marks_awarded = Some(marks);
        grade.status = GRADE_GRADED.to_string();
        grade.comment = optional_text(&body.comment, "comment", MAX_COMMENT_LEN)?;
        grade.audit.push(AuditEntry {
            at: now(),
            actor: caller.actor.clone(),
            action: "second_grade_submitted".to_string(),
            detail: format!("Second grader awarded {marks} of {} marks", grade.max_marks),
        });
        grade.updated_at = now();
        state.grades.insert(grade.id.clone(), grade.clone());
        let attempt_id = grade.attempt_id.clone();
        let attempt = state
            .attempts
            .get(&attempt_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Attempt not found"))?;
        let pending = state.grades.iter().any(|entry| {
            entry.value().attempt_id == attempt_id
                && (entry.value().status == GRADE_PENDING
                    || entry.value().status == GRADE_SECOND_GRADING
                    || entry.value().status == GRADE_DISPUTED)
        });
        if pending {
            Ok(json!({ "grade": grade, "grading": "partial" }))
        } else if attempt.status == ATTEMPT_SUBMITTED {
            let result = finalize_grading(&state, &attempt_id)?;
            Ok(json!({ "grade": grade, "grading": "complete", "result": result }))
        } else if attempt.status == ATTEMPT_GRADED {
            // The grade was disputed after grading: drop the old
            // result, reopen the attempt, and recompute from the
            // stored answers plus the new manual grade.
            if let Some(result_id) = attempt.result_id.clone() {
                state.results.remove(&result_id);
            }
            if let Some(mut attempt) = state.attempts.get_mut(&attempt_id) {
                attempt.status = ATTEMPT_SUBMITTED.to_string();
                attempt.result_id = None;
                attempt.score = None;
                attempt.passed = None;
                attempt.graded_at = None;
            }
            let result = finalize_grading(&state, &attempt_id)?;
            Ok(json!({ "grade": grade, "grading": "complete", "result": result }))
        } else {
            Ok(json!({ "grade": grade, "grading": "partial" }))
        }
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: results and analytics
// ---------------------------------------------------------------------------

fn result_visible(
    _state: &ExamsState,
    ctx: &AppContext,
    caller: &Caller,
    result: &ExamResult,
) -> UnifiedResult<()> {
    let is_manager = check_capability(ctx, &caller.role, PERM_EXAMS_MANAGE);
    let is_grader = check_capability(ctx, &caller.role, PERM_EXAMS_GRADE);
    if is_manager || is_grader || result.user_id == caller.actor {
        return Ok(());
    }
    Err(UnifiedError::not_found("Result not found"))
}

async fn list_exam_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ExamResult>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may list all results of an exam",
            ));
        }
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let params = query.into_inner();
        let mut items: Vec<ExamResult> = state
            .results
            .iter()
            .filter(|entry| {
                let result = entry.value();
                result.exam_id == exam.id
                    && (params
                        .filters
                        .get("passed")
                        .map(|v| result.passed.to_string() == *v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &result.status == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("user_id")
                        .map(|v| &result.user_id == v)
                        .unwrap_or(true))
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.graded_at.cmp(&a.graded_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn get_result(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamResult> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let result = state
            .results
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
        result_visible(&state, &ctx, &caller, &result)?;
        Ok(result)
    })();
    json::respond(result, debug)
}

async fn list_my_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ExamResult>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<ExamResult> = state
            .results
            .iter()
            .filter(|entry| {
                let result = entry.value();
                result.user_id == caller.actor
                    && (params
                        .filters
                        .get("exam_id")
                        .map(|v| &result.exam_id == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("status")
                        .map(|v| &result.status == v)
                        .unwrap_or(true))
                    && (params
                        .filters
                        .get("passed")
                        .map(|v| result.passed.to_string() == *v)
                        .unwrap_or(true))
            })
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.graded_at.cmp(&a.graded_at));
        if params.order.as_deref() == Some("asc") {
            items.reverse();
        }
        Ok(page_of(items, &params))
    })();
    json::respond(result, debug)
}

async fn publish_exam_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let mut published = 0usize;
        let mut published_ids: Vec<String> = Vec::new();
        for entry in state.results.iter() {
            let result = entry.value();
            if result.exam_id == exam.id && result.status == RESULT_DRAFT {
                if let Some(mut result) = state.results.get_mut(&result.id) {
                    result.status = RESULT_PUBLISHED.to_string();
                    result.published_at = Some(now());
                    result.updated_at = now();
                    published_ids.push(result.id.clone());
                    published += 1;
                }
            }
        }
        state.emit(
            EVT_RESULT_PUBLISHED,
            json!({
                "exam_id": exam.id,
                "published_count": published,
                "result_ids": published_ids,
                "actor": caller.actor,
            }),
        );
        for result_id in &published_ids {
            if let Some(result) = state.results.get(result_id) {
                state.notify(
                    "exams.result_published",
                    &result.user_id,
                    "Your exam result is published",
                    &format!(
                        "Your result for exam \"{}\" is now published.",
                        exam.title
                    ),
                    Some(format!("/api/v1/exams/results/{result_id}")),
                );
            }
        }
        Ok(json!({
            "exam_id": exam.id,
            "published_count": published,
            "result_ids": published_ids,
        }))
    })();
    json::respond(result, debug)
}

async fn request_result_certificate(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let result = state
            .results
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Result not found"))?;
        result_visible(&state, &ctx, &caller, &result)?;
        if !result.passed {
            return Err(UnifiedError::conflict(
                "Certificates are requested only for passed results",
            ));
        }
        if result.status != RESULT_PUBLISHED {
            return Err(UnifiedError::conflict(
                "Result must be published before a certificate is requested",
            ));
        }
        state.emit(
            EVT_CERTIFICATE_REQUESTED,
            json!({
                "attempt_id": result.attempt_id,
                "exam_id": result.exam_id,
                "user_id": result.user_id,
                "result_id": result.id,
                "score": result.score,
                "passed": result.passed,
                "module": "exams",
                "requested_by": caller.actor,
            }),
        );
        Ok(json!({
            "result_id": result.id,
            "certificate_requested": true,
        }))
    })();
    json::respond(result, debug)
}

async fn get_exam_analytics(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may read exam analytics",
            ));
        }
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        let settings = state.settings();
        let results: Vec<ExamResult> = state
            .results
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .map(|entry| entry.value().clone())
            .collect();
        let total = results.len();
        let passed_count = results.iter().filter(|r| r.passed).count();
        let pass_rate = if total > 0 {
            passed_count as f64 / total as f64 * 100.0
        } else {
            0.0
        };
        let mean = if total > 0 {
            results.iter().map(|r| r.percentage).sum::<f64>() / total as f64
        } else {
            0.0
        };
        let mut sorted: Vec<f64> = results.iter().map(|r| r.percentage).collect();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = if sorted.is_empty() {
            0.0
        } else if sorted.len() % 2 == 1 {
            sorted[sorted.len() / 2]
        } else {
            (sorted[sorted.len() / 2 - 1] + sorted[sorted.len() / 2]) / 2.0
        };
        let buckets = settings.distribution_buckets.max(1) as usize;
        let mut distribution: Vec<u64> = vec![0; buckets];
        for result in &results {
            let index = ((result.percentage / 100.0) * buckets as f64)
                .floor()
                .min((buckets - 1) as f64) as usize;
            distribution[index] += 1;
        }
        let stats = question_statistics(&state, &exam.id);
        let attempts_total = state
            .attempts
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .count();
        let signals_total = state
            .proctor_signals
            .iter()
            .filter(|entry| entry.value().exam_id == exam.id)
            .count();
        Ok(json!({
            "exam_id": exam.id,
            "attempts_total": attempts_total,
            "results_total": total,
            "passed_count": passed_count,
            "pass_rate": pass_rate,
            "mean_percentage": mean,
            "median_percentage": median,
            "score_distribution": {
                "buckets": buckets,
                "counts": distribution,
            },
            "proctor_signals_total": signals_total,
            "question_statistics": stats,
        }))
    })();
    json::respond(result, debug)
}

async fn get_question_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may read question statistics",
            ));
        }
        let exam = state
            .exams
            .get(path.as_str())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        Ok(question_statistics(&state, &exam.id))
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: AI (smart module)
// ---------------------------------------------------------------------------

async fn ai_draft_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<AiDraftQuestionsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = async {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let topic = require_text(&input.topic, "topic", MAX_TITLE_LEN)?;
        let count = input.count.unwrap_or(5).clamp(1, 50);
        let difficulty = match &input.difficulty {
            Some(raw) => require_in_list(raw, "difficulty", &settings.difficulty_levels)?,
            None => "medium".to_string(),
        };
        let question_type = match &input.question_type {
            Some(raw) => require_in_list(raw, "question_type", &settings.question_types)?,
            None => "multiple_choice_single".to_string(),
        };
        let category = match &input.category {
            Some(raw) => require_in_list(raw, "category", &settings.categories)?,
            None => "general".to_string(),
        };
        let bank_id = match &input.bank_id {
            Some(raw) => {
                let id = require_text(raw, "bank_id", MAX_REF_LEN)?;
                if !state.banks.contains_key(&id) {
                    return Err(UnifiedError::bad_request("Question bank not found"));
                }
                Some(id)
            }
            None => None,
        };
        let ai_input = json!({
            "topic": topic,
            "count": count,
            "difficulty": difficulty,
            "question_type": question_type,
            "category": category,
        });
        // The AI module's Caller is the type `run_ai_capability`
        // takes; it carries the same actor and role as this
        // module's caller. Company resolution goes through the AI
        // module's helper (header first, then the user hook).
        let ai_caller = crate::modules::ai::Caller {
            actor: caller.actor.clone(),
            role: caller.role.clone(),
        };
        let company_id = crate::modules::ai::resolve_caller_company(&ctx, &req, &ai_caller);
        let output = run_ai_capability(
            &ctx,
            &ai_caller,
            company_id,
            "exams",
            "draft_questions",
            ai_input,
        )
        .await?;
        // AI-drafted questions are stored as DRAFT records only. A
        // human must review and publish them; they are never
        // auto-published.
        let mut stored: Vec<String> = Vec::new();
        if let Some(bank_id) = bank_id {
            if let Some(drafts) = output.get("questions").and_then(|v| v.as_array()) {
                for draft in drafts {
                    let text = match draft.get("text").and_then(|v| v.as_str()) {
                        Some(text) => text.to_string(),
                        None => continue,
                    };
                    let draft_input = CreateQuestionInput {
                        question_type: draft
                            .get("question_type")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&question_type)
                            .to_string(),
                        category: draft
                            .get("category")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&category)
                            .to_string(),
                        difficulty: draft
                            .get("difficulty")
                            .and_then(|v| v.as_str())
                            .unwrap_or(&difficulty)
                            .to_string(),
                        text,
                        options: draft
                            .get("options")
                            .and_then(|v| serde_json::from_value(v.clone()).ok()),
                        correct_answer: draft.get("correct_answer").cloned(),
                        answer_key: draft
                            .get("answer_key")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        marks: draft.get("marks").and_then(|v| v.as_f64()),
                        status: Some("draft".to_string()),
                    };
                    if let Ok(question) =
                        build_question(&state, &settings, &bank_id, &caller.actor, draft_input)
                    {
                        let id = question.id.clone();
                        state.questions.insert(id.clone(), question);
                        stored.push(id);
                    }
                }
            }
        }
        Ok(json!({
            "ai_output": output,
            "stored_draft_question_ids": stored,
            "auto_published": false,
        }))
    }
    .await;
    json::respond(result, debug)
}

async fn ai_analyse_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<AiAnalyseResultsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = async {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may analyse exam results",
            ));
        }
        let exam_id = require_text(&body.exam_id, "exam_id", MAX_REF_LEN)?;
        let exam = state
            .exams
            .get(&exam_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
        // Only anonymised statistics are sent to the AI helper -
        // never raw student answers or student identities.
        let stats = question_statistics(&state, &exam.id);
        let ai_input = json!({
            "exam_id": exam.id,
            "statistics": stats,
        });
        let ai_caller = crate::modules::ai::Caller {
            actor: caller.actor.clone(),
            role: caller.role.clone(),
        };
        let company_id = crate::modules::ai::resolve_caller_company(&ctx, &req, &ai_caller);
        let output = run_ai_capability(
            &ctx,
            &ai_caller,
            company_id,
            "exams",
            "analyse_results",
            ai_input,
        )
        .await?;
        Ok(json!({
            "exam_id": exam.id,
            "ai_output": output,
        }))
    }
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: settings
// ---------------------------------------------------------------------------

async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamsSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        Ok(state.settings())
    })();
    json::respond(result, debug)
}

async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<UpdateExamsSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ExamsSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let input = body.into_inner();
        let mut settings = state.settings();
        if let Some(values) = input.question_types {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "question_types must not be empty",
                ));
            }
            settings.question_types = values;
        }
        if let Some(values) = input.difficulty_levels {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "difficulty_levels must not be empty",
                ));
            }
            settings.difficulty_levels = values;
        }
        if let Some(values) = input.categories {
            if values.is_empty() {
                return Err(UnifiedError::bad_request("categories must not be empty"));
            }
            settings.categories = values;
        }
        if let Some(values) = input.proctor_signal_types {
            if values.is_empty() {
                return Err(UnifiedError::bad_request(
                    "proctor_signal_types must not be empty",
                ));
            }
            settings.proctor_signal_types = values;
        }
        if let Some(value) = input.default_pass_mark {
            settings.default_pass_mark = require_percentage(value, "default_pass_mark")?;
        }
        if let Some(value) = input.default_duration_minutes {
            if value < 1 || value > 10080 {
                return Err(UnifiedError::bad_request(
                    "default_duration_minutes must be between 1 and 10080",
                ));
            }
            settings.default_duration_minutes = value;
        }
        if let Some(value) = input.default_attempts_allowed {
            settings.default_attempts_allowed = value.max(1);
        }
        if let Some(value) = input.default_show_results {
            let checked = require_text(&value, "default_show_results", MAX_SLUG_LEN)?;
            if checked != "immediately" && checked != "after_grading" {
                return Err(UnifiedError::bad_request(
                    "default_show_results must be \"immediately\" or \"after_grading\"",
                ));
            }
            settings.default_show_results = checked;
        }
        if let Some(value) = input.distribution_buckets {
            if value < 1 || value > 100 {
                return Err(UnifiedError::bad_request(
                    "distribution_buckets must be between 1 and 100",
                ));
            }
            settings.distribution_buckets = value;
        }
        if let Some(value) = input.short_answer_case_insensitive {
            settings.short_answer_case_insensitive = value;
        }
        if let Some(value) = input.certificate_on_pass {
            settings.certificate_on_pass = value;
        }
        if let Some(value) = input.max_questions_per_exam {
            if value < 1 || value > 1000 {
                return Err(UnifiedError::bad_request(
                    "max_questions_per_exam must be between 1 and 1000",
                ));
            }
            settings.max_questions_per_exam = value;
        }
        if let Some(value) = input.max_options_per_question {
            if value < 2 || value > 20 {
                return Err(UnifiedError::bad_request(
                    "max_options_per_question must be between 2 and 20",
                ));
            }
            settings.max_options_per_question = value;
        }
        *state
            .settings
            .write()
            .expect("settings lock poisoned") = settings.clone();
        Ok(settings)
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_ai_draft_grounding(state: ExamsStateRef, ctx: &mut HookContext) {
    let topic = ctx
        .get("topic")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let mut by_category: HashMap<String, u64> = HashMap::new();
    let mut by_difficulty: HashMap<String, u64> = HashMap::new();
    let mut by_type: HashMap<String, u64> = HashMap::new();
    let mut total = 0u64;
    for entry in state.questions.iter() {
        let question = entry.value();
        if question.superseded_by.is_some() {
            continue;
        }
        if !topic.is_empty()
            && !question.text.to_lowercase().contains(&topic.to_lowercase())
            && !question.category.to_lowercase().contains(&topic.to_lowercase())
        {
            continue;
        }
        total += 1;
        *by_category.entry(question.category.clone()).or_insert(0) += 1;
        *by_difficulty
            .entry(question.difficulty.clone())
            .or_insert(0) += 1;
        *by_type
            .entry(question.question_type.clone())
            .or_insert(0) += 1;
    }
    ctx.add_result(json!({
        "exams": "draft_questions_grounding",
        "topic": topic,
        "existing_question_count": total,
        "by_category": by_category,
        "by_difficulty": by_difficulty,
        "by_question_type": by_type,
    }));
}

fn on_ai_analyse_grounding(state: ExamsStateRef, ctx: &mut HookContext) {
    let exam_id = match ctx.get("exam_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            ctx.add_result(json!({
                "exams": "skipped",
                "reason": "analyse request carries no exam_id",
            }));
            return;
        }
    };
    if !state.exams.contains_key(&exam_id) {
        ctx.add_result(json!({
            "exams": "skipped",
            "reason": "no exam matches this exam_id",
        }));
        return;
    }
    // Anonymised per-question statistics only - never raw student
    // answers or student identities.
    let stats = question_statistics(&state, &exam_id);
    ctx.add_result(json!({
        "exams": "analyse_results_grounding",
        "exam_id": exam_id,
        "statistics": stats,
    }));
}

fn on_course_deleted(state: ExamsStateRef, ctx: &mut HookContext) {
    let course_id = match ctx.get("course_id").and_then(|v| v.as_str()) {
        Some(id) => id.to_string(),
        None => {
            ctx.add_result(json!({
                "exams": "skipped",
                "reason": "course event carries no course_id",
            }));
            return;
        }
    };
    let mut affected = 0usize;
    for entry in state.exams.iter() {
        let exam = entry.value();
        if exam.course_id.as_deref() == Some(course_id.as_str()) {
            affected += 1;
        }
    }
    ctx.add_result(json!({
        "exams": "course_deleted_noted",
        "course_id": course_id,
        "linked_exams": affected,
        "note": "Linked exams keep their link; the course itself is never deleted here",
    }));
}

// ---------------------------------------------------------------------------
// Module implementation
// ---------------------------------------------------------------------------

impl Module for ExamsModule {
    fn id(&self) -> &'static str {
        "exams"
    }

    fn name(&self) -> &'static str {
        "Online Exams"
    }

    fn description(&self) -> &'static str {
        "Online exams: question banks with versioned questions, timed exams with scheduled windows and seeded randomisation, server-side attempt timers with autosave and resume, auto-grading for objective questions, manual grading queues with rubrics, disputes and second graders, audited regrades and score overrides, result publication, per-question statistics and score distribution, certificate requests and academic grade flow as events only, and AI-assisted question drafting and result analysis. All lists, defaults, and limits are admin-configurable settings, never hard-coded."
    }

    fn is_core(&self) -> bool {
        false
    }

    fn requires(&self) -> &[&'static str] {
        &["elearning", "academic", "notifications", "billing", "storage", "roles"]
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["elearning", "academic", "notifications", "billing", "storage", "roles"]
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
        register_ai_capabilities("exams", AI_CAPABILITIES);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            EVT_AI_DRAFT_GROUNDING,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_draft_grounding(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVT_AI_ANALYSE_GROUNDING,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_ai_analyse_grounding(state.clone(), ctx)
            }),
        );
        let state = self.state.clone();
        bus.register(
            EVT_COURSE_DELETED,
            "exams",
            Arc::new(move |ctx: &mut HookContext| {
                on_course_deleted(state.clone(), ctx)
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/exams")
                .app_data(state)
                .route("/question-banks", web::get().to(list_banks))
                .route("/question-banks", web::post().to(create_bank))
                .route("/question-banks/{id}", web::get().to(get_bank))
                .route("/question-banks/{id}", web::patch().to(update_bank))
                .route("/question-banks/{id}", web::delete().to(delete_bank))
                .route("/question-banks/{id}/questions", web::get().to(list_bank_questions))
                .route("/question-banks/{id}/questions", web::post().to(create_question))
                .route("/question-banks/{id}/questions/import", web::post().to(import_questions))
                .route("/question-banks/{id}/questions/export", web::get().to(export_questions))
                .route("/questions/{id}", web::get().to(get_question))
                .route("/questions/{id}", web::patch().to(update_question))
                .route("/questions/{id}", web::delete().to(delete_question))
                .route("/questions/{id}/versions", web::get().to(list_question_versions))
                .route("/exams", web::get().to(list_exams))
                .route("/exams", web::post().to(create_exam))
                .route("/exams/{id}", web::get().to(get_exam))
                .route("/exams/{id}", web::patch().to(update_exam))
                .route("/exams/{id}", web::delete().to(delete_exam))
                .route("/exams/{id}/publish", web::post().to(publish_exam))
                .route("/exams/{id}/archive", web::post().to(archive_exam))
                .route("/exams/{id}/close", web::post().to(close_exam))
                .route("/exams/{id}/duplicate", web::post().to(duplicate_exam))
                .route("/exams/{id}/questions", web::get().to(list_exam_questions))
                .route("/exams/{id}/questions", web::post().to(attach_exam_question))
                .route("/exams/{id}/questions/{question_id}", web::delete().to(detach_exam_question))
                .route("/exams/{id}/attempts", web::post().to(start_attempt))
                .route("/attempts", web::get().to(list_attempts))
                .route("/attempts/{id}", web::get().to(get_attempt))
                .route("/my/attempts", web::get().to(list_my_attempts))
                .route("/attempts/{id}/heartbeat", web::post().to(heartbeat_attempt))
                .route("/attempts/{id}/autosave", web::post().to(autosave_attempt))
                .route("/attempts/{id}/submit", web::post().to(submit_attempt))
                .route("/attempts/{id}/expire", web::post().to(expire_attempt))
                .route("/attempts/{id}/proctor-signal", web::post().to(record_proctor_signal))
                .route("/attempts/{id}/proctor-signals", web::get().to(list_proctor_signals))
                .route("/exams/{id}/rubrics", web::get().to(list_rubrics))
                .route("/exams/{id}/rubrics", web::post().to(create_rubric))
                .route("/rubrics/{id}", web::get().to(get_rubric))
                .route("/rubrics/{id}", web::patch().to(update_rubric))
                .route("/rubrics/{id}", web::delete().to(delete_rubric))
                .route("/grading-queue", web::get().to(list_grading_queue))
                .route("/attempts/{id}/grading", web::get().to(get_attempt_grading))
                .route("/attempts/{id}/grade", web::post().to(grade_attempt))
                .route("/attempts/{id}/regrade", web::post().to(regrade_attempt))
                .route("/attempts/{id}/override", web::post().to(override_attempt_score))
                .route("/grades/{id}/dispute", web::post().to(dispute_grade))
                .route("/grades/{id}/second-grade", web::post().to(second_grade))
                .route("/exams/{id}/results", web::get().to(list_exam_results))
                .route("/results/{id}", web::get().to(get_result))
                .route("/my/results", web::get().to(list_my_results))
                .route("/exams/{id}/results/publish", web::post().to(publish_exam_results))
                .route("/results/{id}/certificate", web::post().to(request_result_certificate))
                .route("/exams/{id}/analytics", web::get().to(get_exam_analytics))
                .route("/exams/{id}/question-stats", web::get().to(get_question_stats))
                .route("/ai/draft-questions", web::post().to(ai_draft_questions))
                .route("/ai/analyse-results", web::post().to(ai_analyse_results))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks", owner: "exams", description: "List question banks (paginated, filter by category, owner_id, company_id)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/question-banks", owner: "exams", description: "Create a question bank (manager; category validated against settings)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks/{id}", owner: "exams", description: "Get one question bank." },
            RouteInfo { method: "PATCH", path: "/api/v1/exams/question-banks/{id}", owner: "exams", description: "Update a question bank (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/exams/question-banks/{id}", owner: "exams", description: "Delete an empty question bank (manager; blocked while it holds questions)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks/{id}/questions", owner: "exams", description: "List questions of a bank (paginated, filter by type, difficulty, category, status; correct answers hidden from non-managers)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/question-banks/{id}/questions", owner: "exams", description: "Add a question to a bank (manager; type, category, difficulty validated against settings)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/question-banks/{id}/questions/import", owner: "exams", description: "Import questions from JSON (manager; drafts only; per-item errors reported)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/question-banks/{id}/questions/export", owner: "exams", description: "Export questions of a bank as JSON (correct answers included only for managers)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/questions/{id}", owner: "exams", description: "Get one question (correct answers hidden from non-managers)." },
            RouteInfo { method: "PATCH", path: "/api/v1/exams/questions/{id}", owner: "exams", description: "Update a question (manager; editing a used question creates a new version, old attempts keep their version)." },
            RouteInfo { method: "DELETE", path: "/api/v1/exams/questions/{id}", owner: "exams", description: "Delete a question (manager; blocked once attempts exist or it is attached to an exam)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/questions/{id}/versions", owner: "exams", description: "List the version history of a question (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams", owner: "exams", description: "List exams (paginated, filter by status, category, course_id, bank_id; learners see published or closed only)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams", owner: "exams", description: "Create an exam in draft (manager; duration, pass mark, attempts allowed, settings)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}", owner: "exams", description: "Get one exam (drafts only for managers)." },
            RouteInfo { method: "PATCH", path: "/api/v1/exams/exams/{id}", owner: "exams", description: "Update an exam (manager; closed or archived exams cannot be edited)." },
            RouteInfo { method: "DELETE", path: "/api/v1/exams/exams/{id}", owner: "exams", description: "Delete an exam with no attempts (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/publish", owner: "exams", description: "Publish a draft exam (manager; needs at least one question)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/archive", owner: "exams", description: "Archive a published or draft exam (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/close", owner: "exams", description: "Close a published exam (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/duplicate", owner: "exams", description: "Duplicate an exam as a new draft with its question set (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/questions", owner: "exams", description: "List the question set of an exam (paginated, ordered by position)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/questions", owner: "exams", description: "Attach a question to an exam with a pinned version (manager)." },
            RouteInfo { method: "DELETE", path: "/api/v1/exams/exams/{id}/questions/{question_id}", owner: "exams", description: "Detach a question from an exam (manager; blocked once attempts exist)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/attempts", owner: "exams", description: "Start an attempt (taker; resumes an in-progress attempt with remaining server time; seeded randomised order)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts", owner: "exams", description: "List attempts (own only unless manager or grader; filter by exam_id, user_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts/{id}", owner: "exams", description: "Get one attempt with server remaining time (own only unless manager or grader)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/my/attempts", owner: "exams", description: "The caller's attempts (paginated, filter by exam_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/heartbeat", owner: "exams", description: "Heartbeat (taker; server remaining time; the server expires overdue attempts)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/autosave", owner: "exams", description: "Autosave draft answers (taker; answers validated against the attempt's question set)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/submit", owner: "exams", description: "Submit an attempt for grading (taker; auto-grades objective answers, queues essays for manual grading)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/expire", owner: "exams", description: "Mark an in-progress attempt expired (manager)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/proctor-signal", owner: "exams", description: "Record a proctoring signal such as a tab switch (taker; recorded only, never auto-fails an attempt)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts/{id}/proctor-signals", owner: "exams", description: "List the proctoring signals of an attempt (own only unless manager or grader)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/rubrics", owner: "exams", description: "List the rubrics of an exam (paginated)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/rubrics", owner: "exams", description: "Create a rubric with criteria (manager)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/rubrics/{id}", owner: "exams", description: "Get one rubric." },
            RouteInfo { method: "PATCH", path: "/api/v1/exams/rubrics/{id}", owner: "exams", description: "Update a rubric (manager; blocked once it has graded answers)." },
            RouteInfo { method: "DELETE", path: "/api/v1/exams/rubrics/{id}", owner: "exams", description: "Delete a rubric (manager; blocked while referenced by manual grades)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/grading-queue", owner: "exams", description: "List attempts waiting for manual grading (grader; filter by exam_id, attempt_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/attempts/{id}/grading", owner: "exams", description: "The manual-grading view of one attempt (grader; essay answers plus rubrics)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/grade", owner: "exams", description: "Submit manual grades for an attempt's essays (grader; rubric criterion scores must sum to the awarded marks)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/regrade", owner: "exams", description: "Reopen a graded attempt for grading again (manager; audited)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/attempts/{id}/override", owner: "exams", description: "Override the final score of a graded attempt (manager; audited, original score kept in the event)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/grades/{id}/dispute", owner: "exams", description: "Flag a manual grade as disputed (grader)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/grades/{id}/second-grade", owner: "exams", description: "Second grader's grade for a disputed grade (grader; must differ from the first grader)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/results", owner: "exams", description: "List results of an exam (manager or grader; filter by passed, status, user_id)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/results/{id}", owner: "exams", description: "Get one result (own only unless manager or grader)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/my/results", owner: "exams", description: "The caller's results (paginated, filter by exam_id, status, passed)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/exams/{id}/results/publish", owner: "exams", description: "Publish the draft results of an exam (manager; notifies students)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/results/{id}/certificate", owner: "exams", description: "Request a certificate for a passed, published result (events only; the e-learning certificate flow issues it)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/analytics", owner: "exams", description: "Exam analytics (manager or grader; pass rate, mean, median, score distribution, question statistics, proctor signal count)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/exams/{id}/question-stats", owner: "exams", description: "Per-question statistics (manager or grader; attempted %, correct %, discrimination index)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/ai/draft-questions", owner: "exams", description: "Draft questions with AI help (manager; drafts only, never auto-published)." },
            RouteInfo { method: "POST", path: "/api/v1/exams/ai/analyse-results", owner: "exams", description: "Analyse an exam's anonymised result statistics with AI help (manager or grader)." },
            RouteInfo { method: "GET", path: "/api/v1/exams/settings", owner: "exams", description: "Get the configurable exams settings (question types, difficulties, categories, defaults)." },
            RouteInfo { method: "PUT", path: "/api/v1/exams/settings", owner: "exams", description: "Update the configurable exams settings (manager)." },
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(ExamsModule::new()));
}
