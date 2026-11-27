//! Recruitment - HTTP handlers for interviews.

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
use super::{constants::*, types::*, state::*, helpers::*, module::*, handlers_jobs::*, handlers_applicants::*, handlers_evaluations::*, handlers_offers::*};

// Interview handlers

pub(crate) async fn list_interviews(
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
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<Interview> = state
            .interviews
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|interview| {
                if interview.applicant_id != applicant.id {
                    return false;
                }
                if let Some(status) = &status_filter {
                    if &interview.status != status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by(|a, b| a.scheduled_at.cmp(&b.scheduled_at));
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_interview(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateInterviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Interview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let applicant = load_applicant(&state, &path.into_inner())?;
        require_consent(&applicant, "scheduling an interview")?;
        if !matches!(
            applicant.stage,
            ApplicantStage::Screening | ApplicantStage::Interview
        ) {
            return Err(UnifiedError::conflict(format!(
                "Interviews can only be scheduled while the applicant is in screening or interview (now: {})",
                applicant.stage.as_str()
            )));
        }
        let input = body.into_inner();
        if input.scheduled_at <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "scheduled_at must be in the future",
            ));
        }
        let mut interviewers: Vec<String> = Vec::new();
        for raw in &input.interviewers {
            let interviewer = require_text(raw, "interviewers[]", MAX_NAME_LEN)?;
            if !interviewers.contains(&interviewer) {
                interviewers.push(interviewer);
            }
        }
        if interviewers.is_empty() {
            return Err(UnifiedError::bad_request(
                "At least one interviewer is required",
            ));
        }
        if interviewers.len() > 20 {
            return Err(UnifiedError::bad_request(
                "At most 20 interviewers can be listed",
            ));
        }
        let notes = match &input.notes {
            Some(raw) => Some(require_text(raw, "notes", MAX_NOTES_LEN)?),
            None => None,
        };
        let now = Utc::now();
        let mut interview = Interview {
            id: Uuid::now_v7(),
            applicant_id: applicant.id,
            job_id: applicant.job_id,
            scheduled_at: input.scheduled_at,
            interview_type: input.interview_type.unwrap_or_default(),
            interviewers,
            notes,
            outcome: None,
            outcome_notes: None,
            status: "scheduled".to_string(),
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        schedule_interview_reminder(&state, &mut interview);
        state.interviews.insert(interview.id, interview.clone());
        state.emit(
            "recruitment.interview.scheduled",
            json!({
                "interview_id": interview.id.to_string(),
                "applicant_id": applicant.id.to_string(),
                "job_id": applicant.job_id.to_string(),
                "scheduled_at": interview.scheduled_at.to_rfc3339(),
                "scheduler_status": interview.scheduler_status,
                "actor": caller.actor,
            }),
        );
        Ok(interview)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_interview(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateInterviewRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Interview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut interview = load_interview(&state, &path.into_inner())?;
        if interview.status != "scheduled" {
            return Err(UnifiedError::conflict(format!(
                "Only a scheduled interview can be updated (interview is {})",
                interview.status
            )));
        }
        let input = body.into_inner();
        if let Some(scheduled_at) = input.scheduled_at {
            if scheduled_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "scheduled_at must be in the future",
                ));
            }
            interview.scheduled_at = scheduled_at;
        }
        if let Some(interview_type) = input.interview_type {
            interview.interview_type = interview_type;
        }
        if let Some(raw_list) = input.interviewers {
            let mut interviewers: Vec<String> = Vec::new();
            for raw in &raw_list {
                let interviewer = require_text(raw, "interviewers[]", MAX_NAME_LEN)?;
                if !interviewers.contains(&interviewer) {
                    interviewers.push(interviewer);
                }
            }
            if interviewers.is_empty() {
                return Err(UnifiedError::bad_request(
                    "At least one interviewer is required",
                ));
            }
            if interviewers.len() > 20 {
                return Err(UnifiedError::bad_request(
                    "At most 20 interviewers can be listed",
                ));
            }
            interview.interviewers = interviewers;
        }
        if let Some(raw) = &input.notes {
            interview.notes = match raw {
                Some(value) => Some(require_text(value, "notes", MAX_NOTES_LEN)?),
                None => None,
            };
        }
        interview.updated_at = Utc::now();
        state.interviews.insert(interview.id, interview.clone());
        state.emit(
            "recruitment.interview.updated",
            json!({
                "interview_id": interview.id.to_string(),
                "applicant_id": interview.applicant_id.to_string(),
                "actor": caller.actor,
            }),
        );
        Ok(interview)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn record_outcome(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<RecruitmentState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<InterviewOutcomeRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Interview> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_RECRUITMENT_MANAGE)?;
        let mut interview = load_interview(&state, &path.into_inner())?;
        if interview.status != "scheduled" {
            return Err(UnifiedError::conflict(format!(
                "Only a scheduled interview can record an outcome (interview is {})",
                interview.status
            )));
        }
        let input = body.into_inner();
        let outcome = input.outcome.trim().to_lowercase();
        if !matches!(outcome.as_str(), "passed" | "failed" | "no_show") {
            return Err(UnifiedError::bad_request(
                "outcome must be \"passed\", \"failed\", or \"no_show\"",
            ));
        }
        let outcome_notes = match &input.outcome_notes {
            Some(raw) => Some(require_text(raw, "outcome_notes", MAX_NOTES_LEN)?),
            None => None,
        };
        interview.outcome = Some(outcome.clone());
        interview.outcome_notes = outcome_notes;
        interview.status = "completed".to_string();
        interview.updated_at = Utc::now();
        state.interviews.insert(interview.id, interview.clone());
        state.emit(
            "recruitment.interview.outcome_recorded",
            json!({
                "interview_id": interview.id.to_string(),
                "applicant_id": interview.applicant_id.to_string(),
                "outcome": outcome,
                "actor": caller.actor,
            }),
        );
        Ok(interview)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
