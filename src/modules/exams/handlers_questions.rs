//! exams - handlers questions.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_ai::*, handlers_settings::*, hooks::*};



// ---------------------------------------------------------------------------
// Handlers: questions
// ---------------------------------------------------------------------------
pub(crate) fn question_admin_view(question: &Question) -> Value {
    json!(question)
}


pub(crate) async fn list_bank_questions(
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
        let bank = require_bank_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn create_question(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let bank = require_bank_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) fn build_question(
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


pub(crate) async fn get_question(
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
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let question = require_question_in_scope(&state, path.as_str(), &_scope)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        if is_manager {
            Ok(question_admin_view(&question))
        } else {
            Ok(json!(QuestionPublicView::from(&question)))
        }
    })();
    json::respond(result, debug)
}


pub(crate) async fn update_question(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let input = body.into_inner();
        let existing = require_question_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn delete_question(
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
        let question = require_question_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn list_question_versions(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let question = require_question_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn import_questions(
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
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_MANAGE)?;
        let settings = state.settings();
        let bank = require_bank_by_id_in_scope(&state, path.as_str(), &_scope)?;
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


pub(crate) async fn export_questions(
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
        let _scope = caller_company(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let bank = require_bank_by_id_in_scope(&state, path.as_str(), &_scope)?;
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
