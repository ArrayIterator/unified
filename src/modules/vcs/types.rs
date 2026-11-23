// Third-party VCS integrations — Public record, request, and view types.

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
use super::providers::*;
use super::store::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_providers::*;
use super::handlers_connections::*;
use super::handlers_drafts::*;
use super::webhooks::*;
use super::module::*;

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateProviderSettings {
    /// Personal access / OAuth token (write-only; empty clears it).
    #[serde(default)]
    pub token: Option<String>,
    /// Shared secret for webhook verification (write-only; empty clears it).
    #[serde(default)]
    pub webhook_secret: Option<String>,
    /// Optional API base URL override (empty clears it).
    #[serde(default)]
    pub api_base_override: Option<String>,
    /// Extra allowed self-hosted GitLab hostnames.
    #[serde(default)]
    pub gitlab_hosts: Option<Vec<String>>,
}

// ---------------------------------------------------------------------------
// Records.
// ---------------------------------------------------------------------------

/// A repository connected to a software record.
#[derive(Debug, Clone, Serialize)]
pub struct RepositoryConnection {
    pub id: Uuid,
    pub provider: Provider,
    pub owner: String,
    pub repo: String,
    /// Self-hosted GitLab host (hostname, optionally with a port).
    /// `None` means the provider's public cloud instance.
    pub host: Option<String>,
    /// Id of the software record this repository feeds (the software
    /// module's version id format: a UUID string).
    pub software_id: String,
    pub enabled: bool,
    /// `never_synced`, `syncing`, `synced`, or `failed`.
    pub sync_status: String,
    pub last_synced_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One asset attached to a draft version.
#[derive(Debug, Clone, Serialize)]
pub struct DraftAsset {
    pub name: String,
    /// Canonical asset URL with query string and fragment removed.
    pub url: String,
    pub size_bytes: Option<u64>,
    /// SHA-256 hex digest, set once the asset has been downloaded and
    /// verified through the allowlisted downloader.
    pub sha256: Option<String>,
    pub downloaded_at: Option<DateTime<Utc>>,
}

/// A software version imported from a provider, always a draft.
/// Nothing in this module publishes a draft.
#[derive(Debug, Clone, Serialize)]
pub struct VersionDraft {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub software_id: String,
    pub provider: Provider,
    pub version: String,
    pub tag: String,
    pub title: Option<String>,
    pub changelog: String,
    pub published_at: Option<DateTime<Utc>>,
    /// Always `draft`.
    pub status: String,
    /// `import`, `sync`, or `webhook`.
    pub source: String,
    pub assets: Vec<DraftAsset>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One manual or webhook-triggered sync run.
#[derive(Debug, Clone, Serialize)]
pub struct SyncHistoryEntry {
    pub id: Uuid,
    pub connection_id: Uuid,
    pub provider: Provider,
    /// `completed` or `failed`.
    pub status: String,
    pub releases_found: u32,
    pub drafts_created: u32,
    pub error: Option<String>,
    pub rate_limit_remaining: Option<u32>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub duration_ms: u64,
}

/// Rate-limit information surfaced in list metadata.
#[derive(Debug, Clone, Default, Serialize)]
pub struct RateLimitInfo {
    pub remaining: Option<u32>,
    pub reset_at: Option<DateTime<Utc>>,
    pub retry_after_seconds: Option<u64>,
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
pub struct CreateConnection {
    pub provider: Provider,
    pub owner: String,
    pub repo: String,
    #[serde(default)]
    pub host: Option<String>,
    pub software_id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateConnection {
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub software_id: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportRelease {
    /// Tag name on the provider, for example `v1.2.3`.
    pub tag: String,
    /// Optional semver override; defaults to the tag with a leading
    /// `v` stripped.
    #[serde(default)]
    pub version: Option<String>,
    /// Optional notes override; defaults to the provider release body.
    #[serde(default)]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DownloadAssetRequest {
    /// Release-asset URL. Must be on an allowlisted host.
    pub url: String,
    /// File name to record on the draft.
    pub name: String,
}
