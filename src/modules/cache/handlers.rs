//! HTTP handlers for cache entries and tenant entries.

use unified_core::cache::CacheValue;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::Value;
use std::sync::Arc;
use super::types::{CacheEntryView, CacheStatusView, InvalidateRequest, PERM_MANAGE, PERM_READ, PERM_WRITE, SetEntryRequest, authorize, cache_value_to_json, ttl_from_request};

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn get_status(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<CacheStatusView> = (|| {
        let config = ctx.config.read().expect("config lock poisoned");
        Ok(CacheStatusView {
            adapter: ctx.cache.adapter_kind(),
            default_ttl_secs: ctx.cache.default_ttl().map(|ttl| ttl.as_secs()),
            redis_enabled: config.redis.enabled,
            filesystem_dir: config.cache.dir.clone(),
            failover_active: ctx.cache.failover_active(),
            max_entries: config.cache.max_entries,
        })
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_entry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    key: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let key = key.into_inner();
    let result: UnifiedResult<CacheEntryView> = (|| async {
        let entry = ctx
            .cache
            .get(&key)
            .await?
            .ok_or_else(|| UnifiedError::not_found(format!("Cache entry \"{key}\" not found")))?;
        Ok(CacheEntryView {
            key,
            value: cache_value_to_json(&entry.value),
            expires_at: entry.expires_at.map(|time| time.to_rfc3339()),
            ttl_remaining_secs: entry.ttl_remaining(),
        })
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn set_entry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<SetEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<CacheEntryView> = (|| async {
        let ttl = ttl_from_request(body.ttl_secs);
        let value = CacheValue::from(body.value.clone());
        ctx.cache.set(&body.key, value, ttl).await?;
        let entry = ctx
            .cache
            .get(&body.key)
            .await?
            .ok_or_else(|| UnifiedError::internal("Cache entry vanished after write"))?;
        Ok(CacheEntryView {
            key: body.key.clone(),
            value: cache_value_to_json(&entry.value),
            expires_at: entry.expires_at.map(|time| time.to_rfc3339()),
            ttl_remaining_secs: entry.ttl_remaining(),
        })
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn delete_entry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    key: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let key = key.into_inner();
    let result: UnifiedResult<Value> = (|| async {
        let deleted = ctx.cache.delete(&key).await?;
        Ok(serde_json::json!({ "key": key, "deleted": deleted }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn clear_cache(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        ctx.cache.clear().await?;
        Ok(serde_json::json!({ "cleared": true, "adapter": ctx.cache.adapter_kind() }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn prune_cache(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let pruned = ctx.cache.prune().await?;
        Ok(serde_json::json!({ "pruned": pruned, "adapter": ctx.cache.adapter_kind() }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn invalidate_prefix(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<InvalidateRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE) {
        return json::error_response(&error, debug);
    }
    let result: UnifiedResult<Value> = (|| async {
        let removed = ctx.cache.invalidate_prefix(&body.prefix).await?;
        Ok(serde_json::json!({ "prefix": body.prefix, "removed": removed }))
    })()
    .await;
    json::respond(result, debug)
}

// -- Tenant-scoped handlers. The tenant id is a scope, not an
// authorization: the caller still needs the cache capability, and
// the facade keeps the entry under `tenant:{id}:{key}`. --

pub(crate) async fn get_tenant_entry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return json::error_response(&error, debug);
    }
    let (tenant_id, key) = path.into_inner();
    let result: UnifiedResult<CacheEntryView> = (|| async {
        let entry = ctx
            .cache
            .get_tenant(&tenant_id, &key)
            .await?
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Cache entry \"{key}\" not found for tenant \"{tenant_id}\""
                ))
            })?;
        Ok(CacheEntryView {
            key,
            value: cache_value_to_json(&entry.value),
            expires_at: entry.expires_at.map(|time| time.to_rfc3339()),
            ttl_remaining_secs: entry.ttl_remaining(),
        })
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn set_tenant_entry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    tenant: web::Path<String>,
    body: web::Json<SetEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let tenant_id = tenant.into_inner();
    let result: UnifiedResult<CacheEntryView> = (|| async {
        let ttl = ttl_from_request(body.ttl_secs);
        let value = CacheValue::from(body.value.clone());
        ctx.cache
            .set_tenant(&tenant_id, &body.key, value, ttl)
            .await?;
        let entry = ctx
            .cache
            .get_tenant(&tenant_id, &body.key)
            .await?
            .ok_or_else(|| UnifiedError::internal("Cache entry vanished after write"))?;
        Ok(CacheEntryView {
            key: body.key.clone(),
            value: cache_value_to_json(&entry.value),
            expires_at: entry.expires_at.map(|time| time.to_rfc3339()),
            ttl_remaining_secs: entry.ttl_remaining(),
        })
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn delete_tenant_entry(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_WRITE) {
        return json::error_response(&error, debug);
    }
    let (tenant_id, key) = path.into_inner();
    let result: UnifiedResult<Value> = (|| async {
        let deleted = ctx.cache.delete_tenant(&tenant_id, &key).await?;
        Ok(serde_json::json!({ "tenant": tenant_id, "key": key, "deleted": deleted }))
    })()
    .await;
    json::respond(result, debug)
}

pub(crate) async fn invalidate_tenant(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    tenant: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE) {
        return json::error_response(&error, debug);
    }
    let tenant_id = tenant.into_inner();
    let result: UnifiedResult<Value> = (|| async {
        let removed = ctx.cache.invalidate_tenant(&tenant_id).await?;
        Ok(serde_json::json!({ "tenant": tenant_id, "removed": removed }))
    })()
    .await;
    json::respond(result, debug)
}
