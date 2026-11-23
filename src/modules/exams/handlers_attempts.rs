//! exams - handlers attempts.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use rand::{Rng, SeedableRng};
use rand::rngs::StdRng;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



// ---------------------------------------------------------------------------
// Handlers: attempts (taking an exam)
// ---------------------------------------------------------------------------
pub(crate) fn exam_window_open(exam: &Exam, at: DateTime<Utc>) -> UnifiedResult<()> {
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


pub(crate) fn visible_attempt(
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


pub(crate) async fn start_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
        // Pay-before-attempt: a paid exam needs an active paid
        // activation for this user; free exams pass through.
        state.require_paid_activation(&exam, &caller.actor)?;
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


pub(crate) async fn list_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttemptView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        let params = query.into_inner();
        let mut items: Vec<AttemptView> = state
            .attempts
            .iter()
            .filter(|entry| {
                let attempt = entry.value();
                exam_in_scope(&state, &attempt.exam_id, &_scope)
                    && (is_manager
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


pub(crate) async fn get_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        expire_if_due(&state, path.as_str());
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
        visible_attempt(&state, &ctx, &caller, &attempt)?;
        Ok(state.attempt_view(&attempt))
    })();
    json::respond(result, debug)
}


pub(crate) async fn list_my_attempts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AttemptView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<AttemptView> = state
            .attempts
            .iter()
            .filter(|entry| {
                let attempt = entry.value();
                exam_in_scope(&state, &attempt.exam_id, &_scope)
                    && attempt.user_id == caller.actor
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


pub(crate) async fn heartbeat_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        // The server decides expiry: an attempt past its deadline is
        // expired here, whatever the client clock says.
        expire_if_due(&state, path.as_str());
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) fn validate_answers(
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


pub(crate) async fn autosave_attempt(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        expire_if_due(&state, path.as_str());
        let mut attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn submit_attempt(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        expire_if_due(&state, path.as_str());
        let mut attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn expire_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AttemptView> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn record_proctor_signal(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_TAKE)?;
        let settings = state.settings();
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn list_proctor_signals(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_exam(status: &str) -> Exam {
        Exam {
            id: "exam-1".to_string(),
            title: "Test".to_string(),
            description: String::new(),
            category: "general".to_string(),
            bank_ids: Vec::new(),
            course_id: None,
            academic_enrollment_id: None,
            academic_component_id: None,
            status: status.to_string(),
            opens_at: None,
            closes_at: None,
            duration_minutes: 60,
            pass_mark: 60.0,
            attempts_allowed: 3,
            price_minor: 0,
            is_paid: false,
            settings: ExamSettings::default(),
            total_marks: 100.0,
            created_by: "admin".to_string(),
            company_id: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn exam_window_open_accepts_published_exam_without_window() {
        let exam = test_exam(EXAM_PUBLISHED);
        assert!(exam_window_open(&exam, Utc::now()).is_ok());
    }

    #[test]
    fn exam_window_open_rejects_unpublished_exam() {
        let exam = test_exam(EXAM_DRAFT);
        assert!(exam_window_open(&exam, Utc::now()).is_err());
    }

    #[test]
    fn exam_window_open_enforces_open_and_close_times() {
        let now = Utc::now();
        let mut exam = test_exam(EXAM_PUBLISHED);
        exam.opens_at = Some(now + chrono::Duration::hours(1));
        assert!(exam_window_open(&exam, now).is_err());
        exam.opens_at = Some(now - chrono::Duration::hours(1));
        exam.closes_at = Some(now - chrono::Duration::minutes(1));
        assert!(exam_window_open(&exam, now).is_err());
        exam.closes_at = Some(now + chrono::Duration::hours(1));
        assert!(exam_window_open(&exam, now).is_ok());
    }
}
