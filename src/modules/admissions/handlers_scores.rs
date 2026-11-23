// admissions module — Score handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_TEXT, PERM_ADMISSIONS_READ, PERM_ADMISSIONS_SCORE};
use super::helpers::{
    caller, caller_company, load_application_for_read, matches_search, new_id, now, optional_text,
    paginate, require_permission, require_tenant, require_text, round4,
};
use super::state::AdmissionsState;
use super::types::{ApplicationScore, CreateScore, ScoreTotal, UpdateScore};

// ---------------------------------------------------------------------------
// Handlers: scores
// ---------------------------------------------------------------------------

pub(crate) async fn list_scores(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<ApplicationScore>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        let params = query.into_inner();
        let scores = state.scores_of(&application.id);
        let items: Vec<ApplicationScore> = scores
            .into_iter()
            .filter(|score| {
                matches_search(&score.stage_key, &params)
                    && params
                        .filters
                        .get("stage_id")
                        .is_none_or(|stage| score.stage_id == *stage)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateScore>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApplicationScore> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_SCORE)?;
        let application_id = path.as_str().to_string();
        let application = state.application(&application_id)?;
        if matches!(application.status.as_str(), "withdrawn" | "enrolled") {
            return Err(UnifiedError::conflict(format!(
                "Scores cannot be recorded for an application in status \"{}\"",
                application.status
            )));
        }
        let input = body.into_inner();
        let stage = state
            .stages
            .get(&input.stage_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))?;
        if stage.period_id != application.period_id {
            return Err(UnifiedError::not_found("Stage not found"));
        }
        let scale_max = state.score_scale_max();
        if !input.score.is_finite() || input.score < 0.0 || input.score > scale_max {
            return Err(UnifiedError::bad_request(format!(
                "score must be between 0 and {scale_max}"
            )));
        }
        if state.scores.iter().any(|entry| {
            let score = entry.value();
            score.application_id == application_id && score.stage_id == stage.id
        }) {
            return Err(UnifiedError::conflict(format!(
                "A score already exists for stage \"{}\"; update it instead",
                stage.stage_key
            )));
        }
        let note = optional_text(&input.note, "note", MAX_TEXT)?;
        let score = ApplicationScore {
            id: new_id(),
            application_id: application.id.clone(),
            period_id: application.period_id.clone(),
            stage_id: stage.id.clone(),
            stage_key: stage.stage_key.clone(),
            score: round4(input.score),
            scored_by: caller.actor.clone(),
            note,
            created_at: now(),
            updated_at: now(),
        };
        state.scores.insert(score.id.clone(), score.clone());
        let total = state.refresh_total(&application.id);
        if application.status == "submitted" {
            if let Some(mut record) = state.applications.get_mut(&application.id) {
                record.status = "under_review".to_string();
                record.updated_at = now();
            }
        }
        state.emit(
            "admissions.score_recorded",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "stage_id": stage.id,
                "score": score.score,
                "total_score": total,
            }),
        );
        Ok(score)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateScore>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ApplicationScore> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_SCORE)?;
        let (application_id, score_id) = path.into_inner();
        let application = state.application(&application_id)?;
        let input = body.into_inner();
        let mut score = state
            .scores
            .get(&score_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Score not found"))?;
        if score.application_id != application_id {
            return Err(UnifiedError::not_found("Score not found"));
        }
        if let Some(value) = input.score {
            let scale_max = state.score_scale_max();
            if !value.is_finite() || value < 0.0 || value > scale_max {
                return Err(UnifiedError::bad_request(format!(
                    "score must be between 0 and {scale_max}"
                )));
            }
            score.score = round4(value);
        }
        if let Some(note) = input.note {
            score.note = match note {
                Some(value) => Some(require_text(&value, "note", MAX_TEXT)?),
                None => None,
            };
        }
        score.scored_by = caller.actor.clone();
        score.updated_at = now();
        state.scores.insert(score.id.clone(), score.clone());
        let total = state.refresh_total(&application.id);
        state.emit(
            "admissions.score_recorded",
            json!({
                "application_id": application.id,
                "period_id": application.period_id,
                "stage_id": score.stage_id,
                "score": score.score,
                "total_score": total,
            }),
        );
        Ok(score)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_score(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_SCORE)?;
        let (application_id, score_id) = path.into_inner();
        let application = state.application(&application_id)?;
        let score = state
            .scores
            .get(&score_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Score not found"))?;
        if score.application_id != application_id {
            return Err(UnifiedError::not_found("Score not found"));
        }
        state.scores.remove(score_id.as_str());
        state.refresh_total(&application.id);
        Ok(json!({ "deleted": true, "id": score_id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_score_total(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ScoreTotal> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let application_id = path.as_str().to_string();
        let application = load_application_for_read(&state, &ctx, &caller, &application_id)?;
        Ok(state.compute_total(&application.id))
    })();
    json::respond(result, debug)
}
