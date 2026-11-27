//! exams - handlers grading.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



// Handlers: rubrics and manual grading
pub(crate) fn validate_rubric_criteria(
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


pub(crate) async fn list_rubrics(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn create_rubric(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn get_rubric(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Rubric> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        require_rubric_in_scope(&state, path.as_str(), &_scope)
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_rubric(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let mut rubric = require_rubric_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn delete_rubric(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let rubric = require_rubric_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn list_grading_queue(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ExamGrade>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let params = query.into_inner();
        let mut items: Vec<ExamGrade> = state
            .grades
            .iter()
            .filter(|entry| {
                let grade = entry.value();
                exam_in_scope(&state, &grade.exam_id, &_scope)
                    && grade.status == GRADE_PENDING
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


pub(crate) async fn get_attempt_grading(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn grade_attempt(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let input = body.into_inner();
        let grader = input.grader_id.clone().unwrap_or_else(|| caller.actor.clone());
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn regrade_attempt(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn override_attempt_score(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let reason = require_text(&body.reason, "reason", MAX_COMMENT_LEN)?;
        let attempt = require_attempt_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn dispute_grade(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let reason = require_text(&body.reason, "reason", MAX_COMMENT_LEN)?;
        let mut grade = require_grade_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn second_grade(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_GRADE)?;
        let mut grade = require_grade_in_scope(&state, path.as_str(), &_scope)?;
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
