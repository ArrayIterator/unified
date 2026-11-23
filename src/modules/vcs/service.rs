// Third-party VCS integrations — Business logic shared by handlers and hooks.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use futures_util::StreamExt;
use hmac::{Hmac, Mac};
use reqwest::header;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::providers::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

pub(crate) fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Read rate-limit headers from a provider response.
pub(crate) fn rate_limit_from_headers(headers: &header::HeaderMap) -> RateLimitInfo {
    let header_value = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    let remaining = header_value("x-ratelimit-remaining")
        .or_else(|| header_value("ratelimit-remaining"))
        .and_then(|value| value.trim().parse::<u32>().ok());
    let reset_raw = header_value("x-ratelimit-reset")
        .or_else(|| header_value("ratelimit-reset"))
        .and_then(|value| value.trim().parse::<u64>().ok());
    let reset_at = reset_raw.map(|value| {
        if value > 1_000_000_000 {
            DateTime::from_timestamp(value as i64, 0).unwrap_or_else(Utc::now)
        } else {
            Utc::now() + chrono::Duration::seconds(value as i64)
        }
    });
    let retry_after_seconds = headers
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    RateLimitInfo {
        remaining,
        reset_at,
        retry_after_seconds,
    }
}

/// Refuse a provider call while the provider says to back off.
/// Returns a 429 error carrying the remaining quota in metadata.
pub(crate) fn check_rate_limit(state: &VcsState, key: &str) -> UnifiedResult<()> {
    let Some(entry) = state.rate_limits.get(key) else {
        return Ok(());
    };
    let limits = entry.value();
    if let Some(until) = limits.retry_after_until {
        if until > Instant::now() {
            let wait = until.duration_since(Instant::now()).as_secs();
            return Err(UnifiedError::new(
                "Provider rate limit is active; back off before retrying",
                429,
            )
            .with_metadata("retry_after_seconds", json!(wait))
            .with_metadata(
                "rate_limit_remaining",
                limits
                    .remaining
                    .map(Value::from)
                    .unwrap_or(Value::Null),
            ));
        }
    }
    if limits.remaining == Some(0) {
        if let Some(reset_at) = limits.reset_at {
            if reset_at > Utc::now() {
                return Err(UnifiedError::new(
                    "Provider rate limit is exhausted; back off until the reset time",
                    429,
                )
                .with_metadata("rate_limit_remaining", json!(0))
                .with_metadata("rate_limit_reset_at", json!(reset_at.to_rfc3339())));
            }
        }
    }
    Ok(())
}

pub(crate) fn record_rate_limit(state: &VcsState, key: &str, info: &RateLimitInfo) {
    let retry_after_until = info
        .retry_after_seconds
        .map(|secs| Instant::now() + Duration::from_secs(secs.min(MAX_RETRY_AFTER_SECS)));
    state.rate_limits.insert(
        key.to_string(),
        RateLimitState {
            remaining: info.remaining,
            reset_at: info.reset_at,
            retry_after_until,
        },
    );
}

pub(crate) fn rate_limit_key(provider: Provider, host: Option<&str>) -> String {
    match host.filter(|host| !host.is_empty()) {
        Some(host) => format!("{}:{host}", provider.as_str()),
        None => provider.as_str().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Draft creation (shared by import, sync, and webhooks).
// ---------------------------------------------------------------------------

/// Create a draft version from a provider release. The draft is always
/// `draft` status: nothing here publishes. Emits `vcs.release.imported`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create_draft(
    hooks: &Arc<HookBus>,
    state: &VcsState,
    connection: &RepositoryConnection,
    release: &ProviderRelease,
    source: &str,
    version_override: Option<String>,
    notes_override: Option<String>,
) -> UnifiedResult<VersionDraft> {
    let version = match version_override {
        Some(version) => validate_version_string(&version)?.to_string(),
        None => version_from_tag(&release.tag)?.to_string(),
    };
    if state.drafts.iter().any(|entry| {
        let draft = entry.value();
        draft.connection_id == connection.id && draft.tag == release.tag
    }) {
        return Err(UnifiedError::conflict(format!(
            "A draft already exists for tag \"{}\" on this connection",
            release.tag
        )));
    }
    let notes = notes_override
        .map(|notes| notes.chars().take(MAX_NOTES_LEN).collect())
        .unwrap_or_else(|| {
            release
                .notes
                .chars()
                .take(MAX_NOTES_LEN)
                .collect::<String>()
        });
    let assets: Vec<DraftAsset> = release
        .assets
        .iter()
        .filter_map(|asset| {
            Some(DraftAsset {
                name: asset.name.clone(),
                url: redact_url(&asset.url)?,
                size_bytes: asset.size,
                sha256: None,
                downloaded_at: None,
            })
        })
        .collect();
    let draft = VersionDraft {
        id: Uuid::now_v7(),
        connection_id: connection.id,
        software_id: connection.software_id.clone(),
        provider: connection.provider,
        version,
        tag: release.tag.clone(),
        title: release.name.clone(),
        changelog: notes,
        published_at: release.published_at,
        status: "draft".to_string(),
        source: source.to_string(),
        assets,
        created_at: now(),
        updated_at: now(),
    };
    state.drafts.insert(draft.id, draft.clone());
    let payload = serde_json::to_value(&draft).unwrap_or(Value::Null);
    hooks.emit("vcs.release.imported", payload);
    Ok(draft)
}

pub(crate) fn find_connection(state: &VcsState, id: Uuid) -> UnifiedResult<RepositoryConnection> {
    state
        .connections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))
}

pub(crate) fn find_connection_by_repo(
    state: &VcsState,
    provider: Provider,
    owner: &str,
    repo: &str,
) -> Option<RepositoryConnection> {
    state
        .connections
        .iter()
        .map(|entry| entry.value().clone())
        .find(|connection| {
            connection.provider == provider
                && connection.enabled
                && connection.owner.eq_ignore_ascii_case(owner)
                && connection.repo.eq_ignore_ascii_case(repo)
        })
}

pub(crate) fn connection_view(state: &VcsState, id: Uuid) -> UnifiedResult<RepositoryConnection> {
    find_connection(state, id)
}

// ---------------------------------------------------------------------------
// Provider handlers.
// ---------------------------------------------------------------------------

pub(crate) fn provider_view(state: &VcsState, provider: Provider) -> ProviderView {
    let settings = state.provider_settings(provider);
    ProviderView {
        provider: provider.as_str().to_string(),
        display_name: provider.display_name().to_string(),
        api_base: provider.api_base(Some(&settings), None),
        auth_scheme: provider.auth_scheme().to_string(),
        webhook_signature_header: provider.webhook_signature_header().to_string(),
        configured: settings
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty()),
        webhook_configured: settings
            .webhook_secret
            .as_deref()
            .is_some_and(|secret| !secret.is_empty()),
    }
}

pub(crate) fn record_sync_history(
    state: &VcsState,
    connection: &RepositoryConnection,
    status: &str,
    releases_found: u32,
    drafts_created: u32,
    error: Option<String>,
    rate_limit_remaining: Option<u32>,
    started_at: DateTime<Utc>,
) -> SyncHistoryEntry {
    let finished_at = now();
    let entry = SyncHistoryEntry {
        id: Uuid::now_v7(),
        connection_id: connection.id,
        provider: connection.provider,
        status: status.to_string(),
        releases_found,
        drafts_created,
        error,
        rate_limit_remaining,
        started_at,
        finished_at,
        duration_ms: finished_at
            .signed_duration_since(started_at)
            .num_milliseconds()
            .max(0) as u64,
    };
    state.sync_history.insert(entry.id, entry.clone());
    entry
}

pub(crate) fn list_sync_history_filtered(
    state: &VcsState,
    params: &PaginationParams,
    connection_id: Option<Uuid>,
) -> UnifiedResult<Value> {
    let sort = params.allowed_sort(&["status", "started_at"], "started_at");
    let mut items: Vec<SyncHistoryEntry> = state
        .sync_history
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            connection_id.is_none_or(|id| entry.connection_id == id)
                && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| entry.provider.as_str() == provider)
                && params
                    .filters
                    .get("status")
                    .is_none_or(|status| entry.status == *status)
                && params
                    .filters
                    .get("connection_id")
                    .is_none_or(|id| entry.connection_id.to_string() == *id)
        })
        .collect();
    match sort.as_str() {
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.started_at.cmp(&b.started_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    Ok(paginate(items, params).to_json())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limit_key_scopes_by_provider_and_host() {
        assert_eq!(rate_limit_key(Provider::GitHub, None), "github");
        assert_eq!(rate_limit_key(Provider::GitHub, Some("api.github.com")), "github:api.github.com");
        assert_eq!(rate_limit_key(Provider::GitLab, Some("")), "gitlab");
        assert_eq!(rate_limit_key(Provider::Bitbucket, None), "bitbucket");
    }
}
