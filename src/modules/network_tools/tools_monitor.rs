//! Uptime probes, monitor evaluation and certificate watch checks.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use actix_web::{web, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use std::time::{Instant};
use tokio::time::timeout;
use uuid::Uuid;
use super::types::{CERT_ALERT_THRESHOLDS_DAYS, DEFAULT_TOTAL_TIMEOUT_SECS, NetworkToolCertWatch, NetworkToolUptimeEvent, ToolResult, network_tool_events};
use super::store::{cert_watches_store, db_insert_uptime_event, db_pool, db_upsert_cert_watch, db_upsert_monitor, uptime_events_store, uptime_monitors_store};
use super::helpers::{notify, record_run};
use super::validation::{clamp_total_timeout, guard_url, pinned_http_client, url_port_or_default};
use super::tools_tls::tool_ssl_check;

// ---------------------------------------------------------------------------
// Uptime: one-off check plus monitor evaluation.
// ---------------------------------------------------------------------------

pub(crate) async fn run_uptime_probe(
    url_text: &str,
    expected_status: i32,
    timeout_secs: u64,
    allow_private: bool,
) -> UnifiedResult<(u16, u64, String)> {
    let total_timeout = clamp_total_timeout(Some(timeout_secs));
    let (url, addrs) = guard_url(url_text, allow_private).await?;
    // Pin the connection to the guarded addresses (same
    // DNS-rebinding protection as the HTTP checker); the original
    // hostname stays in the URL, so Host and TLS SNI are unchanged.
    let parsed_url = reqwest::Url::parse(&url)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid URL: \"{url}\"")))?;
    let host = parsed_url.host_str().unwrap_or("").to_string();
    let port = url_port_or_default(&parsed_url);
    let client = pinned_http_client(&host, &addrs, port, total_timeout)?;
    let started = Instant::now();
    let response = timeout(total_timeout, client.get(url).send())
        .await
        .map_err(|_| UnifiedError::bad_request("Uptime probe timed out"))?
        .map_err(|error| UnifiedError::bad_request(format!("Uptime probe failed: {error}")))?;
    let status = response.status().as_u16();
    let latency_ms = started.elapsed().as_millis() as u64;
    let outcome = if status == expected_status as u16 {
        "up"
    } else {
        "down"
    };
    Ok((status, latency_ms, outcome.to_string()))
}

/// Evaluate one monitor: probe it, store the event, update the
/// failure counter, and emit `network_tools.uptime.down` plus a
/// notification when the failure threshold is crossed. Returns the
/// monitor's new status.
pub(crate) async fn evaluate_monitor(
    ctx: &AppContext,
    monitor_id: Uuid,
    actor: &str,
    allow_private: bool,
) -> UnifiedResult<Value> {
    let Some(mut monitor) = uptime_monitors_store()
        .get(&monitor_id)
        .map(|entry| entry.value().clone())
    else {
        return Err(UnifiedError::not_found(format!(
            "Uptime monitor {monitor_id} not found"
        )));
    };
    let probe = run_uptime_probe(
        &monitor.url,
        monitor.expected_status,
        DEFAULT_TOTAL_TIMEOUT_SECS,
        allow_private,
    )
    .await;
    let (http_status, latency_ms, status) = match &probe {
        Ok((status, latency, outcome)) => (Some(*status as i32), Some(*latency as i64), outcome.clone()),
        Err(error) => (None, None, format!("down: {}", error.message)),
    };
    let is_up = status == "up";
    if is_up {
        monitor.consecutive_failures = 0;
        monitor.last_status = Some("up".to_string());
    } else {
        monitor.consecutive_failures += 1;
        monitor.last_status = Some("down".to_string());
    }
    monitor.last_checked_at = Some(Utc::now());
    monitor.updated_at = Utc::now();
    let event = NetworkToolUptimeEvent {
        id: Uuid::new_v4(),
        monitor_id,
        status: if is_up { "up".to_string() } else { "down".to_string() },
        http_status,
        latency_ms,
        detail: json!({
            "url": monitor.url,
            "expected_status": monitor.expected_status,
            "consecutive_failures": monitor.consecutive_failures,
            "error": match &probe {
                Ok(_) => Value::Null,
                Err(error) => json!(error.message.clone()),
            },
        }),
        created_at: Utc::now(),
    };
    uptime_events_store().insert(event.id, event.clone());
    uptime_monitors_store().insert(monitor.id, monitor.clone());
    if let Some(pool) = db_pool(ctx) {
        let _ = db_insert_uptime_event(&pool, &event);
        let _ = db_upsert_monitor(&pool, &monitor);
    }
    if is_up {
        ctx.hooks.emit(
            network_tool_events::UPTIME_UP,
            json!({
                "monitor_id": monitor.id,
                "url": monitor.url,
                "http_status": http_status,
                "latency_ms": latency_ms,
            }),
        );
    } else {
        ctx.hooks.emit(
            network_tool_events::UPTIME_DOWN,
            json!({
                "monitor_id": monitor.id,
                "url": monitor.url,
                "http_status": http_status,
                "consecutive_failures": monitor.consecutive_failures,
                "failure_threshold": monitor.failure_threshold,
            }),
        );
        if monitor.consecutive_failures >= monitor.failure_threshold {
            notify(
                ctx,
                actor,
                format!("Uptime monitor \"{}\" is down", monitor.name),
                format!(
                    "The monitor for {} has failed {} time(s) in a row (threshold {}); last HTTP status {}.",
                    monitor.url,
                    monitor.consecutive_failures,
                    monitor.failure_threshold,
                    http_status.map(|s| s.to_string()).unwrap_or_else(|| "unreachable".to_string())
                ),
                "network_tools.uptime.down",
            );
        }
    }
    Ok(json!({
        "monitor_id": monitor.id,
        "status": monitor.last_status,
        "http_status": http_status,
        "latency_ms": latency_ms,
        "consecutive_failures": monitor.consecutive_failures,
    }))
}

// ---------------------------------------------------------------------------
// Certificate expiry watch evaluation.
// ---------------------------------------------------------------------------

/// Check every enabled watch: prefer the certificates module's live
/// data for managed domains, otherwise probe live with the SSL/TLS
/// checker. Alert at 30/14/7/1 days (and once on expiry) through
/// `network_tools.cert.expiring` plus a direct `notification.requested`.
/// Returns a summary of what was checked.
pub(crate) async fn check_cert_watches(ctx: &AppContext, actor: &str) -> UnifiedResult<Value> {
    let watches: Vec<NetworkToolCertWatch> = cert_watches_store()
        .iter()
        .filter(|entry| entry.value().enabled)
        .map(|entry| entry.value().clone())
        .collect();
    let mut checked = 0u64;
    let mut expiring: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for mut watch in watches {
        // Managed-domain data first (certificates module storage).
        let mut days: Option<i64> = None;
        if let Some(storage) = ctx.tls() {
            for info in storage.list_certificates() {
                if info.domain.eq_ignore_ascii_case(&watch.domain) {
                    days = Some(info.days_remaining);
                    break;
                }
            }
        }
        // Otherwise probe live.
        if days.is_none() {
            match tool_ssl_check(ctx, &watch.domain, 443, DEFAULT_TOTAL_TIMEOUT_SECS, false).await
            {
                Ok(result) => {
                    days = result
                        .result
                        .get("days_remaining")
                        .and_then(|v| v.as_i64());
                }
                Err(_) => {
                    continue;
                }
            }
        }
        let Some(days) = days else {
            continue;
        };
        checked += 1;
        watch.last_days_remaining = Some(days as i32);
        watch.last_checked_at = Some(Utc::now());
        let mut crossed: Vec<i64> = Vec::new();
        if days < 0 {
            if !watch.alerted_thresholds.contains(&-1) {
                crossed.push(-1);
            }
            expired.push(json!({
                "domain": watch.domain,
                "days_remaining": days,
            }));
        } else {
            for threshold in CERT_ALERT_THRESHOLDS_DAYS {
                if days <= threshold && !watch.alerted_thresholds.contains(&(threshold as i32)) {
                    crossed.push(threshold);
                }
            }
            if !crossed.is_empty() {
                expiring.push(json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": crossed,
                }));
            }
        }
        for threshold in &crossed {
            watch.alerted_thresholds.push(*threshold as i32);
            ctx.hooks.emit(
                network_tool_events::CERT_EXPIRING,
                json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": threshold,
                    "expired": *threshold == -1,
                }),
            );
            if *threshold == -1 {
                notify(
                    ctx,
                    actor,
                    format!("Certificate for {} has expired", watch.domain),
                    format!(
                        "The certificate for {} expired {} day(s) ago; renewal is due immediately.",
                        watch.domain,
                        -days
                    ),
                    "network_tools.cert.expiring",
                );
            } else {
                notify(
                    ctx,
                    actor,
                    format!("Certificate for {} expires in {days} day(s)", watch.domain),
                    format!(
                        "The certificate for {} expires in {days} day(s); renewal is due within the configured window.",
                        watch.domain
                    ),
                    "network_tools.cert.expiring",
                );
            }
        }
        watch.updated_at = Utc::now();
        cert_watches_store().insert(watch.id, watch.clone());
        if let Some(pool) = db_pool(ctx) {
            let _ = db_upsert_cert_watch(&pool, &watch);
        }
    }
    Ok(json!({
        "checked": checked,
        "expiring": expiring,
        "expired": expired,
    }))
}

/// Synchronous alert scan used by the `network_tools.cert.check_requested`
/// hook handler: re-evaluates thresholds from the last known expiry
/// (managed-domain storage or the stored `last_days_remaining`)
/// without any network probe, so it can run inside a sync handler.
pub(crate) fn check_cert_watches_sync(ctx: &AppContext) -> Value {
    let watches: Vec<NetworkToolCertWatch> = cert_watches_store()
        .iter()
        .filter(|entry| entry.value().enabled)
        .map(|entry| entry.value().clone())
        .collect();
    let mut checked = 0u64;
    let mut expiring: Vec<Value> = Vec::new();
    let mut expired: Vec<Value> = Vec::new();
    for mut watch in watches {
        let mut days: Option<i64> = watch.last_days_remaining.map(i64::from);
        if let Some(storage) = ctx.tls() {
            for info in storage.list_certificates() {
                if info.domain.eq_ignore_ascii_case(&watch.domain) {
                    days = Some(info.days_remaining);
                    break;
                }
            }
        }
        let Some(days) = days else {
            continue;
        };
        checked += 1;
        let mut crossed: Vec<i64> = Vec::new();
        if days < 0 {
            if !watch.alerted_thresholds.contains(&-1) {
                crossed.push(-1);
            }
            expired.push(json!({ "domain": watch.domain, "days_remaining": days }));
        } else {
            for threshold in CERT_ALERT_THRESHOLDS_DAYS {
                if days <= threshold && !watch.alerted_thresholds.contains(&(threshold as i32)) {
                    crossed.push(threshold);
                }
            }
            if !crossed.is_empty() {
                expiring.push(json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": crossed,
                }));
            }
        }
        for threshold in &crossed {
            watch.alerted_thresholds.push(*threshold as i32);
            ctx.hooks.emit(
                network_tool_events::CERT_EXPIRING,
                json!({
                    "domain": watch.domain,
                    "days_remaining": days,
                    "threshold_days": threshold,
                    "expired": *threshold == -1,
                }),
            );
            notify(
                ctx,
                "system",
                if *threshold == -1 {
                    format!("Certificate for {} has expired", watch.domain)
                } else {
                    format!("Certificate for {} expires in {days} day(s)", watch.domain)
                },
                format!(
                    "The certificate for {} expires in {days} day(s); renewal is due within the configured window.",
                    watch.domain
                ),
                "network_tools.cert.expiring",
            );
        }
        watch.updated_at = Utc::now();
        cert_watches_store().insert(watch.id, watch.clone());
    }
    json!({
        "checked": checked,
        "expiring": expiring,
        "expired": expired,
        "note": "Sync scan from last known expiry only; use POST /api/v1/network-tools/cert-watches/check for a live probe.",
    })
}

// ---------------------------------------------------------------------------
// HTTP handlers.
// ---------------------------------------------------------------------------

/// Run a tool, record the run (success or failure), and answer with
/// the standard envelope. Failures are recorded too, with the error
/// message as the summary - never with a response body.
pub(crate) async fn execute_tool(
    ctx: web::Data<Arc<AppContext>>,
    started: Instant,
    actor: &str,
    company_id: Option<&str>,
    tool: &str,
    target: &str,
    outcome: UnifiedResult<ToolResult>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    match outcome {
        Ok(mut result) => {
            result.duration_ms = started.elapsed().as_millis() as u64;
            let summary = json!({
                "outcome": result.outcome,
                "duration_ms": result.duration_ms,
                "method": result.method,
            });
            let run_id = record_run(
                &ctx,
                actor,
                company_id,
                tool,
                target,
                started,
                &result.outcome,
                summary,
            );
            let mut payload = json!(result);
            payload["run_id"] = json!(run_id);
            json::respond(Ok::<Value, UnifiedError>(payload), debug)
        }
        Err(error) => {
            let message = error.message.clone();
            record_run(
                &ctx,
                actor,
                company_id,
                tool,
                target,
                started,
                "failed",
                json!({ "error": message }),
            );
            json::respond(Err::<Value, UnifiedError>(error), debug)
        }
    }
}
