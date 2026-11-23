//! HTTP handlers for social analytics: per-post metric
//! rows, manual entry, the provider verification-token
//! metrics-ingest webhook, and the per-account rollup.
//!
//! Metrics arrive only through the webhook or through
//! manual entry — this module never fetches live numbers
//! from a network, so a metric row is always a reported
//! value, never a scraped one. Re-ingesting the same
//! metric from the same source replaces the value instead
//! of double-counting it.

use actix_web::{HttpRequest, HttpResponse, web};
use chrono::Utc;
use serde_json::{Value, json};
use std::sync::Arc;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::PaginationParams;
use uuid::Uuid;

use super::crypto::webhook_secret_matches;
use super::helpers::{
    caller, caller_company, created, err, load_account, load_account_for_webhook, load_post, ok,
    paginate, parse_date_filter, record_audit, require_caller_company, require_manage,
    require_tenant, require_view, validate_metric_name, validate_metric_value,
};
use super::state::SocialMarketingState;
use super::types::{
    METRIC_SOURCE_MANUAL, METRIC_SOURCE_WEBHOOK, MetricsIngestRequest, RecordMetricRequest,
    SocialMetric,
};

/// List metric rows (paginated, filter by post, account,
/// metric, source; filter by recorded date range with
/// `from` / `to`).
pub(crate) async fn list_metrics(
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
        let post_filter = params.filters.get("post_id").cloned();
        let account_filter = params.filters.get("account_id").cloned();
        let metric_filter = params.filters.get("metric").cloned();
        let source_filter = params.filters.get("source").cloned();
        let from_filter = match params.filters.get("from") {
            Some(raw) => Some(parse_date_filter(raw, "from")?),
            None => None,
        };
        let to_filter = match params.filters.get("to") {
            Some(raw) => Some(parse_date_filter(raw, "to")?),
            None => None,
        };
        let mut items = state.metrics_of(&tenant_id);
        items.retain(|metric| {
            if let Some(post_id) = &post_filter
                && metric.post_id.to_string() != *post_id
            {
                return false;
            }
            if let Some(account_id) = &account_filter {
                match &metric.account_id {
                    Some(id) if id.to_string() == *account_id => {}
                    _ => return false,
                }
            }
            if let Some(name) = &metric_filter
                && metric.metric != *name
            {
                return false;
            }
            if let Some(source) = &source_filter
                && metric.source != *source
            {
                return false;
            }
            if let Some(from) = from_filter
                && metric.recorded_at < from
            {
                return false;
            }
            if let Some(to) = to_filter
                && metric.recorded_at > to
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

/// Record one metric value for one post by hand (manager):
/// creates the row, or replaces the value when the same
/// (post, metric, manual) row already exists.
pub(crate) async fn record_metric(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<String>,
    body: web::Json<RecordMetricRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let tenant = require_tenant(&req)?;
        let tenant_id = tenant.tenant_id.to_string();
        let _company = require_caller_company(&tenant, &req)?;
        let caller = caller(&req)?;
        require_manage(&ctx, &caller.role)?;
        let input = body.into_inner();
        let post = load_post(&state, &tenant_id, &path.into_inner())?;
        let metric_name = validate_metric_name(&input.metric)?;
        let value = validate_metric_value(input.value)?;
        let now = Utc::now();
        let existing = state.metric_for(&tenant_id, post.id, &metric_name, METRIC_SOURCE_MANUAL);
        let row = match existing {
            Some(mut row) => {
                let before = serde_json::to_value(&row).unwrap_or(Value::Null);
                row.value = value;
                row.recorded_at = now;
                row.updated_at = now;
                state.metrics.insert(row.id, row.clone());
                record_audit(
                    &state,
                    &tenant_id,
                    "metric",
                    &row.id.to_string(),
                    "update",
                    &caller.actor,
                    Some(before),
                    Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
                );
                row
            }
            None => {
                let row = SocialMetric {
                    id: Uuid::new_v4(),
                    tenant_id: tenant_id.clone(),
                    post_id: post.id,
                    account_id: None,
                    metric: metric_name,
                    value,
                    source: METRIC_SOURCE_MANUAL.to_string(),
                    recorded_at: now,
                    created_at: now,
                    updated_at: now,
                };
                state.metrics.insert(row.id, row.clone());
                record_audit(
                    &state,
                    &tenant_id,
                    "metric",
                    &row.id.to_string(),
                    "create",
                    &caller.actor,
                    None,
                    Some(serde_json::to_value(&row).unwrap_or(Value::Null)),
                );
                row
            }
        };
        Ok(serde_json::to_value(&row).unwrap_or(Value::Null))
    })();
    match result {
        Ok(data) => created(data),
        Err(error) => err(error, debug),
    }
}

/// Per-account metrics rollup: summed metrics of every
/// post targeting the account, the post count, and the
/// engagement rate in basis points.
pub(crate) async fn account_rollup(
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
        let account = load_account(&state, &tenant_id, &path.into_inner())?;
        let totals = state.rollup_of_account(&tenant_id, account.id);
        let posts = state.posts_of_account(&tenant_id, account.id);
        Ok(json!({
            "account_id": account.id.to_string(),
            "network_id": account.network_id.to_string(),
            "handle": account.handle,
            "post_count": posts.len(),
            "metrics": totals,
            "engagement_bps": super::types::engagement_bps(&totals),
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}

/// Metrics-ingest webhook (PUBLIC, provider verification
/// token gated): a network posts reported metric values for
/// one post. The account's webhook secret arrives in the
/// `x-social-webhook-secret` header and must match the
/// stored hash; the post must target this account, or the
/// call answers 404 with the same message as an unknown
/// post, so account ids cannot be probed apart. Each
/// (metric, value) pair upserts the (post, metric, webhook)
/// row. Like the SMS webhooks, this route relies on the API
/// Manager's admin-managed `ApiAccessRule` records
/// (default deny): an administrator must create the
/// matching rule for this path.
pub(crate) async fn metrics_ingest(
    state: web::Data<SocialMarketingState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<MetricsIngestRequest>,
) -> HttpResponse {
    let debug = false;
    let result: UnifiedResult<Value> = (|| {
        let account = load_account_for_webhook(&state, &path.into_inner())?;
        let presented = req
            .headers()
            .get("x-social-webhook-secret")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !webhook_secret_matches(account.webhook_secret_hash.as_deref(), presented) {
            return Err(UnifiedError::new("Invalid webhook secret".to_string(), 401));
        }
        let input = body.into_inner();
        let post_id = super::helpers::parse_uuid(&input.post_id, "post")?;
        let post = state
            .posts
            .get(&post_id)
            .map(|entry| entry.value().clone())
            .filter(|post| {
                post.tenant_id == account.tenant_id
                    && post.account_ids.contains(&account.id.to_string())
            })
            .ok_or_else(|| UnifiedError::not_found(format!("Post not found: {post_id}")))?;
        if input.metrics.is_empty() {
            return Err(UnifiedError::bad_request(
                "metrics must not be empty".to_string(),
            ));
        }
        if input.metrics.len() > 100 {
            return Err(UnifiedError::bad_request(
                "At most 100 metric values per ingest call".to_string(),
            ));
        }
        let now = Utc::now();
        let mut rows: Vec<Value> = Vec::new();
        for item in &input.metrics {
            let metric_name = validate_metric_name(&item.metric)?;
            let value = validate_metric_value(item.value)?;
            let existing = state.metric_for(
                &account.tenant_id,
                post.id,
                &metric_name,
                METRIC_SOURCE_WEBHOOK,
            );
            let row = match existing {
                Some(mut row) => {
                    row.value = value;
                    row.account_id = Some(account.id);
                    row.recorded_at = now;
                    row.updated_at = now;
                    state.metrics.insert(row.id, row.clone());
                    row
                }
                None => {
                    let row = SocialMetric {
                        id: Uuid::new_v4(),
                        tenant_id: account.tenant_id.clone(),
                        post_id: post.id,
                        account_id: Some(account.id),
                        metric: metric_name,
                        value,
                        source: METRIC_SOURCE_WEBHOOK.to_string(),
                        recorded_at: now,
                        created_at: now,
                        updated_at: now,
                    };
                    state.metrics.insert(row.id, row.clone());
                    row
                }
            };
            rows.push(serde_json::to_value(&row).unwrap_or(Value::Null));
        }
        Ok(json!({
            "account_id": account.id.to_string(),
            "post_id": post.id.to_string(),
            "ingested": rows.len(),
            "metrics": rows,
        }))
    })();
    match result {
        Ok(data) => ok(data),
        Err(error) => err(error, debug),
    }
}
