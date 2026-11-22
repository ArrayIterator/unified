// Cache module — HTTP access to the shared application cache.
//
// Routes under `/api/v1/cache`:
//
//   GET    /                              cache status (adapter, default TTL, Redis enabled, failover state)
//   GET    /{key}                         read one entry (404 on a miss or an expired entry)
//   POST   /                              store an entry: {"key", "value", "ttl_secs"?}
//   DELETE /{key}                         delete one entry
//   POST   /clear                         remove every entry
//   POST   /prune                         evict entries past the filesystem bound
//   POST   /invalidate                    {"prefix"} -> delete every entry under a key prefix
//   GET    /tenant/{tenant}/{key}         read one tenant-scoped entry
//   POST   /tenant/{tenant}               store a tenant-scoped entry: {"key", "value", "ttl_secs"?}
//   DELETE /tenant/{tenant}/{key}         delete one tenant-scoped entry
//   POST   /tenant/{tenant}/invalidate    delete every entry belonging to one tenant
//
// `value` is plain JSON on the way in and on the way out: strings,
// numbers, booleans, arrays and objects round-trip as themselves. Raw
// bytes are returned as a base64 string. `ttl_secs` omitted means the
// configured default TTL; `ttl_secs: 0` means the entry never expires.
//
// Capability checks: every handler requires the caller identity
// (`X-User-Id` + `X-Role` headers) and one capability, checked through
// the `role.check` hook (fails closed when no handler answers):
// `cache.read` for reads, `cache.write` for writes, `cache.manage`
// for clear / prune / invalidate. Tenant-scoped routes use the same
// capabilities; the tenant id comes from the URL, and the facade
// prefixes the key with `tenant:{id}:`, so a tenant entry is never
// reachable through the unscoped routes.
//
// The tenant id for a request normally comes from the middleware
// `TenantContext` (resolved from the Host header). These routes take
// it from the URL instead so an operator or a background job can
// address a tenant explicitly; a caller must hold the cache
// capability either way — the tenant id is a scope, not an
// authorization.

use crate::core::cache::CacheValue;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Capabilities (checked through the `role.check` hook; fails closed).
// ---------------------------------------------------------------------------

const PERM_READ: &str = "cache.read";
const PERM_WRITE: &str = "cache.write";
const PERM_MANAGE: &str = "cache.manage";

struct Caller {
    role: String,
}

fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // The actor header is required (anonymous callers are refused)
    // but the capability check runs on the role only.
    let _actor = req
        .headers()
        .get("x-user-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-User-Id header"))?;
    let role = req
        .headers()
        .get("x-role")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| UnifiedError::unauthorized("Missing X-Role header"))?;
    Ok(Caller { role })
}

fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
    let found = caller(req)?;
    let context = ctx.hooks.emit(
        "role.check",
        serde_json::json!({ "role": found.role, "permission": permission, "allowed": false }),
    );
    let allowed = context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if allowed {
        Ok(found)
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{permission}\"",
            found.role
        )))
    }
}

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CacheStatusView {
    adapter: &'static str,
    default_ttl_secs: Option<u64>,
    redis_enabled: bool,
    filesystem_dir: String,
    /// True when the adapter is currently serving from its fallback
    /// backend because the primary failed.
    failover_active: bool,
    /// Maximum entries kept by the filesystem adapter (0 means
    /// unlimited).
    max_entries: u64,
}

#[derive(Debug, Serialize)]
struct CacheEntryView {
    key: String,
    value: Value,
    expires_at: Option<String>,
    ttl_remaining_secs: Option<u64>,
}

fn cache_value_to_json(value: &CacheValue) -> Value {
    match value {
        CacheValue::Text(text) => Value::String(text.clone()),
        CacheValue::Number(number) => Value::from(*number),
        CacheValue::Boolean(flag) => Value::from(*flag),
        CacheValue::Bytes(bytes) => {
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes))
        }
        CacheValue::Object(object) => object.clone(),
    }
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SetEntryRequest {
    key: String,
    value: Value,
    /// Time-to-live in seconds. Omitted means the configured default TTL;
    /// 0 means the entry never expires.
    #[serde(default)]
    ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct InvalidateRequest {
    prefix: String,
}

fn ttl_from_request(ttl_secs: Option<u64>) -> Option<Duration> {
    match ttl_secs {
        None => None,
        Some(0) => Some(Duration::ZERO),
        Some(secs) => Some(Duration::from_secs(secs)),
    }
}

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn get_status(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
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

async fn get_entry(
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

async fn set_entry(
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

async fn delete_entry(
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

async fn clear_cache(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
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

async fn prune_cache(ctx: web::Data<Arc<AppContext>>, req: HttpRequest) -> HttpResponse {
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

async fn invalidate_prefix(
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

async fn get_tenant_entry(
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

async fn set_tenant_entry(
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

async fn delete_tenant_entry(
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

async fn invalidate_tenant(
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

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct CacheModule;

impl Module for CacheModule {
    fn id(&self) -> &'static str {
        "cache"
    }

    fn name(&self) -> &'static str {
        "Cache"
    }

    fn description(&self) -> &'static str {
        "Shared application cache: filesystem adapter by default, Redis when redis.enabled is true, with filesystem failover, tenant-scoped entries, stampede-protected fills, and prefix invalidation."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/cache", owner: "cache", description: "Get status (adapter, default TTL, Redis enabled, failover state)." },
            RouteInfo { method: "POST", path: "/api/v1/cache", owner: "cache", description: "Set entry (capability cache.write)." },
            RouteInfo { method: "POST", path: "/api/v1/cache/clear", owner: "cache", description: "Clear cache (capability cache.manage)." },
            RouteInfo { method: "POST", path: "/api/v1/cache/prune", owner: "cache", description: "Evict entries past the filesystem bound (capability cache.manage)." },
            RouteInfo { method: "POST", path: "/api/v1/cache/invalidate", owner: "cache", description: "Delete every entry under a key prefix (capability cache.manage)." },
            RouteInfo { method: "GET", path: "/api/v1/cache/{key}", owner: "cache", description: "Get entry (capability cache.read)." },
            RouteInfo { method: "DELETE", path: "/api/v1/cache/{key}", owner: "cache", description: "Delete entry (capability cache.write)." },
            RouteInfo { method: "GET", path: "/api/v1/cache/tenant/{tenant}/{key}", owner: "cache", description: "Get a tenant-scoped entry (capability cache.read)." },
            RouteInfo { method: "POST", path: "/api/v1/cache/tenant/{tenant}", owner: "cache", description: "Set a tenant-scoped entry (capability cache.write)." },
            RouteInfo { method: "DELETE", path: "/api/v1/cache/tenant/{tenant}/{key}", owner: "cache", description: "Delete a tenant-scoped entry (capability cache.write)." },
            RouteInfo { method: "POST", path: "/api/v1/cache/tenant/{tenant}/invalidate", owner: "cache", description: "Delete every entry of one tenant (capability cache.manage)." },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/cache")
                .route("", web::get().to(get_status))
                .route("", web::post().to(set_entry))
                .route("/clear", web::post().to(clear_cache))
                .route("/prune", web::post().to(prune_cache))
                .route("/invalidate", web::post().to(invalidate_prefix))
                .route("/tenant/{tenant}", web::post().to(set_tenant_entry))
                .route("/tenant/{tenant}/invalidate", web::post().to(invalidate_tenant))
                .route("/tenant/{tenant}/{key}", web::get().to(get_tenant_entry))
                .route("/tenant/{tenant}/{key}", web::delete().to(delete_tenant_entry))
                .route("/{key}", web::get().to(get_entry))
                .route("/{key}", web::delete().to(delete_entry)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CacheModule));
}
