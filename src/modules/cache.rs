// Cache module — HTTP access to the shared application cache.
//
// Routes under `/api/v1/cache`:
//
//   GET    /           cache status (adapter, default TTL, Redis enabled)
//   GET    /{key}      read one entry (404 on a miss or an expired entry)
//   POST   /           store an entry: {"key", "value", "ttl_secs"?}
//   DELETE /{key}      delete one entry
//   POST   /clear      remove every entry
//
// `value` is plain JSON on the way in and on the way out: strings,
// numbers, booleans, arrays and objects round-trip as themselves. Raw
// bytes are returned as a base64 string. `ttl_secs` omitted means the
// configured default TTL; `ttl_secs: 0` means the entry never expires.

use crate::core::cache::CacheValue;
use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::{web, HttpResponse};
use base64::Engine;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Views.
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct CacheStatusView {
    adapter: &'static str,
    default_ttl_secs: Option<u64>,
    redis_enabled: bool,
    filesystem_dir: String,
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

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn get_status(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CacheStatusView> = (|| {
        let config = ctx.config.read().expect("config lock poisoned");
        Ok(CacheStatusView {
            adapter: ctx.cache.adapter_kind(),
            default_ttl_secs: ctx.cache.default_ttl().map(|ttl| ttl.as_secs()),
            redis_enabled: config.redis.enabled,
            filesystem_dir: config.cache.dir.clone(),
        })
    })();
    json::respond(result, debug)
}

async fn get_entry(ctx: web::Data<Arc<AppContext>>, key: web::Path<String>) -> HttpResponse {
    let debug = ctx.is_debug();
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
    body: web::Json<SetEntryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<CacheEntryView> = (|| async {
        let ttl = match body.ttl_secs {
            None => None,
            Some(0) => Some(Duration::ZERO),
            Some(secs) => Some(Duration::from_secs(secs)),
        };
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

async fn delete_entry(ctx: web::Data<Arc<AppContext>>, key: web::Path<String>) -> HttpResponse {
    let debug = ctx.is_debug();
    let key = key.into_inner();
    let result: UnifiedResult<Value> = (|| async {
        let deleted = ctx.cache.delete(&key).await?;
        Ok(serde_json::json!({ "key": key, "deleted": deleted }))
    })()
    .await;
    json::respond(result, debug)
}

async fn clear_cache(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        ctx.cache.clear().await?;
        Ok(serde_json::json!({ "cleared": true, "adapter": ctx.cache.adapter_kind() }))
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
        "Shared application cache: filesystem adapter by default, Redis when redis.enabled is true."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/cache", owner: "cache", description: "Get status" },
            RouteInfo { method: "POST", path: "/api/v1/cache", owner: "cache", description: "Set entry" },
            RouteInfo { method: "POST", path: "/api/v1/cache/clear", owner: "cache", description: "Clear cache" },
            RouteInfo { method: "GET", path: "/api/v1/cache/{key}", owner: "cache", description: "Get entry" },
            RouteInfo { method: "DELETE", path: "/api/v1/cache/{key}", owner: "cache", description: "Delete entry" },
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/cache")
                .route("", web::get().to(get_status))
                .route("", web::post().to(set_entry))
                .route("/clear", web::post().to(clear_cache))
                .route("/{key}", web::get().to(get_entry))
                .route("/{key}", web::delete().to(delete_entry)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CacheModule));
}
