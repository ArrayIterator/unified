// surveys module - Survey handlers.
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

use super::constants::{MAX_DESCRIPTION_LEN, MAX_TITLE_LEN, PERM_SURVEYS_MANAGE, PERM_SURVEYS_VIEW};
use super::types::{Survey, SurveyQuestion};
use super::requests::{CreateSurveyRequest, UpdateSurveyRequest};
use super::state::SurveysState;
use super::helpers::{
    caller, caller_company, created, err, load_survey, ok, optional_text, paginate, questions_for,
    require_company, require_permission, require_tenant, require_text,
};

// Survey handlers

pub(crate) fn survey_view(state: &SurveysState, survey: &Survey) -> Value {
    let mut value = serde_json::to_value(survey).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut value {
        map.insert(
            "questions".to_string(),
            serde_json::to_value(questions_for(state, survey.id)).unwrap_or(Value::Null),
        );
        let versions: Vec<u32> = state
            .surveys
            .iter()
            .filter(|entry| entry.value().family_id == survey.family_id)
            .map(|entry| entry.value().version)
            .collect();
        map.insert(
            "family_versions".to_string(),
            serde_json::to_value(versions).unwrap_or(Value::Null),
        );
    }
    value
}

pub(crate) async fn list_surveys(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let family_filter = params.filters.get("family_id").cloned();
        let creator_filter = params.filters.get("created_by").cloned();
        let search = params.search.clone().unwrap_or_default().to_lowercase();
        let mut items: Vec<Value> = state
            .surveys
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|survey| {
                if let Some(status) = &status_filter {
                    if survey.status != *status {
                        return false;
                    }
                }
                if let Some(family) = &family_filter {
                    if survey.family_id.to_string() != *family {
                        return false;
                    }
                }
                if let Some(creator) = &creator_filter {
                    if survey.created_by != *creator {
                        return false;
                    }
                }
                if !search.is_empty() {
                    let haystack = format!(
                        "{} {}",
                        survey.title,
                        survey.description.clone().unwrap_or_default()
                    )
                    .to_lowercase();
                    if !haystack.contains(&search) {
                        return false;
                    }
                }
                true
            })
            .map(|survey| survey_view(&state, &survey))
            .collect();
        let sort = params.allowed_sort(&["title", "created_at", "version"], "created_at");
        let descending = params.normalized().order == "desc";
        items.sort_by(|a, b| {
            let ordering = match sort.as_str() {
                "title" => a
                    .get("title")
                    .and_then(Value::as_str)
                    .cmp(&b.get("title").and_then(Value::as_str)),
                "version" => a
                    .get("version")
                    .and_then(Value::as_u64)
                    .cmp(&b.get("version").and_then(Value::as_u64)),
                _ => a
                    .get("created_at")
                    .and_then(Value::as_str)
                    .cmp(&b.get("created_at").and_then(Value::as_str)),
            };
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    body: web::Json<CreateSurveyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let input = body.into_inner();
        let title = require_text(&input.title, "title", MAX_TITLE_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let now = Utc::now();
        let id = Uuid::now_v7();
        let survey = Survey {
            id,
            family_id: id,
            version: 1,
            supersedes: None,
            title,
            description,
            status: "draft".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.surveys.insert(id, survey.clone());
        state.emit(
            "surveys.created",
            json!({
                "survey_id": id.to_string(),
                "family_id": id.to_string(),
                "version": 1,
                "title": survey.title,
                "created_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        let survey = load_survey(&state, &path)?;
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a survey. A draft is edited in place. A published (or
/// archived) survey is never edited in place: the update creates a
/// new draft version in the same family (questions copied), and
/// the new version is returned - responses keep referencing the
/// version they answered.
pub(crate) async fn update_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateSurveyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let input = body.into_inner();
        let survey = load_survey(&state, &path)?;
        let title = match &input.title {
            Some(raw) => Some(require_text(raw, "title", MAX_TITLE_LEN)?),
            None => None,
        };
        let description = match &input.description {
            Some(inner) => match inner {
                Some(raw) => Some(Some(require_text(raw, "description", MAX_DESCRIPTION_LEN)?)),
                None => Some(None),
            },
            None => None,
        };
        if survey.status == "draft" {
            let mut updated = survey.clone();
            if let Some(title) = &title {
                updated.title = title.clone();
            }
            if let Some(description) = &description {
                updated.description = description.clone();
            }
            updated.updated_at = Utc::now();
            state.surveys.insert(updated.id, updated.clone());
            state.emit(
                "surveys.updated",
                json!({
                    "survey_id": updated.id.to_string(),
                    "version": updated.version,
                    "updated_by": caller.actor,
                }),
            );
            return Ok(survey_view(&state, &updated));
        }
        let now = Utc::now();
        let next_version = state
            .surveys
            .iter()
            .filter(|entry| entry.value().family_id == survey.family_id)
            .map(|entry| entry.value().version)
            .max()
            .unwrap_or(survey.version)
            + 1;
        let new_id = Uuid::now_v7();
        let new_survey = Survey {
            id: new_id,
            family_id: survey.family_id,
            version: next_version,
            supersedes: Some(survey.id),
            title: title.unwrap_or_else(|| survey.title.clone()),
            description: description.unwrap_or_else(|| survey.description.clone()),
            status: "draft".to_string(),
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        state.surveys.insert(new_id, new_survey.clone());
        for question in questions_for(&state, survey.id) {
            let copy = SurveyQuestion {
                id: Uuid::now_v7(),
                survey_id: new_id,
                position: question.position,
                question_type: question.question_type,
                text: question.text,
                required: question.required,
                options: question.options,
                min_value: question.min_value,
                max_value: question.max_value,
                created_at: now,
                updated_at: now,
            };
            state.questions.insert(copy.id, copy);
        }
        state.emit(
            "surveys.version_created",
            json!({
                "survey_id": new_id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": next_version,
                "supersedes": survey.id.to_string(),
                "created_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &new_survey))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a draft survey version and everything that belongs to
/// it: questions, responses (and their answers), and distributions
/// (with their pending recipients). Only called after the draft /
/// no-submitted-responses checks in `delete_survey`.
pub(crate) fn delete_survey_records(state: &SurveysState, survey: &Survey, actor: &str) {
    let question_ids: Vec<Uuid> = questions_for(state, survey.id)
        .iter()
        .map(|question| question.id)
        .collect();
    for id in question_ids {
        state.questions.remove(&id);
    }
    let response_ids: Vec<Uuid> = state
        .responses
        .iter()
        .filter(|entry| entry.value().survey_id == survey.id)
        .map(|entry| *entry.key())
        .collect();
    let answer_ids: Vec<Uuid> = state
        .answers
        .iter()
        .filter(|entry| response_ids.contains(&entry.value().response_id))
        .map(|entry| *entry.key())
        .collect();
    for id in answer_ids {
        state.answers.remove(&id);
    }
    for id in response_ids {
        state.responses.remove(&id);
    }
    let distribution_ids: Vec<Uuid> = state
        .distributions
        .iter()
        .filter(|entry| entry.value().survey_id == survey.id)
        .map(|entry| *entry.key())
        .collect();
    for id in distribution_ids {
        state.distributions.remove(&id);
        state.pending_recipients.remove(&id);
    }
    state.surveys.remove(&survey.id);
    state.emit(
        "surveys.deleted",
        json!({
            "survey_id": survey.id.to_string(),
            "family_id": survey.family_id.to_string(),
            "version": survey.version,
            "deleted_by": actor,
        }),
    );
}

pub(crate) async fn delete_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let survey = load_survey(&state, &path)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only a draft survey version can be deleted; archive a published version instead",
            ));
        }
        if state
            .responses
            .iter()
            .any(|entry| entry.value().survey_id == survey.id && entry.value().submitted_at.is_some())
        {
            return Err(UnifiedError::conflict(
                "This survey version already has submitted responses and cannot be deleted",
            ));
        }
        delete_survey_records(&state, &survey, &caller.actor);
        Ok(json!({ "deleted": true, "id": survey.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn publish_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let mut survey = load_survey(&state, &path)?;
        if survey.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only a draft survey can be published (current status: {})",
                survey.status
            )));
        }
        if questions_for(&state, survey.id).is_empty() {
            return Err(UnifiedError::conflict(
                "A survey with no questions cannot be published",
            ));
        }
        let now = Utc::now();
        let mut archived: Vec<String> = Vec::new();
        for mut entry in state.surveys.iter_mut() {
            if entry.value().family_id == survey.family_id
                && entry.value().status == "published"
                && entry.value().id != survey.id
            {
                entry.value_mut().status = "archived".to_string();
                entry.value_mut().updated_at = now;
                archived.push(entry.value().id.to_string());
            }
        }
        survey.status = "published".to_string();
        survey.updated_at = now;
        state.surveys.insert(survey.id, survey.clone());
        state.emit(
            "surveys.published",
            json!({
                "survey_id": survey.id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": survey.version,
                "archived_versions": archived,
                "published_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Archiving is only for published versions: drafts are deleted,
/// not archived, and archived versions stay archived.
pub(crate) fn check_archivable(survey: &Survey) -> UnifiedResult<()> {
    if survey.status == "archived" {
        return Err(UnifiedError::conflict("This survey version is already archived"));
    }
    if survey.status == "draft" {
        return Err(UnifiedError::conflict(
            "Only a published survey version can be archived",
        ));
    }
    Ok(())
}

pub(crate) async fn archive_survey(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let mut survey = load_survey(&state, &path)?;
        check_archivable(&survey)?;
        survey.status = "archived".to_string();
        survey.updated_at = Utc::now();
        state.surveys.insert(survey.id, survey.clone());
        state.emit(
            "surveys.archived",
            json!({
                "survey_id": survey.id.to_string(),
                "family_id": survey.family_id.to_string(),
                "version": survey.version,
                "archived_by": caller.actor,
            }),
        );
        Ok(survey_view(&state, &survey))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::surveys::{SurveyAnswer, SurveyResponse};

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

    #[test]
    fn delete_survey_records_removes_answers_of_removed_responses() {
        let state = SurveysState::default();
        let survey = survey("draft");
        state.surveys.insert(survey.id, survey.clone());
        let now = Utc::now();
        let response = SurveyResponse {
            id: Uuid::new_v4(),
            survey_id: survey.id,
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
        };
        state.responses.insert(response.id, response.clone());
        let question = SurveyQuestion {
            id: Uuid::new_v4(),
            survey_id: survey.id,
            position: 1,
            question_type: "text".to_string(),
            text: "Question".to_string(),
            required: false,
            options: Vec::new(),
            min_value: None,
            max_value: None,
            created_at: now,
            updated_at: now,
        };
        state.questions.insert(question.id, question.clone());
        state.answers.insert(
            Uuid::new_v4(),
            SurveyAnswer {
                id: Uuid::new_v4(),
                response_id: response.id,
                question_id: question.id,
                score: None,
                text: Some("Great".to_string()),
                choice: None,
                created_at: now,
            },
        );
        delete_survey_records(&state, &survey, "test");
        assert!(state.surveys.get(&survey.id).is_none());
        assert!(state.responses.is_empty());
        assert!(
            state.answers.is_empty(),
            "answers of removed responses must not survive"
        );
        assert!(state.questions.is_empty());
    }

    #[test]
    fn check_archivable_rejects_drafts_and_archived_versions() {
        assert!(check_archivable(&survey("published")).is_ok());
        let error = check_archivable(&survey("draft")).unwrap_err();
        assert!(error.to_string().contains("published"));
        assert!(check_archivable(&survey("archived")).is_err());
    }
}
