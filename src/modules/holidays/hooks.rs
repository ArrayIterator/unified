//! Hook-bus handlers: answering requests from other modules.

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

// Hook handlers.

/// Answer one `holidays.is_holiday` request. The request MUST carry
/// `tenant_id`: without it this handler adds no result (fail closed
/// - the caller falls back to weekends only, as the calendar module
/// documents). The answer is scoped to the tenant's enabled configs:
/// a country the tenant never selected answers
/// `source: "not_configured"`. A manual override wins over a synced
/// holiday on the same date.
pub(crate) fn on_is_holiday_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(date_text) = ctx.get("date").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(country_raw) = ctx.get("country").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let Ok(date) = NaiveDate::parse_from_str(date_text.trim(), "%Y-%m-%d") else {
        return;
    };
    let Ok(country) = normalize_country_code(&country_raw) else {
        return;
    };
    let answer = answer_is_holiday(&state, &tenant_id, &country, date);
    ctx.add_result(answer);
}

/// Shared single-day answer logic for the hook and the HTTP endpoint.
pub(crate) fn answer_is_holiday(
    state: &HolidaysState,
    tenant_id: &str,
    country: &str,
    date: NaiveDate,
) -> Value {
    let selected = state.tenant_countries(tenant_id);
    if !selected.contains(country) {
        return json!({
            "date": date.to_string(),
            "country": country,
            "is_holiday": false,
            "name": Value::Null,
            "source": "not_configured",
            "provider": Value::Null,
        });
    }
    if let Some(override_entry) = state.tenant_override(tenant_id, country, date) {
        if override_entry.override_kind == "working_day" {
            return json!({
                "date": date.to_string(),
                "country": country,
                "is_holiday": false,
                "name": Value::Null,
                "source": "manual",
                "provider": "manual",
            });
        }
        return json!({
            "date": date.to_string(),
            "country": country,
            "is_holiday": true,
            "name": override_entry.name,
            "source": "manual",
            "provider": "manual",
        });
    }
    if let Some(holiday) = state.synced_on(country, date) {
        return json!({
            "date": date.to_string(),
            "country": country,
            "is_holiday": true,
            "name": holiday.name,
            "source": "synced",
            "provider": holiday.provider_id,
        });
    }
    json!({
        "date": date.to_string(),
        "country": country,
        "is_holiday": false,
        "name": Value::Null,
        "source": "synced",
        "provider": Value::Null,
    })
}

/// Answer one `holidays.non_working_days` request. Same tenant
/// scoping as `on_is_holiday_requested`: no `tenant_id` means no
/// result. The range is clamped to MAX_HOOK_RANGE_DAYS. Overrides
/// win over synced holidays on the same date; a `working_day`
/// override removes that date from the answer.
pub(crate) fn on_non_working_days_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(start_text) = ctx.get("start").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(end_text) = ctx.get("end").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(country_raw) = ctx.get("country").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let Ok(start) = NaiveDate::parse_from_str(start_text.trim(), "%Y-%m-%d") else {
        return;
    };
    let Ok(end) = NaiveDate::parse_from_str(end_text.trim(), "%Y-%m-%d") else {
        return;
    };
    let Ok(country) = normalize_country_code(&country_raw) else {
        return;
    };
    let (start, end) = if end < start { (end, start) } else { (start, end) };
    let span = end.signed_duration_since(start).num_days();
    let end = if span > MAX_HOOK_RANGE_DAYS {
        start + chrono::Duration::days(MAX_HOOK_RANGE_DAYS)
    } else {
        end
    };
    let selected = state.tenant_countries(&tenant_id);
    if !selected.contains(&country) {
        ctx.add_result(json!({
            "start": start.to_string(),
            "end": end.to_string(),
            "country": country,
            "in_scope": false,
            "dates": [],
        }));
        return;
    }
    let mut dates: Vec<Value> = Vec::new();
    let mut day = start;
    while day <= end {
        if let Some(override_entry) = state.tenant_override(&tenant_id, &country, day) {
            if override_entry.override_kind != "working_day" {
                dates.push(json!({
                    "date": day.to_string(),
                    "name": override_entry.name,
                    "source": "manual",
                    "provider": "manual",
                }));
            }
        } else {
            let mut day_holidays: Vec<Holiday> = state
                .holidays
                .iter()
                .filter(|entry| entry.country == country && entry.date == day)
                .map(|entry| entry.clone())
                .collect();
            day_holidays.sort_by(|a, b| a.name.cmp(&b.name));
            for holiday in day_holidays {
                dates.push(json!({
                    "date": day.to_string(),
                    "name": holiday.name,
                    "source": "synced",
                    "provider": holiday.provider_id,
                }));
            }
        }
        day += chrono::Duration::days(1);
    }
    ctx.add_result(json!({
        "start": start.to_string(),
        "end": end.to_string(),
        "country": country,
        "in_scope": true,
        "dates": dates,
    }));
}

/// Scheduler listener for `holidays.sync`: payload
/// {"config_id", "tenant_id"}. Runs the sync in a spawned task so
/// the synchronous hook bus never blocks on network I/O. When no
/// async runtime is available, the request is recorded as a failed
/// run instead of being silently dropped. A config whose tenant id
/// does not match the payload is ignored (no run, no leak).
pub(crate) fn on_sync_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(config_id_text) = ctx.get("config_id").and_then(Value::as_str).map(str::to_string) else {
        return;
    };
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let Ok(config_id) = Uuid::parse_str(&config_id_text) else {
        return;
    };
    let Some(config) = state.configs.get(&config_id).map(|entry| entry.clone()) else {
        return;
    };
    if config.tenant_id != tenant_id || !config.enabled {
        return;
    }
    // No secret configured means an empty secret: keyless
    // providers sync fine, and a sync needing a stored key fails
    // closed in `provider_key`.
    let secret = state.secret().unwrap_or_default();
    let Some(bus) = state.bus() else {
        return;
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            let task_state = state.clone();
            let task_bus = bus.clone();
            handle.spawn(async move {
                run_sync_for_config(&task_state, &task_bus, &secret, &config, "scheduled").await;
            });
        }
        Err(_) => {
            let now = Utc::now();
            let run_id = Uuid::new_v4();
            state.runs.insert(
                run_id,
                HolidaySyncRun {
                    id: run_id,
                    tenant_id: config.tenant_id.clone(),
                    company_id: config.company_id.clone(),
                    config_id: config.id,
                    provider_id: config.provider_id.clone(),
                    country: config.countries.first().cloned().unwrap_or_default(),
                    year: config.years.first().copied().unwrap_or(0),
                    status: "failed".to_string(),
                    trigger: "scheduled".to_string(),
                    holidays_found: 0,
                    holidays_stored: 0,
                    error: Some("No async runtime available to run the scheduled sync".to_string()),
                    started_at: now,
                    finished_at: now,
                    fetched_at: now,
                },
            );
        }
    }
}

/// Scheduler listener for `holidays.sync_all`: payload
/// {"tenant_id"}. Syncs every enabled config of that tenant, each
/// in its own spawned task.
pub(crate) fn on_sync_all_requested(state: Arc<HolidaysState>, ctx: &mut HookContext) {
    let Some(tenant_id) = ctx
        .get("tenant_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
    else {
        return;
    };
    // The tenant id must be a valid UUID (the tenancy schema name
    // derives from it): an unparseable id gets no answer, fail closed.
    if schema_name_for_str(&tenant_id).is_err() {
        return;
    }
    let configs: Vec<HolidaySyncConfig> = state
        .configs
        .iter()
        .filter(|entry| entry.tenant_id == tenant_id && entry.enabled)
        .map(|entry| entry.clone())
        .collect();
    if configs.is_empty() {
        return;
    }
    // No secret configured means an empty secret: keyless
    // providers sync fine, and a sync needing a stored key fails
    // closed in `provider_key`.
    let secret = state.secret().unwrap_or_default();
    let Some(bus) = state.bus() else {
        return;
    };
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            for config in configs {
                let task_state = state.clone();
                let task_bus = bus.clone();
                let task_secret = secret.clone();
                handle.spawn(async move {
                    run_sync_for_config(&task_state, &task_bus, &task_secret, &config, "sync_all").await;
                });
            }
        }
        Err(_) => {
            let now = Utc::now();
            for config in configs {
                let run_id = Uuid::new_v4();
                state.runs.insert(
                    run_id,
                    HolidaySyncRun {
                        id: run_id,
                        tenant_id: config.tenant_id.clone(),
                        company_id: config.company_id.clone(),
                        config_id: config.id,
                        provider_id: config.provider_id.clone(),
                        country: config.countries.first().cloned().unwrap_or_default(),
                        year: config.years.first().copied().unwrap_or(0),
                        status: "failed".to_string(),
                        trigger: "sync_all".to_string(),
                        holidays_found: 0,
                        holidays_stored: 0,
                        error: Some("No async runtime available to run the scheduled sync".to_string()),
                        started_at: now,
                        finished_at: now,
                        fetched_at: now,
                    },
                );
            }
        }
    }
}
