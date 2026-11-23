// Newsletter module - campaign handlers.
//
// Split from `src/modules/newsletter.rs`; the module root re-exports the public API of this module.

use unified_core::error::UnifiedError;
use unified_core::json as api_json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::constants::{EVENT_SCHEDULER_ENQUEUE, PERM_NEWSLETTER_MANAGE, PERM_NEWSLETTER_VIEW};
use super::helpers::{hook_errors, matches_search, now_string, paginate, pick_hook_success, render_template, request_send, validate_connection_id, validate_email_address};
use super::sending::{send_campaign};
use super::state::{
    NewsletterState, authorize, caller_company, campaigns_store, lists_store, require_tenant,
};
use super::types::{Campaign, CampaignStats};

// ---------------------------------------------------------------------------
// Campaign handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CampaignInput {
    pub(crate) list_id: String,
    pub(crate) name: String,
    pub(crate) template_id: String,
    pub(crate) subject: Option<String>,
    pub(crate) preview_text: Option<String>,
    pub(crate) connection_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CampaignUpdateInput {
    pub(crate) name: Option<String>,
    pub(crate) template_id: Option<String>,
    pub(crate) subject: Option<String>,
    pub(crate) preview_text: Option<String>,
    pub(crate) connection_id: Option<String>,
}

pub(crate) async fn list_campaigns(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_VIEW) {
        return api_json::error_response(&error, debug);
    }
    let params = query.into_inner();
    let mut items: Vec<Campaign> = campaigns_store()
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|campaign| {
            matches_search(&format!("{} {}", campaign.name, campaign.subject), &params)
                && params
                    .filters
                    .get("list_id")
                    .is_none_or(|list_id| campaign.list_id == *list_id)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| campaign.status == *status)
        })
        .collect();
    items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    api_json::success_response(page.to_json(), 200)
}

pub(crate) async fn create_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    body: web::Json<CampaignInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if input.name.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Campaign name must not be empty"),
            debug,
        );
    }
    if input.template_id.trim().is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("template_id must not be empty (a GrapeJS email template id)"),
            debug,
        );
    }
    if let Err(error) = validate_connection_id(&input.connection_id) {
        return api_json::error_response(&error, debug);
    }
    if !lists_store().contains_key(&input.list_id) {
        return api_json::error_response(
            &UnifiedError::not_found("Newsletter list not found"),
            debug,
        );
    }
    let now = now_string();
    let campaign = Campaign {
        id: Uuid::now_v7().to_string(),
        list_id: input.list_id,
        name: input.name.trim().to_string(),
        template_id: input.template_id.trim().to_string(),
        subject: input.subject.unwrap_or_default(),
        preview_text: input.preview_text.filter(|v| !v.trim().is_empty()),
        connection_id: input.connection_id.filter(|v| !v.trim().is_empty()),
        status: "draft".to_string(),
        scheduled_at: None,
        sent_at: None,
        stats: CampaignStats::default(),
        created_at: now.clone(),
        updated_at: now,
    };
    campaigns_store().insert(campaign.id.clone(), campaign.clone());
    api_json::success_response(campaign, 201)
}

pub(crate) async fn get_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_VIEW) {
        return api_json::error_response(&error, debug);
    }
    match campaigns_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(campaign) => api_json::success_response(campaign, 200),
        None => api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug),
    }
}

pub(crate) async fn update_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<CampaignUpdateInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if let Err(error) = validate_connection_id(&input.connection_id) {
        return api_json::error_response(&error, debug);
    }
    let mut store = campaigns_store();
    let Some(mut campaign) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug);
    };
    if campaign.status != "draft" && campaign.status != "scheduled" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Only draft or scheduled campaigns can be updated (status is \"{}\")",
                campaign.status
            )),
            debug,
        );
    }
    if let Some(name) = input.name {
        if name.trim().is_empty() {
            return api_json::error_response(&UnifiedError::bad_request("Campaign name must not be empty"), debug);
        }
        campaign.name = name.trim().to_string();
    }
    if let Some(template_id) = input.template_id {
        campaign.template_id = template_id.trim().to_string();
    }
    if let Some(subject) = input.subject {
        campaign.subject = subject;
    }
    if let Some(preview_text) = input.preview_text {
        campaign.preview_text = if preview_text.trim().is_empty() { None } else { Some(preview_text) };
    }
    if let Some(connection_id) = input.connection_id {
        campaign.connection_id = if connection_id.trim().is_empty() { None } else { Some(connection_id) };
    }
    campaign.updated_at = now_string();
    let campaign = campaign.clone();
    drop(store);
    api_json::success_response(campaign, 200)
}

pub(crate) async fn delete_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    match campaigns_store().remove(path.as_str()) {
        Some(_) => api_json::success_response(
            json!({ "deleted": true, "id": path.as_str() }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScheduleInput {
    pub(crate) scheduled_at: String,
}

pub(crate) async fn schedule_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<ScheduleInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    let scheduled_at = match DateTime::parse_from_rfc3339(&input.scheduled_at) {
        Ok(when) => when.with_timezone(&Utc),
        Err(_) => {
            return api_json::error_response(
                &UnifiedError::bad_request("scheduled_at must be an RFC 3339 timestamp, for example 2026-12-01T09:00:00Z"),
                debug,
            )
        }
    };
    if scheduled_at <= Utc::now() {
        return api_json::error_response(
            &UnifiedError::bad_request("scheduled_at must be in the future (use send-now for immediate delivery)"),
            debug,
        );
    }
    let mut store = campaigns_store();
    let Some(mut campaign) = store.get_mut(path.as_str()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug);
    };
    if campaign.status != "draft" && campaign.status != "scheduled" {
        return api_json::error_response(
            &UnifiedError::conflict(format!(
                "Only draft campaigns can be scheduled (status is \"{}\")",
                campaign.status
            )),
            debug,
        );
    }
    campaign.status = "scheduled".to_string();
    campaign.scheduled_at = Some(scheduled_at.to_rfc3339());
    campaign.updated_at = now_string();
    let campaign = campaign.clone();
    drop(store);
    // Hand execution to the Scheduler module. The scheduler only knows
    // its built-in kinds today, so it may reject `newsletter.send`;
    // the campaign stays scheduled either way and the
    // `newsletter.campaign.send` hook is the execution entry point.
    let mut scheduler_accepted = false;
    let mut scheduler_note: Option<String> = None;
    if let Some(hooks) = state.hooks() {
        let answer = hooks.emit(
            EVENT_SCHEDULER_ENQUEUE,
            json!({
                "kind": "newsletter.send",
                "payload": { "campaign_id": campaign.id },
            }),
        );
        // Accept when any handler succeeds; aggregate the errors
        // when every handler failed.
        if pick_hook_success(&answer.results).is_some() {
            scheduler_accepted = true;
        } else if answer.results.is_empty() {
            scheduler_note = Some("Scheduler did not answer the scheduler.enqueue event".to_string());
        } else {
            let reasons = hook_errors(&answer.results);
            scheduler_note = Some(if reasons.is_empty() {
                "Scheduler rejected the newsletter.send job".to_string()
            } else {
                reasons.join("; ")
            });
        }
    } else {
        scheduler_note = Some("No hook bus is available".to_string());
    }
    api_json::success_response(
        json!({
            "campaign": campaign,
            "scheduler_accepted": scheduler_accepted,
            "scheduler_note": scheduler_note,
        }),
        200,
    )
}

pub(crate) async fn send_now_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let state: Arc<NewsletterState> = state.get_ref().clone();
    match send_campaign(&state, &path.into_inner()).await {
        Ok(result) => api_json::success_response(result, 200),
        Err(error) => api_json::error_response(&error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct CampaignTestSendInput {
    pub(crate) to: String,
}

pub(crate) async fn test_send_campaign(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
    body: web::Json<CampaignTestSendInput>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_MANAGE) {
        return api_json::error_response(&error, debug);
    }
    let input = body.into_inner();
    if let Err(error) = validate_email_address(&input.to) {
        return api_json::error_response(&error, debug);
    }
    let Some(campaign) = campaigns_store().get(path.as_str()).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug);
    };
    let Some(list) = lists_store().get(&campaign.list_id).map(|entry| entry.value().clone()) else {
        return api_json::error_response(&UnifiedError::not_found("Campaign list not found"), debug);
    };
    let (rendered_html, rendered_subject) = match render_template(&state, &campaign.template_id, &HashMap::new()) {
        Ok(rendered) => rendered,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let subject = if campaign.subject.trim().is_empty() {
        rendered_subject
    } else {
        campaign.subject.clone()
    };
    let sent = request_send(
        &state,
        json!({
            "connection_id": campaign.connection_id.clone().or_else(|| list.default_connection_id.clone()),
            "to": input.to,
            "from": list.sender_email,
            "from_name": list.sender_name,
            "subject": format!("[TEST] {subject}"),
            "body_html": rendered_html,
            "metadata": { "campaign_id": campaign.id, "test": "true" },
        }),
    );
    if sent {
        api_json::success_response(
            json!({ "queued": true, "campaign_id": campaign.id, "to": input.to }),
            200,
        )
    } else {
        api_json::error_response(
            &UnifiedError::new(
                "No email provider answered the email.send_requested event: no provider handler is registered",
                503,
            ),
            debug,
        )
    }
}

pub(crate) async fn campaign_stats(
    req: HttpRequest,
    state: web::Data<Arc<NewsletterState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = state.is_debug();
    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if let Err(error) = caller_company(&_tenant, &req) {
        return api_json::error_response(&error, debug);
    }
    if let Err(error) = authorize(&req, &state, PERM_NEWSLETTER_VIEW) {
        return api_json::error_response(&error, debug);
    }
    match campaigns_store().get(path.as_str()).map(|entry| entry.value().clone()) {
        Some(campaign) => api_json::success_response(
            json!({
                "campaign_id": campaign.id,
                "status": campaign.status,
                "stats": campaign.stats.view(),
            }),
            200,
        ),
        None => api_json::error_response(&UnifiedError::not_found("Campaign not found"), debug),
    }
}
