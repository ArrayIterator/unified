// admissions module - Stage definition handlers.
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

use super::constants::{MAX_SHORT, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ};
use super::helpers::{
    caller, caller_company, matches_search, new_id, now, paginate, require_permission,
    require_tenant, require_text, round4, slugify,
};
use super::state::AdmissionsState;
use super::types::{CreateStage, StageDefinition, UpdateStage};

// Handlers: stage definitions

pub(crate) async fn list_stages(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<StageDefinition>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let stages = state.stages_of(&period_id);
        let items: Vec<StageDefinition> = stages
            .into_iter()
            .filter(|stage| {
                matches_search(&format!("{} {}", stage.stage_key, stage.name), &params)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_stage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateStage>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StageDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Stages cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let stage_key = slugify(&input.stage_key);
        if stage_key.is_empty() {
            return Err(UnifiedError::bad_request(
                "stage_key must contain at least one letter or digit",
            ));
        }
        let name = require_text(&input.name, "name", MAX_SHORT)?;
        let weight = match input.weight {
            Some(weight) => {
                if !weight.is_finite() || weight < 0.0 || weight > 1000.0 {
                    return Err(UnifiedError::bad_request(
                        "weight must be between 0 and 1000",
                    ));
                }
                round4(weight)
            }
            None => 1.0,
        };
        let scale_max = state.score_scale_max();
        let pass_score = match input.pass_score {
            Some(score) => {
                if !score.is_finite() || score < 0.0 || score > scale_max {
                    return Err(UnifiedError::bad_request(format!(
                        "pass_score must be between 0 and {scale_max}"
                    )));
                }
                Some(round4(score))
            }
            None => None,
        };
        if state.stages.iter().any(|entry| {
            let stage = entry.value();
            stage.period_id == period_id && stage.stage_key == stage_key
        }) {
            return Err(UnifiedError::conflict(format!(
                "A stage with key \"{stage_key}\" already exists in this period",
            )));
        }
        let position = match input.position {
            Some(position) => position,
            None => state
                .stages_of(&period_id)
                .iter()
                .map(|stage| stage.position)
                .max()
                .map(|max| max + 1)
                .unwrap_or(1),
        };
        let stage = StageDefinition {
            id: new_id(),
            period_id,
            stage_key,
            name,
            position,
            weight,
            pass_score,
            required: input.required.unwrap_or(true),
            created_at: now(),
            updated_at: now(),
        };
        state.stages.insert(stage.id.clone(), stage.clone());
        Ok(stage)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_stage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateStage>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<StageDefinition> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, stage_id) = path.into_inner();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Stages cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let mut stage = state
            .stages
            .get(&stage_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))?;
        if stage.period_id != period_id {
            return Err(UnifiedError::not_found("Stage not found"));
        }
        if let Some(name) = input.name {
            stage.name = require_text(&name, "name", MAX_SHORT)?;
        }
        if let Some(position) = input.position {
            stage.position = position;
        }
        if let Some(weight) = input.weight {
            if !weight.is_finite() || weight < 0.0 || weight > 1000.0 {
                return Err(UnifiedError::bad_request(
                    "weight must be between 0 and 1000",
                ));
            }
            stage.weight = round4(weight);
        }
        if let Some(pass_score) = input.pass_score {
            let scale_max = state.score_scale_max();
            stage.pass_score = match pass_score {
                Some(score) => {
                    if !score.is_finite() || score < 0.0 || score > scale_max {
                        return Err(UnifiedError::bad_request(format!(
                            "pass_score must be between 0 and {scale_max}"
                        )));
                    }
                    Some(round4(score))
                }
                None => None,
            };
        }
        if let Some(required) = input.required {
            stage.required = required;
        }
        stage.updated_at = now();
        state.stages.insert(stage.id.clone(), stage.clone());
        // Collect the ids first: refreshing totals mutates the
        // applications map, which must not happen while iterating it.
        let application_ids: Vec<String> = state
            .applications
            .iter()
            .filter(|entry| entry.value().period_id == period_id)
            .map(|entry| entry.value().id.clone())
            .collect();
        for application_id in application_ids {
            state.refresh_total(&application_id);
        }
        Ok(stage)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_stage(
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
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, stage_id) = path.into_inner();
        let stage = state
            .stages
            .get(&stage_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))?;
        if stage.period_id != period_id {
            return Err(UnifiedError::not_found("Stage not found"));
        }
        if state
            .scores
            .iter()
            .any(|entry| entry.value().stage_id == stage_id)
        {
            return Err(UnifiedError::conflict(
                "Stage still has recorded scores; keep it and set its weight to 0 instead",
            ));
        }
        state
            .stages
            .remove(stage_id.as_str())
            .map(|_| json!({ "deleted": true, "id": stage_id }))
            .ok_or_else(|| UnifiedError::not_found("Stage not found"))
    })();
    json::respond(result, debug)
}
