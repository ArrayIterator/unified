// Webhooks - Request types.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use rand::{distr::Alphanumeric, Rng};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// Request types.

#[derive(Debug, Deserialize)]
pub(crate) struct CreateEndpointRequest {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) events: Vec<String>,
    /// Signing secret. When omitted, a random 48-character secret is generated.
    #[serde(default)]
    pub(crate) secret: Option<String>,
    #[serde(default = "default_true")]
    pub(crate) active: bool,
    #[serde(default = "default_max_attempts")]
    pub(crate) max_attempts: u32,
    #[serde(default = "default_backoff_base_secs")]
    pub(crate) backoff_base_secs: u64,
    #[serde(default = "default_timeout_secs")]
    pub(crate) timeout_secs: u64,
}

pub(crate) fn default_true() -> bool {
    true
}

pub(crate) fn default_max_attempts() -> u32 {
    DEFAULT_MAX_ATTEMPTS
}

pub(crate) fn default_backoff_base_secs() -> u64 {
    DEFAULT_BACKOFF_BASE_SECS
}

pub(crate) fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Debug, Deserialize)]
pub(crate) struct UpdateEndpointRequest {
    #[serde(default)]
    pub(crate) name: Option<String>,
    #[serde(default)]
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) events: Option<Vec<String>>,
    /// Replaces the signing secret. The old secret stops working at once.
    #[serde(default)]
    pub(crate) secret: Option<String>,
    #[serde(default)]
    pub(crate) active: Option<bool>,
    #[serde(default)]
    pub(crate) paused: Option<bool>,
    #[serde(default)]
    pub(crate) max_attempts: Option<u32>,
    #[serde(default)]
    pub(crate) backoff_base_secs: Option<u64>,
    #[serde(default)]
    pub(crate) timeout_secs: Option<u64>,
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn find_endpoint(id: Uuid) -> UnifiedResult<WebhookEndpoint> {
    endpoints_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Webhook endpoint not found"))
}

pub(crate) fn validate_retry_policy(max_attempts: u32, backoff_base_secs: u64, timeout_secs: u64) -> UnifiedResult<()> {
    if max_attempts == 0 || max_attempts > MAX_MAX_ATTEMPTS {
        return Err(UnifiedError::bad_request(format!(
            "max_attempts must be between 1 and {MAX_MAX_ATTEMPTS}"
        )));
    }
    if backoff_base_secs == 0 || backoff_base_secs > MAX_BACKOFF_BASE_SECS {
        return Err(UnifiedError::bad_request(format!(
            "backoff_base_secs must be between 1 and {MAX_BACKOFF_BASE_SECS}"
        )));
    }
    if timeout_secs == 0 || timeout_secs > MAX_TIMEOUT_SECS {
        return Err(UnifiedError::bad_request(format!(
            "timeout_secs must be between 1 and {MAX_TIMEOUT_SECS}"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_retry_policy_accepts_defaults() {
        assert!(validate_retry_policy(3, 5, 30).is_ok());
    }

    #[test]
    fn validate_retry_policy_rejects_zero_attempts() {
        assert!(validate_retry_policy(0, 5, 30).is_err());
    }

    #[test]
    fn validate_retry_policy_rejects_too_many_attempts() {
        assert!(validate_retry_policy(11, 5, 30).is_err());
    }

    #[test]
    fn validate_retry_policy_rejects_bad_timeout() {
        assert!(validate_retry_policy(3, 5, 0).is_err());
        assert!(validate_retry_policy(3, 5, 121).is_err());
        assert!(validate_retry_policy(3, 0, 30).is_err());
    }
}
