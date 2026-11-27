//! Cloudflare connector module. - Handlers: cache.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::state::*;
use super::status_handlers::*;

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use reqwest::Method;
use serde_json::json;
use serde_json::Value;
use std::sync::Arc;

pub(super) async fn purge_cache(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
    input: web::Json<PurgeCacheInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let input = input.into_inner();
        let urls = input.urls.unwrap_or_default();
        let prefixes = input.prefixes.unwrap_or_default();
        if input.purge_everything && !input.confirm {
            return Err(UnifiedError::bad_request(
                "purge_everything requires confirm=true",
            ));
        }
        if input.purge_everything && (!urls.is_empty() || !prefixes.is_empty()) {
            return Err(UnifiedError::bad_request(
                "purge_everything cannot be combined with urls or prefixes",
            ));
        }
        // Cloudflare accepts only one purge mode per call; mixing
        // urls and prefixes in one request is rejected here instead
        // of silently dropping one of them.
        if !urls.is_empty() && !prefixes.is_empty() {
            return Err(UnifiedError::bad_request(
                "Purge urls and prefixes in separate requests",
            ));
        }
        if !input.purge_everything && urls.is_empty() && prefixes.is_empty() {
            return Err(UnifiedError::bad_request(
                "Provide urls, prefixes, or purge_everything=true",
            ));
        }
        if urls.len() > CF_MAX_PURGE_TARGETS {
            return Err(UnifiedError::bad_request(format!(
                "At most {CF_MAX_PURGE_TARGETS} URLs can be purged in one request"
            )));
        }
        if prefixes.len() > CF_MAX_PURGE_TARGETS {
            return Err(UnifiedError::bad_request(format!(
                "At most {CF_MAX_PURGE_TARGETS} prefixes can be purged in one request"
            )));
        }
        for url in urls.iter().chain(prefixes.iter()) {
            let parsed = url::Url::parse(url).map_err(|_| {
                UnifiedError::bad_request(format!("Invalid URL \"{url}\""))
            })?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(UnifiedError::bad_request(format!(
                    "Purge URL \"{url}\" must use http or https"
                )));
            }
        }
        let (kind, targets, body) = if input.purge_everything {
            (
                "everything",
                Vec::new(),
                json!({ "purge_everything": true }),
            )
        } else if !urls.is_empty() {
            ("urls", urls.clone(), json!({ "files": urls }))
        } else {
            (
                "prefixes",
                prefixes.clone(),
                json!({ "prefixes": prefixes }),
            )
        };
        cf_call(
            &state,
            Method::POST,
            &format!("/zones/{zone_id}/purge_cache"),
            Some(body),
            &[],
        )
        .await?;
        let record = state.record_purge(&zone_id, kind, targets.clone());
        ctx.hooks.emit(
            "cloudflare.cache.purged",
            json!({
                "purge_id": record.id,
                "zone_id": zone_id,
                "kind": kind,
                "target_count": targets.len(),
            }),
        );
        Ok(json!({
            "purged": true,
            "purge_id": record.id,
            "zone_id": zone_id,
            "kind": kind,
            "target_count": targets.len(),
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn list_purges_for_zone(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let zone_id = zone_id.into_inner();
    if let Err(error) = validate_cf_id(&zone_id, "zone") {
        return json::error_response(&error, ctx.is_debug());
    }
    let mut items: Vec<CfPurgeRecord> = state
        .purge_history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|record| record.zone_id == zone_id)
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|record| {
            record.kind.to_lowercase().contains(&needle)
                || record
                    .targets
                    .iter()
                    .any(|target| target.to_lowercase().contains(&needle))
        });
    }
    if let Some(kind) = params.filters.get("kind") {
        items.retain(|record| record.kind == *kind);
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    json::success_response(paginate_local(items, &params), 200)
}

pub(super) async fn list_purges(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW) {
        return json::error_response(&error, ctx.is_debug());
    }
    let mut items: Vec<CfPurgeRecord> = state
        .purge_history
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|record| {
            record.kind.to_lowercase().contains(&needle)
                || record.zone_id.to_lowercase().contains(&needle)
                || record
                    .targets
                    .iter()
                    .any(|target| target.to_lowercase().contains(&needle))
        });
    }
    if let Some(kind) = params.filters.get("kind") {
        items.retain(|record| record.kind == *kind);
    }
    if let Some(zone_id) = params.filters.get("zone_id") {
        items.retain(|record| record.zone_id == *zone_id);
    }
    items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    json::success_response(paginate_local(items, &params), 200)
}

pub(super) async fn get_development_mode(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/settings/development_mode"),
            None,
            &[],
        )
        .await?;
        let result = cf_result(&envelope);
        Ok(json!({
            "setting": "development_mode",
            "value": result.get("value").cloned().unwrap_or(Value::Null),
            "enabled": result.get("value").and_then(Value::as_str) == Some("on"),
            "editable": result.get("editable").and_then(Value::as_bool).unwrap_or(false),
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn set_development_mode(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
    input: web::Json<DevelopmentModeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let value = if input.enabled { "on" } else { "off" };
        let envelope = cf_call(
            &state,
            Method::PATCH,
            &format!("/zones/{zone_id}/settings/development_mode"),
            Some(json!({ "value": value })),
            &[],
        )
        .await?;
        let result = cf_result(&envelope);
        Ok(json!({
            "setting": "development_mode",
            "value": result.get("value").cloned().unwrap_or(json!(value)),
            "enabled": input.enabled,
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn get_cache_level(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_VIEW)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/settings/cache_level"),
            None,
            &[],
        )
        .await?;
        let result = cf_result(&envelope);
        Ok(json!({
            "setting": "cache_level",
            "value": result.get("value").cloned().unwrap_or(Value::Null),
            "editable": result.get("editable").and_then(Value::as_bool).unwrap_or(false),
        }))
    })()
    .await;
    json::respond(result, debug)
}

pub(super) async fn set_cache_level(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    zone_id: web::Path<String>,
    input: web::Json<CacheLevelInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        authorize(&ctx, &req, PERM_CLOUDFLARE_MANAGE)?;
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let value = input.value.trim().to_lowercase();
        if !CF_CACHE_LEVELS.contains(&value.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "cache_level must be one of {}",
                CF_CACHE_LEVELS.join(", ")
            )));
        }
        let envelope = cf_call(
            &state,
            Method::PATCH,
            &format!("/zones/{zone_id}/settings/cache_level"),
            Some(json!({ "value": value })),
            &[],
        )
        .await?;
        let result = cf_result(&envelope);
        Ok(json!({
            "setting": "cache_level",
            "value": result.get("value").cloned().unwrap_or(json!(value)),
        }))
    })()
    .await;
    json::respond(result, debug)
}
