// surveys module - Distribution handlers.
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

use super::constants::{DEFAULT_TOKEN_DAYS, MAX_ID_LEN, MAX_RECIPIENTS, PERM_SURVEYS_MANAGE, PERM_SURVEYS_VIEW, TRIGGERS};
use super::types::SurveyDistribution;
use super::requests::{CreateDistributionRequest, SendDistributionRequest};
use super::state::SurveysState;
use super::helpers::{
    caller, caller_company, created, err, load_distribution, load_survey, ok, optional_text,
    paginate, require_company, require_permission, require_tenant, valid_email,
};
use super::service::{schedule_distribution, send_distribution_invitations};

// Distribution handlers

pub(crate) async fn list_distributions(
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
        let trigger_filter = params.filters.get("trigger").cloned();
        let status_filter = params.filters.get("status").cloned();
        let mut items: Vec<SurveyDistribution> = state
            .distributions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|distribution| {
                if distribution.survey_id != survey.id {
                    return false;
                }
                if let Some(trigger) = &trigger_filter {
                    if distribution.trigger != *trigger {
                        return false;
                    }
                }
                if let Some(status) = &status_filter {
                    if distribution.status != *status {
                        return false;
                    }
                }
                true
            })
            .collect();
        items.sort_by_key(|distribution| distribution.created_at);
        Ok(paginate(items, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyDistribution> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let survey = load_survey(&state, &path)?;
        if survey.status != "published" {
            return Err(UnifiedError::conflict(
                "Distributions can only be created for a published survey version",
            ));
        }
        let input = body.into_inner();
        let trigger = input.trigger.trim().to_lowercase();
        if !TRIGGERS.contains(&trigger.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "trigger must be one of: {}",
                TRIGGERS.join(", ")
            )));
        }
        if trigger == "scheduled" && input.scheduled_at.is_none() {
            return Err(UnifiedError::bad_request(
                "A scheduled distribution needs scheduled_at",
            ));
        }
        if let Some(scheduled_at) = input.scheduled_at {
            if scheduled_at <= Utc::now() {
                return Err(UnifiedError::bad_request("scheduled_at must be in the future"));
            }
        }
        if input.recipients.len() > MAX_RECIPIENTS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_RECIPIENTS} recipients are allowed per distribution"
            )));
        }
        for recipient in &input.recipients {
            if recipient.recipient_id.trim().is_empty() || recipient.recipient_id.len() > MAX_ID_LEN {
                return Err(UnifiedError::bad_request(
                    "Every recipient needs a non-empty recipient_id (max 255 characters)",
                ));
            }
            if let Some(email) = &recipient.recipient_email {
                if !valid_email(email) {
                    return Err(UnifiedError::bad_request(format!("Invalid recipient email: {email}")));
                }
            }
        }
        let expires_in_days = match input.expires_in_days {
            Some(days) if (1..=365).contains(&days) => days,
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "expires_in_days must be between 1 and 365",
                ))
            }
            None => DEFAULT_TOKEN_DAYS,
        };
        let now = Utc::now();
        let mut distribution = SurveyDistribution {
            id: Uuid::now_v7(),
            survey_id: survey.id,
            trigger: trigger.clone(),
            department_id: optional_text(&input.department_id, "department_id", MAX_ID_LEN)?,
            product_id: optional_text(&input.product_id, "product_id", MAX_ID_LEN)?,
            agent_id: optional_text(&input.agent_id, "agent_id", MAX_ID_LEN)?,
            responsible_user_id: optional_text(
                &input.responsible_user_id,
                "responsible_user_id",
                MAX_ID_LEN,
            )?,
            scheduled_at: input.scheduled_at,
            expires_in_days,
            status: "pending".to_string(),
            scheduler_status: "not_scheduled".to_string(),
            scheduler_job_id: None,
            sent_count: 0,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        if !input.recipients.is_empty() {
            state
                .pending_recipients
                .insert(distribution.id, input.recipients);
        }
        if trigger == "scheduled" {
            schedule_distribution(&state, &mut distribution);
        }
        state.distributions.insert(distribution.id, distribution.clone());
        state.emit(
            "surveys.distribution.created",
            json!({
                "survey_id": survey.id.to_string(),
                "distribution_id": distribution.id.to_string(),
                "trigger": trigger,
                "scheduler_status": distribution.scheduler_status,
                "created_by": caller.actor,
            }),
        );
        Ok(distribution)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<SurveyDistribution> = (|| {
        let _tenant = require_tenant(&req)?;
        let _scope = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_VIEW)?;
        load_distribution(&state, &path)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_distribution(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<SurveysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<SendDistributionRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = require_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_SURVEYS_MANAGE)?;
        let mut distribution = load_distribution(&state, &path)?;
        if distribution.status == "cancelled" {
            return Err(UnifiedError::conflict("A cancelled distribution cannot be sent"));
        }
        if distribution.trigger == "ticket_closed" || distribution.trigger == "purchase" {
            return Err(UnifiedError::conflict(
                "Trigger distributions send through their trigger events, not through a manual send",
            ));
        }
        let input = body.into_inner();
        if input.recipients.len() > MAX_RECIPIENTS {
            return Err(UnifiedError::bad_request(format!(
                "at most {MAX_RECIPIENTS} recipients are allowed per send"
            )));
        }
        for recipient in &input.recipients {
            if recipient.recipient_id.trim().is_empty() || recipient.recipient_id.len() > MAX_ID_LEN {
                return Err(UnifiedError::bad_request(
                    "Every recipient needs a non-empty recipient_id (max 255 characters)",
                ));
            }
            if let Some(email) = &recipient.recipient_email {
                if !valid_email(email) {
                    return Err(UnifiedError::bad_request(format!("Invalid recipient email: {email}")));
                }
            }
        }
        let issued = send_distribution_invitations(&state, &mut distribution, input.recipients)?;
        Ok(json!({
            "distribution_id": distribution.id.to_string(),
            "issued": issued,
            "sent_count": distribution.sent_count,
            "status": distribution.status,
            "sent_by": caller.actor,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_distribution(
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
        let distribution = load_distribution(&state, &path)?;
        if distribution.status != "pending" {
            return Err(UnifiedError::conflict(
                "Only a pending distribution can be deleted",
            ));
        }
        let pending_ids: Vec<Uuid> = state
            .responses
            .iter()
            .filter(|entry| {
                entry.value().distribution_id == Some(distribution.id)
                    && entry.value().submitted_at.is_none()
            })
            .map(|entry| *entry.key())
            .collect();
        for id in pending_ids {
            state.responses.remove(&id);
        }
        state.pending_recipients.remove(&distribution.id);
        state.distributions.remove(&distribution.id);
        Ok(json!({ "deleted": true, "id": distribution.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
