//! exams - handlers exams.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



// ---------------------------------------------------------------------------
// Handlers: exams
// ---------------------------------------------------------------------------
pub(crate) fn exam_settings_from(input: &Option<ExamSettingsInput>, defaults: &ExamsSettings) -> UnifiedResult<ExamSettings> {
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


pub(crate) fn valid_exam_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        (EXAM_DRAFT, EXAM_PUBLISHED)
            | (EXAM_PUBLISHED, EXAM_ARCHIVED)
            | (EXAM_DRAFT, EXAM_ARCHIVED)
            | (EXAM_PUBLISHED, EXAM_CLOSED)
            | (EXAM_ARCHIVED, EXAM_DRAFT)
    )
}


pub(crate) async fn list_exams(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Exam>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let params = query.into_inner();
        let mut items: Vec<Exam> = state
            .exams
            .iter()
            .filter(|entry| {
                let exam = entry.value();
                in_company_scope(&exam.company_id, &_scope)
                    && (is_manager || exam.status == EXAM_PUBLISHED || exam.status == EXAM_CLOSED)
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


pub(crate) async fn create_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<CreateExamInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
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
            require_bank_by_id_in_scope(&state, bank_id, &_scope)?;
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
        let price_minor = input.price_minor.unwrap_or(0);
        let is_paid = input.is_paid.unwrap_or(false);
        validate_pricing(price_minor, is_paid)?;
        let exam_settings = exam_settings_from(&input.settings, &settings)?;
        let company_id = require_company_match(&_tenant, &req, &optional_text(&input.company_id, "company_id", MAX_SLUG_LEN)?)?;
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
            price_minor,
            is_paid,
            settings: exam_settings,
            total_marks: 0.0,
            created_by: caller.actor,
            company_id,
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


pub(crate) async fn get_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        if !is_manager && exam.status == EXAM_DRAFT {
            return Err(UnifiedError::not_found("Exam not found"));
        }
        Ok(exam)
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_exam(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let mut exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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
                require_bank_by_id_in_scope(&state, bank_id, &_scope)?;
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
        if input.price_minor.is_some() || input.is_paid.is_some() {
            let price_minor = input.price_minor.unwrap_or(exam.price_minor);
            let is_paid = input.is_paid.unwrap_or(exam.is_paid);
            validate_pricing(price_minor, is_paid)?;
            exam.price_minor = price_minor;
            exam.is_paid = is_paid;
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


pub(crate) async fn delete_exam(
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
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) fn change_exam_status(
    state: &ExamsState,
    caller: &Caller,
    exam_id: &str,
    target: &str,
    event: &str,
    scope: &Option<String>,
) -> UnifiedResult<Exam> {
    let mut exam = state
        .exams
        .get(exam_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Exam not found"))?;
    require_record_in_company_scope(&exam.company_id, scope)?;
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


pub(crate) async fn publish_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        change_exam_status(&state, &caller, path.as_str(), EXAM_PUBLISHED, EVT_EXAM_PUBLISHED, &_scope)
    })();
    json::respond(result, debug)
}


pub(crate) async fn archive_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        change_exam_status(&state, &caller, path.as_str(), EXAM_ARCHIVED, EVT_EXAM_ARCHIVED, &_scope)
    })();
    json::respond(result, debug)
}


pub(crate) async fn close_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        change_exam_status(&state, &caller, path.as_str(), EXAM_CLOSED, EVT_EXAM_CLOSED, &_scope)
    })();
    json::respond(result, debug)
}


pub(crate) async fn duplicate_exam(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Exam> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let source = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn list_exam_questions(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn attach_exam_question(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let exam = require_exam_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn detach_exam_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let (exam_id, question_id) = path.into_inner();
        let exam = require_exam_by_id_in_scope(&state, &exam_id, &_scope)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_exam() -> Exam {
        Exam {
            id: "exam-1".to_string(),
            title: "Certification".to_string(),
            description: String::new(),
            category: "general".to_string(),
            bank_ids: Vec::new(),
            course_id: None,
            academic_enrollment_id: None,
            academic_component_id: None,
            status: EXAM_DRAFT.to_string(),
            opens_at: None,
            closes_at: None,
            duration_minutes: 60,
            pass_mark: 60.0,
            attempts_allowed: 3,
            price_minor: 0,
            is_paid: false,
            settings: ExamSettings::default(),
            total_marks: 0.0,
            created_by: "admin".to_string(),
            company_id: None,
            created_at: now(),
            updated_at: now(),
        }
    }

    #[test]
    fn archived_event_is_distinct_from_closed_event() {
        assert_eq!(EVT_EXAM_ARCHIVED, "exams.exam.archived");
        assert_ne!(EVT_EXAM_ARCHIVED, EVT_EXAM_CLOSED);
    }

    #[test]
    fn archiving_an_exam_emits_the_archived_event_not_the_closed_event() {
        use unified_core::hooks::{HookBus, HookContext};
        use std::sync::Mutex;
        let state: ExamsStateRef = Arc::new(ExamsState::default());
        state.exams.insert("exam-1".to_string(), test_exam());
        let bus = Arc::new(HookBus::new());
        let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_writer = seen.clone();
        bus.register(
            EVT_EXAM_ARCHIVED,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_writer
                    .lock()
                    .expect("seen lock poisoned")
                    .push(ctx.event.clone());
            }),
        );
        let seen_closed: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_closed_writer = seen_closed.clone();
        bus.register(
            EVT_EXAM_CLOSED,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_closed_writer
                    .lock()
                    .expect("seen lock poisoned")
                    .push(ctx.event.clone());
            }),
        );
        *state.hooks.write().expect("hooks lock poisoned") = Some(bus);
        let caller = Caller {
            actor: "admin".to_string(),
            role: "admin".to_string(),
        };
        let archived = change_exam_status(&state, &caller, "exam-1", EXAM_ARCHIVED, EVT_EXAM_ARCHIVED, &None)
            .expect("a draft exam must archive");
        assert_eq!(archived.status, EXAM_ARCHIVED);
        assert_eq!(
            *seen.lock().expect("seen lock poisoned"),
            vec!["exams.exam.archived".to_string()]
        );
        assert!(
            seen_closed.lock().expect("seen lock poisoned").is_empty(),
            "archiving must not emit exams.exam.closed"
        );
    }

    #[test]
    fn valid_exam_transition_allows_the_documented_moves() {
        assert!(valid_exam_transition(EXAM_DRAFT, EXAM_PUBLISHED));
        assert!(valid_exam_transition(EXAM_DRAFT, EXAM_ARCHIVED));
        assert!(valid_exam_transition(EXAM_PUBLISHED, EXAM_ARCHIVED));
        assert!(valid_exam_transition(EXAM_PUBLISHED, EXAM_CLOSED));
        assert!(valid_exam_transition(EXAM_ARCHIVED, EXAM_DRAFT));
    }

    #[test]
    fn valid_exam_transition_rejects_all_other_moves() {
        assert!(!valid_exam_transition(EXAM_PUBLISHED, EXAM_DRAFT));
        assert!(!valid_exam_transition(EXAM_DRAFT, EXAM_CLOSED));
        assert!(!valid_exam_transition(EXAM_CLOSED, EXAM_DRAFT));
        assert!(!valid_exam_transition(EXAM_CLOSED, EXAM_PUBLISHED));
        assert!(!valid_exam_transition(EXAM_ARCHIVED, EXAM_PUBLISHED));
        assert!(!valid_exam_transition("draft", "draft"));
    }
}
