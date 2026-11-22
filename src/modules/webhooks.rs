// Webhooks Manager — outgoing webhook endpoints and deliveries.
//
// Routes under `/api/v1/webhooks`:
//
//   GET    /events              catalog of subscribable event names
//   GET    /endpoints           list endpoints (paginated, filterable)
//   POST   /endpoints           create an endpoint subscription
//   GET    /endpoints/{id}      get one endpoint
//   PUT    /endpoints/{id}      update an endpoint
//   DELETE /endpoints/{id}      delete an endpoint
//   POST   /endpoints/{id}/pause    pause deliveries to an endpoint
//   POST   /endpoints/{id}/resume   resume deliveries to an endpoint
//   POST   /endpoints/{id}/test     send a `webhook.ping` test delivery
//   GET    /deliveries          list deliveries (paginated, filterable)
//   GET    /deliveries/{id}     get one delivery
//   POST   /deliveries/{id}/redeliver  retry a delivery with its stored payload
//
// Delivery model: the module registers a hook handler for every known
// event name. When an event fires, every active, unpaused endpoint
// subscribed to it gets a delivery record, and a background task posts
// the payload. `notification.webhook` (emitted by the notifications
// module when the webhook channel is selected) has its own handler:
// it fans out the same way, and when no endpoint subscribes to it the
// miss is recorded as a `webhook.failed` event instead of being
// silently dropped. Each delivery is signed with HMAC-SHA256 over
// `{unix_timestamp}.{body}`; the signature travels in
// `X-Unified-Signature: sha256=<hex>` with `X-Unified-Timestamp`.
// Transient failures (connection errors, timeouts, HTTP 408 / 429 /
// 5xx) are retried with exponential backoff up to `max_attempts`;
// other 4xx responses fail the delivery immediately. Redirects are
// never followed, so a redirect cannot smuggle a delivery to an
// internal address.
//
// URL safety: only https is accepted, except http to a loopback host
// for local development. URLs with embedded credentials are rejected.
// IP-literal hosts that are private, link-local, or unspecified are
// rejected. Hostnames are resolved again at delivery time and the
// delivery is refused when every resolved address is private, so a
// DNS rebinding between creation and delivery cannot reach internal
// services. The endpoint secret is stored but never serialized: API
// responses only report `secret_configured`.
//
// Hooks emitted: `webhook.delivered`, `webhook.failed`,
// `webhook.endpoint.created` (plus `webhook.endpoint.updated`,
// `webhook.endpoint.deleted`, `webhook.endpoint.paused`).

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry};
use crate::core::pagination::{PageResult, PaginationParams};
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

type HmacSha256 = Hmac<Sha256>;

// ---------------------------------------------------------------------------
// Constants.
// ---------------------------------------------------------------------------

/// Default retry attempts per delivery (1..=10).
const DEFAULT_MAX_ATTEMPTS: u32 = 3;
/// Maximum retry attempts per delivery.
const MAX_MAX_ATTEMPTS: u32 = 10;
/// Default base backoff in seconds; doubled per attempt, capped.
const DEFAULT_BACKOFF_BASE_SECS: u64 = 5;
const MAX_BACKOFF_BASE_SECS: u64 = 300;
/// Backoff ceiling in seconds.
const MAX_BACKOFF_SECS: u64 = 300;
/// Default per-delivery request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const MAX_TIMEOUT_SECS: u64 = 120;
/// User-Agent sent with every delivery.
const USER_AGENT: &str = concat!("unified-webhooks/", env!("CARGO_PKG_VERSION"));

/// Well-known events emitted by this module.
pub mod webhook_events {
    pub const DELIVERED: &str = "webhook.delivered";
    pub const FAILED: &str = "webhook.failed";
    pub const ENDPOINT_CREATED: &str = "webhook.endpoint.created";
    pub const ENDPOINT_UPDATED: &str = "webhook.endpoint.updated";
    pub const ENDPOINT_DELETED: &str = "webhook.endpoint.deleted";
    pub const ENDPOINT_PAUSED: &str = "webhook.endpoint.paused";
    pub const PING: &str = "webhook.ping";
}

/// Every event name an endpoint may subscribe to: the core events plus
/// the module events this application defines.
pub const KNOWN_EVENTS: &[&str] = &[
    events::APP_START,
    events::APP_STOP,
    events::APP_RELOAD,
    events::APP_PAUSE,
    events::APP_RESUME,
    events::APP_MAINTENANCE,
    events::CONFIG_VALIDATE,
    events::CONFIG_RELOAD,
    events::REQUEST_BEFORE,
    events::REQUEST_AFTER,
    events::AUTH_LOGIN,
    events::AUTH_LOGOUT,
    events::AUTH_FAILED,
    events::USER_CREATED,
    events::USER_UPDATED,
    events::ROLE_CREATED,
    events::MODULE_ENABLED,
    events::MODULE_DISABLED,
    events::SSL_RENEW,
    events::SSL_SWAP,
    events::GEO_LOOKUP,
    events::LICENSE_ISSUED,
    events::LICENSE_REVOKED,
    events::DOWNLOAD_STARTED,
    events::DOWNLOAD_FINISHED,
    events::SYNC_CONNECTED,
    "apikey.created",
    "apikey.rotated",
    "apikey.revoked",
    "apikey.used",
    "webhook.delivered",
    "webhook.failed",
    "webhook.endpoint.created",
    "webhook.endpoint.updated",
    "webhook.endpoint.deleted",
    "webhook.endpoint.paused",
    "webhook.ping",
    "import.completed",
    "import.failed",
    "export.completed",
    "export.requested",
    "security.bruteforce.locked",
    "security.bruteforce.unlocked",
    "security.auth.failed",
    "security.ip.flagged",
    "security.ip.unflagged",
    "security.settings.updated",
    "security.rate_limited",
    "module.auto_activated",
    "notification.requested",
    "notification.webhook",
    "license.validate_requested",
];

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();
static ENDPOINTS: OnceLock<DashMap<Uuid, WebhookEndpoint>> = OnceLock::new();
static DELIVERIES: OnceLock<DashMap<Uuid, WebhookDelivery>> = OnceLock::new();

fn endpoints_store() -> &'static DashMap<Uuid, WebhookEndpoint> {
    ENDPOINTS.get_or_init(DashMap::new)
}

fn deliveries_store() -> &'static DashMap<Uuid, WebhookDelivery> {
    DELIVERIES.get_or_init(DashMap::new)
}

fn emit(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}

// ---------------------------------------------------------------------------
// Types.
// ---------------------------------------------------------------------------

/// One subscribed endpoint. The signing secret is never serialized.
#[derive(Debug, Clone)]
pub struct WebhookEndpoint {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    /// HMAC signing secret. Never returned by any API response.
    pub secret: String,
    pub active: bool,
    pub paused: bool,
    pub max_attempts: u32,
    pub backoff_base_secs: u64,
    pub timeout_secs: u64,
    pub delivery_count: u64,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WebhookEndpointView {
    pub id: Uuid,
    pub name: String,
    pub url: String,
    pub events: Vec<String>,
    pub secret_configured: bool,
    pub active: bool,
    pub paused: bool,
    pub max_attempts: u32,
    pub backoff_base_secs: u64,
    pub timeout_secs: u64,
    pub delivery_count: u64,
    pub last_delivery_at: Option<DateTime<Utc>>,
    pub last_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WebhookEndpoint {
    fn view(&self) -> WebhookEndpointView {
        WebhookEndpointView {
            id: self.id,
            name: self.name.clone(),
            url: self.url.clone(),
            events: self.events.clone(),
            secret_configured: !self.secret.is_empty(),
            active: self.active,
            paused: self.paused,
            max_attempts: self.max_attempts,
            backoff_base_secs: self.backoff_base_secs,
            timeout_secs: self.timeout_secs,
            delivery_count: self.delivery_count,
            last_delivery_at: self.last_delivery_at,
            last_status: self.last_status.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Lifecycle of one delivery.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Queued,
    Delivering,
    Delivered,
    Failed,
    Retrying,
}

impl DeliveryStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Delivering => "delivering",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Retrying => "retrying",
        }
    }
}

/// One delivery attempt record. The payload is kept so a failed
/// delivery can be redelivered manually with the exact same body.
#[derive(Debug, Clone, Serialize)]
pub struct WebhookDelivery {
    pub id: Uuid,
    pub endpoint_id: Uuid,
    pub event: String,
    pub payload: Value,
    /// SHA-256 hex digest of the serialized payload body.
    pub payload_hash: String,
    pub status: String,
    pub attempts: u32,
    pub max_attempts: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub status_code: Option<u16>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
}

impl WebhookDelivery {
    fn set_status(&mut self, status: DeliveryStatus) {
        self.status = status.as_str().to_string();
        self.updated_at = Utc::now();
    }
}

// ---------------------------------------------------------------------------
// Signing.
// ---------------------------------------------------------------------------

/// HMAC-SHA256 signature over `{timestamp}.{body}`, hex encoded.
/// Returns the header value `sha256=<hex>`.
pub fn sign_payload(secret: &str, timestamp: i64, body: &[u8]) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(format!("{timestamp}.").as_bytes());
    mac.update(body);
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("sha256={hex}")
}

/// Verify a received signature with a constant-time comparison.
pub fn verify_signature(secret: &str, timestamp: i64, body: &[u8], signature: &str) -> bool {
    let expected = sign_payload(secret, timestamp, body);
    if expected.len() != signature.len() {
        return false;
    }
    expected
        .bytes()
        .zip(signature.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

fn payload_hash(body: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(body);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// ---------------------------------------------------------------------------
// URL safety (SSRF guard).
// ---------------------------------------------------------------------------

fn is_loopback(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => addr.is_loopback(),
        IpAddr::V6(addr) => addr.is_loopback(),
    }
}

/// True when the address is private, link-local, or unspecified: never
/// a valid webhook target. Loopback is handled separately (allowed
/// only over http, for local development).
fn is_forbidden_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(addr) => {
            addr.is_private()
                || addr.is_link_local()
                || addr.is_unspecified()
                || addr.is_broadcast()
                || addr.is_multicast()
                || addr.is_documentation()
                || addr.is_benchmarking()
                || addr.is_reserved()
        }
        IpAddr::V6(addr) => {
            addr.is_unspecified()
                || addr.is_multicast()
                || (addr.segments()[0] & 0xfe00) == 0xfc00 // unique-local fc00::/7
                || (addr.segments()[0] & 0xffc0) == 0xfe80 // link-local fe80::/10
                || addr.is_unique_local()
        }
    }
}

/// Validate a webhook target URL at creation/update time: https only
/// (http only to a loopback host), no embedded credentials, no
/// forbidden IP-literal host.
pub fn validate_webhook_url(raw: &str) -> UnifiedResult<Url> {
    let url = Url::parse(raw.trim())
        .map_err(|_| UnifiedError::bad_request(format!("Invalid webhook URL \"{raw}\"")))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UnifiedError::bad_request(
            "Webhook URL must not contain embedded credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(UnifiedError::bad_request("Webhook URL must have a host"));
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        if is_loopback(ip) {
            if url.scheme() != "http" && url.scheme() != "https" {
                return Err(UnifiedError::bad_request(
                    "Webhook URL must use http or https",
                ));
            }
        } else {
            if url.scheme() != "https" {
                return Err(UnifiedError::bad_request(
                    "Webhook URL must use https (http is only allowed for loopback hosts)",
                ));
            }
            if is_forbidden_ip(ip) {
                return Err(UnifiedError::bad_request(
                    "Webhook URL host is a private, link-local, or reserved IP address",
                ));
            }
        }
    } else {
        if url.scheme() != "https" {
            return Err(UnifiedError::bad_request(
                "Webhook URL must use https (http is only allowed for loopback hosts)",
            ));
        }
        if host.eq_ignore_ascii_case("localhost") {
            return Err(UnifiedError::bad_request(
                "Use an explicit loopback IP (127.0.0.1 or [::1]) instead of \"localhost\"",
            ));
        }
    }
    Ok(url)
}

/// At delivery time, resolve the host and refuse the delivery when
/// every resolved address is forbidden. This closes the DNS-rebinding
/// gap between endpoint creation and delivery.
fn resolved_addresses_are_safe(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if let Ok(ip) = host.parse::<IpAddr>() {
        return is_loopback(ip) || !is_forbidden_ip(ip);
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return false;
    };
    let mut any = false;
    let mut any_safe = false;
    for socket in addresses {
        any = true;
        let ip = socket.ip();
        if is_loopback(ip) || !is_forbidden_ip(ip) {
            any_safe = true;
        }
    }
    any && any_safe
}

fn validate_event_names(events: &[String]) -> UnifiedResult<()> {
    if events.is_empty() {
        return Err(UnifiedError::bad_request(
            "events must contain at least one event name",
        ));
    }
    for event in events {
        if !KNOWN_EVENTS.contains(&event.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Unknown event \"{event}\"; see GET /api/v1/webhooks/events for the subscribable events"
            )));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Delivery worker.
// ---------------------------------------------------------------------------

fn build_client(timeout_secs: u64) -> UnifiedResult<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

fn backoff_delay(attempt: u32, base_secs: u64) -> Duration {
    let secs = base_secs
        .saturating_mul(1u64 << attempt.min(6))
        .min(MAX_BACKOFF_SECS);
    Duration::from_secs(secs.max(1))
}

fn is_retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

/// Deliver one delivery record with retries. Runs as a background
/// task; updates the stored record and emits `webhook.delivered` or
/// `webhook.failed` when it finishes.
async fn run_delivery(delivery_id: Uuid) {
    let Some(record) = deliveries_store().get(&delivery_id).map(|e| e.value().clone()) else {
        return;
    };
    let Some(endpoint) = endpoints_store()
        .get(&record.endpoint_id)
        .map(|e| e.value().clone())
    else {
        mark_failed(delivery_id, "Webhook endpoint no longer exists");
        return;
    };
    let Ok(url) = Url::parse(&endpoint.url) else {
        mark_failed(delivery_id, "Webhook endpoint URL is no longer valid");
        return;
    };
    if !resolved_addresses_are_safe(&url) {
        mark_failed(
            delivery_id,
            "Webhook target host resolves to a private or reserved address; delivery refused",
        );
        return;
    }
    let Ok(client) = build_client(endpoint.timeout_secs) else {
        mark_failed(delivery_id, "Cannot build HTTP client for delivery");
        return;
    };
    let body = match serde_json::to_vec(&record.payload) {
        Ok(body) => body,
        Err(error) => {
            mark_failed(delivery_id, &format!("Cannot serialize delivery payload: {error}"));
            return;
        }
    };
    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        set_delivery_attempt(delivery_id, attempt, DeliveryStatus::Delivering);
        let timestamp = Utc::now().timestamp();
        let signature = sign_payload(&endpoint.secret, timestamp, &body);
        let started = std::time::Instant::now();
        let response = client
            .post(url.clone())
            .header(header::CONTENT_TYPE, "application/json")
            .header("X-Unified-Signature", signature)
            .header("X-Unified-Timestamp", timestamp.to_string())
            .header("X-Unified-Event", record.event.clone())
            .header("X-Unified-Delivery", delivery_id.to_string())
            .body(body.clone())
            .send()
            .await;
        let duration_ms = started.elapsed().as_millis() as u64;
        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                // Drain the body so the connection can be reused.
                let _ = response.bytes().await;
                if (200..300).contains(&status) {
                    finish_delivery(delivery_id, status, duration_ms, None);
                    emit(
                        webhook_events::DELIVERED,
                        serde_json::json!({
                            "delivery_id": delivery_id,
                            "endpoint_id": endpoint.id,
                            "event": record.event.clone(),
                            "status_code": status,
                            "duration_ms": duration_ms,
                            "attempts": attempt,
                        }),
                    );
                    return;
                }
                if is_retryable_status(status) && attempt < record.max_attempts {
                    let delay = backoff_delay(attempt, endpoint.backoff_base_secs);
                    set_delivery_retrying(delivery_id, status, duration_ms, delay, &format!("HTTP {status}"));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                let message = format!("Delivery failed with HTTP {status}");
                finish_delivery(delivery_id, status, duration_ms, Some(message.clone()));
                emit(
                    webhook_events::FAILED,
                    serde_json::json!({
                        "delivery_id": delivery_id,
                        "endpoint_id": endpoint.id,
                        "event": record.event.clone(),
                        "status_code": status,
                        "error": message,
                        "attempts": attempt,
                    }),
                );
                return;
            }
            Err(error) => {
                let retryable = error.is_connect() || error.is_timeout() || error.is_body();
                let message = format!("Delivery request failed: {error}");
                if retryable && attempt < record.max_attempts {
                    let delay = backoff_delay(attempt, endpoint.backoff_base_secs);
                    set_delivery_retrying(delivery_id, None, duration_ms, delay, &message);
                    tokio::time::sleep(delay).await;
                    continue;
                }
                finish_delivery(delivery_id, None, duration_ms, Some(message.clone()));
                emit(
                    webhook_events::FAILED,
                    serde_json::json!({
                        "delivery_id": delivery_id,
                        "endpoint_id": endpoint.id,
                        "event": record.event.clone(),
                        "error": message,
                        "attempts": attempt,
                    }),
                );
                return;
            }
        }
    }
}

fn set_delivery_attempt(id: Uuid, attempt: u32, status: DeliveryStatus) {
    if let Some(mut record) = deliveries_store().get_mut(&id) {
        record.attempts = attempt;
        record.set_status(status);
    }
}

fn set_delivery_retrying(
    id: Uuid,
    status_code: Option<u16>,
    duration_ms: u64,
    delay: Duration,
    error: &str,
) {
    if let Some(mut record) = deliveries_store().get_mut(&id) {
        record.status_code = status_code;
        record.duration_ms = Some(duration_ms);
        record.error = Some(error.to_string());
        record.next_retry_at = Some(Utc::now() + chrono::Duration::from_std(delay).unwrap_or(chrono::Duration::zero()));
        record.set_status(DeliveryStatus::Retrying);
    }
}

fn finish_delivery(id: Uuid, status_code: Option<u16>, duration_ms: u64, error: Option<String>) {
    let delivered = error.is_none();
    if let Some(mut record) = deliveries_store().get_mut(&id) {
        record.status_code = status_code;
        record.duration_ms = Some(duration_ms);
        record.error = error;
        record.next_retry_at = None;
        if delivered {
            record.delivered_at = Some(Utc::now());
        }
        record.set_status(if delivered {
            DeliveryStatus::Delivered
        } else {
            DeliveryStatus::Failed
        });
    }
    if let Some(delivery) = deliveries_store().get(&id) {
        if let Some(mut endpoint) = endpoints_store().get_mut(&delivery.endpoint_id) {
            endpoint.last_delivery_at = Some(Utc::now());
            endpoint.last_status = Some(delivery.status.clone());
            endpoint.updated_at = Utc::now();
        }
    }
}

fn mark_failed(id: Uuid, error: &str) {
    if let Some(mut record) = deliveries_store().get_mut(&id) {
        record.error = Some(error.to_string());
        record.set_status(DeliveryStatus::Failed);
    }
}

/// Create a delivery record for one endpoint and start its background
/// task when a Tokio runtime is available. When called outside a
/// runtime (for example from a non-async hook emitter) the record
/// stays `queued` and can be started later through manual redelivery.
fn enqueue_delivery(endpoint: &WebhookEndpoint, event: &str, payload: &Value) -> Uuid {
    let body = serde_json::to_vec(payload).unwrap_or_default();
    let delivery = WebhookDelivery {
        id: Uuid::now_v7(),
        endpoint_id: endpoint.id,
        event: event.to_string(),
        payload: payload.clone(),
        payload_hash: payload_hash(&body),
        status: DeliveryStatus::Queued.as_str().to_string(),
        attempts: 0,
        max_attempts: endpoint.max_attempts,
        next_retry_at: None,
        status_code: None,
        duration_ms: None,
        error: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
        delivered_at: None,
    };
    let id = delivery.id;
    deliveries_store().insert(id, delivery);
    if let Some(mut endpoint_entry) = endpoints_store().get_mut(&endpoint.id) {
        endpoint_entry.delivery_count += 1;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move { run_delivery(id).await });
        }
        Err(_) => {
            // No runtime on this thread: the delivery stays queued
            // until a redelivery request runs it.
        }
    }
    id
}

/// Dispatch an event to every matching endpoint. Called by the hook
/// handlers registered in `register_hooks`, and directly by any code
/// that wants to fan an event out without going through the bus.
pub fn dispatch_event(event: &str, payload: &Value) -> Vec<Uuid> {
    let mut ids = Vec::new();
    for entry in endpoints_store().iter() {
        let endpoint = entry.value();
        if !endpoint.active || endpoint.paused {
            continue;
        }
        if !endpoint.events.iter().any(|name| name == event) {
            continue;
        }
        ids.push(enqueue_delivery(endpoint, event, payload));
    }
    ids
}

// ---------------------------------------------------------------------------
// Request types.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct CreateEndpointRequest {
    name: String,
    url: String,
    events: Vec<String>,
    /// Signing secret. When omitted, a random 48-character secret is generated.
    #[serde(default)]
    secret: Option<String>,
    #[serde(default = "default_true")]
    active: bool,
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default = "default_backoff_base_secs")]
    backoff_base_secs: u64,
    #[serde(default = "default_timeout_secs")]
    timeout_secs: u64,
}

fn default_true() -> bool {
    true
}

fn default_max_attempts() -> u32 {
    DEFAULT_MAX_ATTEMPTS
}

fn default_backoff_base_secs() -> u64 {
    DEFAULT_BACKOFF_BASE_SECS
}

fn default_timeout_secs() -> u64 {
    DEFAULT_TIMEOUT_SECS
}

#[derive(Debug, Deserialize)]
struct UpdateEndpointRequest {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    events: Option<Vec<String>>,
    /// Replaces the signing secret. The old secret stops working at once.
    #[serde(default)]
    secret: Option<String>,
    #[serde(default)]
    active: Option<bool>,
    #[serde(default)]
    paused: Option<bool>,
    #[serde(default)]
    max_attempts: Option<u32>,
    #[serde(default)]
    backoff_base_secs: Option<u64>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
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

fn find_endpoint(id: Uuid) -> UnifiedResult<WebhookEndpoint> {
    endpoints_store()
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Webhook endpoint not found"))
}

fn validate_retry_policy(max_attempts: u32, backoff_base_secs: u64, timeout_secs: u64) -> UnifiedResult<()> {
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

// ---------------------------------------------------------------------------
// Handlers.
// ---------------------------------------------------------------------------

async fn list_events(ctx: web::Data<Arc<AppContext>>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        Ok(serde_json::json!({
            "events": KNOWN_EVENTS,
            "count": KNOWN_EVENTS.len(),
        }))
    })();
    json::respond(result, debug)
}

async fn create_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<CreateEndpointRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        let name = body.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(UnifiedError::bad_request(
                "name is required and must be at most 120 characters",
            ));
        }
        let url = validate_webhook_url(&body.url)?;
        validate_event_names(&body.events)?;
        validate_retry_policy(body.max_attempts, body.backoff_base_secs, body.timeout_secs)?;
        let secret = match &body.secret {
            Some(secret) if !secret.trim().is_empty() => {
                if secret.chars().count() < 16 {
                    return Err(UnifiedError::bad_request(
                        "secret must be at least 16 characters",
                    ));
                }
                secret.clone()
            }
            _ => rand::rng()
                .sample_iter(&Alphanumeric)
                .take(48)
                .map(char::from)
                .collect(),
        };
        let now = Utc::now();
        let endpoint = WebhookEndpoint {
            id: Uuid::now_v7(),
            name,
            url: url.to_string(),
            events: body.events.clone(),
            secret,
            active: body.active,
            paused: false,
            max_attempts: body.max_attempts,
            backoff_base_secs: body.backoff_base_secs,
            timeout_secs: body.timeout_secs,
            delivery_count: 0,
            last_delivery_at: None,
            last_status: None,
            created_at: now,
            updated_at: now,
        };
        let view = endpoint.view();
        endpoints_store().insert(endpoint.id, endpoint);
        emit(
            webhook_events::ENDPOINT_CREATED,
            serde_json::json!({
                "endpoint_id": view.id,
                "name": view.name.clone(),
                "url": view.url.clone(),
                "events": view.events.clone(),
            }),
        );
        Ok(view)
    })();
    match result {
        Ok(view) => json::success_response(view, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_endpoints(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<WebhookEndpointView> = endpoints_store()
            .iter()
            .map(|entry| entry.value().view())
            .collect();
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|endpoint| {
                endpoint.name.to_lowercase().contains(&needle)
                    || endpoint.url.to_lowercase().contains(&needle)
            });
        }
        if let Some(event) = params.filters.get("event") {
            items.retain(|endpoint| endpoint.events.iter().any(|name| name == event));
        }
        if let Some(active) = params.filters.get("active") {
            let wanted = active == "true";
            items.retain(|endpoint| endpoint.active == wanted);
        }
        if let Some(paused) = params.filters.get("paused") {
            let wanted = paused == "true";
            items.retain(|endpoint| endpoint.paused == wanted);
        }
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_endpoint(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(find_endpoint(id.into_inner()).map(|endpoint| endpoint.view()), debug)
}

async fn update_endpoint(
    ctx: web::Data<Arc<AppContext>>,
    id: web::Path<Uuid>,
    body: web::Json<UpdateEndpointRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<WebhookEndpointView> = (|| {
        if let Some(name) = &body.name {
            let name = name.trim();
            if name.is_empty() || name.chars().count() > 120 {
                return Err(UnifiedError::bad_request(
                    "name must be non-empty and at most 120 characters",
                ));
            }
        }
        let url = match &body.url {
            Some(raw) => Some(validate_webhook_url(raw)?),
            None => None,
        };
        if let Some(events) = &body.events {
            validate_event_names(events)?;
        }
        if let Some(secret) = &body.secret {
            if secret.chars().count() < 16 {
                return Err(UnifiedError::bad_request(
                    "secret must be at least 16 characters",
                ));
            }
        }
        let mut endpoint = endpoints_store()
            .get_mut(&id)
            .ok_or_else(|| UnifiedError::not_found("Webhook endpoint not found"))?;
        let current_policy = (
            body.max_attempts.unwrap_or(endpoint.max_attempts),
            body.backoff_base_secs.unwrap_or(endpoint.backoff_base_secs),
            body.timeout_secs.unwrap_or(endpoint.timeout_secs),
        );
        validate_retry_policy(current_policy.0, current_policy.1, current_policy.2)?;
        if let Some(name) = &body.name {
            endpoint.name = name.trim().to_string();
        }
        if let Some(url) = url {
            endpoint.url = url.to_string();
        }
        if let Some(events) = &body.events {
            endpoint.events = events.clone();
        }
        if let Some(secret) = &body.secret {
            endpoint.secret = secret.clone();
        }
        if let Some(active) = body.active {
            endpoint.active = active;
        }
        if let Some(paused) = body.paused {
            endpoint.paused = paused;
        }
        endpoint.max_attempts = current_policy.0;
        endpoint.backoff_base_secs = current_policy.1;
        endpoint.timeout_secs = current_policy.2;
        endpoint.updated_at = Utc::now();
        let view = endpoint.view();
        drop(endpoint);
        emit(
            webhook_events::ENDPOINT_UPDATED,
            serde_json::json!({
                "endpoint_id": view.id,
                "name": view.name.clone(),
                "url": view.url.clone(),
                "events": view.events.clone(),
            }),
        );
        Ok(view)
    })();
    json::respond(result, debug)
}

async fn delete_endpoint(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let removed = endpoints_store().remove(&id);
        match removed {
            Some((_, endpoint)) => {
                emit(
                    webhook_events::ENDPOINT_DELETED,
                    serde_json::json!({ "endpoint_id": id, "name": endpoint.name }),
                );
                Ok(serde_json::json!({ "id": id, "deleted": true }))
            }
            None => Err(UnifiedError::not_found("Webhook endpoint not found")),
        }
    })();
    json::respond(result, debug)
}

fn set_paused(id: Uuid, paused: bool) -> UnifiedResult<WebhookEndpointView> {
    let mut endpoint = endpoints_store()
        .get_mut(&id)
        .ok_or_else(|| UnifiedError::not_found("Webhook endpoint not found"))?;
    endpoint.paused = paused;
    endpoint.updated_at = Utc::now();
    let view = endpoint.view();
    drop(endpoint);
    if paused {
        emit(
            webhook_events::ENDPOINT_PAUSED,
            serde_json::json!({ "endpoint_id": id }),
        );
    }
    Ok(view)
}

async fn pause_endpoint(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(set_paused(id.into_inner(), true), debug)
}

async fn resume_endpoint(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    json::respond(set_paused(id.into_inner(), false), debug)
}

async fn test_endpoint(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<Value> = (|| {
        let endpoint = find_endpoint(id)?;
        if !endpoint.active {
            return Err(UnifiedError::conflict(
                "Cannot test a disabled webhook endpoint; enable it first",
            ));
        }
        let payload = serde_json::json!({
            "event": webhook_events::PING,
            "endpoint_id": endpoint.id,
            "sent_at": Utc::now().to_rfc3339(),
            "message": "Test delivery from the webhooks manager",
        });
        let delivery_id = enqueue_delivery(&endpoint, webhook_events::PING, &payload);
        Ok(serde_json::json!({
            "delivery_id": delivery_id,
            "endpoint_id": endpoint.id,
            "event": webhook_events::PING,
        }))
    })();
    match result {
        Ok(view) => json::success_response(view, 202),
        Err(error) => json::error_response(&error, debug),
    }
}

async fn list_deliveries(
    ctx: web::Data<Arc<AppContext>>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let mut items: Vec<WebhookDelivery> = deliveries_store()
            .iter()
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(endpoint_id) = params.filters.get("endpoint_id") {
            items.retain(|delivery| delivery.endpoint_id.to_string() == *endpoint_id);
        }
        if let Some(event) = params.filters.get("event") {
            items.retain(|delivery| delivery.event == *event);
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|delivery| delivery.status == *status);
        }
        if let Some(search) = &params.search {
            let needle = search.to_lowercase();
            items.retain(|delivery| delivery.event.to_lowercase().contains(&needle));
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

async fn get_delivery(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<WebhookDelivery> = (|| {
        deliveries_store()
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Webhook delivery not found"))
    })();
    json::respond(result, debug)
}

async fn redeliver(ctx: web::Data<Arc<AppContext>>, id: web::Path<Uuid>) -> HttpResponse {
    let debug = ctx.is_debug();
    let id = id.into_inner();
    let result: UnifiedResult<WebhookDelivery> = (|| {
        let delivery = deliveries_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Webhook delivery not found"))?;
        if delivery.status == "delivering" || delivery.status == "retrying" {
            return Err(UnifiedError::conflict(format!(
                "Cannot redeliver a delivery in status \"{}\"",
                delivery.status
            )));
        }
        if let Some(mut record) = deliveries_store().get_mut(&id) {
            record.attempts = 0;
            record.status_code = None;
            record.error = None;
            record.next_retry_at = None;
            record.delivered_at = None;
            record.set_status(DeliveryStatus::Queued);
        }
        let delivery_id = id;
        tokio::spawn(async move { run_delivery(delivery_id).await });
        deliveries_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Webhook delivery not found"))
    })();
    match result {
        Ok(view) => json::success_response(view, 202),
        Err(error) => json::error_response(&error, debug),
    }
}

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct WebhooksModule;

impl Module for WebhooksModule {
    fn id(&self) -> &'static str {
        "webhooks"
    }

    fn name(&self) -> &'static str {
        "Webhooks Manager"
    }

    fn description(&self) -> &'static str {
        "Outgoing webhooks: endpoint subscriptions with HMAC-SHA256 signed deliveries, retry with exponential backoff, a per-attempt delivery log, manual redelivery, and test deliveries."
    }

    fn requires(&self) -> &[&'static str] {
        &["security"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &["reqwest", "hmac", "sha2", "url"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        let _ = HOOK_BUS.set(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        // One handler per subscribable event. The handler only fans
        // the event out to matching endpoints; the HTTP work happens
        // in background tasks, never on the emitting thread.
        // Webhook lifecycle events (`webhook.*`) are deliberately NOT
        // re-dispatched: a delivery emits `webhook.delivered`, and
        // fanning that out again would loop forever through any
        // endpoint subscribed to it. `webhook.ping` is only ever sent
        // by an explicit test request.
        for event_name in KNOWN_EVENTS {
            if event_name.starts_with("webhook.") || *event_name == "notification.webhook" {
                continue;
            }
            let event = event_name.to_string();
            let handler: crate::core::hooks::HookHandler =
                Arc::new(move |context: &mut crate::core::hooks::HookContext| {
                    let payload = context.data.clone();
                    dispatch_event(&event, &payload);
                });
            bus.register(&event, "webhooks", handler);
        }
        // The notifications module emits `notification.webhook` when
        // the webhook channel is selected for a notification, with
        // `{"event", "notification_id", "user_id", "title", "body",
        // "kind"}` (plus `"retry": true` on a retry). This listener
        // delivers that payload to every active, unpaused endpoint
        // subscribed to `notification.webhook`, through the same
        // `dispatch_event` / delivery-record path as every other
        // event, and reports the outcome as the hook result. When no
        // endpoint matches, the miss is recorded as a
        // `webhook.failed` event (which the notifications module
        // subscribes to) instead of being silently dropped: a
        // notification whose webhook channel delivers nowhere is a
        // misconfiguration the operator must see.
        let handler: crate::core::hooks::HookHandler =
            Arc::new(|context: &mut crate::core::hooks::HookContext| {
                let payload = context.data.clone();
                let deliveries = dispatch_event("notification.webhook", &payload);
                context.add_result(serde_json::json!({
                    "matched_endpoints": deliveries.len(),
                    "delivery_ids": deliveries,
                }));
                if deliveries.is_empty() {
                    emit(
                        webhook_events::FAILED,
                        serde_json::json!({
                            "event": "notification.webhook",
                            "endpoint_id": "none",
                            "notification_id": payload.get("notification_id"),
                            "error": "No webhook endpoint is subscribed to \"notification.webhook\", so the notification was not delivered anywhere",
                            "attempts": 0,
                        }),
                    );
                }
            });
        bus.register("notification.webhook", "webhooks", handler);
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/webhooks")
                .route("/events", web::get().to(list_events))
                .route("/endpoints", web::get().to(list_endpoints))
                .route("/endpoints", web::post().to(create_endpoint))
                .route("/endpoints/{id}", web::get().to(get_endpoint))
                .route("/endpoints/{id}", web::put().to(update_endpoint))
                .route("/endpoints/{id}", web::delete().to(delete_endpoint))
                .route("/endpoints/{id}/pause", web::post().to(pause_endpoint))
                .route("/endpoints/{id}/resume", web::post().to(resume_endpoint))
                .route("/endpoints/{id}/test", web::post().to(test_endpoint))
                .route("/deliveries", web::get().to(list_deliveries))
                .route("/deliveries/{id}", web::get().to(get_delivery))
                .route("/deliveries/{id}/redeliver", web::post().to(redeliver)),
        );
    }

    fn route_info(&self) -> Vec<crate::core::modules::RouteInfo> {
        use crate::core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/webhooks/events", owner: "webhooks", description: "Catalog of subscribable event names" },
            RouteInfo { method: "GET", path: "/api/v1/webhooks/endpoints", owner: "webhooks", description: "List webhook endpoints (paginated)" },
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints", owner: "webhooks", description: "Create a webhook endpoint subscription" },
            RouteInfo { method: "GET", path: "/api/v1/webhooks/endpoints/{id}", owner: "webhooks", description: "Get one webhook endpoint" },
            RouteInfo { method: "PUT", path: "/api/v1/webhooks/endpoints/{id}", owner: "webhooks", description: "Update a webhook endpoint" },
            RouteInfo { method: "DELETE", path: "/api/v1/webhooks/endpoints/{id}", owner: "webhooks", description: "Delete a webhook endpoint" },
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints/{id}/pause", owner: "webhooks", description: "Pause deliveries to an endpoint" },
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints/{id}/resume", owner: "webhooks", description: "Resume deliveries to an endpoint" },
            RouteInfo { method: "POST", path: "/api/v1/webhooks/endpoints/{id}/test", owner: "webhooks", description: "Send a webhook.ping test delivery" },
            RouteInfo { method: "GET", path: "/api/v1/webhooks/deliveries", owner: "webhooks", description: "List webhook deliveries (paginated)" },
            RouteInfo { method: "GET", path: "/api/v1/webhooks/deliveries/{id}", owner: "webhooks", description: "Get one webhook delivery" },
            RouteInfo { method: "POST", path: "/api/v1/webhooks/deliveries/{id}/redeliver", owner: "webhooks", description: "Redeliver a webhook delivery" },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WebhooksModule));
}
