// surveys module - Public response handlers.
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

use super::constants::{MAX_ID_LEN, MAX_OPTION_LEN, MAX_TEXT_LEN};
use super::types::{Survey, SurveyAnswer, SurveyResponse};
use super::requests::SubmitResponseRequest;
use super::state::SurveysState;
use super::helpers::{err, ok, questions_for, require_text, score_bounds};
use super::service::maybe_create_followup;

// Public response handlers (token-gated; see the public-access note)

pub(crate) fn find_response_by_token(state: &SurveysState, token: &str) -> UnifiedResult<SurveyResponse> {
    let token = token.trim();
    if token.is_empty() || token.len() > MAX_ID_LEN {
        return Err(UnifiedError::bad_request("Invalid response token"));
    }
    state
        .responses
        .iter()
        .map(|entry| entry.value().clone())
        .find(|response| response.token == token)
        .ok_or_else(|| UnifiedError::not_found("Response not found for this token"))
}

/// Load the survey behind a public response invitation, refusing
/// archived versions: an archived survey no longer accepts reads
/// or submissions of its invitations.
pub(crate) fn load_public_survey(
    state: &SurveysState,
    response: &SurveyResponse,
) -> UnifiedResult<Survey> {
    let survey = state
        .surveys
        .get(&response.survey_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found(format!("Survey not found: {}", response.survey_id)))?;
    if survey.status == "archived" {
        return Err(UnifiedError::conflict(
            "This survey version is archived and no longer accepts responses",
        ));
    }
    Ok(survey)
}

pub(crate) async fn get_public_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let response = find_response_by_token(&state, &path)?;
        if response.submitted_at.is_some() {
            return Err(UnifiedError::conflict("This response was already submitted"));
        }
        if response.token_expires_at < Utc::now() {
            return Err(UnifiedError::bad_request("This response link has expired"));
        }
        let survey = load_public_survey(&state, &response)?;
        Ok(json!({
            "survey_id": survey.id.to_string(),
            "title": survey.title,
            "description": survey.description,
            "version": survey.version,
            "token_expires_at": response.token_expires_at.to_rfc3339(),
            "questions": questions_for(&state, survey.id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn submit_response(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    path: web::Path<String>,
    body: web::Json<SubmitResponseRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut response = find_response_by_token(&state, &path)?;
        if response.submitted_at.is_some() {
            return Err(UnifiedError::conflict("This response was already submitted"));
        }
        if response.token_expires_at < Utc::now() {
            return Err(UnifiedError::bad_request("This response link has expired"));
        }
        let survey = load_public_survey(&state, &response)?;
        let questions = questions_for(&state, survey.id);
        if questions.is_empty() {
            return Err(UnifiedError::conflict("This survey has no questions"));
        }
        let input = body.into_inner();
        if input.answers.is_empty() {
            return Err(UnifiedError::bad_request("At least one answer is required"));
        }
        if input.answers.len() > questions.len() {
            return Err(UnifiedError::bad_request(
                "More answers were given than the survey has questions",
            ));
        }
        let mut seen: Vec<Uuid> = Vec::new();
        let mut validated: Vec<SurveyAnswer> = Vec::new();
        let now = Utc::now();
        for submitted in &input.answers {
            if seen.contains(&submitted.question_id) {
                return Err(UnifiedError::bad_request(format!(
                    "Duplicate answer for question {}",
                    submitted.question_id
                )));
            }
            seen.push(submitted.question_id);
            let Some(question) = questions.iter().find(|item| item.id == submitted.question_id) else {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown question: {}",
                    submitted.question_id
                )));
            };
            let mut answer = SurveyAnswer {
                id: Uuid::now_v7(),
                response_id: response.id,
                question_id: question.id,
                score: None,
                text: None,
                choice: None,
                created_at: now,
            };
            match question.question_type.as_str() {
                "nps" | "csat" | "rating" => {
                    let Some(score) = submitted.score else {
                        return Err(UnifiedError::bad_request(format!(
                            "Question \"{}\" needs a score",
                            question.text
                        )));
                    };
                    let (min, max) = score_bounds(&question.question_type);
                    if let (Some(min), Some(max)) = (min, max) {
                        if score < min || score > max {
                            return Err(UnifiedError::bad_request(format!(
                                "Score for \"{}\" must be between {min} and {max}",
                                question.text
                            )));
                        }
                    }
                    answer.score = Some(score);
                }
                "text" => {
                    let Some(raw) = &submitted.text else {
                        return Err(UnifiedError::bad_request(format!(
                            "Question \"{}\" needs a text answer",
                            question.text
                        )));
                    };
                    answer.text = Some(require_text(raw, "answer", MAX_TEXT_LEN)?);
                }
                "multiple_choice" => {
                    let Some(choice) = &submitted.choice else {
                        return Err(UnifiedError::bad_request(format!(
                            "Question \"{}\" needs a choice",
                            question.text
                        )));
                    };
                    let choice = require_text(choice, "choice", MAX_OPTION_LEN)?;
                    if !question.options.contains(&choice) {
                        return Err(UnifiedError::bad_request(format!(
                            "Choice \"{choice}\" is not one of the options for \"{}\"",
                            question.text
                        )));
                    }
                    answer.choice = Some(choice);
                }
                _ => {
                    return Err(UnifiedError::bad_request(format!(
                        "Unsupported question type: {}",
                        question.question_type
                    )));
                }
            }
            validated.push(answer);
        }
        for question in &questions {
            if question.required && !seen.contains(&question.id) {
                return Err(UnifiedError::bad_request(format!(
                    "Required question \"{}\" was not answered",
                    question.text
                )));
            }
        }
        for answer in validated {
            state.answers.insert(answer.id, answer);
        }
        response.submitted_at = Some(now);
        response.updated_at = now;
        state.responses.insert(response.id, response.clone());
        state.emit(
            "surveys.response.received",
            json!({
                "survey_id": survey.id.to_string(),
                "response_id": response.id.to_string(),
                "recipient_id": response.recipient_id,
                "submitted_at": now.to_rfc3339(),
            }),
        );
        maybe_create_followup(&state, &survey, &mut response);
        Ok(json!({
            "submitted": true,
            "response_id": response.id.to_string(),
            "survey_id": survey.id.to_string(),
            "follow_up_status": response.follow_up_status,
            "follow_up_task_id": response.follow_up_task_id,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn survey(status: &str) -> Survey {
        let now = Utc::now();
        let id = Uuid::new_v4();
        Survey {
            id,
            family_id: id,
            version: 1,
            supersedes: None,
            title: "Feedback".to_string(),
            description: None,
            status: status.to_string(),
            created_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn response(survey_id: Uuid) -> SurveyResponse {
        let now = Utc::now();
        SurveyResponse {
            id: Uuid::new_v4(),
            survey_id,
            distribution_id: None,
            recipient_id: "user-1".to_string(),
            recipient_email: None,
            agent_id: None,
            department_id: None,
            product_id: None,
            token: "token-1".to_string(),
            token_expires_at: now + chrono::Duration::days(30),
            submitted_at: None,
            follow_up_status: "none".to_string(),
            follow_up_task_id: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn load_public_survey_refuses_archived_versions_like_submit_response() {
        let state = SurveysState::default();
        let archived = survey("archived");
        state.surveys.insert(archived.id, archived.clone());
        let response = response(archived.id);
        let error = load_public_survey(&state, &response).unwrap_err();
        assert!(error.to_string().contains("archived"));

        let published = survey("published");
        state.surveys.insert(published.id, published.clone());
        let response = response(published.id);
        assert!(load_public_survey(&state, &response).is_ok());
    }
}
