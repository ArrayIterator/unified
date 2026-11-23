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
//
// Honesty, configuration, and event notes that followed the
// route list above now live at the top of `listeners.rs`.

// Split into one file per concern under `src/modules/elearning/`.
// This root only declares the submodules and re-exports the
// public API at its original paths.

pub mod constants;
pub mod settings;
pub mod types;
pub mod requests;
pub mod state;
pub mod helpers;
pub mod listeners;
pub mod courses;
pub mod modules_lessons;
pub mod enrollments;
pub mod quizzes;
pub mod certificates;
pub mod reviews;
pub mod paths;
pub mod settings_handlers;
pub mod ai;
pub mod module;

pub(crate) use self::constants::{MAX_TITLE_LEN, MAX_DESCRIPTION_LEN, MAX_TEXT_LEN, MAX_SLUG_LEN, MAX_REF_LEN, MAX_COMMENT_LEN, MAX_QUESTIONS_PER_QUIZ, MAX_OPTIONS_PER_QUESTION, PERM_ELEARNING_VIEW, PERM_ELEARNING_MANAGE, PERM_ELEARNING_INSTRUCT, ORDER_PAID, STORE_ORDER_COMPLETED, STORE_ORDER_REFUNDED, STORE_ORDER_CANCELLED, ANCHOR_CONFIRMED, ANCHOR_FAILED, STATUS_DRAFT, STATUS_PUBLISHED, STATUS_ARCHIVED, ENROLL_ACTIVE, ENROLL_PENDING_PAYMENT, ENROLL_COMPLETED, ENROLL_CANCELLED, ENROLL_SUSPENDED};
pub use self::settings::{ElearningSettings};
pub use self::types::{Course, CourseModule, Lesson, Enrollment, LessonProgress, Quiz, QuizQuestion, AttemptAnswer, QuizAttempt, QuizAttemptView, AttemptReviewItem, Certificate, Review, LearningPath, LearningPathCourse, CourseProgress, PathProgress, PathCourseProgress};
pub use self::requests::{CreateCourseRequest, UpdateCourseRequest, CreateModuleRequest, UpdateModuleRequest, CreateLessonRequest, UpdateLessonRequest, EnrollRequest, CreateQuizRequest, UpdateQuizRequest, CreateQuestionRequest, UpdateQuestionRequest, SubmitAttemptRequest, CreateReviewRequest, UpdateReviewRequest, CreatePathRequest, UpdatePathRequest, AddPathCourseRequest, UpdateSettingsRequest};
pub use self::state::{ElearningState};
pub(crate) use self::helpers::{Caller, caller, check_capability, require_permission, can_teach, require_teach, require_course_write, course_visible_to, require_course_visible, parse_uuid, require_text, optional_text, require_in_list, require_currency, ok, created, err, paginate, opt_string, valid_course_transition, load_course, load_module, load_lesson, load_enrollment, load_quiz_for_lesson, load_question, load_attempt, load_certificate, load_review, load_path, find_enrollment, ordered_lessons, lesson_available, quiz_passed, course_progress, new_certificate_code, certificate_fingerprint, issue_certificate, paths_completed_by_course, maybe_complete_course};
pub(crate) use self::listeners::{order_course_id, on_course_requested, on_order_paid, on_order_refunded_or_cancelled, on_anchor_outcome};
pub(crate) use self::courses::{list_courses, create_course, get_course, update_course, delete_course, delete_lesson_rows, transition_course, publish_course, archive_course};
pub(crate) use self::modules_lessons::{list_course_modules, create_course_module, get_module, update_module, delete_module, list_module_lessons, create_lesson, get_lesson, update_lesson, delete_lesson};
pub(crate) use self::enrollments::{enroll_course, list_enrollments, get_enrollment, cancel_enrollment, list_my_enrollments, complete_lesson, get_course_progress, get_user_course_progress, list_my_progress};
pub(crate) use self::quizzes::{get_lesson_quiz, create_lesson_quiz, update_lesson_quiz, validate_question_input, list_quiz_questions, create_quiz_question, update_question, delete_question, submit_quiz_attempt, attempt_view, list_quiz_attempts, get_quiz_attempt};
pub(crate) use self::certificates::{list_certificates, get_certificate, verify_certificate, constant_time_eq, revoke_certificate, anchor_certificate};
pub(crate) use self::reviews::{course_rating_summary, list_course_reviews, create_course_review, get_review, update_review, delete_review};
pub(crate) use self::paths::{list_paths, create_path, get_path, update_path, delete_path, add_path_course, remove_path_course, compute_path_progress, get_path_progress, list_my_paths};
pub(crate) use self::settings_handlers::{get_settings, update_settings};
pub use self::ai::{AI_CAPABILITIES};
pub(crate) use self::ai::{lesson_grounding, quiz_grounding, ai_capability};
pub use self::module::{ElearningModule, register};
