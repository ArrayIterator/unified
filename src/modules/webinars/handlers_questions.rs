// webinars -- Q&A handlers.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use super::helpers::*;
use super::state::*;
use super::types::*;

// Q&A handlers

pub(crate) fn require_qa_access(
    state: &WebinarsState,
    webinar: &Webinar,
    user_id: &str,
    roles: &[String],
) -> UnifiedResult<()> {
    if is_staff(webinar, user_id, roles) {
        return Ok(());
    }
    match find_registration(state, webinar.id, user_id) {
        Some(row) if row.status == "registered" => Ok(()),
        _ => Err(UnifiedError::forbidden(
            "Only registered attendees and webinar staff can use Q&A",
        )),
    }
}

pub(crate) async fn list_questions(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let result: UnifiedResult<PageResult<QuestionView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let params = query.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        let answered_filter = params.filters.get("answered").cloned();
        let mut items: Vec<QuestionView> = state
            .questions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| {
                if row.webinar_id != webinar.id {
                    return false;
                }
                if let Some(answered) = &answered_filter {
                    let expected = match answered.as_str() {
                        "true" => true,
                        "false" => false,
                        _ => return false,
                    };
                    if row.answered != expected {
                        return false;
                    }
                }
                true
            })
            .map(|row| question_view(&row, &actor))
            .collect();
        // Most upvoted first; ties break by earliest asked.
        items.sort_by(|a, b| {
            b.upvotes
                .cmp(&a.upvotes)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error),
    }
}

pub(crate) async fn ask_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AskQuestionRequest>,
) -> HttpResponse {
    let result: UnifiedResult<QuestionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let input = body.into_inner();
        let webinar = load_webinar(&state, &path.into_inner())?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        if !webinar.qa_enabled {
            return Err(UnifiedError::unprocessable(
                "Q&A is disabled for this webinar",
            ));
        }
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Questions can only be asked while the webinar is live",
            ));
        }
        let body_text = require_text(&input.body, "body", MAX_QA_BODY_LEN)?;
        let now = Utc::now();
        let question = WebinarQuestion {
            id: Uuid::now_v7(),
            webinar_id: webinar.id,
            author_id: actor.clone(),
            body: body_text,
            upvoted_by: Vec::new(),
            answered: false,
            answered_by: None,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(question.id, question.clone());
        state.emit(
            "webinar.question.asked",
            json!({
                "webinar_id": webinar.id.to_string(),
                "question_id": question.id.to_string(),
                "author_id": actor,
                "body": question.body,
            }),
        );
        Ok(question_view(&question, &actor))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn upvote_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<QuestionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, question_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        require_qa_access(&state, &webinar, &actor, &roles)?;
        if webinar.status != WebinarStatus::Live {
            return Err(UnifiedError::unprocessable(
                "Questions can only be upvoted while the webinar is live",
            ));
        }
        let mut question = load_question(&state, webinar.id, &question_id)?;
        // One upvote per user: a repeat upvote is a no-op that
        // returns the current state instead of an error.
        if !question.upvoted_by.iter().any(|id| id == &actor) {
            question.upvoted_by.push(actor.clone());
            question.updated_at = Utc::now();
            state.questions.insert(question.id, question.clone());
            state.emit(
                "webinar.question.upvoted",
                json!({
                    "webinar_id": webinar.id.to_string(),
                    "question_id": question.id.to_string(),
                    "user_id": actor,
                    "upvotes": question.upvoted_by.len(),
                }),
            );
        }
        Ok(question_view(&question, &actor))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn answer_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<QuestionView> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, question_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        if !is_staff(&webinar, &actor, &roles) {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, a co-host, a panelist, or an admin can mark a question answered",
            ));
        }
        let mut question = load_question(&state, webinar.id, &question_id)?;
        if question.answered {
            return Err(UnifiedError::unprocessable(
                "This question is already marked answered",
            ));
        }
        question.answered = true;
        question.answered_by = Some(actor.clone());
        question.updated_at = Utc::now();
        state.questions.insert(question.id, question.clone());
        state.emit(
            "webinar.question.answered",
            json!({
                "webinar_id": webinar.id.to_string(),
                "question_id": question.id.to_string(),
                "answered_by": actor,
            }),
        );
        Ok(question_view(&question, &actor))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error),
    }
}

pub(crate) async fn delete_question(
    state: web::Data<WebinarsState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let actor = actor_id(&req)?;
        let roles = caller_roles(&req);
        let (webinar_id, question_id) = path.into_inner();
        let webinar = load_webinar(&state, &webinar_id)?;
        let question = load_question(&state, webinar.id, &question_id)?;
        if !is_host(&webinar, &actor) && !is_admin(&roles) && question.author_id != actor {
            return Err(UnifiedError::forbidden(
                "Only the webinar host, an admin, or the question's author can delete it",
            ));
        }
        state.questions.remove(&question.id);
        state.emit(
            "webinar.question.deleted",
            json!({
                "webinar_id": webinar.id.to_string(),
                "question_id": question.id.to_string(),
                "deleted_by": actor,
            }),
        );
        Ok(json!({ "deleted": true, "question_id": question.id.to_string() }))
    })();
    match result {
        Ok(value) => ok(value),
        Err(error) => err(error),
    }
}
