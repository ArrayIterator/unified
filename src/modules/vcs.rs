// Third-party VCS integrations module.
//
// Provider abstraction for GitHub, GitLab, and Bitbucket: repository
// connections linked to software records, release/tag import as review
// drafts (never auto-published), signed webhook receivers, allowlisted
// release-asset downloads, manual sync with rate-limit handling, and
// sync history. Everything is emitted through the hook bus; nothing in
// this module calls another module directly.
//
// Settings keys read by this module (namespace `vcs`, managed through
// the settings system; the module also emits `settings.merge` so the
// settings module can persist them under `vcs.<provider>`):
//
//   - `vcs.github.token` / `vcs.gitlab.token` / `vcs.bitbucket.token`
//     (write-only personal access / OAuth token, never returned, never
//     logged)
//   - `vcs.github.webhook_secret` / `vcs.gitlab.webhook_secret` /
//     `vcs.bitbucket.webhook_secret` (shared secret used to verify
//     inbound webhooks)
//   - `vcs.github.api_base` / `vcs.gitlab.api_base` /
//     `vcs.bitbucket.api_base` (optional API base URL override)
//   - `vcs.gitlab.hosts` (JSON array of extra allowed self-hosted
//     GitLab hostnames, used both as API hosts and as allowed asset
//     download hosts)
//
// Persistence: this module keeps its records in memory (the same
// pattern as the software module). The wiring builder adds the
// Diesel tables listed at the bottom of this file's report note:
// `vcs_connections`, `vcs_version_drafts`, `vcs_sync_history`, and
// `vcs_provider_settings`.
//
// Security notes:
//   - Tokens and webhook secrets are write-only: they are accepted on
//     input, kept in a struct that does not implement `Serialize`, and
//     are never written to a log line, an error message, or a response
//     payload. Hook payloads carry only `configured: true/false`
//     flags.
//   - Release-asset downloads accept only allowlisted hosts
//     (`github.com`, `api.github.com`, `objects.githubusercontent.com`,
//     `gitlab.com`, `bitbucket.org`, `api.bitbucket.org`, plus the
//     configured GitLab hosts). Every redirect target is re-validated
//     against the same allowlist, the scheme must be `https` (plain
//     `http` is allowed only for configured self-hosted GitLab hosts),
//     URLs with userinfo or IP-literal hosts are rejected, and stored
//     asset URLs are redacted (query string and fragment removed).
//   - Webhooks are verified before any payload is acted on: GitHub
//     uses HMAC-SHA256 (`X-Hub-Signature-256`), GitLab uses its shared
//     token header (`X-Gitlab-Token`), and Bitbucket — which provides
//     no signature scheme — requires the configured webhook secret in
//     the `X-Webhook-Secret` header. Unsigned or unverifiable payloads
//     are rejected with the standard error envelope.
//   - Imported versions are always drafts. Nothing in this module
//     publishes a release; publishing stays with the software module.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext, HookHandler};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
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

type HmacSha256 = Hmac<Sha256>;

/// User-Agent sent with provider requests.
const USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));
/// Per-request timeout for provider API calls and asset downloads.
const HTTP_TIMEOUT_SECS: u64 = 30;
/// Maximum release-asset download size (512 MiB).
const MAX_ASSET_BYTES: u64 = 512 * 1024 * 1024;
/// Maximum redirects followed when downloading a release asset.
const MAX_ASSET_REDIRECTS: u32 = 3;
/// Longest `Retry-After` delay honoured from a provider.
const MAX_RETRY_AFTER_SECS: u64 = 3600;
/// Maximum length of a stored changelog / notes body.
const MAX_NOTES_LEN: usize = 20_000;
/// Maximum length of a tag, owner, or repository slug.
const MAX_SLUG_LEN: usize = 128;

/// Built-in hosts a release asset may be downloaded from. Configured
/// self-hosted GitLab hosts are added to this list at runtime.
const BUILTIN_ASSET_HOSTS: [&str; 6] = [
    "github.com",
    "api.github.com",
    "objects.githubusercontent.com",
    "gitlab.com",
    "bitbucket.org",
    "api.bitbucket.org",
];

// ---------------------------------------------------------------------------
// Provider abstraction.
// ---------------------------------------------------------------------------

/// A third-party version-control hosting provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    GitHub,
    GitLab,
    Bitbucket,
}

impl Provider {
    /// Parse a provider name from a path segment or filter value.
    pub fn parse(value: &str) -> UnifiedResult<Self> {
        match value.to_ascii_lowercase().as_str() {
            "github" => Ok(Self::GitHub),
            "gitlab" => Ok(Self::GitLab),
            "bitbucket" => Ok(Self::Bitbucket),
            _ => Err(UnifiedError::bad_request(format!(
                "Unknown VCS provider \"{value}\" (expected github, gitlab or bitbucket)"
            ))),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GitHub => "github",
            Self::GitLab => "gitlab",
            Self::Bitbucket => "bitbucket",
        }
    }

    pub fn display_name(&self) -> &'static str {
        match self {
            Self::GitHub => "GitHub",
            Self::GitLab => "GitLab",
            Self::Bitbucket => "Bitbucket",
        }
    }

    /// Default API base URL for the provider.
    pub fn default_api_base(&self) -> &'static str {
        match self {
            Self::GitHub => "https://api.github.com",
            Self::GitLab => "https://gitlab.com/api/v4",
            Self::Bitbucket => "https://api.bitbucket.org/2.0",
        }
    }

    /// Header name carrying the credential for this provider.
    /// GitLab personal access tokens use `Private-Token`; GitHub and
    /// Bitbucket use a Bearer token.
    pub fn auth_header_name(&self) -> header::HeaderName {
        match self {
            Self::GitLab => header::HeaderName::from_static("private-token"),
            _ => header::AUTHORIZATION,
        }
    }

    /// Header value for a credential. The token itself is never logged.
    pub fn auth_header_value(&self, token: &str) -> String {
        match self {
            Self::GitLab => token.to_string(),
            _ => format!("Bearer {token}"),
        }
    }

    /// Header carrying the webhook signature for this provider, if any.
    pub fn webhook_signature_header(&self) -> &'static str {
        match self {
            Self::GitHub => "x-hub-signature-256",
            Self::GitLab => "x-gitlab-token",
            // Bitbucket provides no signature scheme; the module checks
            // the configured shared secret in this custom header.
            Self::Bitbucket => "x-webhook-secret",
        }
    }
}

/// Behaviour every provider must expose: where its API lives, how it
/// authenticates, and where its releases and tags are listed.
pub trait VcsProvider {
    /// API base URL, honouring a settings override and, for GitLab, a
    /// self-hosted instance host.
    fn api_base(&self, settings: Option<&ProviderSettings>, host: Option<&str>) -> String;
    /// Short description of the auth scheme, for API responses.
    fn auth_scheme(&self) -> &'static str;
    /// Path (relative to the API base) listing releases.
    fn releases_path(&self, owner: &str, repo: &str) -> String;
    /// Path (relative to the API base) listing tags.
    fn tags_path(&self, owner: &str, repo: &str) -> String;
}

impl VcsProvider for Provider {
    fn api_base(&self, settings: Option<&ProviderSettings>, host: Option<&str>) -> String {
        if let Some(settings) = settings {
            if let Some(base) = &settings.api_base_override {
                if !base.trim().is_empty() {
                    return base.trim().trim_end_matches('/').to_string();
                }
            }
        }
        if let Self::GitLab = self {
            if let Some(host) = host.filter(|host| !host.trim().is_empty()) {
                return format!("https://{}/api/v4", host.trim());
            }
        }
        self.default_api_base().to_string()
    }

    fn auth_scheme(&self) -> &'static str {
        match self {
            Self::GitLab => "private-token",
            _ => "bearer",
        }
    }

    fn releases_path(&self, owner: &str, repo: &str) -> String {
        match self {
            Self::GitHub => format!("/repos/{owner}/{repo}/releases"),
            Self::GitLab => format!(
                "/projects/{}/releases",
                encode_path_segment(&format!("{owner}/{repo}"))
            ),
            // Bitbucket has no releases API; tags are the release list.
            Self::Bitbucket => self.tags_path(owner, repo),
        }
    }

    fn tags_path(&self, owner: &str, repo: &str) -> String {
        match self {
            Self::GitHub => format!("/repos/{owner}/{repo}/tags"),
            Self::GitLab => format!(
                "/projects/{}/repository/tags",
                encode_path_segment(&format!("{owner}/{repo}"))
            ),
            Self::Bitbucket => format!("/repositories/{owner}/{repo}/refs/tags"),
        }
    }
}

/// Percent-encode one path segment (everything except the unreserved
/// set). Used for GitLab project paths, where `owner/repo` becomes a
/// single URL-encoded segment.
fn encode_path_segment(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Provider settings (write-only credentials).
// ---------------------------------------------------------------------------

/// Per-provider settings. This struct deliberately does NOT implement
/// `Serialize`: the token and webhook secret must never be serialized
/// into a response, a log line, or a hook payload.
#[derive(Debug, Clone, Default)]
pub struct ProviderSettings {
    pub token: Option<String>,
    pub webhook_secret: Option<String>,
    pub api_base_override: Option<String>,
    pub gitlab_hosts: Vec<String>,
}

/// Public view of a provider's settings: configuration flags only.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderSettingsView {
    pub provider: String,
    pub configured: bool,
    pub webhook_configured: bool,
    pub api_base: String,
    pub gitlab_hosts: Vec<String>,
}

/// Public view of a provider.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderView {
    pub provider: String,
    pub display_name: String,
    pub api_base: String,
    pub auth_scheme: String,
    pub webhook_signature_header: String,
    pub configured: bool,
    pub webhook_configured: bool,
}

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

/// Provider rate-limit state, kept per provider (and per GitLab host).
#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub remaining: Option<u32>,
    pub reset_at: Option<DateTime<Utc>>,
    pub retry_after_until: Option<Instant>,
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

// ---------------------------------------------------------------------------
// Module state.
// ---------------------------------------------------------------------------

pub struct VcsState {
    hooks: RwLock<Arc<HookBus>>,
    pub connections: DashMap<Uuid, RepositoryConnection>,
    pub drafts: DashMap<Uuid, VersionDraft>,
    pub sync_history: DashMap<Uuid, SyncHistoryEntry>,
    pub provider_settings: DashMap<String, ProviderSettings>,
    pub rate_limits: DashMap<String, RateLimitState>,
}

impl VcsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            connections: DashMap::new(),
            drafts: DashMap::new(),
            sync_history: DashMap::new(),
            provider_settings: DashMap::new(),
            rate_limits: DashMap::new(),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    fn provider_settings(&self, provider: Provider) -> ProviderSettings {
        self.provider_settings
            .get(provider.as_str())
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Extra allowed GitLab hosts for one provider (settings key
    /// `vcs.gitlab.hosts`).
    fn gitlab_hosts(&self, provider: Provider) -> Vec<String> {
        if provider == Provider::GitLab {
            self.provider_settings(provider).gitlab_hosts
        } else {
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Small helpers.
// ---------------------------------------------------------------------------

fn now() -> DateTime<Utc> {
    Utc::now()
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Owner / repository slug: letters, digits, `-`, `_`, `.`; no slashes,
/// no whitespace, not `.` or `..`.
fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SLUG_LEN
        && value != "."
        && value != ".."
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// Hostname with an optional `:port`. No scheme, no path, no userinfo.
fn valid_host(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || value.contains('/') || value.contains('@') {
        return false;
    }
    let (host, port) = match value.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (value, None),
    };
    if let Some(port) = port {
        if port.is_empty() || port.parse::<u16>().is_err() {
            return false;
        }
    }
    !host.is_empty()
        && host
            .split('.')
            .all(|label| {
                !label.is_empty()
                    && label.len() <= 63
                    && label
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-')
                    && !label.starts_with('-')
                    && !label.ends_with('-')
            })
}

/// Parse a version from a tag: strip one leading `v`/`V`, then require
/// strict semver.
fn version_from_tag(tag: &str) -> UnifiedResult<Version> {
    let stripped = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
    Version::parse(stripped).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Tag \"{tag}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH, optionally prefixed with \"v\")"
        ))
    })
}

fn validate_version_string(version: &str) -> UnifiedResult<Version> {
    Version::parse(version.trim()).map_err(|_| {
        UnifiedError::bad_request(format!(
            "Version \"{version}\" is not a valid semantic version (expected MAJOR.MINOR.PATCH)"
        ))
    })
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Constant-time string comparison, so signature and token checks do
/// not leak how many leading characters matched.
fn secrets_equal(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.bytes()
        .zip(b.bytes())
        .fold(0u8, |acc, (x, y)| acc | (x ^ y))
        == 0
}

/// Canonical asset URL for storage: scheme, host, and path only. The
/// query string and fragment are removed because provider asset URLs
/// can carry signed query parameters, which must never be stored or
/// returned. Returns `None` when the URL does not parse.
fn redact_url(raw: &str) -> Option<String> {
    let mut url = url::Url::parse(raw).ok()?;
    url.set_query(None);
    url.set_fragment(None);
    let _ = url.set_username("");
    let _ = url.set_password(None);
    Some(url.to_string())
}

fn parse_datetime(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|parsed| parsed.with_timezone(&Utc))
}

/// True when the host is an IP literal (never an allowed asset host).
fn is_ip_literal(host: &str) -> bool {
    host.parse::<IpAddr>().is_ok()
}

/// True when `host` is an allowed release-asset download host.
fn allowed_asset_host(host: &str, gitlab_hosts: &[String]) -> bool {
    let host = host.to_ascii_lowercase();
    if BUILTIN_ASSET_HOSTS.contains(&host.as_str()) {
        return true;
    }
    gitlab_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host)
}

/// Validate a release-asset URL against the allowlist (SSRF guard).
/// Only `https` is accepted, except plain `http` for configured
/// self-hosted GitLab hosts. Rejects userinfo, IP-literal hosts, and
/// any host outside the allowlist. The URL is never echoed back in an
/// error: the message names only the offending host.
fn validate_asset_url(raw: &str, gitlab_hosts: &[String]) -> UnifiedResult<url::Url> {
    let url = url::Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request("Asset URL is not a valid URL"))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Asset URL must not contain credentials",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| UnifiedError::bad_request("Asset URL has no host"))?
        .to_ascii_lowercase();
    if is_ip_literal(&host) {
        return Err(UnifiedError::forbidden(
            "Asset downloads from IP-literal hosts are not allowed",
        ));
    }
    if !allowed_asset_host(&host, gitlab_hosts) {
        return Err(UnifiedError::forbidden(format!(
            "Asset host \"{host}\" is not allowlisted for downloads"
        )));
    }
    let is_configured_gitlab = gitlab_hosts
        .iter()
        .any(|allowed| allowed.to_ascii_lowercase() == host);
    match url.scheme() {
        "https" => Ok(url),
        "http" if is_configured_gitlab => Ok(url),
        _ => Err(UnifiedError::bad_request(
            "Asset URL must use https (http is allowed only for configured self-hosted GitLab hosts)",
        )),
    }
}

fn build_client() -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

/// Read rate-limit headers from a provider response.
fn rate_limit_from_headers(headers: &header::HeaderMap) -> RateLimitInfo {
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
fn check_rate_limit(state: &VcsState, key: &str) -> UnifiedResult<()> {
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

fn record_rate_limit(state: &VcsState, key: &str, info: &RateLimitInfo) {
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

fn rate_limit_key(provider: Provider, host: Option<&str>) -> String {
    match host.filter(|host| !host.is_empty()) {
        Some(host) => format!("{}:{host}", provider.as_str()),
        None => provider.as_str().to_string(),
    }
}

// ---------------------------------------------------------------------------
// Provider API: releases and tags.
// ---------------------------------------------------------------------------

/// One release or tag as returned by a provider.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderRelease {
    pub tag: String,
    /// Semver parsed from the tag, when the tag is a valid version.
    pub version: Option<String>,
    pub name: Option<String>,
    pub notes: String,
    pub published_at: Option<DateTime<Utc>>,
    pub prerelease: bool,
    pub assets: Vec<ProviderAsset>,
}

/// One downloadable file attached to a provider release.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderAsset {
    pub name: String,
    pub url: String,
    pub size: Option<u64>,
}

/// Releases fetched from a provider, with the rate-limit state the
/// provider reported (surfaced in response metadata, never as an
/// error unless the provider actually failed).
pub struct ProviderReleases {
    pub items: Vec<ProviderRelease>,
    pub rate_limit: RateLimitInfo,
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(|field| field.as_str())
        .map(str::to_string)
}

fn parse_release_list(provider: Provider, body: &Value) -> Vec<ProviderRelease> {
    let values: Vec<&Value> = match provider {
        Provider::Bitbucket => body
            .get("values")
            .and_then(|values| values.as_array())
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
        _ => body
            .as_array()
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        match provider {
            Provider::GitHub => {
                // Skip provider-side drafts: only real releases are
                // importable.
                if value
                    .get("draft")
                    .and_then(|field| field.as_bool())
                    .unwrap_or(false)
                {
                    continue;
                }
                let Some(tag) = string_field(value, "tag_name") else {
                    continue;
                };
                let assets = value
                    .get("assets")
                    .and_then(|assets| assets.as_array())
                    .map(|assets| {
                        assets
                            .iter()
                            .filter_map(|asset| {
                                Some(ProviderAsset {
                                    name: string_field(asset, "name")?,
                                    url: string_field(asset, "browser_download_url")
                                        .or_else(|| string_field(asset, "url"))?,
                                    size: asset.get("size").and_then(|size| size.as_u64()),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(value, "name"),
                    notes: string_field(value, "body").unwrap_or_default(),
                    published_at: parse_datetime(
                        value.get("published_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: value
                        .get("prerelease")
                        .and_then(|field| field.as_bool())
                        .unwrap_or(false),
                    assets,
                });
            }
            Provider::GitLab => {
                let Some(tag) = string_field(value, "tag_name") else {
                    continue;
                };
                let assets = value
                    .get("assets")
                    .and_then(|assets| assets.get("links"))
                    .and_then(|links| links.as_array())
                    .map(|links| {
                        links
                            .iter()
                            .filter_map(|link| {
                                Some(ProviderAsset {
                                    name: string_field(link, "name")?,
                                    url: string_field(link, "url")?,
                                    size: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                out.push(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(value, "name"),
                    notes: string_field(value, "description").unwrap_or_default(),
                    published_at: parse_datetime(
                        value.get("released_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: false,
                    assets,
                });
            }
            Provider::Bitbucket => {
                let Some(tag) = string_field(value, "name") else {
                    continue;
                };
                let published_at = value
                    .get("target")
                    .and_then(|target| target.get("date"))
                    .and_then(|date| date.as_str());
                out.push(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: None,
                    notes: String::new(),
                    published_at: parse_datetime(published_at),
                    prerelease: false,
                    assets: Vec::new(),
                });
            }
        }
    }
    out
}

fn parse_tag_list(provider: Provider, body: &Value) -> Vec<ProviderRelease> {
    let values: Vec<&Value> = match provider {
        Provider::Bitbucket => body
            .get("values")
            .and_then(|values| values.as_array())
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
        _ => body
            .as_array()
            .map(|values| values.iter().collect())
            .unwrap_or_default(),
    };
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let (tag, published_at) = match provider {
            Provider::GitHub => (
                string_field(value, "name"),
                value
                    .get("commit")
                    .and_then(|commit| commit.get("created_at"))
                    .and_then(|date| date.as_str())
                    .map(str::to_string),
            ),
            Provider::GitLab => (
                string_field(value, "name"),
                value
                    .get("commit")
                    .and_then(|commit| commit.get("created_at"))
                    .and_then(|date| date.as_str())
                    .map(str::to_string),
            ),
            Provider::Bitbucket => (
                string_field(value, "name"),
                value
                    .get("target")
                    .and_then(|target| target.get("date"))
                    .and_then(|date| date.as_str())
                    .map(str::to_string),
            ),
        };
        let Some(tag) = tag else { continue };
        out.push(ProviderRelease {
            version: version_from_tag(&tag).ok().map(|version| version.to_string()),
            tag,
            name: None,
            notes: String::new(),
            published_at: parse_datetime(published_at.as_deref()),
            prerelease: false,
            assets: Vec::new(),
        });
    }
    out
}

/// Fetch one page from a provider API path. Returns the parsed JSON
/// body together with the rate-limit state the provider reported.
/// Provider failures are mapped to the standard error envelope; the
/// credential is sent only as a header and never appears in any
/// message.
async fn provider_get(
    state: &VcsState,
    provider: Provider,
    connection: &RepositoryConnection,
    path: &str,
    page: u32,
    per_page: u32,
) -> UnifiedResult<(Value, RateLimitInfo)> {
    let settings = state.provider_settings(provider);
    let base = provider.api_base(Some(&settings), connection.host.as_deref());
    let separator = if path.contains('?') { "&" } else { "?" };
    let (page_param, per_page_param) = match provider {
        // Bitbucket uses `pagelen` instead of `per_page`.
        Provider::Bitbucket => ("page", "pagelen"),
        _ => ("page", "per_page"),
    };
    let url = format!("{base}{path}{separator}{page_param}={page}&{per_page_param}={per_page}");
    let client = build_client()?;
    let mut request = client.get(url);
    if let Some(token) = settings.token.as_deref().filter(|token| !token.is_empty()) {
        request = request.header(provider.auth_header_name(), provider.auth_header_value(token));
    }
    match provider {
        Provider::GitHub => {
            request = request.header(header::ACCEPT, "application/vnd.github+json");
        }
        _ => {}
    }
    let response = request
        .send()
        .await
        .map_err(|error| UnifiedError::internal(format!("Provider request failed: {error}")))?;
    let limits = rate_limit_from_headers(response.headers());
    record_rate_limit(state, &rate_limit_key(provider, connection.host.as_deref()), &limits);
    let status = response.status();
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(UnifiedError::new(
            "Provider rate limit reached; back off before retrying",
            429,
        )
        .with_metadata(
            "retry_after_seconds",
            limits
                .retry_after_seconds
                .map(Value::from)
                .unwrap_or(Value::Null),
        )
        .with_metadata(
            "rate_limit_remaining",
            limits.remaining.map(Value::from).unwrap_or(Value::Null),
        ));
    }
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(UnifiedError::not_found(format!(
            "Repository \"{}/{}\" was not found on {}",
            connection.owner,
            connection.repo,
            provider.display_name()
        )));
    }
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(UnifiedError::unauthorized(format!(
            "Provider authentication failed for {} (check the configured token)",
            provider.display_name()
        )));
    }
    if !status.is_success() {
        return Err(UnifiedError::internal(format!(
            "Provider request failed with HTTP {status}"
        )));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| UnifiedError::internal(format!("Provider returned invalid JSON: {error}")))?;
    Ok((body, limits))
}

/// List releases for a connection (paginated). When the provider has
/// no releases, tags are listed instead so a tag-only repository can
/// still be imported. Bitbucket lists tags directly.
async fn fetch_releases(
    state: &VcsState,
    connection: &RepositoryConnection,
    page: u32,
    per_page: u32,
) -> UnifiedResult<ProviderReleases> {
    let provider = connection.provider;
    check_rate_limit(state, &rate_limit_key(provider, connection.host.as_deref()))?;
    if provider == Provider::Bitbucket {
        let path = provider.tags_path(&connection.owner, &connection.repo);
        let (body, limits) =
            provider_get(state, provider, connection, &path, page, per_page).await?;
        return Ok(ProviderReleases {
            items: parse_tag_list(provider, &body),
            rate_limit: limits,
        });
    }
    let releases_path = provider.releases_path(&connection.owner, &connection.repo);
    let (body, limits) =
        provider_get(state, provider, connection, &releases_path, page, per_page).await?;
    let items = parse_release_list(provider, &body);
    if !items.is_empty() {
        return Ok(ProviderReleases {
            items,
            rate_limit: limits,
        });
    }
    let tags_path = provider.tags_path(&connection.owner, &connection.repo);
    let (tag_body, tag_limits) =
        provider_get(state, provider, connection, &tags_path, page, per_page).await?;
    Ok(ProviderReleases {
        items: parse_tag_list(provider, &tag_body),
        rate_limit: tag_limits,
    })
}

// ---------------------------------------------------------------------------
// Draft creation (shared by import, sync, and webhooks).
// ---------------------------------------------------------------------------

/// Create a draft version from a provider release. The draft is always
/// `draft` status: nothing here publishes. Emits `vcs.release.imported`.
#[allow(clippy::too_many_arguments)]
fn create_draft(
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

fn find_connection(state: &VcsState, id: Uuid) -> UnifiedResult<RepositoryConnection> {
    state
        .connections
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))
}

fn find_connection_by_repo(
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

fn connection_view(state: &VcsState, id: Uuid) -> UnifiedResult<RepositoryConnection> {
    find_connection(state, id)
}

/// Ask the hook bus whether a software id is acceptable. When no
/// handler is registered the id is accepted; a handler that answers
/// `valid: false` rejects it with 422.
fn validate_software_id(ctx: &AppContext, software_id: &str) -> UnifiedResult<()> {
    if !ctx.hooks.has("vcs.software.validate") {
        return Ok(());
    }
    let context = ctx.hooks.emit(
        "vcs.software.validate",
        json!({ "software_id": software_id }),
    );
    let invalid = context
        .results
        .iter()
        .any(|result| result.get("valid").and_then(|value| value.as_bool()) == Some(false));
    if invalid {
        return Err(UnifiedError::unprocessable(format!(
            "Software \"{software_id}\" is not valid for a VCS connection"
        )));
    }
    Ok(())
}

fn validate_connection_input(
    provider: Provider,
    owner: &str,
    repo: &str,
    host: Option<&str>,
    software_id: &str,
) -> UnifiedResult<()> {
    if !valid_slug(owner) {
        return Err(UnifiedError::bad_request(
            "Owner must be a slug of letters, digits, '-', '_' or '.'",
        ));
    }
    if !valid_slug(repo) {
        return Err(UnifiedError::bad_request(
            "Repository must be a slug of letters, digits, '-', '_' or '.'",
        ));
    }
    if let Some(host) = host.filter(|host| !host.is_empty()) {
        if provider != Provider::GitLab {
            return Err(UnifiedError::bad_request(
                "A custom host is only supported for self-hosted GitLab instances",
            ));
        }
        if !valid_host(host) {
            return Err(UnifiedError::bad_request(
                "Host must be a hostname, optionally with a port (no scheme or path)",
            ));
        }
    }
    if software_id.trim().is_empty() || software_id.len() > 256 {
        return Err(UnifiedError::bad_request(
            "software_id is required and must be at most 256 characters",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Provider handlers.
// ---------------------------------------------------------------------------

fn provider_view(state: &VcsState, provider: Provider) -> ProviderView {
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

async fn list_providers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let providers = [Provider::GitHub, Provider::GitLab, Provider::Bitbucket]
        .iter()
        .map(|provider| provider_view(&state, *provider))
        .collect::<Vec<ProviderView>>();
    json::respond(UnifiedResult::Ok(providers), debug)
}

async fn get_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderView> =
        Provider::parse(&path.into_inner()).map(|provider| provider_view(&state, provider));
    json::respond(result, debug)
}

async fn get_provider_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderSettingsView> = (|| {
        let provider = Provider::parse(&path.into_inner())?;
        let settings = state.provider_settings(provider);
        Ok(ProviderSettingsView {
            provider: provider.as_str().to_string(),
            configured: settings
                .token
                .as_deref()
                .is_some_and(|token| !token.is_empty()),
            webhook_configured: settings
                .webhook_secret
                .as_deref()
                .is_some_and(|secret| !secret.is_empty()),
            api_base: provider.api_base(Some(&settings), None),
            gitlab_hosts: settings.gitlab_hosts.clone(),
        })
    })();
    json::respond(result, debug)
}

async fn update_provider_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    path: web::Path<String>,
    body: web::Json<UpdateProviderSettings>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<ProviderSettingsView> = (|| {
        let provider = Provider::parse(&path.into_inner())?;
        let input = body.into_inner();
        if input.token.is_none()
            && input.webhook_secret.is_none()
            && input.api_base_override.is_none()
            && input.gitlab_hosts.is_none()
        {
            return Err(UnifiedError::bad_request(
                "No settings provided (expected token, webhook_secret, api_base_override, or gitlab_hosts)",
            ));
        }
        if let Some(base) = &input.api_base_override {
            if !base.trim().is_empty() {
                let parsed = url::Url::parse(base.trim()).map_err(|_| {
                    UnifiedError::bad_request("api_base_override must be a valid URL")
                })?;
                if parsed.scheme() != "https" {
                    return Err(UnifiedError::bad_request(
                        "api_base_override must use https",
                    ));
                }
            }
        }
        if let Some(hosts) = &input.gitlab_hosts {
            for host in hosts {
                if !valid_host(host) {
                    return Err(UnifiedError::bad_request(format!(
                        "GitLab host \"{host}\" is not a valid hostname"
                    )));
                }
            }
        }
        let mut settings = state.provider_settings(provider);
        if let Some(token) = input.token {
            settings.token = if token.trim().is_empty() {
                None
            } else {
                Some(token.trim().to_string())
            };
        }
        if let Some(secret) = input.webhook_secret {
            settings.webhook_secret = if secret.trim().is_empty() {
                None
            } else {
                Some(secret)
            };
        }
        if let Some(base) = input.api_base_override {
            settings.api_base_override = if base.trim().is_empty() {
                None
            } else {
                Some(base.trim().to_string())
            };
        }
        if let Some(hosts) = input.gitlab_hosts {
            settings.gitlab_hosts = hosts
                .into_iter()
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect();
        }
        let configured = settings
            .token
            .as_deref()
            .is_some_and(|token| !token.is_empty());
        let webhook_configured = settings
            .webhook_secret
            .as_deref()
            .is_some_and(|secret| !secret.is_empty());
        state
            .provider_settings
            .insert(provider.as_str().to_string(), settings.clone());
        // Merge into the settings system under `vcs.<provider>` without
        // ever carrying the credential value: only flags are emitted.
        ctx.hooks.emit(
            "settings.merge",
            json!({
                "namespace": format!("vcs.{}", provider.as_str()),
                "settings": {
                    "token_configured": configured,
                    "webhook_configured": webhook_configured,
                    "api_base": provider.api_base(Some(&settings), None),
                    "gitlab_hosts": settings.gitlab_hosts,
                },
            }),
        );
        ctx.hooks.emit(
            "vcs.provider.settings.updated",
            json!({
                "provider": provider.as_str(),
                "configured": configured,
                "webhook_configured": webhook_configured,
            }),
        );
        Ok(ProviderSettingsView {
            provider: provider.as_str().to_string(),
            configured,
            webhook_configured,
            api_base: provider.api_base(Some(&settings), None),
            gitlab_hosts: settings.gitlab_hosts.clone(),
        })
    })();
    match result {
        Ok(view) => json::success_response(view, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Connection handlers.
// ---------------------------------------------------------------------------

async fn list_connections(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = params.into_inner();
        let sort = params.allowed_sort(
            &["provider", "owner", "repo", "sync_status", "created_at"],
            "created_at",
        );
        let mut items: Vec<RepositoryConnection> = state
            .connections
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|connection| {
                matches_search(
                    &format!(
                        "{} {}/{} {}",
                        connection.provider.as_str(),
                        connection.owner,
                        connection.repo,
                        connection.software_id
                    ),
                    &params,
                ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| connection.provider.as_str() == provider)
                    && params
                        .filters
                        .get("software_id")
                        .is_none_or(|software_id| connection.software_id == *software_id)
                    && params.filters.get("enabled").is_none_or(|flag| {
                        connection.enabled == (*flag == "true" || *flag == "1")
                    })
                    && params
                        .filters
                        .get("sync_status")
                        .is_none_or(|status| connection.sync_status == *status)
            })
            .collect();
        match sort.as_str() {
            "provider" => items.sort_by(|a, b| a.provider.as_str().cmp(b.provider.as_str())),
            "owner" => items.sort_by(|a, b| a.owner.cmp(&b.owner)),
            "repo" => items.sort_by(|a, b| a.repo.cmp(&b.repo)),
            "sync_status" => items.sort_by(|a, b| a.sync_status.cmp(&b.sync_status)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn create_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    body: web::Json<CreateConnection>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RepositoryConnection> = (|| {
        let input = body.into_inner();
        let owner = input.owner.trim().to_string();
        let repo = input.repo.trim().to_string();
        let host = input
            .host
            .map(|host| host.trim().to_string())
            .filter(|host| !host.is_empty());
        let software_id = input.software_id.trim().to_string();
        validate_connection_input(
            input.provider,
            &owner,
            &repo,
            host.as_deref(),
            &software_id,
        )?;
        validate_software_id(&ctx, &software_id)?;
        if state.connections.iter().any(|entry| {
            let connection = entry.value();
            connection.provider == input.provider
                && connection.owner.eq_ignore_ascii_case(&owner)
                && connection.repo.eq_ignore_ascii_case(&repo)
                && connection.host.as_deref() == host.as_deref()
        }) {
            return Err(UnifiedError::conflict(format!(
                "A connection already exists for {}/{} on {}",
                owner,
                repo,
                input.provider.display_name()
            )));
        }
        let connection = RepositoryConnection {
            id: Uuid::now_v7(),
            provider: input.provider,
            owner,
            repo,
            host,
            software_id,
            enabled: true,
            sync_status: "never_synced".to_string(),
            last_synced_at: None,
            last_error: None,
            created_at: now(),
            updated_at: now(),
        };
        state
            .connections
            .insert(connection.id, connection.clone());
        Ok(connection)
    })();
    match result {
        Ok(connection) => json::success_response(connection, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(connection_view(&state, id.into_inner()), debug)
}

async fn update_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateConnection>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<RepositoryConnection> = (|| {
        let input = body.into_inner();
        let mut connection = state
            .connections
            .get_mut(&id.into_inner())
            .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))?;
        let owner = input
            .owner
            .map(|owner| owner.trim().to_string())
            .unwrap_or_else(|| connection.owner.clone());
        let repo = input
            .repo
            .map(|repo| repo.trim().to_string())
            .unwrap_or_else(|| connection.repo.clone());
        let host = match input.host {
            Some(host) => {
                let host = host.trim().to_string();
                if host.is_empty() {
                    None
                } else {
                    Some(host)
                }
            }
            None => connection.host.clone(),
        };
        let software_id = input
            .software_id
            .map(|software_id| software_id.trim().to_string())
            .unwrap_or_else(|| connection.software_id.clone());
        validate_connection_input(
            connection.provider,
            &owner,
            &repo,
            host.as_deref(),
            &software_id,
        )?;
        if let Some(software_id) = &input.software_id {
            validate_software_id(&ctx, software_id.trim())?;
        }
        connection.owner = owner;
        connection.repo = repo;
        connection.host = host;
        connection.software_id = software_id;
        if let Some(enabled) = input.enabled {
            connection.enabled = enabled;
        }
        connection.updated_at = now();
        Ok(connection.clone())
    })();
    json::respond(result, debug)
}

async fn delete_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        state
            .connections
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))
    })();
    json::respond(result, debug)
}

fn set_connection_enabled(
    state: &VcsState,
    id: Uuid,
    enabled: bool,
) -> UnifiedResult<RepositoryConnection> {
    let mut connection = state
        .connections
        .get_mut(&id)
        .ok_or_else(|| UnifiedError::not_found("Repository connection not found"))?;
    connection.enabled = enabled;
    connection.updated_at = now();
    Ok(connection.clone())
}

async fn enable_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(set_connection_enabled(&state, id.into_inner(), true), debug)
}

async fn disable_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(
        set_connection_enabled(&state, id.into_inner(), false),
        debug,
    )
}

// ---------------------------------------------------------------------------
// Provider releases, import, sync, and sync history.
// ---------------------------------------------------------------------------

async fn list_connection_releases(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let connection = find_connection(&state, id.into_inner())?;
        if !connection.enabled {
            return Err(UnifiedError::conflict(
                "Repository connection is disabled",
            ));
        }
        let pagination = params.normalized();
        let releases = fetch_releases(
            &state,
            &connection,
            pagination.page,
            pagination.per_page,
        )
        .await?;
        let total = releases.items.len() as u64;
        let page = PageResult::new(releases.items, total, &pagination);
        Ok(json!({
            "items": page.items,
            "pagination": page.pagination,
            "rate_limit": releases.rate_limit,
            // The provider paginates independently: `total` counts only
            // this provider page, so callers page through with `page`.
            "note": "pagination.total counts this provider page only; request the next page for more results",
        }))
    })()
    .await;
    json::respond(result, debug)
}

async fn import_release(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
    body: web::Json<ImportRelease>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VersionDraft> = (|| async {
        let connection = find_connection(&state, id.into_inner())?;
        if !connection.enabled {
            return Err(UnifiedError::conflict(
                "Repository connection is disabled",
            ));
        }
        let input = body.into_inner();
        let tag = input.tag.trim().to_string();
        if tag.is_empty() || tag.len() > MAX_SLUG_LEN {
            return Err(UnifiedError::bad_request("tag is required"));
        }
        if let Some(version) = &input.version {
            validate_version_string(version)?;
        }
        // Fetch the provider list so the import only succeeds for a
        // release or tag that really exists on the provider.
        let releases = fetch_releases(&state, &connection, 1, 100).await?;
        let release = releases
            .items
            .iter()
            .find(|release| release.tag == tag)
            .ok_or_else(|| {
                UnifiedError::not_found(format!(
                    "Release or tag \"{tag}\" was not found on {}",
                    connection.provider.display_name()
                ))
            })?
            .clone();
        create_draft(
            &ctx.hooks,
            &state,
            &connection,
            &release,
            "import",
            input.version,
            input.notes,
        )
    })()
    .await;
    match result {
        Ok(draft) => json::success_response(draft, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

fn record_sync_history(
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

async fn sync_connection(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let connection = find_connection(&state, id.into_inner())?;
        if !connection.enabled {
            return Err(UnifiedError::conflict(
                "Repository connection is disabled",
            ));
        }
        let started_at = now();
        if let Some(mut entry) = state.connections.get_mut(&connection.id) {
            entry.sync_status = "syncing".to_string();
            entry.updated_at = now();
        }
        match fetch_releases(&state, &connection, 1, 30).await {
            Ok(releases) => {
                let releases_found = releases.items.len() as u32;
                let mut drafts_created: u32 = 0;
                for release in &releases.items {
                    match create_draft(
                        &ctx.hooks,
                        &state,
                        &connection,
                        release,
                        "sync",
                        None,
                        None,
                    ) {
                        Ok(_) => drafts_created += 1,
                        // A draft for this tag already exists: skip it,
                        // it is not a sync failure.
                        Err(error) if error.code == 409 => {}
                        Err(error) => {
                            let message = error.message.clone();
                            if let Some(mut entry) = state.connections.get_mut(&connection.id) {
                                entry.sync_status = "failed".to_string();
                                entry.last_error = Some(message.clone());
                                entry.updated_at = now();
                            }
                            let history = record_sync_history(
                                &state,
                                &connection,
                                "failed",
                                releases_found,
                                drafts_created,
                                Some(message.clone()),
                                releases.rate_limit.remaining,
                                started_at,
                            );
                            ctx.hooks.emit(
                                "vcs.sync.failed",
                                json!({
                                    "connection_id": connection.id,
                                    "provider": connection.provider.as_str(),
                                    "error": message,
                                    "sync_history_id": history.id,
                                }),
                            );
                            return Err(error);
                        }
                    }
                }
                if let Some(mut entry) = state.connections.get_mut(&connection.id) {
                    entry.sync_status = "synced".to_string();
                    entry.last_synced_at = Some(now());
                    entry.last_error = None;
                    entry.updated_at = now();
                }
                let history = record_sync_history(
                    &state,
                    &connection,
                    "completed",
                    releases_found,
                    drafts_created,
                    None,
                    releases.rate_limit.remaining,
                    started_at,
                );
                let connection = find_connection(&state, connection.id)?;
                ctx.hooks.emit(
                    "vcs.sync.completed",
                    json!({
                        "connection_id": connection.id,
                        "provider": connection.provider.as_str(),
                        "releases_found": releases_found,
                        "drafts_created": drafts_created,
                        "sync_history_id": history.id,
                    }),
                );
                Ok(json!({
                    "connection": connection,
                    "releases_found": releases_found,
                    "drafts_created": drafts_created,
                    "sync_history_id": history.id,
                    "rate_limit": releases.rate_limit,
                }))
            }
            Err(error) => {
                let message = error.message.clone();
                if let Some(mut entry) = state.connections.get_mut(&connection.id) {
                    entry.sync_status = "failed".to_string();
                    entry.last_error = Some(message.clone());
                    entry.updated_at = now();
                }
                let history = record_sync_history(
                    &state,
                    &connection,
                    "failed",
                    0,
                    0,
                    Some(message.clone()),
                    None,
                    started_at,
                );
                ctx.hooks.emit(
                    "vcs.sync.failed",
                    json!({
                        "connection_id": connection.id,
                        "provider": connection.provider.as_str(),
                        "error": message,
                        "sync_history_id": history.id,
                    }),
                );
                Err(error)
            }
        }
    })()
    .await;
    json::respond(result, debug)
}

fn list_sync_history_filtered(
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

async fn list_connection_sync_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let connection_id = id.into_inner();
        find_connection(&state, connection_id)?;
        list_sync_history_filtered(&state, &params.into_inner(), Some(connection_id))
    })();
    json::respond(result, debug)
}

async fn list_sync_history(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> =
        list_sync_history_filtered(&state, &params.into_inner(), None);
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Draft handlers and release-asset download.
// ---------------------------------------------------------------------------

async fn list_drafts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = params.into_inner();
        let sort = params.allowed_sort(&["provider", "version", "tag", "created_at"], "created_at");
        let mut items: Vec<VersionDraft> = state
            .drafts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|draft| {
                matches_search(
                    &format!("{} {} {} {}", draft.version, draft.tag, draft.changelog, draft.software_id),
                    &params,
                ) && params
                    .filters
                    .get("provider")
                    .is_none_or(|provider| draft.provider.as_str() == provider)
                    && params
                        .filters
                        .get("connection_id")
                        .is_none_or(|id| draft.connection_id.to_string() == *id)
                    && params
                        .filters
                        .get("software_id")
                        .is_none_or(|software_id| draft.software_id == *software_id)
                    && params
                        .filters
                        .get("status")
                        .is_none_or(|status| draft.status == *status)
            })
            .collect();
        match sort.as_str() {
            "provider" => items.sort_by(|a, b| a.provider.as_str().cmp(b.provider.as_str())),
            "version" => items.sort_by(|a, b| {
                match (Version::parse(&a.version), Version::parse(&b.version)) {
                    (Ok(left), Ok(right)) => left.cmp(&right),
                    _ => a.version.cmp(&b.version),
                }
            }),
            "tag" => items.sort_by(|a, b| a.tag.cmp(&b.tag)),
            _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
        }
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<VersionDraft> = state
        .drafts
        .get(&id.into_inner())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Version draft not found"));
    json::respond(result, debug)
}

async fn delete_draft(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let id = id.into_inner();
        state
            .drafts
            .remove(&id)
            .map(|_| json!({ "deleted": true, "id": id }))
            .ok_or_else(|| UnifiedError::not_found("Version draft not found"))
    })();
    json::respond(result, debug)
}

/// Download one release asset through the allowlist, verify its
/// SHA-256, and store the reference (redacted URL, size, checksum) on
/// the draft. The bytes themselves are discarded after hashing; the
/// draft keeps only the reference. Redirect targets are re-validated
/// against the same allowlist.
async fn download_draft_asset(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    id: web::Path<Uuid>,
    body: web::Json<DownloadAssetRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<DraftAsset> = (|| async {
        let draft_id = id.into_inner();
        let draft = state
            .drafts
            .get(&draft_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Version draft not found"))?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 256 || name.contains('/') || name.contains('\\') {
            return Err(UnifiedError::bad_request(
                "Asset name is required, must be a plain file name, and must be at most 256 characters",
            ));
        }
        let gitlab_hosts = state.gitlab_hosts(draft.provider);
        let mut url = validate_asset_url(&input.url, &gitlab_hosts)?;
        let settings = state.provider_settings(draft.provider);
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot build HTTP client: {error}"))
            })?;
        let mut hasher = Sha256::new();
        let mut downloaded: u64 = 0;
        let mut redirects: u32 = 0;
        loop {
            let host = url
                .host_str()
                .unwrap_or("")
                .to_ascii_lowercase();
            let is_api_host = matches!(
                host.as_str(),
                "api.github.com" | "api.bitbucket.org" | "gitlab.com"
            ) || gitlab_hosts
                .iter()
                .any(|allowed| allowed.to_ascii_lowercase() == host);
            let mut request = client.get(url.clone());
            if is_api_host {
                if let Some(token) = settings.token.as_deref().filter(|token| !token.is_empty()) {
                    request = request.header(
                        draft.provider.auth_header_name(),
                        draft.provider.auth_header_value(token),
                    );
                }
            }
            let response = request
                .send()
                .await
                .map_err(|error| {
                    UnifiedError::internal(format!("Asset download failed: {error}"))
                })?;
            let status = response.status();
            if status.is_redirection() {
                if redirects >= MAX_ASSET_REDIRECTS {
                    return Err(UnifiedError::bad_request(
                        "Asset download followed too many redirects",
                    ));
                }
                let location = response
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| {
                        UnifiedError::bad_request("Asset download redirect has no location")
                    })?
                    .to_string();
                let next = url.join(&location).map_err(|_| {
                    UnifiedError::bad_request("Asset download redirect URL is invalid")
                })?;
                // Re-validate every redirect target against the
                // allowlist before following it.
                url = validate_asset_url(next.as_str(), &gitlab_hosts)?;
                redirects += 1;
                continue;
            }
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                let retry_after = response
                    .headers()
                    .get(header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|value| value.trim().parse::<u64>().ok());
                return Err(UnifiedError::new(
                    "Asset host rate limit reached; back off before retrying",
                    429,
                )
                .with_metadata(
                    "retry_after_seconds",
                    retry_after.map(Value::from).unwrap_or(Value::Null),
                ));
            }
            if !status.is_success() {
                return Err(UnifiedError::internal(format!(
                    "Asset download failed with HTTP {status}"
                )));
            }
            if let Some(total) = response.content_length() {
                if total > MAX_ASSET_BYTES {
                    return Err(UnifiedError::unprocessable(format!(
                        "Asset size {total} bytes exceeds the maximum of {MAX_ASSET_BYTES} bytes"
                    )));
                }
            }
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|error| {
                    UnifiedError::internal(format!("Asset download failed: {error}"))
                })?;
                downloaded += chunk.len() as u64;
                if downloaded > MAX_ASSET_BYTES {
                    return Err(UnifiedError::unprocessable(format!(
                        "Asset exceeds the maximum of {MAX_ASSET_BYTES} bytes"
                    )));
                }
                hasher.update(&chunk);
            }
            break;
        }
        let sha256 = hex_encode(&hasher.finalize());
        let asset = DraftAsset {
            name,
            url: redact_url(url.as_str()).unwrap_or_else(|| {
                // The URL already passed validation, so redaction only
                // fails in theory; fall back to scheme + host + path so
                // a signed query string can never be stored.
                format!(
                    "{}://{}{}",
                    url.scheme(),
                    url.host_str().unwrap_or(""),
                    url.path()
                )
            }),
            size_bytes: Some(downloaded),
            sha256: Some(sha256),
            downloaded_at: Some(now()),
        };
        if let Some(mut entry) = state.drafts.get_mut(&draft_id) {
            entry.assets.push(asset.clone());
            entry.updated_at = now();
        }
        Ok(asset)
    })()
    .await;
    match result {
        Ok(asset) => json::success_response(asset, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Webhook receivers.
// ---------------------------------------------------------------------------

/// Verify a webhook signature for the provider. Returns 401 when no
/// secret is configured or the payload cannot be verified. The secret
/// itself never appears in any message.
fn verify_webhook_signature(
    provider: Provider,
    req: &HttpRequest,
    body: &[u8],
    secret: &str,
) -> UnifiedResult<()> {
    let header_name = provider.webhook_signature_header();
    let header_value = req
        .headers()
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if header_value.is_empty() {
        return Err(UnifiedError::unauthorized(format!(
            "Webhook signature header \"{header_name}\" is missing"
        )));
    }
    match provider {
        Provider::GitHub => {
            let provided = header_value
                .strip_prefix("sha256=")
                .ok_or_else(|| {
                    UnifiedError::unauthorized(
                        "Webhook signature must use the sha256 scheme",
                    )
                })?
                .to_ascii_lowercase();
            let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
                .map_err(|_| UnifiedError::internal("Cannot initialise webhook verification"))?;
            mac.update(body);
            let expected = hex_encode(&mac.finalize().into_bytes());
            if !secrets_equal(&expected, &provided) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
        // GitLab and Bitbucket compare a shared secret header in
        // constant time.
        Provider::GitLab | Provider::Bitbucket => {
            if !secrets_equal(secret, header_value) {
                return Err(UnifiedError::unauthorized(
                    "Webhook signature verification failed",
                ));
            }
            Ok(())
        }
    }
}

/// Split a `owner/repo` full name. For GitLab the owner part may
/// contain subgroup slashes, so the split is at the last `/`.
fn split_full_name(full_name: &str) -> Option<(String, String)> {
    let (owner, repo) = full_name.rsplit_once('/')?;
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner.to_string(), repo.to_string()))
}

fn webhook_event_name(provider: Provider, req: &HttpRequest) -> String {
    let header_name = match provider {
        Provider::GitHub => "x-github-event",
        Provider::GitLab => "x-gitlab-event",
        Provider::Bitbucket => "x-event-key",
    };
    req.headers()
        .get(header_name)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string()
}

/// Extract a release from a verified webhook payload. Returns `None`
/// for events that carry no release or tag (branch pushes, pings,
/// edits), so those are acknowledged without creating a draft.
fn release_from_webhook(provider: Provider, event: &str, payload: &Value) -> Option<ProviderRelease> {
    match provider {
        Provider::GitHub => match event {
            "push" => {
                let reference = payload.get("ref").and_then(|value| value.as_str())?;
                let tag = reference.strip_prefix("refs/tags/")?;
                let notes = payload
                    .get("head_commit")
                    .and_then(|commit| commit.get("message"))
                    .and_then(|message| message.as_str())
                    .unwrap_or("")
                    .chars()
                    .take(MAX_NOTES_LEN)
                    .collect::<String>();
                Some(ProviderRelease {
                    version: version_from_tag(tag).ok().map(|version| version.to_string()),
                    tag: tag.to_string(),
                    name: None,
                    notes,
                    published_at: None,
                    prerelease: false,
                    assets: Vec::new(),
                })
            }
            "release" => {
                let action = payload.get("action").and_then(|value| value.as_str())?;
                if !matches!(action, "published" | "released") {
                    return None;
                }
                let release = payload.get("release")?;
                let tag = string_field(release, "tag_name")?;
                let assets = release
                    .get("assets")
                    .and_then(|assets| assets.as_array())
                    .map(|assets| {
                        assets
                            .iter()
                            .filter_map(|asset| {
                                Some(ProviderAsset {
                                    name: string_field(asset, "name")?,
                                    url: string_field(asset, "browser_download_url")
                                        .or_else(|| string_field(asset, "url"))?,
                                    size: asset.get("size").and_then(|size| size.as_u64()),
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(release, "name"),
                    notes: string_field(release, "body").unwrap_or_default(),
                    published_at: parse_datetime(
                        release.get("published_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: release
                        .get("prerelease")
                        .and_then(|field| field.as_bool())
                        .unwrap_or(false),
                    assets,
                })
            }
            _ => None,
        },
        Provider::GitLab => match event {
            "Push Hook" | "Tag Push Hook" => {
                let reference = payload.get("ref").and_then(|value| value.as_str())?;
                let tag = reference.strip_prefix("refs/tags/")?;
                Some(ProviderRelease {
                    version: version_from_tag(tag).ok().map(|version| version.to_string()),
                    tag: tag.to_string(),
                    name: None,
                    notes: String::new(),
                    published_at: None,
                    prerelease: false,
                    assets: Vec::new(),
                })
            }
            "Release Hook" => {
                if payload
                    .get("object_kind")
                    .and_then(|value| value.as_str())
                    != Some("release")
                {
                    return None;
                }
                let tag = string_field(payload, "tag")?;
                let assets = payload
                    .get("assets")
                    .and_then(|assets| assets.get("links"))
                    .and_then(|links| links.as_array())
                    .map(|links| {
                        links
                            .iter()
                            .filter_map(|link| {
                                Some(ProviderAsset {
                                    name: string_field(link, "name")?,
                                    url: string_field(link, "url")?,
                                    size: None,
                                })
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                Some(ProviderRelease {
                    version: version_from_tag(&tag).ok().map(|version| version.to_string()),
                    tag,
                    name: string_field(payload, "name"),
                    notes: string_field(payload, "description").unwrap_or_default(),
                    published_at: parse_datetime(
                        payload.get("released_at").and_then(|field| field.as_str()),
                    ),
                    prerelease: false,
                    assets,
                })
            }
            _ => None,
        },
        Provider::Bitbucket => {
            if event != "repo:push" {
                return None;
            }
            let changes = payload
                .get("push")
                .and_then(|push| push.get("changes"))
                .and_then(|changes| changes.as_array())?;
            let tag_change = changes.iter().find(|change| {
                change
                    .get("new")
                    .and_then(|new| new.get("type"))
                    .and_then(|kind| kind.as_str())
                    == Some("tag")
            })?;
            let tag = tag_change
                .get("new")
                .and_then(|new| new.get("name"))
                .and_then(|name| name.as_str())?;
            Some(ProviderRelease {
                version: version_from_tag(tag).ok().map(|version| version.to_string()),
                tag: tag.to_string(),
                name: None,
                notes: String::new(),
                published_at: None,
                prerelease: false,
                assets: Vec::new(),
            })
        }
    }
}

async fn handle_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    provider: Provider,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let settings = state.provider_settings(provider);
        let secret = settings
            .webhook_secret
            .clone()
            .filter(|secret| !secret.is_empty())
            .ok_or_else(|| {
                UnifiedError::unauthorized(format!(
                    "No webhook secret is configured for {}",
                    provider.display_name()
                ))
            })?;
        verify_webhook_signature(provider, &req, &body, &secret)?;
        let payload: Value = serde_json::from_slice(&body).map_err(|_| {
            UnifiedError::bad_request("Webhook payload is not valid JSON")
        })?;
        // Resolve the repository the event belongs to.
        let full_name = match provider {
            Provider::GitHub | Provider::Bitbucket => payload
                .get("repository")
                .and_then(|repository| repository.get("full_name"))
                .and_then(|name| name.as_str())
                .map(str::to_string),
            Provider::GitLab => payload
                .get("project")
                .and_then(|project| project.get("path_with_namespace"))
                .and_then(|name| name.as_str())
                .map(str::to_string),
        };
        let Some(full_name) = full_name else {
            return Err(UnifiedError::bad_request(
                "Webhook payload does not identify a repository",
            ));
        };
        let Some((owner, repo)) = split_full_name(&full_name) else {
            return Err(UnifiedError::bad_request(
                "Webhook payload repository name is invalid",
            ));
        };
        let Some(connection) = find_connection_by_repo(&state, provider, &owner, &repo) else {
            return Ok(json!({
                "accepted": true,
                "matched": false,
                "draft_created": false,
            }));
        };
        let event = webhook_event_name(provider, &req);
        let Some(release) = release_from_webhook(provider, &event, &payload) else {
            return Ok(json!({
                "accepted": true,
                "matched": true,
                "draft_created": false,
                "reason": "event does not carry a release or tag",
            }));
        };
        match create_draft(&ctx.hooks, &state, &connection, &release, "webhook", None, None) {
            Ok(draft) => {
                ctx.hooks.emit(
                    "notification.requested",
                    json!({
                        "kind": "vcs.release.imported",
                        "title": format!(
                            "New {} release {} for {}/{}",
                            provider.display_name(),
                            draft.tag,
                            connection.owner,
                            connection.repo
                        ),
                        "body": format!(
                            "Version {} was imported as a draft for review. It was not published.",
                            draft.version
                        ),
                        "user_id": "",
                    }),
                );
                Ok(json!({
                    "accepted": true,
                    "matched": true,
                    "draft_created": true,
                    "draft_id": draft.id,
                }))
            }
            // A redelivered webhook for a tag that already has a draft
            // is acknowledged, not an error.
            Err(error) if error.code == 409 => Ok(json!({
                "accepted": true,
                "matched": true,
                "draft_created": false,
                "reason": "a draft already exists for this tag",
            })),
            Err(error) => Err(error),
        }
    })();
    json::respond(result, debug)
}

async fn github_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    handle_webhook(ctx, state, Provider::GitHub, req, body).await
}

async fn gitlab_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    handle_webhook(ctx, state, Provider::GitLab, req, body).await
}

async fn bitbucket_webhook(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<VcsState>>,
    req: HttpRequest,
    body: web::Bytes,
) -> HttpResponse {
    handle_webhook(ctx, state, Provider::Bitbucket, req, body).await
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct VcsModule {
    state: Arc<VcsState>,
}

impl VcsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(VcsState::new()),
        }
    }
}

impl Default for VcsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for VcsModule {
    fn id(&self) -> &'static str {
        "vcs"
    }

    fn name(&self) -> &'static str {
        "Third-Party VCS Integrations"
    }

    fn description(&self) -> &'static str {
        "GitHub, GitLab, and Bitbucket integrations: repository connections, release import as review drafts, signed webhooks, allowlisted release-asset downloads, and sync with rate-limit handling."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["software", "settings", "notifications"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Validate `vcs.*` settings values through the config.validate
        // hook: tokens and secrets must be strings, GitLab hosts must
        // be valid hostnames, and API base overrides must be https URLs.
        let handler: HookHandler = Arc::new(|ctx: &mut HookContext| {
            let namespace = ctx
                .get("namespace")
                .and_then(|value| value.as_str())
                .unwrap_or("");
            if namespace != "vcs" {
                return;
            }
            let key = ctx
                .get("key")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            let value = ctx.get("value").cloned().unwrap_or(Value::Null);
            let problem: Option<String> = if key.ends_with(".token")
                || key.ends_with(".webhook_secret")
            {
                if value.is_string() {
                    None
                } else {
                    Some(format!("Setting \"{key}\" must be a string"))
                }
            } else if key == "gitlab.hosts" {
                match value.as_array() {
                    Some(hosts)
                        if hosts.iter().all(|host| {
                            host.as_str().is_some_and(|host| valid_host(host))
                        }) =>
                    {
                        None
                    }
                    _ => Some("Setting \"gitlab.hosts\" must be an array of valid hostnames".to_string()),
                }
            } else if key.ends_with(".api_base") {
                match value.as_str() {
                    Some(base)
                        if url::Url::parse(base)
                            .is_ok_and(|url| url.scheme() == "https") =>
                    {
                        None
                    }
                    _ => Some(format!("Setting \"{key}\" must be a valid https URL")),
                }
            } else {
                None
            };
            if let Some(message) = problem {
                ctx.set("valid", json!(false));
                ctx.set("message", json!(message));
            }
        });
        bus.register(events::CONFIG_VALIDATE, "vcs", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/vcs")
                .route("/providers", web::get().to(list_providers))
                .route("/providers/{provider}", web::get().to(get_provider))
                .route(
                    "/providers/{provider}/settings",
                    web::get().to(get_provider_settings),
                )
                .route(
                    "/providers/{provider}/settings",
                    web::put().to(update_provider_settings),
                )
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::put().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route(
                    "/connections/{id}/enable",
                    web::post().to(enable_connection),
                )
                .route(
                    "/connections/{id}/disable",
                    web::post().to(disable_connection),
                )
                .route(
                    "/connections/{id}/releases",
                    web::get().to(list_connection_releases),
                )
                .route(
                    "/connections/{id}/releases/import",
                    web::post().to(import_release),
                )
                .route("/connections/{id}/sync", web::post().to(sync_connection))
                .route(
                    "/connections/{id}/sync-history",
                    web::get().to(list_connection_sync_history),
                )
                .route("/drafts", web::get().to(list_drafts))
                .route("/drafts/{id}", web::get().to(get_draft))
                .route("/drafts/{id}", web::delete().to(delete_draft))
                .route(
                    "/drafts/{id}/assets",
                    web::post().to(download_draft_asset),
                )
                .route("/sync-history", web::get().to(list_sync_history))
                .route("/webhooks/github", web::post().to(github_webhook))
                .route("/webhooks/gitlab", web::post().to(gitlab_webhook))
                .route("/webhooks/bitbucket", web::post().to(bitbucket_webhook)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/vcs/providers", owner: "vcs", description: "List VCS providers with configuration flags." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/providers/{provider}", owner: "vcs", description: "Get one VCS provider." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/providers/{provider}/settings", owner: "vcs", description: "Get provider settings flags (tokens are never returned)." },
            RouteInfo { method: "PUT", path: "/api/v1/vcs/providers/{provider}/settings", owner: "vcs", description: "Update provider settings (token and webhook secret are write-only)." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections", owner: "vcs", description: "List repository connections (paginated, filter by provider, software_id, enabled, sync_status)." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections", owner: "vcs", description: "Connect a repository to a software record." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections/{id}", owner: "vcs", description: "Get one repository connection." },
            RouteInfo { method: "PUT", path: "/api/v1/vcs/connections/{id}", owner: "vcs", description: "Update a repository connection." },
            RouteInfo { method: "DELETE", path: "/api/v1/vcs/connections/{id}", owner: "vcs", description: "Delete a repository connection." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/enable", owner: "vcs", description: "Enable a repository connection." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/disable", owner: "vcs", description: "Disable a repository connection." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections/{id}/releases", owner: "vcs", description: "List provider releases/tags for a connection (paginated, rate-limit state in metadata)." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/releases/import", owner: "vcs", description: "Import a provider release/tag as a review draft (never published)." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/connections/{id}/sync", owner: "vcs", description: "Sync a connection now: fetch releases, create drafts for new tags." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/connections/{id}/sync-history", owner: "vcs", description: "List sync history for a connection (paginated)." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/drafts", owner: "vcs", description: "List imported version drafts (paginated, filter by provider, connection_id, software_id, status)." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/drafts/{id}", owner: "vcs", description: "Get one version draft." },
            RouteInfo { method: "DELETE", path: "/api/v1/vcs/drafts/{id}", owner: "vcs", description: "Discard a version draft." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/drafts/{id}/assets", owner: "vcs", description: "Download a release asset through the allowlist and store its SHA-256 reference on the draft." },
            RouteInfo { method: "GET", path: "/api/v1/vcs/sync-history", owner: "vcs", description: "List all sync history (paginated, filter by provider, connection_id, status)." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/webhooks/github", owner: "vcs", description: "GitHub webhook receiver (HMAC-SHA256 verified)." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/webhooks/gitlab", owner: "vcs", description: "GitLab webhook receiver (shared token verified)." },
            RouteInfo { method: "POST", path: "/api/v1/vcs/webhooks/bitbucket", owner: "vcs", description: "Bitbucket webhook receiver (configured shared secret verified)." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(VcsModule::new()));
}
