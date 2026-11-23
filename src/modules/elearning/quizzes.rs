// Elearning — Quiz handlers.

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
// Quiz handlers
// ---------------------------------------------------------------------------

/// True when the quiz time limit has run out: `now` is more
/// than `time_limit_minutes` past `started_at`. Quizzes without
/// a limit never expire.
pub(crate) fn time_limit_exceeded(quiz: &Quiz, started_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    match quiz.time_limit_minutes {
        Some(minutes) => now.signed_duration_since(started_at).num_minutes() > minutes,
        None => false,
    }
}

pub(crate) async fn get_lesson_quiz(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Quiz> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, lesson.course_id, &_scope)?;
        load_quiz_for_lesson(&state, lesson.id)
    })();
    match result {
        Ok(quiz) => ok(quiz),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_lesson_quiz(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuizRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Quiz> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
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

pub(crate) async fn update_lesson_quiz(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateQuizRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Quiz> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
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

pub(crate) fn validate_question_input(
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

pub(crate) async fn list_quiz_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, lesson.course_id, &_scope)?;
        let quiz = load_quiz_for_lesson(&state, lesson.id)?;
        let params = query.into_inner();
        // Learners taking the quiz must never see the correct
        // answers: the question list hides `correct_index` unless
        // the caller may teach this course.
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
        let may_teach = check_capability(&ctx, &caller.role, PERM_ELEARNING_MANAGE)
            || (check_capability(&ctx, &caller.role, PERM_ELEARNING_INSTRUCT)
                && course.instructor_id == caller.actor);
        // A learner fetching the questions starts the clock on a
        // timed quiz: the first fetch records started_at, later
        // fetches keep it, so the time limit is measured from the
        // first look at the questions, not from submission.
        if !may_teach && quiz.time_limit_minutes.is_some() {
            if let Some(enrollment) = find_enrollment(&state, lesson.course_id, &caller.actor) {
                if enrollment.status == ENROLL_ACTIVE {
                    state
                        .quiz_attempt_starts
                        .entry((quiz.id, caller.actor.clone()))
                        .or_insert_with(Utc::now);
                }
            }
        }
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

pub(crate) async fn create_quiz_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizQuestion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        let course = load_course(&state, &lesson.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
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

pub(crate) async fn update_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizQuestion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let mut row = load_question(&state, &path.into_inner())?;
        let quiz = state
            .quizzes
            .get(&row.quiz_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quiz not found"))?;
        let course = load_course(&state, &quiz.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
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

pub(crate) async fn delete_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        let row = load_question(&state, &path.into_inner())?;
        let quiz = state
            .quizzes
            .get(&row.quiz_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quiz not found"))?;
        let course = load_course(&state, &quiz.course_id.to_string())?;
        require_record_in_company_scope(&course.company_id, &_scope)?;
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
        Ok(json!({ "deleted": true, "id": row.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn submit_quiz_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SubmitAttemptRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizAttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let lesson = load_lesson(&state, &path.into_inner())?;
        require_course_by_id_in_scope(&state, lesson.course_id, &_scope)?;
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
            if answer.selected_index == question.correct_index {
                score += question.points;
            }
        }
        // Unanswered questions count as wrong: they add to the
        // maximum but not to the score.
        for question in &questions {
            if !seen.contains(&question.id) {
                max_score += question.points;
            }
        }
        // Enforce the time limit: the clock started when the
        // learner first fetched the questions. A learner who
        // never fetched them starts now. An expired attempt is
        // rejected and its clock cleared, so the next fetch
        // starts a fresh attempt.
        let now = Utc::now();
        let started_at = state
            .quiz_attempt_starts
            .get(&(quiz.id, caller.actor.clone()))
            .map(|entry| *entry.value())
            .unwrap_or(now);
        if time_limit_exceeded(&quiz, started_at, now) {
            state.quiz_attempt_starts.remove(&(quiz.id, caller.actor.clone()));
            return Err(UnifiedError::unprocessable(
                "Time limit exceeded for this quiz; fetch the questions again to start a new attempt",
            ));
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
            started_at: Some(started_at),
            submitted_at: now,
        };
        state.attempts.insert(attempt.id, attempt.clone());
        state.quiz_attempt_starts.remove(&(quiz.id, caller.actor.clone()));
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
        // The review is built in exactly one place
        // (`attempt_view`), so a submitted attempt reads back
        // identically to a stored one.
        Ok(attempt_view(&state, attempt))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) fn attempt_view(state: &ElearningState, attempt: QuizAttempt) -> QuizAttemptView {
    let quiz = state
        .quizzes
        .get(&attempt.quiz_id)
        .map(|entry| entry.value().clone());
    let review = match quiz {
        Some(quiz) if quiz.allow_review => {
            let mut questions: Vec<QuizQuestion> = state
                .questions
                .iter()
                .map(|entry| entry.value().clone())
                .filter(|row| row.quiz_id == quiz.id)
                .collect();
            questions.sort_by_key(|row| row.position);
            let items: Vec<AttemptReviewItem> = questions
                .iter()
                .map(|question| {
                    let answer = attempt
                        .answers
                        .iter()
                        .find(|row| row.question_id == question.id);
                    match answer {
                        Some(answer) => {
                            let correct = answer.selected_index == question.correct_index;
                            AttemptReviewItem {
                                question_id: question.id,
                                selected_index: Some(answer.selected_index),
                                correct,
                                correct_index: question.correct_index,
                                points_earned: if correct { question.points } else { 0 },
                                points: question.points,
                            }
                        }
                        // Unanswered questions are reviewed as
                        // unanswered, not as a bogus option index.
                        None => AttemptReviewItem {
                            question_id: question.id,
                            selected_index: None,
                            correct: false,
                            correct_index: question.correct_index,
                            points_earned: 0,
                            points: question.points,
                        },
                    }
                })
                .collect();
            Some(items)
        }
        _ => None,
    };
    QuizAttemptView { attempt, review }
}

pub(crate) async fn list_quiz_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
                if !quiz_course_in_scope(&state, row.quiz_id, &_scope) {
                    return false;
                }
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

pub(crate) async fn get_quiz_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ElearningState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<QuizAttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ELEARNING_VIEW)?;
        let attempt = load_attempt(&state, &path.into_inner())?;
        let quiz = state
            .quizzes
            .get(&attempt.quiz_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quiz not found"))?;
        require_course_by_id_in_scope(&state, quiz.course_id, &_scope)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Vec<String> {
        vec!["3".to_string(), "4".to_string(), "5".to_string()]
    }

    #[test]
    fn validate_question_input_accepts_valid_question() {
        let (question, checked, points) =
            validate_question_input("What is 2 + 2?", &options(), 1, None).unwrap();
        assert_eq!(question, "What is 2 + 2?");
        assert_eq!(checked.len(), 3);
        assert_eq!(points, 1);
    }

    #[test]
    fn validate_question_input_rejects_too_few_options() {
        let one = vec!["only".to_string()];
        assert!(validate_question_input("Q?", &one, 0, None).is_err());
    }

    #[test]
    fn validate_question_input_rejects_out_of_range_correct_index() {
        assert!(validate_question_input("Q?", &options(), 3, None).is_err());
        assert!(validate_question_input("Q?", &options(), 99, None).is_err());
    }

    #[test]
    fn validate_question_input_rejects_zero_points() {
        assert!(validate_question_input("Q?", &options(), 0, Some(0)).is_err());
    }

    #[test]
    fn validate_question_input_rejects_points_over_limit() {
        assert!(validate_question_input("Q?", &options(), 0, Some(1001)).is_err());
    }

    fn test_quiz(allow_review: bool, time_limit_minutes: Option<i64>) -> Quiz {
        let now = Utc::now();
        Quiz {
            id: Uuid::now_v7(),
            lesson_id: Uuid::now_v7(),
            course_id: Uuid::now_v7(),
            title: "Quiz".to_string(),
            description: None,
            pass_mark: 70,
            max_attempts: None,
            allow_review,
            time_limit_minutes,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn time_limit_exceeded_needs_a_limit_and_an_overrun() {
        let now = Utc::now();
        let unlimited = test_quiz(true, None);
        assert!(!time_limit_exceeded(&unlimited, now - chrono::Duration::hours(5), now));
        let limited = test_quiz(true, Some(30));
        assert!(!time_limit_exceeded(&limited, now - chrono::Duration::minutes(10), now));
        // Exactly at the limit is still in time; only past it expires.
        assert!(!time_limit_exceeded(&limited, now - chrono::Duration::minutes(30), now));
        assert!(time_limit_exceeded(&limited, now - chrono::Duration::minutes(31), now));
    }

    #[test]
    fn attempt_view_marks_unanswered_questions_as_null_selection() {
        let state = ElearningState::default();
        let now = Utc::now();
        let quiz = test_quiz(true, None);
        state.quizzes.insert(quiz.id, quiz.clone());
        let first = QuizQuestion {
            id: Uuid::now_v7(),
            quiz_id: quiz.id,
            question: "What is 2 + 2?".to_string(),
            options: options(),
            correct_index: 1,
            points: 2,
            position: 0,
            created_at: now,
            updated_at: now,
        };
        let second = QuizQuestion {
            id: Uuid::now_v7(),
            quiz_id: quiz.id,
            question: "What is 3 + 3?".to_string(),
            options: options(),
            correct_index: 2,
            points: 3,
            position: 1,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(first.id, first.clone());
        state.questions.insert(second.id, second.clone());
        // The learner answered only the first question.
        let attempt = QuizAttempt {
            id: Uuid::now_v7(),
            quiz_id: quiz.id,
            lesson_id: quiz.lesson_id,
            enrollment_id: Uuid::now_v7(),
            user_id: "learner-1".to_string(),
            answers: vec![AttemptAnswer {
                question_id: first.id,
                selected_index: 1,
            }],
            score: 2,
            max_score: 5,
            percent: 40,
            passed: false,
            started_at: Some(now),
            submitted_at: now,
        };
        let view = attempt_view(&state, attempt);
        let review = view.review.expect("review is present when allowed");
        assert_eq!(review.len(), 2);
        assert_eq!(review[0].question_id, first.id);
        assert_eq!(review[0].selected_index, Some(1));
        assert!(review[0].correct);
        assert_eq!(review[0].points_earned, 2);
        assert_eq!(review[1].question_id, second.id);
        assert_eq!(review[1].selected_index, None);
        assert!(!review[1].correct);
        assert_eq!(review[1].points_earned, 0);
        assert_eq!(review[1].correct_index, 2);
    }

    #[test]
    fn attempt_view_hides_review_when_not_allowed() {
        let state = ElearningState::default();
        let now = Utc::now();
        let quiz = test_quiz(false, None);
        state.quizzes.insert(quiz.id, quiz.clone());
        let attempt = QuizAttempt {
            id: Uuid::now_v7(),
            quiz_id: quiz.id,
            lesson_id: quiz.lesson_id,
            enrollment_id: Uuid::now_v7(),
            user_id: "learner-1".to_string(),
            answers: vec![],
            score: 0,
            max_score: 0,
            percent: 0,
            passed: false,
            started_at: Some(now),
            submitted_at: now,
        };
        assert!(attempt_view(&state, attempt).review.is_none());
    }
}
