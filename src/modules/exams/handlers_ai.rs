//! exams - handlers ai.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use crate::modules::ai::run_ai_capability;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};
use std::sync::Arc;
use super::{constants::*, settings::*, types::*, module::*, helpers::*, inputs::*, handlers_banks::*, handlers_questions::*, handlers_exams::*, handlers_attempts::*, handlers_grading::*, handlers_results::*, handlers_settings::*, hooks::*};



// ---------------------------------------------------------------------------
// Handlers: AI (smart module)
// ---------------------------------------------------------------------------
pub(crate) async fn ai_draft_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<AiDraftQuestionsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = async {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
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
                require_bank_by_id_in_scope(&state, &id, &_scope)?;
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
        // module's helper (verified caller first; task 317 removed header trust).
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


pub(crate) async fn ai_analyse_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<ExamsStateRef>,
    req: HttpRequest,
    body: web::Json<AiAnalyseResultsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = async {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        let _scope = require_company_for_write(&_tenant, &req)?;
        require_permission(&ctx, &caller.role, PERM_EXAMS_VIEW)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_EXAMS_MANAGE);
        let is_grader = check_capability(&ctx, &caller.role, PERM_EXAMS_GRADE);
        if !is_manager && !is_grader {
            return Err(UnifiedError::forbidden(
                "Only a manager or grader may analyse exam results",
            ));
        }
        let exam_id = require_text(&body.exam_id, "exam_id", MAX_REF_LEN)?;
        let exam = require_exam_by_id_in_scope(&state, &exam_id, &_scope)?;
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
