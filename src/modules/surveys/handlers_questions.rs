// surveys module - Question handlers.
//
// Split from the former single-file module `src/modules/surveys.rs`.
// The module root (`surveys.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Datelike, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_OPTIONS, MAX_OPTION_LEN, MAX_QUESTION_LEN, PERM_SURVEYS_MANAGE, PERM_SURVEYS_VIEW};
use super::types::SurveyQuestion;
use super::requests::{CreateQuestionRequest, UpdateQuestionRequest};
use super::state::SurveysState;
use super::helpers::{
    caller, caller_company, created, err, load_question, load_survey, ok, paginate, questions_for,
    require_company, require_permission, require_tenant, require_text, score_bounds,
    valid_question_type,
};

// Question handlers

pub(crate) fn validate_options(question_type: &str, options: &[String]) -> UnifiedResult<Vec<String>> {
    if question_type == "multiple_choice" {
        if options.len() < 2 {
            return Err(UnifiedError::bad_request(
                "A multiple choice question needs at least 2 options",
            ));
        }
    } else if !options.is_empty() {
        return Err(UnifiedError::bad_request(
            "options only apply to multiple_choice questions",
        ));
    }
    if options.len() > MAX_OPTIONS {
        return Err(UnifiedError::bad_request(format!(
            "at most {MAX_OPTIONS} options are allowed"
        )));
    }
    let mut cleaned: Vec<String> = Vec::new();
    for raw in options {
        cleaned.push(require_text(raw, "option", MAX_OPTION_LEN)?);
    }
    Ok(cleaned)
}

pub(crate) async fn list_questions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        let params = query.into_inner();
        Ok(paginate(questions_for(&state, survey.id), &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyQuestion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let survey = load_survey(&state, &path)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Questions can only be added to a draft survey; editing a published survey creates a new version",
            ));
        }
        let input = body.into_inner();
        let question_type = valid_question_type(&input.question_type)?;
        let text = require_text(&input.text, "text", MAX_QUESTION_LEN)?;
        let options = validate_options(&question_type, &input.options)?;
        let (min_value, max_value) = score_bounds(&question_type);
        let position = questions_for(&state, survey.id)
            .iter()
            .map(|question| question.position)
            .max()
            .unwrap_or(0)
            + 1;
        let now = Utc::now();
        let question = SurveyQuestion {
            id: Uuid::now_v7(),
            survey_id: survey.id,
            position,
            question_type: question_type.clone(),
            text,
            required: input.required,
            options,
            min_value,
            max_value,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(question.id, question.clone());
        state.emit(
            "surveys.question.created",
            json!({
                "survey_id": survey.id.to_string(),
                "question_id": question.id.to_string(),
                "question_type": question_type,
                "created_by": caller.actor,
            }),
        );
        Ok(question)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyQuestion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let (survey_id, question_id) = path.into_inner();
        let survey = load_survey(&state, &survey_id)?;
        load_question(&state, survey.id, &question_id)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateQuestionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyQuestion> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let (survey_id, question_id) = path.into_inner();
        let survey = load_survey(&state, &survey_id)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Questions can only be updated on a draft survey; editing a published survey creates a new version",
            ));
        }
        let input = body.into_inner();
        let mut question = load_question(&state, survey.id, &question_id)?;
        if let Some(text) = &input.text {
            question.text = require_text(text, "text", MAX_QUESTION_LEN)?;
        }
        if let Some(required) = input.required {
            question.required = required;
        }
        if let Some(options) = &input.options {
            question.options = validate_options(&question.question_type, options)?;
        }
        question.updated_at = Utc::now();
        state.questions.insert(question.id, question.clone());
        state.emit(
            "surveys.question.updated",
            json!({
                "survey_id": survey.id.to_string(),
                "question_id": question.id.to_string(),
                "updated_by": caller.actor,
            }),
        );
        Ok(question)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_question(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let (survey_id, question_id) = path.into_inner();
        let survey = load_survey(&state, &survey_id)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Questions can only be deleted from a draft survey",
            ));
        }
        let question = load_question(&state, survey.id, &question_id)?;
        if state
            .answers
            .iter()
            .any(|entry| entry.value().question_id == question.id)
        {
            return Err(UnifiedError::conflict(
                "This question already has answers and cannot be deleted",
            ));
        }
        state.questions.remove(&question.id);
        state.emit(
            "surveys.question.deleted",
            json!({
                "survey_id": survey.id.to_string(),
                "question_id": question.id.to_string(),
                "deleted_by": caller.actor,
            }),
        );
        Ok(json!({ "deleted": true, "id": question.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_options_accepts_a_multiple_choice_with_two_options() {
        let cleaned = validate_options(
            "multiple_choice",
            &["  Yes  ".to_string(), "No".to_string()],
        )
        .unwrap();
        assert_eq!(cleaned, vec!["Yes".to_string(), "No".to_string()]);
    }

    #[test]
    fn validate_options_rejects_a_multiple_choice_with_one_option() {
        assert!(validate_options("multiple_choice", &["Only".to_string()]).is_err());
    }

    #[test]
    fn validate_options_rejects_options_on_other_question_types() {
        assert!(validate_options("text", &["Extra".to_string()]).is_err());
        assert!(validate_options("nps", &[]).is_ok());
    }

    #[test]
    fn validate_options_enforces_the_maximum_option_count() {
        let many: Vec<String> = (0..MAX_OPTIONS + 1).map(|n| format!("opt-{n}")).collect();
        assert!(validate_options("multiple_choice", &many).is_err());
    }
}
