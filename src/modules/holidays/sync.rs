//! Synchronization engine: fetch, upsert and per-config runs.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, ProductCategory, RouteInfo, RouteVersionSupport};
use unified_core::tenancy::{schema_name_for_str, TenantContext as TenancyContext};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine as _;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use reqwest::header::RETRY_AFTER;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, VecDeque};
use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};
use uuid::Uuid;

use super::*;

/// Payload for a sync-failure alert. Addressed to the tenant
/// admins (`audience: "role:admin"`) so `notification.requested`
/// can resolve recipients: a payload with no `user_id`, `user_ids`
/// or `audience` is rejected by the notifications module and the
/// alert is never delivered. The payload never contains a key, a
/// URL, or a response body - only ids, the country/year, and the
/// short failure reason.
pub(crate) fn sync_failure_payload(
    tenant_id: &str,
    config_id: Uuid,
    summary: &str,
    failures: &[String],
) -> Value {
    let body = if failures.is_empty() {
        summary.to_string()
    } else {
        format!("{summary}: {}", failures.join("; "))
    };
    json!({
        "tenant_id": tenant_id,
        "audience": "role:admin",
        "title": "Holiday sync failed",
        "body": body,
        "kind": "sync_failed",
        "category": "holidays",
        "module": "holidays",
        "priority": "high",
        "link": "/api/v1/holidays/sync-runs",
        "config_id": config_id.to_string(),
    })
}

/// Emit a failure alert through `notification.requested`.
fn notify_sync_failure(
    bus: &HookBus,
    tenant_id: &str,
    config_id: Uuid,
    summary: &str,
    failures: &[String],
) {
    bus.emit(
        "notification.requested",
        sync_failure_payload(tenant_id, config_id, summary, failures),
    );
}

/// Fetch and parse the holidays for one (provider, country, year).
/// The URL is validated with the SSRF rules (https only, allowlist
/// host, safe DNS) before the fetch, and every redirect target is
/// re-validated by the fetch client.
pub(crate) async fn fetch_holidays_for_provider(
    state: &HolidaysState,
    provider: &HolidayProvider,
    config: &HolidaySyncConfig,
    country: &str,
    year: i32,
    secret: &str,
) -> UnifiedResult<Vec<FetchedHoliday>> {
    let (url, allowed_hosts) = build_fetch_url(provider, config, country, year, secret)?;
    validate_provider_url(&url, &allowed_hosts)?;
    let bytes = fetch_with_retries(state, provider, &url).await?;
    match provider.kind {
        ProviderKind::NagerDate => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("Nager.Date response was not valid UTF-8")
            })?;
            parse_nager_response(&text, country, year)
        }
        ProviderKind::Calendarific => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("Calendarific response was not valid UTF-8")
            })?;
            parse_calendarific_response(&text)
        }
        ProviderKind::AbstractApi => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("Abstract API response was not valid UTF-8")
            })?;
            parse_abstract_response(&text)
        }
        ProviderKind::GoogleIcal | ProviderKind::GenericIcal => {
            let text = String::from_utf8(bytes).map_err(|_| {
                UnifiedError::internal("iCal feed was not valid UTF-8")
            })?;
            let (events, _skipped) = parse_ics(&text);
            Ok(events
                .into_iter()
                .map(|event| FetchedHoliday {
                    date: event.date,
                    name: event.summary,
                    local_name: None,
                    global: true,
                    types: Vec::new(),
                    counties: Vec::new(),
                })
                .collect())
        }
    }
}

// Sync engine.

/// Upsert fetched holidays for one (country, year) into the shared
/// store, deduplicated by (country, date, name). Returns how many
/// rows were inserted or updated. A re-sync never duplicates.
fn upsert_holidays(
    state: &HolidaysState,
    provider_id: &str,
    country: &str,
    fetched: &[FetchedHoliday],
) -> i64 {
    let now = Utc::now();
    let mut stored: i64 = 0;
    for item in fetched {
        if item.name.trim().is_empty() {
            continue;
        }
        let key = format!("{}|{}|{}", country, item.date, item.name.trim());
        let name = item.name.trim().to_string();
        match state.holidays.get_mut(&key) {
            Some(mut existing) => {
                existing.local_name = item.local_name.clone().or_else(|| existing.local_name.clone());
                existing.provider_id = provider_id.to_string();
                existing.source = "synced".to_string();
                existing.global = item.global;
                existing.types = item.types.clone();
                existing.counties = item.counties.clone();
                existing.updated_at = now;
                stored += 1;
            }
            None => {
                state.holidays.insert(
                    key,
                    Holiday {
                        id: Uuid::new_v4(),
                        country: country.to_string(),
                        date: item.date,
                        name,
                        local_name: item.local_name.clone(),
                        provider_id: provider_id.to_string(),
                        source: "synced".to_string(),
                        global: item.global,
                        types: item.types.clone(),
                        counties: item.counties.clone(),
                        created_at: now,
                        updated_at: now,
                    },
                );
                stored += 1;
            }
        }
    }
    stored
}

/// Sync one (country, year) for one config, trying the primary
/// provider first and then the fallback chain in order. Records one
/// run row per provider attempt and returns the successful
/// provider's (found, stored) counts, or the last failure.
async fn sync_country_year(
    state: &HolidaysState,
    bus: &HookBus,
    secret: &str,
    config: &HolidaySyncConfig,
    country: &str,
    year: i32,
    trigger: &str,
) -> Result<(String, i64, i64), (String, String)> {
    let mut chain: Vec<String> = vec![config.provider_id.clone()];
    chain.extend(config.fallback_provider_ids.iter().cloned());
    let mut last_error: Option<(String, String)> = None;
    for provider_id in chain {
        let Some(provider) = state.providers.get(&provider_id).map(|entry| entry.clone()) else {
            let started = Utc::now();
            let run_id = Uuid::new_v4();
            state.runs.insert(
                run_id,
                HolidaySyncRun {
                    id: run_id,
                    tenant_id: config.tenant_id.clone(),
                    company_id: config.company_id.clone(),
                    config_id: config.id,
                    provider_id: provider_id.clone(),
                    country: country.to_string(),
                    year,
                    status: "failed".to_string(),
                    trigger: trigger.to_string(),
                    holidays_found: 0,
                    holidays_stored: 0,
                    error: Some(format!("Provider \"{provider_id}\" no longer exists")),
                    started_at: started,
                    finished_at: started,
                    fetched_at: started,
                },
            );
            last_error = Some((
                provider_id.clone(),
                format!("Provider \"{provider_id}\" no longer exists"),
            ));
            continue;
        };
        if !provider.enabled {
            let started = Utc::now();
            let run_id = Uuid::new_v4();
            state.runs.insert(
                run_id,
                HolidaySyncRun {
                    id: run_id,
                    tenant_id: config.tenant_id.clone(),
                    company_id: config.company_id.clone(),
                    config_id: config.id,
                    provider_id: provider_id.clone(),
                    country: country.to_string(),
                    year,
                    status: "failed".to_string(),
                    trigger: trigger.to_string(),
                    holidays_found: 0,
                    holidays_stored: 0,
                    error: Some(format!("Provider \"{provider_id}\" is disabled")),
                    started_at: started,
                    finished_at: started,
                    fetched_at: started,
                },
            );
            last_error = Some((
                provider_id.clone(),
                format!("Provider \"{provider_id}\" is disabled"),
            ));
            continue;
        }
        let started = Utc::now();
        let outcome = fetch_holidays_for_provider(state, &provider, config, country, year, secret).await;
        let finished = Utc::now();
        match outcome {
            Ok(fetched) => {
                let found = fetched.len() as i64;
                let stored = upsert_holidays(state, &provider.id, country, &fetched);
                let run_id = Uuid::new_v4();
                state.runs.insert(
                    run_id,
                    HolidaySyncRun {
                        id: run_id,
                        tenant_id: config.tenant_id.clone(),
                        company_id: config.company_id.clone(),
                        config_id: config.id,
                        provider_id: provider.id.clone(),
                        country: country.to_string(),
                        year,
                        status: "success".to_string(),
                        trigger: trigger.to_string(),
                        holidays_found: found,
                        holidays_stored: stored,
                        error: None,
                        started_at: started,
                        finished_at: finished,
                        fetched_at: finished,
                    },
                );
                return Ok((provider.id.clone(), found, stored));
            }
            Err(error) => {
                let message = error_message_without_secrets(&error);
                let run_id = Uuid::new_v4();
                state.runs.insert(
                    run_id,
                    HolidaySyncRun {
                        id: run_id,
                        tenant_id: config.tenant_id.clone(),
                        company_id: config.company_id.clone(),
                        config_id: config.id,
                        provider_id: provider.id.clone(),
                        country: country.to_string(),
                        year,
                        status: "failed".to_string(),
                        trigger: trigger.to_string(),
                        holidays_found: 0,
                        holidays_stored: 0,
                        error: Some(message.clone()),
                        started_at: started,
                        finished_at: finished,
                        fetched_at: finished,
                    },
                );
                last_error = Some((provider.id.clone(), message));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| ("unknown".to_string(), "Sync failed for an unknown reason".to_string())))
}

/// The error message of a sync failure, with any accidental secret
/// material stripped: messages built in this module never contain a
/// key or a URL, but defence in depth truncates anything suspiciously
/// long instead of storing it verbatim.
fn error_message_without_secrets(error: &UnifiedError) -> String {
    let message = error.to_string();
    if message.len() > 500 {
        message.chars().take(500).collect()
    } else {
        message
    }
}

/// Run a full sync for one config: every selected country crossed
/// with every selected year. Returns a summary; failures are
/// collected and alerted once per config (never silently empty).
/// Previously synced holidays stay cached when a sync fails.
pub(crate) async fn run_sync_for_config(
    state: &HolidaysState,
    bus: &HookBus,
    secret: &str,
    config: &HolidaySyncConfig,
    trigger: &str,
) -> SyncSummary {
    bus.emit(
        EVENT_SYNC_STARTED,
        json!({
            "config_id": config.id,
            "tenant_id": config.tenant_id,
            "trigger": trigger,
        }),
    );
    let mut runs: Vec<Value> = Vec::new();
    let mut attempted: usize = 0;
    let mut succeeded: usize = 0;
    let mut failed: usize = 0;
    let mut holidays_found: i64 = 0;
    let mut holidays_stored: i64 = 0;
    let mut failures: Vec<String> = Vec::new();
    for country in &config.countries {
        for year in &config.years {
            attempted += 1;
            match sync_country_year(state, bus, secret, config, country, *year, trigger).await {
                Ok((provider_id, found, stored)) => {
                    succeeded += 1;
                    holidays_found += found;
                    holidays_stored += stored;
                    runs.push(json!({
                        "country": country,
                        "year": year,
                        "provider_id": provider_id,
                        "status": "success",
                        "holidays_found": found,
                        "holidays_stored": stored,
                    }));
                }
                Err((provider_id, message)) => {
                    failed += 1;
                    failures.push(format!("{country}/{year} via {provider_id}: {message}"));
                    runs.push(json!({
                        "country": country,
                        "year": year,
                        "provider_id": provider_id,
                        "status": "failed",
                        "error": message,
                    }));
                }
            }
        }
    }
    let status = if failed == 0 {
        "finished"
    } else if succeeded == 0 {
        "failed"
    } else {
        "partial"
    };
    if failed > 0 {
        notify_sync_failure(
            bus,
            &config.tenant_id,
            config.id,
            &format!(
                "Holiday sync for config {} finished with status {status} ({succeeded}/{attempted} country-years synced)",
                config.id
            ),
            &failures,
        );
        bus.emit(
            EVENT_SYNC_FAILED,
            json!({
                "config_id": config.id,
                "tenant_id": config.tenant_id,
                "trigger": trigger,
                "status": status,
                "failures": failures,
            }),
        );
    }
    bus.emit(
        EVENT_SYNC_FINISHED,
        json!({
            "config_id": config.id,
            "tenant_id": config.tenant_id,
            "trigger": trigger,
            "status": status,
            "attempted": attempted,
            "succeeded": succeeded,
            "failed": failed,
            "holidays_found": holidays_found,
            "holidays_stored": holidays_stored,
        }),
    );
    SyncSummary {
        config_id: config.id,
        tenant_id: config.tenant_id.clone(),
        trigger: trigger.to_string(),
        runs,
        attempted,
        succeeded,
        failed,
        holidays_found,
        holidays_stored,
        scheduler_status: "manual".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::hooks::HookBus;

    #[test]
    fn sync_failure_payload_addresses_tenant_admins() {
        let payload = sync_failure_payload(
            "tenant-1",
            Uuid::nil(),
            "Holiday sync finished with status failed",
            &["ID/2026 via nager-date: boom".to_string()],
        );
        // The notifications module rejects a payload with no
        // user_id, user_ids or audience; "role:admin" resolves to
        // the tenant admins instead.
        assert_eq!(
            payload.get("audience").and_then(Value::as_str),
            Some("role:admin")
        );
        assert_eq!(
            payload.get("tenant_id").and_then(Value::as_str),
            Some("tenant-1")
        );
        assert_eq!(
            payload.get("config_id").and_then(Value::as_str),
            Some(Uuid::nil().to_string().as_str())
        );
        assert!(
            payload
                .get("body")
                .and_then(Value::as_str)
                .unwrap()
                .contains("boom")
        );
    }

    #[tokio::test]
    async fn manual_sync_summary_reports_scheduler_status_manual() {
        // A config whose provider does not exist fails fast with no
        // network: the summary still carries scheduler_status
        // "manual", never "queued" - a manual sync runs inline
        // only and must not claim a scheduler job was enqueued.
        let state = HolidaysState::default();
        let bus = HookBus::new();
        let now = Utc::now();
        let config = HolidaySyncConfig {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            company_id: String::new(),
            provider_id: "missing-provider".to_string(),
            fallback_provider_ids: Vec::new(),
            countries: vec!["ID".to_string()],
            years: vec![2026],
            schedule: DEFAULT_SCHEDULE.to_string(),
            feed_url: None,
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let summary = run_sync_for_config(&state, &bus, "", &config, "manual").await;
        assert_eq!(summary.scheduler_status, "manual");
        assert_eq!(summary.attempted, 1);
        assert_eq!(summary.succeeded, 0);
        assert_eq!(summary.failed, 1);
    }
}
