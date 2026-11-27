//! Recruitment - HTTP handlers for evaluations.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
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
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_interviews::*, handlers_offers::*};

// Evaluation handlers

pub(crate) async fn list_evaluations(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_VIEW)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let params = query.into_inner();
        let interviewer_filter = params.filters.get("interviewer_id").cloned();
        let mut items: Vec<Evaluation> = state
            .evaluations
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|evaluation| {
                if evaluation.applicant_id != applicant.id {
                    return false;
                }
                if let Some(interviewer) = &interviewer_filter {
                    if &evaluation.interviewer_id != interviewer {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        let page = paginate(items, &params);
        Ok(json!({
            "items": page.items,
            "pagination": page.pagination,
            "aggregate": aggregate_scores(&state, applicant.id),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_evaluation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateEvaluationRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Evaluation> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        let input = body.into_inner();
        let interviewer_id = require_text(&input.interviewer_id, "interviewer_id", MAX_NAME_LEN)?;
        validate_scores(&input.scores)?;
        if let Some(interview_id) = input.interview_id {
            let interview = state
                .interviews
                .get(&interview_id)
                .map(|entry| entry.value().clone())
                .ok_or_else(|| {
                    UnifiedError::not_found(format!("Interview not found: {interview_id}"))
                })?;
            if interview.applicant_id != applicant.id {
                return Err(UnifiedError::bad_request(
                    "interview_id belongs to a different applicant",
                ));
            }
            if state.evaluations.iter().any(|entry| {
                entry.value().interview_id == Some(interview_id)
                    && entry.value().interviewer_id == interviewer_id
            }) {
                return Err(UnifiedError::conflict(
                    "This interviewer already submitted an evaluation for this interview",
                ));
            }
        }
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let evaluation = Evaluation {
            id: Uuid::now_v7(),
            applicant_id: applicant.id,
            job_id: applicant.job_id,
            interviewer_id: interviewer_id.clone(),
            interview_id: input.interview_id,
            scores: input.scores,
            notes,
            created_at: now,
            updated_at: now,
        };
        state.evaluations.insert(evaluation.id, evaluation.clone());
        state.emit(
            "recruitment.evaluation.submitted",
            json!({
                "evaluation_id": evaluation.id.to_string(),
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "interviewer_id": interviewer_id,
                "actor": caller.actor,
            }),
        );
        Ok(evaluation)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}
