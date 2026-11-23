// Marketing Automation — Audience, suppression, send, and settings handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::service_sequences::*;
use super::handlers_campaigns::*;
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Audience, unsubscribe, suppressions, sends, settings.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct AudienceSyncInput {
    pub(crate) contact_id: String,
    pub(crate) email: String,
    pub(crate) name: String,
    pub(crate) company: Option<String>,
    pub(crate) phone: Option<String>,
    pub(crate) tags: Option<Vec<String>>,
    pub(crate) source: Option<String>,
    pub(crate) fields: Option<HashMap<String, Value>>,
    /// Consent signal carried by the sync payload: when present it
    /// seeds the consent cache for the marketing purpose.
    pub(crate) consented: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AudienceSyncBody {
    contacts: Vec<AudienceSyncInput>,
}

pub(crate) async fn sync_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<AudienceSyncBody>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        if input.contacts.is_empty() || input.contacts.len() > 1000 {
            return Err(UnifiedError::bad_request(
                "Provide 1 to 1000 contacts per sync request",
            ));
        }
        let mut upserted = 0u64;
        let mut errors: Vec<String> = Vec::new();
        for contact in input.contacts {
            match upsert_audience_contact(&state, contact) {
                Ok(_) => upserted += 1,
                Err(error) => errors.push(error.message),
            }
        }
        Ok(json!({ "upserted": upserted, "errors": errors }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_audience(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AudienceContact>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<AudienceContact> = state
            .audience
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|contact| {
                matches_search(
                    &format!("{} {} {}", contact.name, contact.email, contact.company.clone().unwrap_or_default()),
                    &params,
                ) && params.filters.get("tag").is_none_or(|tag| {
                    contact.tags.iter().any(|item| item == tag)
                }) && params.filters.get("source").is_none_or(|source| {
                    contact.source.as_deref() == Some(source.as_str())
                })
            })
            .collect();
        items.sort_by(|a, b| a.email.cmp(&b.email));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct UnsubscribeInput {
    email: String,
    campaign_id: Option<String>,
    reason: Option<String>,
    /// Signed recipient token minted by
    /// `MarketingState::sign_unsubscribe_token`. When present, the
    /// request is a recipient self-unsubscribe and needs no admin
    /// caller headers; the token binds the email (and campaign,
    /// when given) and expires. Without a token the endpoint stays
    /// admin-only (`PERM_MARKETING_MANAGE`).
    token: Option<String>,
}

pub(crate) async fn unsubscribe(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<UnsubscribeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Suppression> = (|| {
        let input = body.into_inner();
        if let Some(token) = input.token.as_deref() {
            let (email, token_campaign) = state.verify_unsubscribe_token(token)?;
            if !input.email.trim().is_empty() && input.email.trim().to_lowercase() != email {
                return Err(UnifiedError::unauthorized(
                    "Unsubscribe token does not match the email address",
                ));
            }
            if let Some(campaign_id) = input.campaign_id.as_deref() {
                if token_campaign.as_deref() != Some(campaign_id) {
                    return Err(UnifiedError::unauthorized(
                        "Unsubscribe token does not match the campaign",
                    ));
                }
            }
            return record_unsubscribe(
                &state,
                &email,
                token_campaign.as_deref(),
                input.reason.as_deref(),
            );
        }
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        record_unsubscribe(
            &state,
            &input.email,
            input.campaign_id.as_deref(),
            input.reason.as_deref(),
        )
    })();
    match result {
        Ok(record) => created(record),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_suppressions(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Suppression>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<Suppression> = state
            .suppressions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|record| {
                matches_search(&record.email, &params)
                    && params
                        .filters
                        .get("scope")
                        .is_none_or(|scope| record.scope == *scope)
                    && params
                        .filters
                        .get("campaign_id")
                        .is_none_or(|campaign| record.campaign_id.as_deref() == Some(campaign.as_str()))
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_suppression(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "suppression")?;
        match state.suppressions.remove(&id.to_string()) {
            Some((_, record)) => Ok(json!({
                "deleted": true,
                "id": id.to_string(),
                "email": record.email,
            })),
            None => Err(UnifiedError::not_found("Suppression record not found")),
        }
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_sends(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<MarketingSend>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<MarketingSend> = state
            .sends
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|send| {
                matches_search(&format!("{} {}", send.email, send.contact_id), &params)
                    && params.filters.get("status").is_none_or(|status| send.status == *status)
                    && params.filters.get("campaign_id").is_none_or(|campaign| {
                        send.campaign_id.as_deref() == Some(campaign.as_str())
                    })
                    && params.filters.get("sequence_id").is_none_or(|sequence| {
                        send.sequence_id.as_deref() == Some(sequence.as_str())
                    })
                    && params.filters.get("contact_id").is_none_or(|contact| {
                        send.contact_id == *contact
                    })
            })
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_send(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingSend> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "send")?;
        state
            .sends
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Send record not found"))
    })();
    match result {
        Ok(send) => ok(send),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct SettingsInput {
    default_split_percent_a: Option<u8>,
    ab_min_sample_per_variant: Option<u64>,
    ab_winner_metric: Option<String>,
    warm_threshold: Option<i64>,
    hot_threshold: Option<i64>,
    default_score_decay_days: Option<u64>,
    max_sends_per_run: Option<u64>,
    segment_fields: Option<Vec<String>>,
    segment_operators: Option<Vec<String>>,
    score_actions: Option<Vec<String>>,
    consent_purpose: Option<String>,
}

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        Ok(state.settings_snapshot())
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<SettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingSettings> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        if let Some(split) = input.default_split_percent_a {
            if split == 0 || split > 100 {
                return Err(UnifiedError::bad_request(
                    "default_split_percent_a must be between 1 and 100",
                ));
            }
        }
        if let Some(min_sample) = input.ab_min_sample_per_variant {
            if min_sample == 0 || min_sample > 100_000 {
                return Err(UnifiedError::bad_request(
                    "ab_min_sample_per_variant must be between 1 and 100000",
                ));
            }
        }
        if let Some(metric) = &input.ab_winner_metric {
            if metric != "open_rate" && metric != "click_rate" {
                return Err(UnifiedError::bad_request(
                    "ab_winner_metric must be \"open_rate\" or \"click_rate\"",
                ));
            }
        }
        if let (Some(warm), Some(hot)) = (input.warm_threshold, input.hot_threshold) {
            if warm >= hot {
                return Err(UnifiedError::bad_request(
                    "warm_threshold must be below hot_threshold",
                ));
            }
        }
        if let Some(max) = input.max_sends_per_run {
            if max == 0 || max > 100_000 {
                return Err(UnifiedError::bad_request(
                    "max_sends_per_run must be between 1 and 100000",
                ));
            }
        }
        if let Some(fields) = &input.segment_fields {
            if fields.is_empty() || fields.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "segment_fields must contain 1 to 100 field names",
                ));
            }
        }
        if let Some(operators) = &input.segment_operators {
            if operators.is_empty() || operators.len() > 50 {
                return Err(UnifiedError::bad_request(
                    "segment_operators must contain 1 to 50 operators",
                ));
            }
        }
        let mut settings = state.settings_snapshot();
        if let Some(split) = input.default_split_percent_a {
            settings.default_split_percent_a = split;
        }
        if let Some(min_sample) = input.ab_min_sample_per_variant {
            settings.ab_min_sample_per_variant = min_sample;
        }
        if let Some(metric) = input.ab_winner_metric {
            settings.ab_winner_metric = metric;
        }
        if let Some(warm) = input.warm_threshold {
            settings.warm_threshold = warm;
        }
        if let Some(hot) = input.hot_threshold {
            settings.hot_threshold = hot;
        }
        if let Some(days) = input.default_score_decay_days {
            settings.default_score_decay_days = Some(days);
        }
        if let Some(max) = input.max_sends_per_run {
            settings.max_sends_per_run = max;
        }
        if let Some(fields) = input.segment_fields {
            settings.segment_fields = fields;
        }
        if let Some(operators) = input.segment_operators {
            settings.segment_operators = operators;
        }
        if let Some(actions) = input.score_actions {
            settings.score_actions = actions;
        }
        if let Some(purpose) = input.consent_purpose {
            settings.consent_purpose = require_text(&purpose, "consent_purpose", 100)?;
        }
        if settings.warm_threshold >= settings.hot_threshold {
            return Err(UnifiedError::bad_request(
                "warm_threshold must be below hot_threshold",
            ));
        }
        {
            let mut stored = state
                .settings
                .write()
                .expect("marketing settings lock poisoned");
            *stored = settings.clone();
        }
        Ok(settings)
    })();
    match result {
        Ok(settings) => ok(settings),
        Err(error) => err(error, debug),
    }
}
