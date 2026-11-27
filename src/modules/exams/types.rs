//! exams - types.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use super::{constants::*, settings::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



#[derive(Debug, Clone, Serialize, Deserialize)]
// Records
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
    /// Price in integer minor units, following the store/billing
    /// `price_minor` convention (for example cents, or sen for IDR).
    /// 0 means the exam is free. Defaults to 0 when missing so a
    /// record written before paid exams existed still deserializes.
    #[serde(default)]
    pub price_minor: i64,
    /// Whether the exam is paid. A paid exam requires an active paid
    /// activation for the user before an attempt can start; a free
    /// exam needs none. Standalone: no course link is required.
    /// Defaults to false when missing, matching the pre-243 free
    /// exam records.
    #[serde(default)]
    pub is_paid: bool,
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


/// A paid-exam activation: proof that a user paid for an exam.
/// Created when an order containing the exam is paid (see
/// `on_order_paid`); an active activation unlocks attempts on a paid
/// exam. One active activation per (exam, user); re-paying while one
/// is active returns the existing record instead of duplicating it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExamActivation {
    pub id: String,
    pub exam_id: String,
    pub user_id: String,
    pub order_id: Option<String>,
    pub status: String,
    pub activated_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
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
    pub activations: Arc<DashMap<String, ExamActivation>>,
    /// Per-(exam, user) locks serialising `activate_exam`. The
    /// find-then-insert there must be atomic: without the lock, two
    /// concurrent deliveries of the same paid order could both pass
    /// the lookup and insert two active rows. Lock entries are never
    /// removed, so concurrent callers for one pair always meet the
    /// same lock, while unrelated pairs proceed in parallel.
    pub activation_locks: Arc<DashMap<(String, String), Arc<Mutex<()>>>>,
    pub settings: Arc<RwLock<ExamsSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}


pub type ExamsStateRef = Arc<ExamsState>;


impl ExamsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> ExamsSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    /// Whether the user holds an active paid activation for the exam.
    pub(crate) fn has_active_activation(&self, exam_id: &str, user_id: &str) -> bool {
        self.activations.iter().any(|entry| {
            let activation = entry.value();
            activation.exam_id == exam_id
                && activation.user_id == user_id
                && activation.status == ACTIVATION_ACTIVE
        })
    }

    /// Create an active activation for (exam, user), or return the
    /// existing active one when the user already holds one (paying
    /// twice never creates a duplicate activation). Returns the
    /// activation plus whether it was newly created: callers that
    /// emit `exams.activation.created` must only do so when
    /// `is_new` is true, so a re-delivered order event is never
    /// counted twice downstream.
    ///
    /// The check-and-insert runs under the per-(exam, user) lock,
    /// so two concurrent deliveries cannot both pass the lookup
    /// and insert two active rows.
    pub(crate) fn activate_exam(
        &self,
        exam_id: &str,
        user_id: &str,
        order_id: Option<String>,
    ) -> (ExamActivation, bool) {
        let key = (exam_id.to_string(), user_id.to_string());
        let lock = {
            let entry = self
                .activation_locks
                .entry(key)
                .or_insert_with(|| Arc::new(Mutex::new(())));
            Arc::clone(&entry)
        };
        let _guard = lock.lock().expect("activation lock poisoned");
        if let Some(existing) = self.activations.iter().find(|entry| {
            let activation = entry.value();
            activation.exam_id == exam_id
                && activation.user_id == user_id
                && activation.status == ACTIVATION_ACTIVE
        }) {
            return (existing.value().clone(), false);
        }
        let activation = ExamActivation {
            id: new_id(),
            exam_id: exam_id.to_string(),
            user_id: user_id.to_string(),
            order_id,
            status: ACTIVATION_ACTIVE.to_string(),
            activated_at: now(),
            revoked_at: None,
        };
        self.activations
            .insert(activation.id.clone(), activation.clone());
        (activation, true)
    }

    /// Pay-before-attempt gate: a paid exam requires an active paid
    /// activation for the user; a free exam needs none. Rejected with
    /// the standard error shape (`UnifiedError::forbidden` - the
    /// error type has no payment-required constructor).
    pub(crate) fn require_paid_activation(
        &self,
        exam: &Exam,
        user_id: &str,
    ) -> UnifiedResult<()> {
        if !exam.is_paid {
            return Ok(());
        }
        if self.has_active_activation(&exam.id, user_id) {
            return Ok(());
        }
        Err(UnifiedError::forbidden(
            "This exam is paid. Complete the purchase to start an attempt.",
        )
        .with_metadata("exam_id", Value::String(exam.id.clone())))
    }

    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
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

    pub(crate) fn attempt_view(&self, attempt: &ExamAttempt) -> AttemptView {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_exam(price_minor: i64, is_paid: bool) -> Exam {
        Exam {
            id: "exam-1".to_string(),
            title: "Certification".to_string(),
            description: String::new(),
            category: "general".to_string(),
            bank_ids: Vec::new(),
            course_id: None,
            academic_enrollment_id: None,
            academic_component_id: None,
            status: "published".to_string(),
            opens_at: None,
            closes_at: None,
            duration_minutes: 60,
            pass_mark: 60.0,
            attempts_allowed: 3,
            price_minor,
            is_paid,
            settings: ExamSettings::default(),
            total_marks: 100.0,
            created_by: "admin".to_string(),
            company_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn free_exam_needs_no_activation() {
        let state = ExamsState::default();
        let exam = test_exam(0, false);
        assert!(state.require_paid_activation(&exam, "user-1").is_ok());
    }

    #[test]
    fn paid_exam_blocks_attempt_without_activation() {
        let state = ExamsState::default();
        let exam = test_exam(50_000, true);
        let error = state
            .require_paid_activation(&exam, "user-1")
            .expect_err("paid exam without activation must be rejected");
        assert_eq!(error.code, 403);
        assert!(error.message.contains("paid"));
    }

    #[test]
    fn paid_exam_allows_attempt_with_activation() {
        let state = ExamsState::default();
        let exam = test_exam(50_000, true);
        let (_, is_new) = state.activate_exam(&exam.id, "user-1", Some("order-1".to_string()));
        assert!(is_new);
        assert!(state.require_paid_activation(&exam, "user-1").is_ok());
    }

    #[test]
    fn activation_is_idempotent_for_same_user_and_exam() {
        let state = ExamsState::default();
        let (first, first_is_new) =
            state.activate_exam("exam-1", "user-1", Some("order-1".to_string()));
        let (second, second_is_new) =
            state.activate_exam("exam-1", "user-1", Some("order-2".to_string()));
        assert!(first_is_new);
        assert!(!second_is_new);
        assert_eq!(first.id, second.id);
        assert_eq!(state.activations.len(), 1);
    }

    #[test]
    fn concurrent_activation_creates_a_single_row() {
        let state = Arc::new(ExamsState::default());
        let results: Vec<(ExamActivation, bool)> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        state.activate_exam("exam-1", "user-1", Some("order-1".to_string()))
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|handle| handle.join().expect("activation thread panicked"))
                .collect()
        });
        assert_eq!(state.activations.len(), 1);
        assert_eq!(
            results.iter().filter(|(_, is_new)| *is_new).count(),
            1,
            "exactly one concurrent delivery must report a new activation"
        );
        let first_id = results[0].0.id.clone();
        assert!(results
            .iter()
            .all(|(activation, _)| activation.id == first_id));
    }

    #[test]
    fn revoked_activation_does_not_unlock_attempt() {
        let state = ExamsState::default();
        let exam = test_exam(50_000, true);
        let (activation, _) = state.activate_exam(&exam.id, "user-1", None);
        if let Some(mut entry) = state.activations.get_mut(&activation.id) {
            entry.status = ACTIVATION_REVOKED.to_string();
            entry.revoked_at = Some(Utc::now());
        }
        assert!(!state.has_active_activation(&exam.id, "user-1"));
        assert!(state.require_paid_activation(&exam, "user-1").is_err());
    }

    #[test]
    fn activation_is_scoped_per_user_and_exam() {
        let state = ExamsState::default();
        let exam = test_exam(50_000, true);
        let _ = state.activate_exam(&exam.id, "user-1", None);
        assert!(state.require_paid_activation(&exam, "user-1").is_ok());
        assert!(state.require_paid_activation(&exam, "user-2").is_err());
        let other = Exam {
            id: "exam-2".to_string(),
            ..test_exam(25_000, true)
        };
        assert!(state.require_paid_activation(&other, "user-1").is_err());
    }

    fn exam_json_without_pricing_fields() -> Value {
        serde_json::json!({
            "id": "exam-1",
            "title": "Certification",
            "description": "",
            "category": "general",
            "bank_ids": [],
            "course_id": null,
            "academic_enrollment_id": null,
            "academic_component_id": null,
            "status": "published",
            "opens_at": null,
            "closes_at": null,
            "duration_minutes": 60,
            "pass_mark": 60.0,
            "attempts_allowed": 3,
            "settings": {
                "show_results": "after_grading",
                "allow_review": true,
                "calculator_allowed": false,
                "notes_allowed": false,
                "randomise_questions": true,
                "randomise_options": true
            },
            "total_marks": 100.0,
            "created_by": "admin",
            "company_id": null,
            "created_at": "2026-11-22T00:00:00Z",
            "updated_at": "2026-11-22T00:00:00Z"
        })
    }

    #[test]
    fn exam_deserializes_without_pricing_fields() {
        let exam: Exam = serde_json::from_value(exam_json_without_pricing_fields())
            .expect("a pre-243 exam record without pricing fields must deserialize");
        assert_eq!(exam.price_minor, 0);
        assert!(!exam.is_paid);
    }

    #[test]
    fn exam_deserializes_with_pricing_fields() {
        let mut value = exam_json_without_pricing_fields();
        value["price_minor"] = json!(50_000);
        value["is_paid"] = json!(true);
        let exam: Exam = serde_json::from_value(value)
            .expect("a paid exam record must deserialize");
        assert_eq!(exam.price_minor, 50_000);
        assert!(exam.is_paid);
    }
}
