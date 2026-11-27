//! Cloudflare connector module. - State.
//!
//! Split from `src/modules/cloudflare.rs`; behaviour unchanged.

use super::accounts_zones::*;
use super::cache_handlers::*;
use super::client::*;
use super::constants::*;
use super::dns_handlers::*;
use super::dns_validation::*;
use super::helpers::*;
use super::module::*;
use super::requests::*;
use super::status_handlers::*;

use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::Serialize;
use serde_json::json;
use serde_json::Value;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

/// Rate-limit state, surfaced read-only through the status endpoint.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CfRateLimitState {
    /// True while a 429 backoff window is active.
    pub limited: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limited_until: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_after_seconds: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_429_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfTestResult {
    pub ok: bool,
    pub token_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    pub checked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CfPurgeRecord {
    pub id: Uuid,
    pub zone_id: String,
    /// `urls`, `prefixes`, or `everything`.
    pub kind: String,
    pub targets: Vec<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct CloudflareState {
    /// Connector settings: `api_token` (secret), `account_id`,
    /// `default_zone_id`.
    pub settings: Arc<DashMap<String, Value>>,
    pub rate_limit: Arc<RwLock<CfRateLimitState>>,
    pub purge_history: Arc<DashMap<Uuid, CfPurgeRecord>>,
    pub last_test: Arc<RwLock<Option<CfTestResult>>>,
    pub client: reqwest::Client,
}

impl Default for CloudflareState {
    fn default() -> Self {
        Self {
            settings: Arc::new(DashMap::new()),
            rate_limit: Arc::new(RwLock::new(CfRateLimitState::default())),
            purge_history: Arc::new(DashMap::new()),
            last_test: Arc::new(RwLock::new(None)),
            client: reqwest::Client::new(),
        }
    }
}

impl CloudflareState {
    pub(super) fn api_token(&self) -> Option<String> {
        self.settings
            .get("api_token")
            .and_then(|entry| entry.value().as_str().map(str::to_string))
            .filter(|token| !token.trim().is_empty())
    }

    pub(super) fn account_id(&self) -> Option<String> {
        self.settings
            .get("account_id")
            .and_then(|entry| entry.value().as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty())
    }

    pub(super) fn default_zone_id(&self) -> Option<String> {
        self.settings
            .get("default_zone_id")
            .and_then(|entry| entry.value().as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty())
    }

    /// Build the `settings.merge` payload for this module. Only
    /// flags and non-secret identifiers are emitted - exactly the
    /// VCS pattern. The raw API token is never placed in a hook
    /// payload: it stays in this module's own in-memory store and
    /// only `api_token_configured` is shared, so no bus listener
    /// can observe the credential.
    pub(super) fn settings_merge_payload(&self) -> Value {
        json!({
            "namespace": "cloudflare",
            "settings": {
                "api_token_configured": self.api_token().is_some(),
                "account_id": self.account_id(),
                "default_zone_id": self.default_zone_id(),
            },
        })
    }

    /// True while a 429 backoff window is still active.
    pub(super) fn is_rate_limited(&self) -> Option<u64> {
        let state = self.rate_limit.read().expect("rate limit lock poisoned");
        match state.limited_until {
            Some(until) if until > Utc::now() => Some(
                (until - Utc::now()).num_seconds().max(1) as u64,
            ),
            _ => None,
        }
    }

    pub(super) fn record_rate_limit_headers(&self, response: &reqwest::Response) {
        let headers = response.headers();
        let header_u64 = |name: &str| {
            headers
                .get(name)
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.trim().parse::<u64>().ok())
        };
        let mut state = self.rate_limit.write().expect("rate limit lock poisoned");
        if let Some(limit) = header_u64("x-ratelimit-limit") {
            state.limit = Some(limit);
        }
        if let Some(remaining) = header_u64("x-ratelimit-remaining") {
            state.remaining = Some(remaining);
        }
        if let Some(reset) = header_u64("x-ratelimit-reset") {
            state.reset_at = DateTime::from_timestamp(reset as i64, 0);
        }
    }

    pub(super) fn record_429(&self, retry_after_secs: u64) {
        let retry_after_secs = retry_after_secs.min(CF_MAX_RETRY_AFTER_SECS).max(1);
        let mut state = self.rate_limit.write().expect("rate limit lock poisoned");
        state.limited = true;
        state.limited_until =
            Some(Utc::now() + chrono::Duration::seconds(retry_after_secs as i64));
        state.retry_after_seconds = Some(retry_after_secs);
        state.last_429_at = Some(Utc::now());
    }

    pub(super) fn clear_rate_limit(&self) {
        let mut state = self.rate_limit.write().expect("rate limit lock poisoned");
        state.limited = false;
        state.limited_until = None;
        state.retry_after_seconds = None;
    }

    pub(super) fn rate_limit_view(&self) -> CfRateLimitState {
        let mut view = self.rate_limit.read().expect("rate limit lock poisoned").clone();
        if let Some(until) = view.limited_until {
            if until <= Utc::now() {
                view.limited = false;
                view.limited_until = None;
                view.retry_after_seconds = None;
            }
        }
        view
    }

    pub(super) fn record_purge(
        &self,
        zone_id: &str,
        kind: &str,
        targets: Vec<String>,
    ) -> CfPurgeRecord {
        if self.purge_history.len() >= CF_MAX_PURGE_HISTORY {
            // Drop the oldest entry to keep the history bounded.
            let oldest = self
                .purge_history
                .iter()
                .min_by_key(|entry| entry.value().created_at)
                .map(|entry| *entry.key());
            if let Some(id) = oldest {
                self.purge_history.remove(&id);
            }
        }
        let record = CfPurgeRecord {
            id: Uuid::now_v7(),
            zone_id: zone_id.to_string(),
            kind: kind.to_string(),
            targets,
            status: "completed".to_string(),
            created_at: Utc::now(),
        };
        self.purge_history.insert(record.id, record.clone());
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_records_and_clears() {
        let state = CloudflareState::default();
        assert_eq!(state.is_rate_limited(), None);
        state.record_429(60);
        assert!(matches!(state.is_rate_limited(), Some(secs) if (1..=60).contains(&secs)));
        let view = state.rate_limit_view();
        assert!(view.limited);
        assert_eq!(view.retry_after_seconds, Some(60));
        state.clear_rate_limit();
        assert_eq!(state.is_rate_limited(), None);
        assert!(!state.rate_limit_view().limited);
    }

    #[test]
    fn record_429_clamps_to_supported_range() {
        let state = CloudflareState::default();
        state.record_429(0);
        assert_eq!(state.is_rate_limited(), Some(1));
        state.record_429(10_000);
        assert!(matches!(state.is_rate_limited(), Some(secs) if (1..=300).contains(&secs)));
    }

    #[test]
    fn settings_merge_payload_never_carries_the_token() {
        let state = CloudflareState::default();
        state
            .settings
            .insert("api_token".to_string(), json!("secret-token-value"));
        state
            .settings
            .insert("account_id".to_string(), json!("acc-123"));
        state
            .settings
            .insert("default_zone_id".to_string(), json!("zone-456"));
        let payload = state.settings_merge_payload();
        assert_eq!(payload["namespace"], "cloudflare");
        assert_eq!(payload["settings"]["api_token_configured"], true);
        assert_eq!(payload["settings"]["account_id"], "acc-123");
        assert_eq!(payload["settings"]["default_zone_id"], "zone-456");
        // No key may be named after the credential, and the
        // credential value must not appear anywhere in the payload.
        assert!(payload["settings"].get("api_token").is_none());
        assert!(!payload.to_string().contains("secret-token-value"));
    }

    #[test]
    fn settings_merge_payload_reports_unconfigured_token() {
        let state = CloudflareState::default();
        let payload = state.settings_merge_payload();
        assert_eq!(payload["settings"]["api_token_configured"], false);
        assert!(payload["settings"]["account_id"].is_null());
        assert!(payload["settings"]["default_zone_id"].is_null());
    }

    #[test]
    fn settings_accessors_ignore_blank_values() {
        let state = CloudflareState::default();
        assert_eq!(state.api_token(), None);
        assert_eq!(state.account_id(), None);
        assert_eq!(state.default_zone_id(), None);
        state.settings.insert("api_token".to_string(), json!("  "));
        assert_eq!(state.api_token(), None);
        state
            .settings
            .insert("api_token".to_string(), json!("secret-token"));
        assert_eq!(state.api_token().as_deref(), Some("secret-token"));
    }
}
