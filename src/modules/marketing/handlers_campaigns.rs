// Marketing Automation — Campaign and A/B variant handlers.

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
use super::handlers_segments::*;
use super::handlers_sequences::*;
use super::handlers_scores::*;
use super::handlers_audience::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Campaign handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct CampaignInput {
    name: String,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
    segment_id: Option<String>,
    newsletter_list_id: Option<String>,
    ab_enabled: Option<bool>,
    ab_winner_metric: Option<String>,
    ab_min_sample: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CampaignUpdateInput {
    name: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    template_id: Option<String>,
    segment_id: Option<String>,
    newsletter_list_id: Option<String>,
    ab_enabled: Option<bool>,
    ab_winner_metric: Option<String>,
    ab_min_sample: Option<u64>,
}

pub(crate) async fn list_campaigns(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<MarketingCampaign>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let params = query.into_inner();
        let mut items: Vec<MarketingCampaign> = state
            .campaigns
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|campaign| {
                matches_search(&format!("{} {}", campaign.name, campaign.subject), &params)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| campaign.status == *status)
                    && params
                        .filters
                        .get("segment_id")
                        .is_none_or(|segment| campaign.segment_id.as_deref() == Some(segment.as_str()))
            })
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
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

pub(crate) async fn create_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    body: web::Json<CampaignInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        let subject = require_text(&input.subject, "subject", 500)?;
        validate_campaign_input(&state, &input.segment_id, &input.ab_winner_metric)?;
        if input.body_text.is_none() && input.body_html.is_none() && input.template_id.is_none() {
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html (or a template_id)",
            ));
        }
        let now = now_string();
        let campaign = MarketingCampaign {
            id: new_id(),
            name,
            subject,
            body_text: input.body_text,
            body_html: input.body_html,
            template_id: input.template_id.filter(|value| !value.trim().is_empty()),
            segment_id: input.segment_id.filter(|value| !value.trim().is_empty()),
            newsletter_list_id: input
                .newsletter_list_id
                .filter(|value| !value.trim().is_empty()),
            status: "draft".to_string(),
            scheduled_at: None,
            sent_at: None,
            ab_enabled: input.ab_enabled.unwrap_or(false),
            ab_winner_metric: input
                .ab_winner_metric
                .unwrap_or_else(|| state.settings_snapshot().ab_winner_metric),
            ab_min_sample: input.ab_min_sample,
            winning_variant_id: None,
            stats: CampaignStats::default(),
            created_by: caller.actor.clone(),
            created_at: now.clone(),
            updated_at: now,
        };
        state.campaigns.insert(campaign.id.clone(), campaign.clone());
        state.emit(
            EVENT_MARKETING_CAMPAIGN_CREATED,
            json!({ "campaign_id": campaign.id, "name": campaign.name, "created_by": caller.actor }),
        );
        Ok(campaign)
    })();
    match result {
        Ok(campaign) => created(campaign),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CampaignUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let input = body.into_inner();
        validate_campaign_input(&state, &input.segment_id, &input.ab_winner_metric)?;
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only draft campaigns can be updated",
            ));
        }
        if let Some(name) = input.name {
            campaign.name = require_text(&name, "name", 200)?;
        }
        if let Some(subject) = input.subject {
            campaign.subject = require_text(&subject, "subject", 500)?;
        }
        if let Some(body_text) = input.body_text {
            campaign.body_text = Some(body_text);
        }
        if let Some(body_html) = input.body_html {
            campaign.body_html = Some(body_html);
        }
        if let Some(template_id) = input.template_id {
            campaign.template_id =
                if template_id.trim().is_empty() { None } else { Some(template_id) };
        }
        if let Some(segment_id) = input.segment_id {
            campaign.segment_id =
                if segment_id.trim().is_empty() { None } else { Some(segment_id) };
        }
        if let Some(list_id) = input.newsletter_list_id {
            campaign.newsletter_list_id =
                if list_id.trim().is_empty() { None } else { Some(list_id) };
        }
        if let Some(ab_enabled) = input.ab_enabled {
            campaign.ab_enabled = ab_enabled;
        }
        if let Some(metric) = input.ab_winner_metric {
            campaign.ab_winner_metric = metric;
        }
        if let Some(min_sample) = input.ab_min_sample {
            campaign.ab_min_sample = Some(min_sample);
        }
        campaign.updated_at = now_string();
        Ok(campaign.clone())
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_campaign(
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
        let id = parse_uuid(path.as_str(), "campaign")?;
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Only draft campaigns can be deleted",
            ));
        }
        state.campaigns.remove(&id.to_string());
        let variant_ids: Vec<String> = state
            .variants
            .iter()
            .filter(|entry| entry.value().campaign_id == id.to_string())
            .map(|entry| entry.key().clone())
            .collect();
        for variant_id in &variant_ids {
            state.variants.remove(variant_id);
        }
        Ok(json!({ "deleted": true, "id": id.to_string(), "removed_variants": variant_ids.len() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ScheduleInput {
    scheduled_at: String,
}

pub(crate) async fn schedule_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScheduleInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let input = body.into_inner();
        let when = parse_datetime(&input.scheduled_at, "scheduled_at")?;
        if when <= Utc::now() {
            return Err(UnifiedError::bad_request(
                "scheduled_at must be in the future",
            ));
        }
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" && campaign.status != "paused" {
            return Err(UnifiedError::conflict(
                "Only draft or paused campaigns can be scheduled",
            ));
        }
        if campaign.segment_id.is_none() {
            return Err(UnifiedError::unprocessable(
                "Campaign has no segment; assign a segment before scheduling",
            ));
        }
        campaign.status = "scheduled".to_string();
        campaign.scheduled_at = Some(when.to_rfc3339());
        campaign.updated_at = now_string();
        let scheduled_at = campaign.scheduled_at.clone();
        drop(campaign);
        state.emit(
            EVENT_SCHEDULER_ENQUEUE,
            json!({
                "job_id": new_id(),
                "queue": "marketing",
                "event": EVENT_MARKETING_CAMPAIGN_SEND,
                "payload": { "campaign_id": id.to_string() },
                "run_at": scheduled_at,
            }),
        );
        Ok(state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .expect("campaign still present"))
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn send_campaign_now(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        if !state.campaigns.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Campaign not found"));
        }
        execute_campaign_send(&state, &id.to_string())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn pause_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let mut campaign = state
            .campaigns
            .get_mut(&id.to_string())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "scheduled" && campaign.status != "sending" {
            return Err(UnifiedError::conflict(
                "Only scheduled or sending campaigns can be paused",
            ));
        }
        campaign.status = "paused".to_string();
        campaign.updated_at = now_string();
        Ok(campaign.clone())
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn resume_campaign(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<MarketingCampaign> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_SEND)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        resume_campaign_inner(&state, &id.to_string())
    })();
    match result {
        Ok(campaign) => ok(campaign),
        Err(error) => err(error, debug),
    }
}

/// Resume a paused campaign. Resuming to `scheduled` re-enqueues
/// the same scheduler job `schedule_campaign` emits: a job that
/// fired while the campaign was paused was correctly rejected by
/// `execute_campaign_send`, so without a fresh enqueue a resumed
/// campaign whose run time has already passed would sit in
/// `scheduled` forever and never send.
pub(crate) fn resume_campaign_inner(
    state: &Arc<MarketingState>,
    campaign_id: &str,
) -> UnifiedResult<MarketingCampaign> {
    let mut campaign = state
        .campaigns
        .get_mut(campaign_id)
        .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
    if campaign.status != "paused" {
        return Err(UnifiedError::conflict(
            "Only paused campaigns can be resumed",
        ));
    }
    campaign.status = if campaign.scheduled_at.is_some() {
        "scheduled".to_string()
    } else {
        "draft".to_string()
    };
    campaign.updated_at = now_string();
    let resumed = campaign.clone();
    let scheduled_at = resumed.scheduled_at.clone();
    drop(campaign);
    if resumed.status == "scheduled" {
        state.emit(
            EVENT_SCHEDULER_ENQUEUE,
            json!({
                "job_id": new_id(),
                "queue": "marketing",
                "event": EVENT_MARKETING_CAMPAIGN_SEND,
                "payload": { "campaign_id": campaign_id },
                "run_at": scheduled_at,
            }),
        );
    }
    Ok(resumed)
}

pub(crate) async fn get_campaign_stats(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        // Recompute open/click counts from the tracking events
        // actually received, so the stats never claim tracking the
        // module did not get. The stored counters are not added on
        // top: tracking events are the single source of truth here.
        let mut stats = campaign.stats.clone();
        stats.opened = 0;
        stats.clicked = 0;
        let mut unique_openers: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut unique_clickers: std::collections::HashSet<String> = std::collections::HashSet::new();
        for entry in state.tracking_events.iter() {
            let event = entry.value();
            if event.campaign_id != id.to_string() {
                continue;
            }
            if event.kind == "open" {
                stats.opened += 1;
                unique_openers.insert(event.contact_ref.clone());
            } else if event.kind == "click" {
                stats.clicked += 1;
                unique_clickers.insert(event.contact_ref.clone());
            }
        }
        stats.unique_opened = unique_openers.len() as u64;
        stats.unique_clicked = unique_clickers.len() as u64;
        let variants = campaign_variants(&state, &id.to_string());
        Ok(json!({
            "campaign_id": campaign.id,
            "status": campaign.status,
            "stats": stats.view(),
            "winning_variant_id": campaign.winning_variant_id,
            "variants": variants,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

// ---------------------------------------------------------------------------
// A/B variant handlers.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub(crate) struct VariantInput {
    name: String,
    subject: String,
    body_text: Option<String>,
    body_html: Option<String>,
    weight_percent: Option<u8>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VariantUpdateInput {
    name: Option<String>,
    subject: Option<String>,
    body_text: Option<String>,
    body_html: Option<String>,
    weight_percent: Option<u8>,
}

pub(crate) fn variant_weight_total(state: &MarketingState, campaign_id: &str, except: Option<&str>) -> u32 {
    state
        .variants
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|variant| {
            variant.campaign_id == campaign_id
                && Some(variant.id.as_str()) != except
        })
        .map(|variant| variant.weight_percent as u32)
        .sum()
}

pub(crate) async fn list_variants(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Vec<CampaignVariant>> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_VIEW)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        if !state.campaigns.contains_key(&id.to_string()) {
            return Err(UnifiedError::not_found("Campaign not found"));
        }
        Ok(campaign_variants(&state, &id.to_string()))
    })();
    match result {
        Ok(variants) => ok(variants),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<VariantInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CampaignVariant> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Variants can only be added to draft campaigns",
            ));
        }
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 200)?;
        let subject = require_text(&input.subject, "subject", 500)?;
        if input.body_text.is_none() && input.body_html.is_none() {
            return Err(UnifiedError::bad_request(
                "Provide body_text or body_html for the variant",
            ));
        }
        let weight = input
            .weight_percent
            .unwrap_or_else(|| state.settings_snapshot().default_split_percent_a);
        if weight == 0 || weight > 100 {
            return Err(UnifiedError::bad_request(
                "weight_percent must be between 1 and 100",
            ));
        }
        if variant_weight_total(&state, &id.to_string(), None) + weight as u32 > 100 {
            return Err(UnifiedError::bad_request(
                "Variant weights must sum to at most 100 (the remainder goes to the base content)",
            ));
        }
        let now = now_string();
        let variant = CampaignVariant {
            id: new_id(),
            campaign_id: id.to_string(),
            name,
            subject,
            body_text: input.body_text,
            body_html: input.body_html,
            weight_percent: weight,
            sent: 0,
            opened: 0,
            clicked: 0,
            created_at: now,
        };
        state.variants.insert(variant.id.clone(), variant.clone());
        Ok(variant)
    })();
    match result {
        Ok(variant) => created(variant),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<VariantUpdateInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CampaignVariant> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (campaign_id, variant_id) = path.into_inner();
        let campaign_uuid = parse_uuid(&campaign_id, "campaign")?;
        let variant_uuid = parse_uuid(&variant_id, "variant")?;
        let campaign = state
            .campaigns
            .get(&campaign_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Variants can only be changed on draft campaigns",
            ));
        }
        let input = body.into_inner();
        if let Some(weight) = input.weight_percent {
            if weight == 0 || weight > 100 {
                return Err(UnifiedError::bad_request(
                    "weight_percent must be between 1 and 100",
                ));
            }
            let others =
                variant_weight_total(&state, &campaign_uuid.to_string(), Some(&variant_uuid.to_string()));
            if others + weight as u32 > 100 {
                return Err(UnifiedError::bad_request(
                    "Variant weights must sum to at most 100",
                ));
            }
        }
        let mut variant = state
            .variants
            .get_mut(&variant_uuid.to_string())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        if variant.campaign_id != campaign_uuid.to_string() {
            return Err(UnifiedError::not_found("Variant not found"));
        }
        if let Some(name) = input.name {
            variant.name = require_text(&name, "name", 200)?;
        }
        if let Some(subject) = input.subject {
            variant.subject = require_text(&subject, "subject", 500)?;
        }
        if let Some(body_text) = input.body_text {
            variant.body_text = Some(body_text);
        }
        if let Some(body_html) = input.body_html {
            variant.body_html = Some(body_html);
        }
        if let Some(weight) = input.weight_percent {
            variant.weight_percent = weight;
        }
        Ok(variant.clone())
    })();
    match result {
        Ok(variant) => ok(variant),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_variant(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let (campaign_id, variant_id) = path.into_inner();
        let campaign_uuid = parse_uuid(&campaign_id, "campaign")?;
        let variant_uuid = parse_uuid(&variant_id, "variant")?;
        let campaign = state
            .campaigns
            .get(&campaign_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if campaign.status != "draft" {
            return Err(UnifiedError::conflict(
                "Variants can only be removed from draft campaigns",
            ));
        }
        let variant = state
            .variants
            .get(&variant_uuid.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Variant not found"))?;
        if variant.campaign_id != campaign_uuid.to_string() {
            return Err(UnifiedError::not_found("Variant not found"));
        }
        state.variants.remove(&variant_uuid.to_string());
        Ok(json!({ "deleted": true, "id": variant_uuid.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct AbDecideInput {
    metric: Option<String>,
    min_sample: Option<u64>,
}

/// Pick the A/B winner from real send and tracking data. Refuses
/// (422) when any variant is below the minimum sample or when no
/// tracking event exists for the chosen metric: a winner picked
/// from no data would be fabricated.
pub(crate) async fn ab_decide(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<MarketingState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<AbDecideInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_MARKETING_MANAGE)?;
        let id = parse_uuid(path.as_str(), "campaign")?;
        let input = body.into_inner();
        let campaign = state
            .campaigns
            .get(&id.to_string())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Campaign not found"))?;
        if !campaign.ab_enabled {
            return Err(UnifiedError::unprocessable(
                "A/B testing is not enabled for this campaign",
            ));
        }
        let metric = input.metric.unwrap_or_else(|| campaign.ab_winner_metric.clone());
        if metric != "open_rate" && metric != "click_rate" {
            return Err(UnifiedError::bad_request(
                "metric must be \"open_rate\" or \"click_rate\"",
            ));
        }
        let min_sample = input
            .min_sample
            .or(campaign.ab_min_sample)
            .unwrap_or_else(|| state.settings_snapshot().ab_min_sample_per_variant);
        let variants = campaign_variants(&state, &id.to_string());
        if variants.is_empty() {
            return Err(UnifiedError::unprocessable(
                "Campaign has no A/B variants to compare",
            ));
        }
        // Base content counts as a candidate too, using the sends
        // recorded without a variant id. Base opens/clicks are the
        // tracking events whose reference identifies one of those
        // base sends (by contact id or email).
        let base_sends: Vec<MarketingSend> = state
            .sends
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|send| {
                send.campaign_id == Some(id.to_string())
                    && send.variant_id.is_none()
                    && send.status != "skipped"
            })
            .collect();
        let base_sent = base_sends.len() as u64;
        let base_refs: std::collections::HashSet<String> = base_sends
            .iter()
            .flat_map(|send| [send.contact_id.clone(), send.email.clone()])
            .collect();
        let base_opened = state
            .tracking_events
            .iter()
            .filter(|entry| {
                entry.value().campaign_id == id.to_string()
                    && entry.value().kind == "open"
                    && base_refs.contains(&entry.value().contact_ref)
            })
            .count() as u64;
        let base_clicked = state
            .tracking_events
            .iter()
            .filter(|entry| {
                entry.value().campaign_id == id.to_string()
                    && entry.value().kind == "click"
                    && base_refs.contains(&entry.value().contact_ref)
            })
            .count() as u64;
        let mut candidates: Vec<(String, String, u64, u64)> = Vec::new();
        for variant in &variants {
            let rate_count = if metric == "click_rate" { variant.clicked } else { variant.opened };
            candidates.push((
                variant.id.clone(),
                variant.name.clone(),
                variant.sent,
                rate_count,
            ));
        }
        candidates.push((
            "base".to_string(),
            "base".to_string(),
            base_sent,
            if metric == "click_rate" { base_clicked } else { base_opened },
        ));
        let below_min: Vec<&(String, String, u64, u64)> = candidates
            .iter()
            .filter(|candidate| candidate.2 < min_sample)
            .collect();
        if !below_min.is_empty() {
            return Err(UnifiedError::unprocessable(format!(
                "Not enough data: every variant needs at least {min_sample} sends before a winner can be picked ({} below minimum)",
                below_min.len()
            )));
        }
        let total_events: u64 = candidates.iter().map(|candidate| candidate.3).sum();
        if total_events == 0 {
            return Err(UnifiedError::unprocessable(format!(
                "No {metric} tracking events have been received for this campaign, so no winner can be picked from real data"
            )));
        }
        let mut best: Option<&(String, String, u64, u64)> = None;
        let mut best_rate = -1.0f64;
        for candidate in &candidates {
            let rate = candidate.3 as f64 / candidate.2 as f64;
            if rate > best_rate {
                best_rate = rate;
                best = Some(candidate);
            }
        }
        let winner = best.expect("at least one candidate exists");
        if let Some(mut stored) = state.campaigns.get_mut(&id.to_string()) {
            stored.winning_variant_id = if winner.0 == "base" {
                None
            } else {
                Some(winner.0.clone())
            };
            stored.updated_at = now_string();
        }
        state.emit(
            EVENT_MARKETING_CAMPAIGN_AB_WINNER,
            json!({
                "campaign_id": id.to_string(),
                "metric": metric,
                "min_sample": min_sample,
                "winner_variant_id": winner.0,
                "winner_name": winner.1,
                "winner_rate": best_rate,
            }),
        );
        Ok(json!({
            "campaign_id": id.to_string(),
            "metric": metric,
            "min_sample": min_sample,
            "winner_variant_id": winner.0,
            "winner_name": winner.1,
            "winner_rate": (best_rate * 10000.0).round() / 100.0,
            "candidates": candidates.iter().map(|candidate| json!({
                "variant_id": candidate.0,
                "name": candidate.1,
                "sent": candidate.2,
                "events": candidate.3,
                "rate": if candidate.2 == 0 { 0.0 } else { (candidate.3 as f64 / candidate.2 as f64 * 10000.0).round() / 100.0 },
            })).collect::<Vec<Value>>(),
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
    use std::sync::Mutex;

    fn test_campaign(id: &str, status: &str, scheduled_at: Option<&str>) -> MarketingCampaign {
        MarketingCampaign {
            id: id.to_string(),
            name: "Test campaign".to_string(),
            subject: "Hello".to_string(),
            body_text: Some("Hello".to_string()),
            body_html: None,
            template_id: None,
            segment_id: None,
            newsletter_list_id: None,
            status: status.to_string(),
            scheduled_at: scheduled_at.map(str::to_string),
            sent_at: None,
            ab_enabled: false,
            ab_winner_metric: "open_rate".to_string(),
            ab_min_sample: None,
            winning_variant_id: None,
            stats: CampaignStats::default(),
            created_by: "tester".to_string(),
            created_at: now_string(),
            updated_at: now_string(),
        }
    }

    #[test]
    fn resume_campaign_reenqueues_scheduler_job_when_scheduled() {
        let state = Arc::new(MarketingState::new());
        let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen_clone = Arc::clone(&seen);
        let bus = Arc::new(HookBus::new());
        bus.register(
            EVENT_SCHEDULER_ENQUEUE,
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                seen_clone
                    .lock()
                    .expect("enqueue log lock poisoned")
                    .push(ctx.data.clone());
            }),
        );
        state.set_hooks(bus);
        // Paused with a run time already in the past: without a
        // fresh enqueue this campaign would sit in `scheduled`
        // forever, because the job that fired while it was paused
        // was (correctly) rejected.
        state.campaigns.insert(
            "camp-1".to_string(),
            test_campaign("camp-1", "paused", Some("2026-01-01T00:00:00+00:00")),
        );
        let resumed = resume_campaign_inner(&state, "camp-1").unwrap();
        assert_eq!(resumed.status, "scheduled");
        let events = seen.lock().expect("enqueue log lock poisoned");
        assert_eq!(events.len(), 1);
        assert_eq!(
            events[0].get("event").and_then(Value::as_str),
            Some(EVENT_MARKETING_CAMPAIGN_SEND)
        );
        assert_eq!(
            events[0]
                .get("payload")
                .and_then(|payload| payload.get("campaign_id"))
                .and_then(Value::as_str),
            Some("camp-1")
        );
        assert_eq!(
            events[0].get("run_at").and_then(Value::as_str),
            Some("2026-01-01T00:00:00+00:00")
        );
        drop(events);
        // Resuming a paused campaign with no scheduled time goes
        // back to draft and enqueues nothing.
        state.campaigns.insert(
            "camp-2".to_string(),
            test_campaign("camp-2", "paused", None),
        );
        let resumed = resume_campaign_inner(&state, "camp-2").unwrap();
        assert_eq!(resumed.status, "draft");
        assert_eq!(seen.lock().expect("enqueue log lock poisoned").len(), 1);
    }
}
