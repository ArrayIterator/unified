// Webhooks — Delivery worker.

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
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use url::Url;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Delivery worker.
// ---------------------------------------------------------------------------

pub(crate) fn build_client(timeout_secs: u64) -> UnifiedResult<reqwest::Client> {
    build_client_resolved(timeout_secs, "", &[])
}

/// Build the delivery client, pinning DNS for `host` to the
/// pre-validated addresses. The override keeps the original host in
/// the request URL (Host header and TLS SNI), while the socket only
/// ever connects to an address that passed validation — the client
/// performs no second lookup, so a DNS change between validation
/// and connect cannot steer the request at a private address
/// (DNS-rebinding TOCTOU). IP-literal hosts need no override: no
/// lookup happens for them.
pub(crate) fn build_client_resolved(
    timeout_secs: u64,
    host: &str,
    addrs: &[SocketAddr],
) -> UnifiedResult<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(timeout_secs))
        .redirect(reqwest::redirect::Policy::none());
    if !addrs.is_empty() && host.parse::<IpAddr>().is_err() {
        builder = builder.resolve_to_addrs(host, addrs);
    }
    builder
        .build()
        .map_err(|error| UnifiedError::internal(format!("Cannot build HTTP client: {error}")))
}

pub(crate) fn backoff_delay(attempt: u32, base_secs: u64) -> Duration {
    let secs = base_secs
        .saturating_mul(1u64 << attempt.min(6))
        .min(MAX_BACKOFF_SECS);
    Duration::from_secs(secs.max(1))
}

pub(crate) fn is_retryable_status(status: u16) -> bool {
    status == 408 || status == 429 || (500..600).contains(&status)
}

/// Why a delivery must not be sent right now.
pub(crate) enum DeliveryBlock {
    /// The endpoint was deleted.
    Missing,
    /// The endpoint is disabled.
    Disabled,
    /// Deliveries to the endpoint are paused.
    Paused,
}

impl DeliveryBlock {
    pub(crate) fn message(&self) -> &'static str {
        match self {
            Self::Missing => "Webhook endpoint no longer exists",
            Self::Disabled => "Webhook endpoint is disabled",
            Self::Paused => "Webhook endpoint is paused",
        }
    }
}

/// Re-read the endpoint state: `None` when delivery may proceed,
/// otherwise the reason it must not be sent. Used before every send
/// attempt, so an endpoint disabled or paused while an earlier
/// attempt was backing off stops the delivery instead of sending
/// into a state the operator already shut off.
pub(crate) fn delivery_blocked(endpoint_id: Uuid) -> Option<DeliveryBlock> {
    match endpoints_store().get(&endpoint_id) {
        Some(entry) => {
            let endpoint = entry.value();
            if !endpoint.active {
                Some(DeliveryBlock::Disabled)
            } else if endpoint.paused {
                Some(DeliveryBlock::Paused)
            } else {
                None
            }
        }
        None => Some(DeliveryBlock::Missing),
    }
}

/// Mark a delivery failed because its endpoint is gone, and emit
/// `webhook.failed` so the miss is visible instead of silently
/// dropped (mirrors the `notification.webhook` no-subscriber miss
/// in `register_hooks`).
fn fail_endpoint_missing(delivery_id: Uuid, endpoint_id: Uuid, event: &str, attempts: u32) {
    mark_failed(delivery_id, DeliveryBlock::Missing.message());
    emit(
        webhook_events::FAILED,
        serde_json::json!({
            "delivery_id": delivery_id,
            "endpoint_id": endpoint_id,
            "event": event,
            "error": DeliveryBlock::Missing.message(),
            "attempts": attempts,
        }),
    );
}

/// Deliver one delivery record with retries. Runs as a background
/// task; updates the stored record and emits `webhook.delivered` or
/// `webhook.failed` when it finishes.
pub(crate) async fn run_delivery(delivery_id: Uuid) {
    let Some(record) = deliveries_store().get(&delivery_id).map(|e| e.value().clone()) else {
        return;
    };
    let Some(endpoint) = endpoints_store()
        .get(&record.endpoint_id)
        .map(|e| e.value().clone())
    else {
        fail_endpoint_missing(delivery_id, record.endpoint_id, &record.event, record.attempts);
        return;
    };
    // Refuse deliveries to endpoints that were disabled or paused
    // after the record was queued.
    if !endpoint.active {
        mark_failed(delivery_id, DeliveryBlock::Disabled.message());
        return;
    }
    if endpoint.paused {
        mark_failed(delivery_id, DeliveryBlock::Paused.message());
        return;
    }
    let Ok(url) = Url::parse(&endpoint.url) else {
        mark_failed(delivery_id, "Webhook endpoint URL is no longer valid");
        return;
    };
    // Resolve once and pin the connection to the validated
    // addresses: the client performs no second lookup, so a DNS
    // change between validation and connect cannot steer the
    // delivery at an internal address (DNS-rebinding TOCTOU).
    let addresses = match resolve_validated_addresses(&url) {
        Ok(addresses) => addresses,
        Err(error) => {
            mark_failed(delivery_id, &error.message);
            return;
        }
    };
    let host = url.host_str().unwrap_or_default().to_string();
    let Ok(client) = build_client_resolved(endpoint.timeout_secs, &host, &addresses) else {
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
        // The endpoint may have been disabled, paused, or deleted
        // while an earlier attempt was backing off; re-check before
        // sending instead of delivering into a shut-off endpoint.
        if let Some(blocked) = delivery_blocked(record.endpoint_id) {
            match blocked {
                DeliveryBlock::Missing => {
                    fail_endpoint_missing(delivery_id, record.endpoint_id, &record.event, attempt);
                }
                other => mark_failed(delivery_id, other.message()),
            }
            return;
        }
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
                    finish_delivery(delivery_id, Some(status), duration_ms, None);
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
                    set_delivery_retrying(delivery_id, Some(status), duration_ms, delay, &format!("HTTP {status}"));
                    tokio::time::sleep(delay).await;
                    continue;
                }
                let message = format!("Delivery failed with HTTP {status}");
                finish_delivery(delivery_id, Some(status), duration_ms, Some(message.clone()));
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

pub(crate) fn set_delivery_attempt(id: Uuid, attempt: u32, status: DeliveryStatus) {
    if let Some(mut record) = deliveries_store().get_mut(&id) {
        record.attempts = attempt;
        record.set_status(status);
    }
}

pub(crate) fn set_delivery_retrying(
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

pub(crate) fn finish_delivery(id: Uuid, status_code: Option<u16>, duration_ms: u64, error: Option<String>) {
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

pub(crate) fn mark_failed(id: Uuid, error: &str) {
    if let Some(mut record) = deliveries_store().get_mut(&id) {
        record.error = Some(error.to_string());
        record.status_code = None;
        record.next_retry_at = None;
        record.set_status(DeliveryStatus::Failed);
    }
    // Mirror finish_delivery: the endpoint's last outcome reflects
    // the latest terminal result while the endpoint still exists.
    if let Some(delivery) = deliveries_store().get(&id) {
        if let Some(mut endpoint) = endpoints_store().get_mut(&delivery.endpoint_id) {
            endpoint.last_delivery_at = Some(Utc::now());
            endpoint.last_status = Some(delivery.status.clone());
            endpoint.updated_at = Utc::now();
        }
    }
}

/// Create a delivery record for one endpoint and start its background
/// task when a Tokio runtime is available. When called outside a
/// runtime (for example from a non-async hook emitter) the record
/// stays `queued` and can be started later through manual redelivery.
pub(crate) fn enqueue_delivery(endpoint: &WebhookEndpoint, event: &str, payload: &Value) -> Uuid {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_delay_returns_base_on_first_attempt() {
        assert_eq!(backoff_delay(0, 5), std::time::Duration::from_secs(5));
    }

    #[test]
    fn backoff_delay_doubles_per_attempt() {
        assert_eq!(backoff_delay(1, 5), std::time::Duration::from_secs(10));
        assert_eq!(backoff_delay(2, 5), std::time::Duration::from_secs(20));
    }

    #[test]
    fn backoff_delay_is_capped() {
        assert_eq!(backoff_delay(10, 5), std::time::Duration::from_secs(300));
        assert_eq!(backoff_delay(10, 300), std::time::Duration::from_secs(300));
    }

    #[test]
    fn retryable_status_covers_transient_failures() {
        assert!(is_retryable_status(408));
        assert!(is_retryable_status(429));
        assert!(is_retryable_status(500));
        assert!(is_retryable_status(503));
    }

    #[test]
    fn retryable_status_rejects_success_and_client_errors() {
        assert!(!is_retryable_status(200));
        assert!(!is_retryable_status(301));
        assert!(!is_retryable_status(400));
        assert!(!is_retryable_status(404));
    }

    fn test_endpoint(active: bool, paused: bool) -> WebhookEndpoint {
        WebhookEndpoint {
            id: Uuid::now_v7(),
            name: "test".to_string(),
            url: "https://example.com/hook".to_string(),
            events: vec!["webhook.ping".to_string()],
            secret: "test-secret-that-is-long".to_string(),
            active,
            paused,
            max_attempts: 3,
            backoff_base_secs: 5,
            timeout_secs: 30,
            delivery_count: 0,
            last_delivery_at: None,
            last_status: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn test_delivery(endpoint_id: Uuid) -> WebhookDelivery {
        WebhookDelivery {
            id: Uuid::now_v7(),
            endpoint_id,
            event: "webhook.ping".to_string(),
            payload: serde_json::json!({"event": "webhook.ping"}),
            payload_hash: "hash".to_string(),
            status: DeliveryStatus::Queued.as_str().to_string(),
            attempts: 1,
            max_attempts: 3,
            next_retry_at: Some(Utc::now() + chrono::Duration::seconds(60)),
            status_code: Some(500),
            duration_ms: Some(12),
            error: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            delivered_at: None,
        }
    }

    #[test]
    fn mark_failed_clears_retry_state_and_updates_endpoint_last_status() {
        let endpoint = test_endpoint(true, false);
        let endpoint_id = endpoint.id;
        endpoints_store().insert(endpoint_id, endpoint);
        let delivery = test_delivery(endpoint_id);
        let delivery_id = delivery.id;
        deliveries_store().insert(delivery_id, delivery);

        mark_failed(delivery_id, "boom");

        let record = deliveries_store().get(&delivery_id).unwrap();
        assert_eq!(record.status, DeliveryStatus::Failed.as_str());
        assert_eq!(record.error.as_deref(), Some("boom"));
        assert_eq!(record.status_code, None);
        assert_eq!(record.next_retry_at, None);
        let endpoint = endpoints_store().get(&endpoint_id).unwrap();
        assert_eq!(endpoint.last_status.as_deref(), Some("failed"));
        assert!(endpoint.last_delivery_at.is_some());
    }

    #[test]
    fn mark_failed_without_an_endpoint_only_marks_the_record() {
        let delivery = test_delivery(Uuid::now_v7());
        let delivery_id = delivery.id;
        deliveries_store().insert(delivery_id, delivery);

        // The endpoint never existed: this must not panic.
        mark_failed(delivery_id, "endpoint gone");

        let record = deliveries_store().get(&delivery_id).unwrap();
        assert_eq!(record.status, DeliveryStatus::Failed.as_str());
        assert_eq!(record.next_retry_at, None);
        assert_eq!(record.status_code, None);
    }

    #[test]
    fn delivery_blocked_reports_missing_disabled_paused_and_clear() {
        // Missing endpoint.
        let missing = delivery_blocked(Uuid::now_v7()).unwrap();
        assert_eq!(missing.message(), "Webhook endpoint no longer exists");

        // Disabled endpoint.
        let disabled = test_endpoint(false, false);
        let disabled_id = disabled.id;
        endpoints_store().insert(disabled_id, disabled);
        let blocked = delivery_blocked(disabled_id).unwrap();
        assert_eq!(blocked.message(), "Webhook endpoint is disabled");

        // Paused endpoint.
        let paused = test_endpoint(true, true);
        let paused_id = paused.id;
        endpoints_store().insert(paused_id, paused);
        let blocked = delivery_blocked(paused_id).unwrap();
        assert_eq!(blocked.message(), "Webhook endpoint is paused");

        // Active, unpaused endpoint.
        let clear = test_endpoint(true, false);
        let clear_id = clear.id;
        endpoints_store().insert(clear_id, clear);
        assert!(delivery_blocked(clear_id).is_none());
    }

    #[test]
    fn build_client_resolved_builds_with_dns_overrides() {
        let addrs = vec![SocketAddr::new(IpAddr::from([93, 184, 216, 34]), 443)];
        assert!(build_client_resolved(30, "example.com", &addrs).is_ok());
        // An IP-literal host takes no override but must still build.
        assert!(build_client_resolved(30, "127.0.0.1", &addrs).is_ok());
        assert!(build_client(30).is_ok());
    }
}
