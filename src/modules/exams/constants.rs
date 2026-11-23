//! exams - constants.

use crate::modules::ai::AiCapability;


/// Maximum lengths enforced on user supplied text.
pub(crate) const MAX_TITLE_LEN: usize = 200;

pub(crate) const MAX_DESCRIPTION_LEN: usize = 5000;

pub(crate) const MAX_TEXT_LEN: usize = 20000;

pub(crate) const MAX_SLUG_LEN: usize = 64;

pub(crate) const MAX_REF_LEN: usize = 500;

pub(crate) const MAX_COMMENT_LEN: usize = 2000;

pub(crate) const MAX_QUESTIONS_PER_EXAM: usize = 200;

pub(crate) const MAX_OPTIONS_PER_QUESTION: usize = 10;

pub(crate) const MAX_ANSWERS_PER_ATTEMPT: usize = 500;

pub(crate) const MAX_CRITERIA_PER_RUBRIC: usize = 20;


/// Capability names checked through the `role.check` hook.
pub(crate) const PERM_EXAMS_VIEW: &str = "exams.view";

pub(crate) const PERM_EXAMS_MANAGE: &str = "exams.manage";

pub(crate) const PERM_EXAMS_TAKE: &str = "exams.take";

pub(crate) const PERM_EXAMS_GRADE: &str = "exams.grade";


/// Event names this module emits or listens to.
pub(crate) const EVT_QUESTION_CREATED: &str = "exams.question.created";

pub(crate) const EVT_QUESTION_UPDATED: &str = "exams.question.updated";

pub(crate) const EVT_QUESTIONS_IMPORT: &str = "exams.questions.import_requested";

pub(crate) const EVT_EXAM_CREATED: &str = "exams.exam.created";

pub(crate) const EVT_EXAM_PUBLISHED: &str = "exams.exam.published";

pub(crate) const EVT_EXAM_STARTED: &str = "exams.exam.started";

pub(crate) const EVT_EXAM_CLOSED: &str = "exams.exam.closed";

/// Emitted when an exam is archived. Distinct from
/// `EVT_EXAM_CLOSED`: listeners that tell "archived" apart from
/// "closed" subscribe to this event, not to `exams.exam.closed`.
pub(crate) const EVT_EXAM_ARCHIVED: &str = "exams.exam.archived";

pub(crate) const EVT_ATTEMPT_STARTED: &str = "exams.attempt.started";

pub(crate) const EVT_ATTEMPT_AUTOSAVED: &str = "exams.attempt.autosaved";

pub(crate) const EVT_ATTEMPT_SUBMITTED: &str = "exams.attempt.submitted";

pub(crate) const EVT_ATTEMPT_EXPIRED: &str = "exams.attempt.expired";

pub(crate) const EVT_PROCTOR_SIGNAL: &str = "exams.attempt.proctor_signal";

pub(crate) const EVT_ATTEMPT_GRADED: &str = "exams.attempt.graded";

pub(crate) const EVT_RESULT_PUBLISHED: &str = "exams.result.published";

pub(crate) const EVT_CERTIFICATE_REQUESTED: &str = "exams.certificate.requested";

pub(crate) const EVT_ACADEMIC_GRADE: &str = "academic.grade.recorded";

pub(crate) const EVT_AI_DRAFT_GROUNDING: &str = "exams.ai.draft_questions_requested";

pub(crate) const EVT_AI_ANALYSE_GROUNDING: &str = "exams.ai.analyse_results_requested";

pub(crate) const EVT_COURSE_DELETED: &str = "elearning.course.deleted";

/// Store order events this module listens to: a paid order carrying
/// an exam activates that exam for the buyer (see `on_order_paid`).
pub(crate) const ORDER_PAID: &str = "order.paid";

pub(crate) const STORE_ORDER_COMPLETED: &str = "store.order.completed";

/// Event emitted when a paid exam is activated for a user.
pub(crate) const EVT_EXAM_ACTIVATION_CREATED: &str = "exams.activation.created";


/// Paid-exam activation statuses.
pub(crate) const ACTIVATION_ACTIVE: &str = "active";

pub(crate) const ACTIVATION_REVOKED: &str = "revoked";


/// Exam lifecycle statuses.
pub(crate) const EXAM_DRAFT: &str = "draft";

pub(crate) const EXAM_PUBLISHED: &str = "published";

pub(crate) const EXAM_ARCHIVED: &str = "archived";

pub(crate) const EXAM_CLOSED: &str = "closed";


/// Attempt lifecycle statuses.
pub(crate) const ATTEMPT_IN_PROGRESS: &str = "in_progress";

pub(crate) const ATTEMPT_SUBMITTED: &str = "submitted";

pub(crate) const ATTEMPT_EXPIRED: &str = "expired";

pub(crate) const ATTEMPT_GRADED: &str = "graded";


/// Manual grade statuses.
pub(crate) const GRADE_PENDING: &str = "pending";

pub(crate) const GRADE_GRADED: &str = "graded";

pub(crate) const GRADE_DISPUTED: &str = "disputed";

pub(crate) const GRADE_SECOND_GRADING: &str = "second_grading";


/// Result publication statuses.
pub(crate) const RESULT_DRAFT: &str = "draft";

pub(crate) const RESULT_PUBLISHED: &str = "published";


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
