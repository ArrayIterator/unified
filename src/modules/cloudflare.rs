// Cloudflare connector module.
//
// API-token authenticated access to the Cloudflare v4 API
// (https://api.cloudflare.com only — the module never calls any other
// host): account and zone listing, DNS record CRUD with strict
// validation, cache purge / development mode / cache level, and
// rate-limit handling that honours `Retry-After` on HTTP 429.
//
// The API token lives in this module's settings (persisted through the
// `settings.merge` hook under the `cloudflare` namespace, the same
// pattern the Auth Manager uses). It is never logged and never
// returned by any endpoint; status responses only say whether a token
// is configured.
//
// Interconnection happens only through the hook bus:
//   - `cloudflare.connected` after a successful connection test.
//   - `cloudflare.dns.changed` after a DNS record is created,
//     updated, or deleted (zone id, record id, type, name).
//   - `cloudflare.cache.purged` after a cache purge.
// The Audit module subscribes to the DNS and purge events, so every
// change and purge is recorded in the audit log.

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// The only API host this module ever calls.
const CF_API_BASE: &str = "https://api.cloudflare.com/client/v4";
const CF_HTTP_TIMEOUT_SECS: u64 = 30;
const CF_USER_AGENT: &str = concat!("unified/", env!("CARGO_PKG_VERSION"));
/// Longest `Retry-After` delay honoured from Cloudflare.
const CF_MAX_RETRY_AFTER_SECS: u64 = 300;
/// Default backoff when a 429 arrives without a `Retry-After` header.
const CF_DEFAULT_RETRY_AFTER_SECS: u64 = 60;
/// Maximum purge-history entries kept in memory.
const CF_MAX_PURGE_HISTORY: usize = 200;
/// Maximum URLs or prefixes in one purge request.
const CF_MAX_PURGE_TARGETS: usize = 30;

/// DNS record types this module manages.
const CF_RECORD_TYPES: &[&str] = &["A", "AAAA", "CNAME", "TXT", "MX", "SRV", "CAA"];
/// Cache levels accepted by the Cloudflare zone settings API.
const CF_CACHE_LEVELS: &[&str] = &["aggressive", "basic", "simplified"];

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

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
    fn api_token(&self) -> Option<String> {
        self.settings
            .get("api_token")
            .and_then(|entry| entry.value().as_str().map(str::to_string))
            .filter(|token| !token.trim().is_empty())
    }

    fn account_id(&self) -> Option<String> {
        self.settings
            .get("account_id")
            .and_then(|entry| entry.value().as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty())
    }

    fn default_zone_id(&self) -> Option<String> {
        self.settings
            .get("default_zone_id")
            .and_then(|entry| entry.value().as_str().map(str::to_string))
            .filter(|value| !value.trim().is_empty())
    }

    /// True while a 429 backoff window is still active.
    fn is_rate_limited(&self) -> Option<u64> {
        let state = self.rate_limit.read().expect("rate limit lock poisoned");
        match state.limited_until {
            Some(until) if until > Utc::now() => Some(
                (until - Utc::now()).num_seconds().max(1) as u64,
            ),
            _ => None,
        }
    }

    fn record_rate_limit_headers(&self, response: &reqwest::Response) {
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

    fn record_429(&self, retry_after_secs: u64) {
        let retry_after_secs = retry_after_secs.min(CF_MAX_RETRY_AFTER_SECS).max(1);
        let mut state = self.rate_limit.write().expect("rate limit lock poisoned");
        state.limited = true;
        state.limited_until =
            Some(Utc::now() + chrono::Duration::seconds(retry_after_secs as i64));
        state.retry_after_seconds = Some(retry_after_secs);
        state.last_429_at = Some(Utc::now());
    }

    fn clear_rate_limit(&self) {
        let mut state = self.rate_limit.write().expect("rate limit lock poisoned");
        state.limited = false;
        state.limited_until = None;
        state.retry_after_seconds = None;
    }

    fn rate_limit_view(&self) -> CfRateLimitState {
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

    fn record_purge(&self, zone_id: &str, kind: &str, targets: Vec<String>) -> CfPurgeRecord {
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

// ---------------------------------------------------------------------------
// Cloudflare API client
// ---------------------------------------------------------------------------

/// Validate a Cloudflare zone / record / account id before it is
/// interpolated into a request path. Ids are opaque ASCII tokens, so
/// anything else is rejected rather than sent.
fn validate_cf_id(value: &str, what: &str) -> UnifiedResult<()> {
    if value.is_empty()
        || value.len() > 64
        || !value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(UnifiedError::bad_request(format!("Invalid {what} id")));
    }
    Ok(())
}

fn parse_retry_after(response: &reqwest::Response) -> u64 {
    response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(CF_DEFAULT_RETRY_AFTER_SECS)
        .min(CF_MAX_RETRY_AFTER_SECS)
}

/// Call the Cloudflare v4 API and return the full response envelope.
/// Callers read `result` (and `result_info` for lists) from it.
/// Handles the 429 backoff window and maps Cloudflare error objects
/// onto `UnifiedError` without leaking the API token.
async fn cf_call(
    state: &CloudflareState,
    method: Method,
    path: &str,
    body: Option<Value>,
    query: &[(String, String)],
) -> UnifiedResult<Value> {
    if let Some(wait) = state.is_rate_limited() {
        return Err(UnifiedError::new(
            format!("Cloudflare API rate limit is active; retry in {wait} seconds"),
            429,
        )
        .with_metadata("retry_after_seconds", json!(wait)));
    }
    let token = state.api_token().ok_or_else(|| {
        UnifiedError::unprocessable("Cloudflare API token is not configured")
    })?;
    if !path.starts_with('/') || path.contains("..") {
        return Err(UnifiedError::bad_request("Invalid Cloudflare API path"));
    }
    let mut url = url::Url::parse(&format!("{CF_API_BASE}{path}"))
        .map_err(|_| UnifiedError::bad_request("Invalid Cloudflare API path"))?;
    // The host is a constant, but verify it anyway: this module must
    // never send the token anywhere else.
    if url.host_str() != Some("api.cloudflare.com") {
        return Err(UnifiedError::internal(
            "Cloudflare API host allow-list violation",
        ));
    }
    if !query.is_empty() {
        let mut pairs = url.query_pairs_mut();
        for (key, value) in query {
            pairs.append_pair(key, value);
        }
    }
    let mut request = state
        .client
        .request(method, url)
        .bearer_auth(&token)
        .user_agent(CF_USER_AGENT)
        .timeout(Duration::from_secs(CF_HTTP_TIMEOUT_SECS));
    if let Some(body) = body {
        request = request.json(&body);
    }
    let response = request.send().await.map_err(|error| {
        UnifiedError::internal(format!("Cloudflare API request failed: {error}"))
    })?;
    let status = response.status();
    state.record_rate_limit_headers(&response);
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = parse_retry_after(&response);
        state.record_429(retry_after);
        return Err(UnifiedError::new(
            format!("Cloudflare API rate limit reached; retry in {retry_after} seconds"),
            429,
        )
        .with_metadata("retry_after_seconds", json!(retry_after)));
    }
    let body: Value = response.json().await.map_err(|error| {
        UnifiedError::internal(format!("Cannot parse Cloudflare API response: {error}"))
    })?;
    if !status.is_success() || body.get("success").and_then(Value::as_bool) != Some(true) {
        let first_error = body
            .get("errors")
            .and_then(Value::as_array)
            .and_then(|errors| errors.first())
            .cloned()
            .unwrap_or(Value::Null);
        let message = first_error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Cloudflare API request failed");
        let mut error = UnifiedError::new(
            format!("Cloudflare API error: {message}"),
            status.as_u16(),
        );
        if let Some(code) = first_error.get("code").and_then(Value::as_i64) {
            error = error.with_metadata("cloudflare_error_code", json!(code));
        }
        return Err(error);
    }
    state.clear_rate_limit();
    Ok(body)
}

/// The `result` value of a Cloudflare response envelope.
fn cf_result(body: &Value) -> Value {
    body.get("result").cloned().unwrap_or(Value::Null)
}

/// Build a page of results from a Cloudflare list response: the
/// `result` array plus `result_info.total_count` for pagination.
fn cf_page<T: Serialize>(
    envelope: &Value,
    items: Vec<T>,
    params: &PaginationParams,
) -> Value {
    let total = envelope
        .get("result_info")
        .and_then(|info| info.get("total_count"))
        .and_then(Value::as_u64)
        .unwrap_or(items.len() as u64);
    PageResult::new(items, total, &params.normalized()).to_json()
}

fn cf_query(params: &PaginationParams) -> Vec<(String, String)> {
    let pagination = params.normalized();
    vec![
        ("page".to_string(), pagination.page.to_string()),
        ("per_page".to_string(), pagination.per_page.to_string()),
    ]
}

// ---------------------------------------------------------------------------
// DNS validation
// ---------------------------------------------------------------------------

/// Validate one DNS label: 1-63 characters, letters, digits, hyphens,
/// and a leading underscore for service labels (`_sip`, `_dmarc`).
/// Labels must not start or end with a hyphen.
fn valid_dns_label(label: &str) -> bool {
    if label.is_empty() || label.len() > 63 {
        return false;
    }
    let body = label.strip_prefix('_').unwrap_or(label);
    if body.is_empty() {
        return false;
    }
    if body.starts_with('-') || body.ends_with('-') {
        return false;
    }
    body.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Validate a DNS name: `@` (zone apex), `*` (wildcard), or a
/// dotted list of labels, at most 253 characters in total.
fn valid_dns_name(name: &str) -> bool {
    let name = name.trim().trim_end_matches('.');
    if name == "@" || name == "*" {
        return true;
    }
    if name.is_empty() || name.len() > 253 {
        return false;
    }
    name.split('.').all(valid_dns_label)
}

/// Validate the content of a DNS record for its type, and return a
/// normalized content string (trimmed). `priority` is the record's
/// priority field, used by MX and SRV.
fn validate_record_content(
    record_type: &str,
    content: &str,
    priority: Option<u16>,
) -> UnifiedResult<String> {
    let content = content.trim();
    if content.is_empty() {
        return Err(UnifiedError::bad_request("content must not be empty"));
    }
    match record_type {
        "A" => {
            let address: Ipv4Addr = content.parse().map_err(|_| {
                UnifiedError::bad_request(format!("\"{content}\" is not a valid IPv4 address"))
            })?;
            if address.is_unspecified() || address.is_multicast() {
                return Err(UnifiedError::bad_request(
                    "A record content must be a usable unicast IPv4 address",
                ));
            }
        }
        "AAAA" => {
            let address: Ipv6Addr = content.parse().map_err(|_| {
                UnifiedError::bad_request(format!("\"{content}\" is not a valid IPv6 address"))
            })?;
            let ip = IpAddr::V6(address);
            if ip.is_unspecified() || ip.is_multicast() {
                return Err(UnifiedError::bad_request(
                    "AAAA record content must be a usable unicast IPv6 address",
                ));
            }
        }
        "CNAME" | "MX" => {
            if !valid_dns_name(content) {
                return Err(UnifiedError::bad_request(format!(
                    "\"{content}\" is not a valid hostname"
                )));
            }
            if content.parse::<IpAddr>().is_ok() {
                return Err(UnifiedError::bad_request(format!(
                    "{record_type} record content must be a hostname, not an IP address"
                )));
            }
            if record_type == "MX" && priority.is_none() {
                return Err(UnifiedError::bad_request(
                    "MX records require a priority (0-65535)",
                ));
            }
        }
        "TXT" => {
            if content.chars().count() > 65_535 {
                return Err(UnifiedError::bad_request(
                    "TXT record content must be at most 65535 characters",
                ));
            }
        }
        "SRV" => {
            // Content form: `priority weight port target`.
            let parts: Vec<&str> = content.split_whitespace().collect();
            if parts.len() != 4 {
                return Err(UnifiedError::bad_request(
                    "SRV record content must be \"priority weight port target\"",
                ));
            }
            for (index, part) in parts.iter().take(3).enumerate() {
                if part.parse::<u16>().is_err() {
                    let field = ["priority", "weight", "port"][index];
                    return Err(UnifiedError::bad_request(format!(
                        "SRV record {field} \"{part}\" is not a number between 0 and 65535"
                    )));
                }
            }
            if !valid_dns_name(parts[3]) {
                return Err(UnifiedError::bad_request(format!(
                    "SRV record target \"{}\" is not a valid hostname",
                    parts[3]
                )));
            }
        }
        "CAA" => {
            // Content form: `flags tag "value"`.
            let mut parts = content.splitn(3, ' ');
            let flags = parts.next().unwrap_or("");
            let tag = parts.next().unwrap_or("");
            let value = parts.next().unwrap_or("");
            let flags_ok = flags.parse::<u16>().is_ok_and(|flags| flags <= 255);
            let tag_ok = matches!(tag, "issue" | "issuewild" | "issuemail" | "iodef");
            let value_ok = value.len() >= 2
                && value.starts_with('"')
                && value.ends_with('"');
            if !flags_ok || !tag_ok || !value_ok {
                return Err(UnifiedError::bad_request(
                    "CAA record content must be `flags tag \"value\"` with flags 0-255 and tag issue, issuewild, issuemail, or iodef",
                ));
            }
        }
        _ => {
            return Err(UnifiedError::bad_request(format!(
                "Unsupported record type \"{record_type}\""
            )));
        }
    }
    Ok(content.to_string())
}

/// Validate a TTL: 1 means automatic, otherwise 60-86400 seconds.
fn validate_ttl(ttl: u32) -> UnifiedResult<u32> {
    if ttl == 1 || (60..=86_400).contains(&ttl) {
        Ok(ttl)
    } else {
        Err(UnifiedError::bad_request(
            "TTL must be 1 (automatic) or between 60 and 86400 seconds",
        ))
    }
}

// ---------------------------------------------------------------------------
// Request types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct UpdateSettingsInput {
    #[serde(default)]
    api_token: Option<String>,
    #[serde(default)]
    account_id: Option<String>,
    #[serde(default)]
    default_zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DnsRecordInput {
    #[serde(rename = "type")]
    record_type: String,
    name: String,
    content: String,
    #[serde(default = "default_ttl")]
    ttl: u32,
    #[serde(default)]
    proxied: bool,
    #[serde(default)]
    priority: Option<u16>,
    #[serde(default)]
    comment: Option<String>,
}

fn default_ttl() -> u32 {
    1
}

#[derive(Debug, Deserialize)]
struct UpdateDnsRecordInput {
    #[serde(default, rename = "type")]
    record_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    ttl: Option<u32>,
    #[serde(default)]
    proxied: Option<bool>,
    #[serde(default)]
    priority: Option<u16>,
    #[serde(default)]
    comment: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PurgeCacheInput {
    #[serde(default)]
    urls: Option<Vec<String>>,
    #[serde(default)]
    prefixes: Option<Vec<String>>,
    #[serde(default)]
    purge_everything: bool,
    #[serde(default)]
    confirm: bool,
}

#[derive(Debug, Deserialize)]
struct DevelopmentModeInput {
    enabled: bool,
}

#[derive(Debug, Deserialize)]
struct CacheLevelInput {
    value: String,
}

/// Build the JSON body for a DNS create/update call after validation.
/// For updates, `current` carries the record's existing values so
/// omitted fields keep their values.
fn dns_record_body(
    input_type: &str,
    name: &str,
    content: &str,
    ttl: u32,
    proxied: bool,
    priority: Option<u16>,
    comment: &Option<String>,
) -> UnifiedResult<Value> {
    let record_type = input_type.trim().to_uppercase();
    if !CF_RECORD_TYPES.contains(&record_type.as_str()) {
        return Err(UnifiedError::bad_request(format!(
            "Unsupported record type \"{record_type}\" (expected one of {})",
            CF_RECORD_TYPES.join(", ")
        )));
    }
    let name = name.trim();
    if !valid_dns_name(name) {
        return Err(UnifiedError::bad_request(format!(
            "\"{name}\" is not a valid DNS record name"
        )));
    }
    let content = validate_record_content(&record_type, content, priority)?;
    let ttl = validate_ttl(ttl)?;
    if proxied && !matches!(record_type.as_str(), "A" | "AAAA" | "CNAME") {
        return Err(UnifiedError::bad_request(format!(
            "Only A, AAAA, and CNAME records can be proxied; {record_type} cannot"
        )));
    }
    if let Some(comment) = comment {
        if comment.chars().count() > 200 {
            return Err(UnifiedError::bad_request(
                "comment must be at most 200 characters",
            ));
        }
    }
    let mut body = json!({
        "type": record_type,
        "name": name,
        "content": content,
        "ttl": ttl,
        "proxied": proxied,
    });
    if matches!(record_type.as_str(), "MX" | "SRV") {
        if let Some(priority) = priority {
            body["priority"] = json!(priority);
        }
    }
    if let Some(comment) = comment {
        body["comment"] = json!(comment);
    }
    Ok(body)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn mask_token(token: &str) -> String {
    let tail: String = token.chars().rev().take(4).collect::<String>().chars().rev().collect();
    format!("••••{tail}")
}

fn paginate_local<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> Value {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination).to_json()
}

// ---------------------------------------------------------------------------
// Handlers: status, settings, connection test
// ---------------------------------------------------------------------------

async fn get_status(state: web::Data<CloudflareState>) -> HttpResponse {
    let last_test = state
        .last_test
        .read()
        .expect("last test lock poisoned")
        .clone();
    json::success_response(
        json!({
            "configured": state.api_token().is_some(),
            "api_token_configured": state.api_token().is_some(),
            "account_id": state.account_id(),
            "default_zone_id": state.default_zone_id(),
            "connected": last_test.as_ref().is_some_and(|test| test.ok),
            "last_test": last_test,
            "rate_limit": state.rate_limit_view(),
        }),
        200,
    )
}

async fn update_settings(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<UpdateSettingsInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let input = input.into_inner();
        if let Some(token) = &input.api_token {
            let token = token.trim();
            if token.is_empty() || token.len() > 512 {
                return Err(UnifiedError::bad_request(
                    "api_token must be 1-512 characters",
                ));
            }
            state
                .settings
                .insert("api_token".to_string(), json!(token));
        }
        if let Some(account_id) = &input.account_id {
            validate_cf_id(account_id.trim(), "account")?;
            state
                .settings
                .insert("account_id".to_string(), json!(account_id.trim()));
        }
        if let Some(zone_id) = &input.default_zone_id {
            validate_cf_id(zone_id.trim(), "zone")?;
            state
                .settings
                .insert("default_zone_id".to_string(), json!(zone_id.trim()));
        }
        let stored: Map<String, Value> = state
            .settings
            .iter()
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();
        // Hand the settings to the Settings module for persistence
        // under the `cloudflare` namespace, the same pattern the Auth
        // Manager uses for provider settings.
        ctx.hooks.emit(
            "settings.merge",
            json!({ "namespace": "cloudflare", "settings": stored }),
        );
        let preview = state
            .api_token()
            .map(|token| mask_token(&token));
        Ok(json!({
            "configured": state.api_token().is_some(),
            "api_token_configured": state.api_token().is_some(),
            "api_token_preview": preview,
            "account_id": state.account_id(),
            "default_zone_id": state.default_zone_id(),
        }))
    })();
    json::respond(result, debug)
}

async fn test_connection(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let envelope = cf_call(
            &state,
            Method::GET,
            "/user/tokens/verify",
            None,
            &[],
        )
        .await?;
        let result = cf_result(&envelope);
        let token_status = result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        if token_status != "active" {
            return Err(UnifiedError::unauthorized(format!(
                "Cloudflare API token status is \"{token_status}\""
            )));
        }
        let accounts_envelope = cf_call(
            &state,
            Method::GET,
            "/accounts",
            None,
            &[("page".to_string(), "1".to_string()), ("per_page".to_string(), "1".to_string())],
        )
        .await?;
        let accounts_total = accounts_envelope
            .get("result_info")
            .and_then(|info| info.get("total_count"))
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let first_account = cf_result(&accounts_envelope)
            .as_array()
            .and_then(|accounts| accounts.first().cloned())
            .and_then(|account| {
                account
                    .get("id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            });
        let account_id = state.account_id().or(first_account);
        let test = CfTestResult {
            ok: true,
            token_status: Some(token_status.clone()),
            account_id: account_id.clone(),
            checked_at: Utc::now(),
        };
        *state.last_test.write().expect("last test lock poisoned") = Some(test.clone());
        ctx.hooks.emit(
            "cloudflare.connected",
            json!({
                "account_id": account_id,
                "token_status": token_status,
                "accounts_total": accounts_total,
            }),
        );
        Ok(json!({
            "ok": true,
            "token_status": token_status,
            "account_id": account_id,
            "accounts_total": accounts_total,
            "checked_at": test.checked_at,
        }))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: accounts and zones
// ---------------------------------------------------------------------------

async fn list_accounts(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        // The accounts endpoint supports page/per_page only, so a
        // free-text search is applied to the returned page.
        let envelope = cf_call(&state, Method::GET, "/accounts", None, &cf_query(&params)).await?;
        let mut items = cf_result(&envelope)
            .as_array()
            .cloned()
            .unwrap_or_default();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|account| {
                account
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| name.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
        }
        Ok(cf_page(&envelope, items, &params))
    })()
    .await;
    json::respond(result, debug)
}

async fn list_zones(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let mut query = cf_query(&params);
        // Cloudflare matches `name` exactly; `status` and `account.id`
        // are exact filters too.
        if let Some(search) = &params.search {
            query.push(("name".to_string(), search.clone()));
        }
        if let Some(status) = params.filters.get("status") {
            if !matches!(status.as_str(), "active" | "pending" | "initializing" | "moved" | "deleted" | "deactivated") {
                return Err(UnifiedError::bad_request(format!(
                    "Unknown zone status \"{status}\""
                )));
            }
            query.push(("status".to_string(), status.clone()));
        }
        if let Some(account_id) = params.filters.get("account_id") {
            validate_cf_id(account_id, "account")?;
            query.push(("account.id".to_string(), account_id.clone()));
        }
        let sort = params.allowed_sort(&["name", "status"], "name");
        query.push(("order".to_string(), sort));
        query.push((
            "direction".to_string(),
            params.normalized().order.clone(),
        ));
        let envelope = cf_call(&state, Method::GET, "/zones", None, &query).await?;
        let items = cf_result(&envelope)
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(cf_page(&envelope, items, &params))
    })()
    .await;
    json::respond(result, debug)
}

async fn get_zone(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}"),
            None,
            &[],
        )
        .await?;
        Ok(cf_result(&envelope))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: DNS records
// ---------------------------------------------------------------------------

async fn list_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let mut query = cf_query(&params);
        if let Some(record_type) = params.filters.get("type") {
            let record_type = record_type.trim().to_uppercase();
            if !CF_RECORD_TYPES.contains(&record_type.as_str()) {
                return Err(UnifiedError::bad_request(format!(
                    "Unsupported record type \"{record_type}\""
                )));
            }
            query.push(("type".to_string(), record_type));
        }
        if let Some(search) = &params.search {
            query.push(("name".to_string(), search.clone()));
        }
        if let Some(name) = params.filters.get("name") {
            query.push(("name".to_string(), name.clone()));
        }
        if let Some(content) = params.filters.get("content") {
            query.push(("content".to_string(), content.clone()));
        }
        if let Some(proxied) = params.filters.get("proxied") {
            if !matches!(proxied.as_str(), "true" | "false") {
                return Err(UnifiedError::bad_request(
                    "proxied filter must be \"true\" or \"false\"",
                ));
            }
            query.push(("proxied".to_string(), proxied.clone()));
        }
        query.push(("match".to_string(), "all".to_string()));
        let sort = params.allowed_sort(&["type", "name", "content", "ttl", "proxied"], "name");
        query.push(("order".to_string(), sort));
        query.push((
            "direction".to_string(),
            params.normalized().order.clone(),
        ));
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records"),
            None,
            &query,
        )
        .await?;
        let items = cf_result(&envelope)
            .as_array()
            .cloned()
            .unwrap_or_default();
        Ok(cf_page(&envelope, items, &params))
    })()
    .await;
    json::respond(result, debug)
}

async fn create_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
    input: web::Json<DnsRecordInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let zone_id = zone_id.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        let input = input.into_inner();
        let body = dns_record_body(
            &input.record_type,
            &input.name,
            &input.content,
            input.ttl,
            input.proxied,
            input.priority,
            &input.comment,
        )?;
        let record_type = body["type"].as_str().unwrap_or("").to_string();
        let name = body["name"].as_str().unwrap_or("").to_string();
        let envelope = cf_call(
            &state,
            Method::POST,
            &format!("/zones/{zone_id}/dns_records"),
            Some(body),
            &[],
        )
        .await?;
        let created = cf_result(&envelope);
        let record_id = created
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        ctx.hooks.emit(
            "cloudflare.dns.changed",
            json!({
                "action": "created",
                "zone_id": zone_id,
                "record_id": record_id,
                "type": record_type,
                "name": name,
            }),
        );
        Ok(created)
    })()
    .await;
    match result {
        Ok(record) => json::success_response(record, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn get_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (zone_id, record_id) = path.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        validate_cf_id(&record_id, "record")?;
        let envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        Ok(cf_result(&envelope))
    })()
    .await;
    json::respond(result, debug)
}

async fn update_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
    input: web::Json<UpdateDnsRecordInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (zone_id, record_id) = path.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        validate_cf_id(&record_id, "record")?;
        let input = input.into_inner();
        // Read the current record first so omitted fields keep their
        // values, and so a missing record returns 404 before any
        // validation of the update body.
        let current_envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        let current = cf_result(&current_envelope);
        let record_type = input
            .record_type
            .clone()
            .or_else(|| current.get("type").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let name = input
            .name
            .clone()
            .or_else(|| current.get("name").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let content = input
            .content
            .clone()
            .or_else(|| current.get("content").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default();
        let ttl = input.ttl.or_else(|| {
            current
                .get("ttl")
                .and_then(Value::as_u64)
                .map(|ttl| ttl as u32)
        }).unwrap_or(1);
        let proxied = input.proxied.or_else(|| {
            current.get("proxied").and_then(Value::as_bool)
        }).unwrap_or(false);
        let priority = input.priority.or_else(|| {
            current
                .get("priority")
                .and_then(Value::as_u64)
                .map(|priority| priority as u16)
        });
        let comment = input.comment.or_else(|| {
            current
                .get("comment")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let body = dns_record_body(
            &record_type,
            &name,
            &content,
            ttl,
            proxied,
            priority,
            &comment,
        )?;
        let envelope = cf_call(
            &state,
            Method::PUT,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            Some(body),
            &[],
        )
        .await?;
        let updated = cf_result(&envelope);
        ctx.hooks.emit(
            "cloudflare.dns.changed",
            json!({
                "action": "updated",
                "zone_id": zone_id,
                "record_id": record_id,
                "type": record_type,
                "name": name,
            }),
        );
        Ok(updated)
    })()
    .await;
    json::respond(result, debug)
}

async fn delete_dns(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
        let (zone_id, record_id) = path.into_inner();
        validate_cf_id(&zone_id, "zone")?;
        validate_cf_id(&record_id, "record")?;
        // Read the record first so the change event carries its type
        // and name, and so a missing record returns 404.
        let current_envelope = cf_call(
            &state,
            Method::GET,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        let current = cf_result(&current_envelope);
        let record_type = current
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let name = current
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        cf_call(
            &state,
            Method::DELETE,
            &format!("/zones/{zone_id}/dns_records/{record_id}"),
            None,
            &[],
        )
        .await?;
        ctx.hooks.emit(
            "cloudflare.dns.changed",
            json!({
                "action": "deleted",
                "zone_id": zone_id,
                "record_id": record_id,
                "type": record_type,
                "name": name,
            }),
        );
        Ok(json!({ "deleted": true, "id": record_id }))
    })()
    .await;
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Handlers: cache
// ---------------------------------------------------------------------------

async fn purge_cache(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
    input: web::Json<PurgeCacheInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
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

async fn list_purges_for_zone(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn list_purges(
    state: web::Data<CloudflareState>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
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

async fn get_development_mode(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
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

async fn set_development_mode(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
    input: web::Json<DevelopmentModeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
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

async fn get_cache_level(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
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

async fn set_cache_level(
    state: web::Data<CloudflareState>,
    ctx: web::Data<Arc<AppContext>>,
    zone_id: web::Path<String>,
    input: web::Json<CacheLevelInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| async {
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

// ---------------------------------------------------------------------------
// Module definition
// ---------------------------------------------------------------------------

pub struct CloudflareModule {
    state: CloudflareState,
}

impl CloudflareModule {
    pub fn new() -> Self {
        Self {
            state: CloudflareState::default(),
        }
    }
}

impl Default for CloudflareModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CloudflareModule {
    fn id(&self) -> &'static str {
        "cloudflare"
    }

    fn name(&self) -> &'static str {
        "Cloudflare Connector"
    }

    fn description(&self) -> &'static str {
        "Cloudflare API connector: account and zone listing, DNS record CRUD with validation, cache purge, development mode, and cache level, with rate-limit backoff and hook events for DNS changes and purges."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/status", owner: "cloudflare", description: "Get status" },
            RouteInfo { method: "PUT", path: "/api/v1/cloudflare/settings", owner: "cloudflare", description: "Update settings" },
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/test", owner: "cloudflare", description: "Test connection" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/accounts", owner: "cloudflare", description: "List accounts" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones", owner: "cloudflare", description: "List zones" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}", owner: "cloudflare", description: "Get zone" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/dns", owner: "cloudflare", description: "List dns" },
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/zones/{zone_id}/dns", owner: "cloudflare", description: "Create dns" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/dns/{record_id}", owner: "cloudflare", description: "Get dns" },
            RouteInfo { method: "PUT", path: "/api/v1/cloudflare/zones/{zone_id}/dns/{record_id}", owner: "cloudflare", description: "Update dns" },
            RouteInfo { method: "DELETE", path: "/api/v1/cloudflare/zones/{zone_id}/dns/{record_id}", owner: "cloudflare", description: "Delete dns" },
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/zones/{zone_id}/cache/purge", owner: "cloudflare", description: "Purge cache" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/cache/purges", owner: "cloudflare", description: "List purges for zone" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/cache/purges", owner: "cloudflare", description: "List purges" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/cache/development-mode", owner: "cloudflare", description: "Get development mode" },
            RouteInfo { method: "POST", path: "/api/v1/cloudflare/zones/{zone_id}/cache/development-mode", owner: "cloudflare", description: "Set development mode" },
            RouteInfo { method: "GET", path: "/api/v1/cloudflare/zones/{zone_id}/cache/level", owner: "cloudflare", description: "Get cache level" },
            RouteInfo { method: "PUT", path: "/api/v1/cloudflare/zones/{zone_id}/cache/level", owner: "cloudflare", description: "Set cache level" },
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/cloudflare")
                .app_data(state)
                .route("/status", web::get().to(get_status))
                .route("/settings", web::put().to(update_settings))
                .route("/test", web::post().to(test_connection))
                .route("/accounts", web::get().to(list_accounts))
                .route("/zones", web::get().to(list_zones))
                .route("/zones/{zone_id}", web::get().to(get_zone))
                .route("/zones/{zone_id}/dns", web::get().to(list_dns))
                .route("/zones/{zone_id}/dns", web::post().to(create_dns))
                .route("/zones/{zone_id}/dns/{record_id}", web::get().to(get_dns))
                .route("/zones/{zone_id}/dns/{record_id}", web::put().to(update_dns))
                .route(
                    "/zones/{zone_id}/dns/{record_id}",
                    web::delete().to(delete_dns),
                )
                .route("/zones/{zone_id}/cache/purge", web::post().to(purge_cache))
                .route(
                    "/zones/{zone_id}/cache/purges",
                    web::get().to(list_purges_for_zone),
                )
                .route("/cache/purges", web::get().to(list_purges))
                .route(
                    "/zones/{zone_id}/cache/development-mode",
                    web::get().to(get_development_mode),
                )
                .route(
                    "/zones/{zone_id}/cache/development-mode",
                    web::post().to(set_development_mode),
                )
                .route(
                    "/zones/{zone_id}/cache/level",
                    web::get().to(get_cache_level),
                )
                .route(
                    "/zones/{zone_id}/cache/level",
                    web::put().to(set_cache_level),
                ),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(CloudflareModule::new()));
}
