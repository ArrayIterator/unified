//! HTTP handlers for social campaigns: campaigns grouping
//! posts (name, date range, goal as data), with a per-post
//! status rollup and a per-campaign metrics rollup computed
//! live from the post and metric rows.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::helpers::{
    caller, caller_company, campaign_is_editable, created, err, load_campaign, ok, optional_text,
    paginate, parse_date_filter, parse_optional_timestamp, parse_timestamp, record_audit,
    require_caller_company, require_manage, require_tenant, require_text, require_view,
    validate_campaign_status, validate_date_range, validate_goal_target, validate_metric_name,
};
use super::state::SocialMarketingState;
use super::types::{
    CampaignStatus, CreateCampaignRequest, EVENT_CAMPAIGN_CREATED, MAX_DESCRIPTION_LEN,
    MAX_NAME_LEN, SocialCampaign, UpdateCampaignRequest, campaign_can_transition,
};

/// Enrich a campaign view with its live post counts, its
/// summed metrics, and its engagement rate (basis points).
fn campaign_view(
    state: &SocialMarketingState,
    tenant_id: &str,
    campaign: &SocialCampaign,
) -> Value {
    let counts = state.post_counts_of_campaign(tenant_id, campaign.id);
    let totals = state.rollup_of_campaign(tenant_id, campaign.id);
    let engagement_bps = super::types::engagement_bps(&totals);
    let goal_progress_bps = match (campaign.goal_metric.as_deref(), campaign.goal_target) {
        (Some(metric), Some(target)) if target > 0 => {
            let actual = match metric {
                "impressions" => totals.impressions,
                "likes" => totals.likes,
                "shares" => totals.shares,
                "comments" => totals.comments,
                "clicks" => totals.clicks,
                _ => totals.other,
            };
            Some(actual * 10_000 / target)
        }
        _ => None,
    };
    let mut view = serde_json::to_value(campaign).unwrap_or(Value::Null);
    if let Some(object) = view.as_object_mut() {
        object.insert(
            "post_counts".to_string(),
            serde_json::to_value(&counts).unwrap_or(Value::Null),
        );
        object.insert(
            "metrics".to_string(),
            serde_json::to_value(&totals).unwrap_or(Value::Null),
        );
        object.insert("engagement_bps".to_string(), Value::from(engagement_bps));
        object.insert(
            "goal_progress_bps".to_string(),
            match goal_progress_bps {
                Some(value) => Value::from(value),
                None => Value::Null,
            },
        );
    }
    view
}

/// List campaigns (paginated, filter by status, goal
/// metric; search by name/goal; filter by created date
/// range with `from` / `to`).
pub(crate) async fn list_campaigns(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let goal_metric_filter = params.filters.get("goal_metric").cloned();
        let from_filter = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to_filter = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.campaigns_of(&tenant_id);
        items.retain(|campaign| {
            if let Some(status) = &status_filter
                && campaign.status.as_str() != status
            {
                return false;
            }
            if let Some(goal_metric) = &goal_metric_filter {
                match &campaign.goal_metric {
                    Some(metric) if metric == goal_metric => {}
                    _ => return false,
                }
            }
            if let Some(from) = from_filter
                && campaign.created_at < from
            {
                return false;
            }
            if let Some(to) = to_filter
                && campaign.created_at > to
            {
                return false;
            }
            if let Some(needle) = &search
                && !campaign.name.to_lowercase().contains(needle)
                && !campaign.goal.to_lowercase().contains(needle)
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| campaign_view(&state, &tenant_id, item))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Create a campaign (manager). The goal is data: a
/// free-text goal label, an optional metric slug the goal
/// is measured in, and an optional integer target count.
/// The campaign starts as a draft.
pub(crate) async fn create_campaign(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateCampaignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let name = require_text(&input.name, "name", MAX_NAME_LEN)?;
        let description = optional_text(&input.description, "description", MAX_DESCRIPTION_LEN)?;
        let goal = require_text(&input.goal, "goal", MAX_DESCRIPTION_LEN)?;
        let goal_metric = match &input.goal_metric {
            Some(metric) => Some(validate_metric_name(metric)?),
            None => None,
        };
        let goal_target = match input.goal_target {
            Some(target) => Some(validate_goal_target(target)?),
            None => None,
        };
        let starts_at = parse_optional_timestamp(&input.starts_at, "starts_at")?;
        let ends_at = parse_optional_timestamp(&input.ends_at, "ends_at")?;
        validate_date_range(starts_at, ends_at)?;
        let now = Utc::now();
        let campaign = SocialCampaign {
            id: Uuid::new_v4(),
            tenant_id: tenant_id.clone(),
            name,
            description,
            goal,
            goal_metric,
            goal_target,
            starts_at,
            ends_at,
            status: CampaignStatus::Draft,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        let view = campaign_view(&state, &tenant_id, &campaign);
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "create",
            &caller.actor,
            None,
            Some(view.clone()),
        );
        state.emit(
            EVENT_CAMPAIGN_CREATED,
            json!({
                "tenant_id": tenant_id,
                "campaign_id": campaign.id.to_string(),
                "name": campaign.name,
                "goal": campaign.goal,
                "created_by": caller.actor,
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Get one campaign with its live post counts, summed
/// metrics, engagement rate, and goal progress.
pub(crate) async fn get_campaign(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        Ok(campaign_view(&state, &tenant_id, &campaign))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Update a campaign (manager). Only drafts and cancelled
/// campaigns are editable in full; the status moves through
/// its own machine (draft -> active -> paused / completed /
/// cancelled) and is validated against it.
pub(crate) async fn update_campaign(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<UpdateCampaignRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let mut campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        let before = campaign_view(&state, &tenant_id, &campaign);
        // Field edits are judged against the status the
        // campaign had before this call, not the status
        // this call moves it to.
        let was_editable = campaign_is_editable(campaign.status);
        if let Some(status) = &input.status {
            let target = validate_campaign_status(status)?;
            if !campaign_can_transition(campaign.status, target) {
                return Err(UnifiedError::conflict(format!(
                    "Campaign in status {} cannot move to {}",
                    campaign.status.as_str(),
                    target.as_str()
                )));
            }
            campaign.status = target;
        }
        let has_field_update = input.name.is_some()
            || input.description.is_some()
            || input.goal.is_some()
            || input.goal_metric.is_some()
            || input.goal_target.is_some()
            || input.starts_at.is_some()
            || input.ends_at.is_some();
        if has_field_update && !was_editable {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be edited",
                campaign.status.as_str()
            )));
        }
        if let Some(name) = &input.name {
            campaign.name = require_text(name, "name", MAX_NAME_LEN)?;
        }
        if let Some(description) = &input.description {
            campaign.description = match description {
                Some(text) => Some(require_text(text, "description", MAX_DESCRIPTION_LEN)?),
                None => None,
            };
        }
        if let Some(goal) = &input.goal {
            campaign.goal = require_text(goal, "goal", MAX_DESCRIPTION_LEN)?;
        }
        if let Some(goal_metric) = &input.goal_metric {
            campaign.goal_metric = match goal_metric {
                Some(metric) => Some(validate_metric_name(metric)?),
                None => None,
            };
        }
        if let Some(goal_target) = &input.goal_target {
            campaign.goal_target = match goal_target {
                Some(target) => Some(validate_goal_target(*target)?),
                None => None,
            };
        }
        if let Some(starts_at) = &input.starts_at {
            campaign.starts_at = match starts_at {
                Some(raw) => Some(parse_timestamp(raw, "starts_at")?),
                None => None,
            };
        }
        if let Some(ends_at) = &input.ends_at {
            campaign.ends_at = match ends_at {
                Some(raw) => Some(parse_timestamp(raw, "ends_at")?),
                None => None,
            };
        }
        validate_date_range(campaign.starts_at, campaign.ends_at)?;
        campaign.updated_at = Utc::now();
        let view = campaign_view(&state, &tenant_id, &campaign);
        state.campaigns.insert(campaign.id, campaign.clone());
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "update",
            &caller.actor,
            Some(before),
            Some(view.clone()),
        );
        Ok(view)
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Delete a campaign (manager). Only drafts and cancelled
/// campaigns can be deleted; its posts keep their
/// campaign id as a dangling reference, exactly like the
/// SMS module keeps message rows when a campaign is
/// deleted.
pub(crate) async fn delete_campaign(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        if !campaign_is_editable(campaign.status) {
            return Err(UnifiedError::conflict(format!(
                "Campaign in status {} cannot be deleted",
                campaign.status.as_str()
            )));
        }
        state.campaigns.remove(&campaign.id);
        record_audit(
            &state,
            &tenant_id,
            "campaign",
            &campaign.id.to_string(),
            "delete",
            &caller.actor,
            Some(campaign_view(&state, &tenant_id, &campaign)),
            None,
        );
        Ok(json!({ "deleted": true, "campaign_id": campaign.id.to_string() }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// List the posts of one campaign (paginated, filter by
/// status, network, account; search by text/hashtags).
pub(crate) async fn list_campaign_posts(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        let params = query.into_inner();
        let status_filter = params.filters.get("status").cloned();
        let network_filter = params.filters.get("network_id").cloned();
        let account_filter = params.filters.get("account_id").cloned();
        let search = params.search.clone().map(|needle| needle.to_lowercase());
        let mut items = state.posts_of_campaign(&tenant_id, campaign.id);
        items.retain(|post| {
            if let Some(status) = &status_filter
                && post.status.as_str() != status
            {
                return false;
            }
            if let Some(network_id) = &network_filter
                && post.network_id.to_string() != *network_id
            {
                return false;
            }
            if let Some(account_id) = &account_filter
                && !post.account_ids.contains(account_id)
            {
                return false;
            }
            if let Some(needle) = &search
                && !post.text.to_lowercase().contains(needle)
                && !post.hashtags.iter().any(|tag| tag.contains(needle))
            {
                return false;
            }
            true
        });
        let views: Vec<Value> = items
            .iter()
            .map(|item| serde_json::to_value(item).unwrap_or(Value::Null))
            .collect();
        Ok(paginate(views, &params).to_json())
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Per-campaign metrics rollup: summed metrics of every
/// post in the campaign, the post counts by status, the
/// engagement rate in basis points, and the goal progress
/// in basis points when the campaign names a goal metric
/// and target.
pub(crate) async fn campaign_rollup(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_view(&ctx, &caller.role)?;
        let campaign = load_campaign(&state, &tenant_id, &path.into_inner())?;
        Ok(campaign_view(&state, &tenant_id, &campaign))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
