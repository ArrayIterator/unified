//! Cache permissions, caller checks, views and TTL helpers.

use base64::Engine;
use unified_core::cache::CacheValue;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::{HttpRequest};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

// Capabilities (checked through the `role.check` hook; fails closed).

pub(crate) const PERM_READ: &str = "cache.read";

pub(crate) const PERM_WRITE: &str = "cache.write";

pub(crate) const PERM_MANAGE: &str = "cache.manage";

pub(crate) struct Caller {
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // The actor header is required (anonymous callers are refused)
    // but the capability check runs on the role only.
    let verified = unified_core::caller::require_caller(req)?;
    let role = verified.role().to_string();
    Ok(Caller { role })
}

pub(crate) fn authorize(ctx: &AppContext, req: &HttpRequest, permission: &str) -> UnifiedResult<Caller> {
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

// Views.

#[derive(Debug, Serialize)]
pub(crate) struct CacheStatusView {
    pub(crate) adapter: &'static str,
    pub(crate) default_ttl_secs: Option<u64>,
    pub(crate) redis_enabled: bool,
    pub(crate) filesystem_dir: String,
    /// True when the adapter is currently serving from its fallback
    /// backend because the primary failed.
    pub(crate) failover_active: bool,
    /// Maximum entries kept by the filesystem adapter (0 means
    /// unlimited).
    pub(crate) max_entries: u64,
}

#[derive(Debug, Serialize)]
pub(crate) struct CacheEntryView {
    pub(crate) key: String,
    pub(crate) value: Value,
    pub(crate) expires_at: Option<String>,
    pub(crate) ttl_remaining_secs: Option<u64>,
}

pub(crate) fn cache_value_to_json(value: &CacheValue) -> Value {
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

// Request types.

#[derive(Debug, Deserialize)]
pub(crate) struct SetEntryRequest {
    pub(crate) key: String,
    pub(crate) value: Value,
    /// Time-to-live in seconds. Omitted means the configured default TTL;
    /// 0 means the entry never expires.
    #[serde(default)]
    pub(crate) ttl_secs: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct InvalidateRequest {
    pub(crate) prefix: String,
}

pub(crate) fn ttl_from_request(ttl_secs: Option<u64>) -> Option<Duration> {
    match ttl_secs {
        None => None,
        Some(0) => Some(Duration::ZERO),
        Some(secs) => Some(Duration::from_secs(secs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ttl_from_request_maps_zero_to_never_expire() {
        assert_eq!(ttl_from_request(None), None);
        assert_eq!(ttl_from_request(Some(0)), Some(Duration::ZERO));
        assert_eq!(
            ttl_from_request(Some(60)),
            Some(Duration::from_secs(60))
        );
    }

    #[test]
    fn cache_value_to_json_converts_variants() {
        assert_eq!(
            cache_value_to_json(&CacheValue::Text("hi".to_string())),
            json!("hi")
        );
        assert_eq!(cache_value_to_json(&CacheValue::Number(42)), json!(42));
        assert_eq!(
            cache_value_to_json(&CacheValue::Boolean(true)),
            json!(true)
        );
        assert_eq!(
            cache_value_to_json(&CacheValue::Bytes(b"hi".to_vec())),
            json!("aGk=")
        );
        assert_eq!(
            cache_value_to_json(&CacheValue::Object(json!({"a": 1}))),
            json!({"a": 1})
        );
    }
}
