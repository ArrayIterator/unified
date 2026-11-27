//! HTTP handlers: sync configs and sync runs.

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

// Handlers: sync configs (tenant business data).

fn load_config_for_tenant(
    state: &HolidaysState,
    caller: &Caller,
    config_id: Uuid,
) -> UnifiedResult<HolidaySyncConfig> {
    let Some(config) = state.configs.get(&config_id).map(|entry| entry.clone()) else {
        return Err(not_found_for_tenant("Sync config"));
    };
    if config.tenant_id != caller.tenant_id {
        return Err(not_found_for_tenant("Sync config"));
    }
    Ok(config)
}

pub(crate) async fn list_configs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .configs
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .map(|entry| entry.public_view())
            .collect();
        if let Some(provider_id) = params.filters.get("provider_id") {
            items.retain(|item| {
                item.get("provider_id").and_then(Value::as_str) == Some(provider_id.as_str())
            });
        }
        if let Some(country) = params.filters.get("country") {
            let wanted = country.trim().to_ascii_uppercase();
            items.retain(|item| {
                item.get("countries")
                    .and_then(Value::as_array)
                    .is_some_and(|countries| {
                        countries
                            .iter()
                            .filter_map(Value::as_str)
                            .any(|code| code == wanted)
                    })
            });
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled.trim() == "true";
            items.retain(|item| {
                item.get("enabled").and_then(Value::as_bool).unwrap_or(false) == wanted
            });
        }
        items.sort_by(|a, b| {
            a.get("created_at")
                .and_then(Value::as_str)
                .cmp(&b.get("created_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateConfigRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let provider_id = body
            .provider_id
            .as_deref()
            .unwrap_or(DEFAULT_PROVIDER_ID)
            .trim()
            .to_string();
        let provider = load_enabled_provider(&state, &provider_id)?;
        let fallback_provider_ids = validate_fallback_chain(
            &state,
            &provider_id,
            body.fallback_provider_ids.as_deref().unwrap_or(&[]),
        )?;
        let countries = validate_config_countries(&state, &body.countries)?;
        let years = match body.years {
            Some(years) => validate_years(&years)?,
            None => default_years(),
        };
        let schedule = validate_schedule(body.schedule.as_deref().unwrap_or(DEFAULT_SCHEDULE))?;
        let feed_url = match body.feed_url {
            Some(raw) => {
                if provider.kind != ProviderKind::GenericIcal {
                    return Err(UnifiedError::bad_request(
                        "feed_url only applies to the generic_ical provider",
                    ));
                }
                let url = raw.trim().to_string();
                if url.is_empty() {
                    return Err(UnifiedError::bad_request(
                        "feed_url must not be empty for the generic_ical provider",
                    ));
                }
                Some(url)
            }
            None => None,
        };
        if provider.kind == ProviderKind::GenericIcal && feed_url.is_none() {
            return Err(UnifiedError::bad_request(
                "The generic_ical provider needs a feed_url on the sync config",
            ));
        }
        if let Some(url) = feed_url.as_deref() {
            let parsed = url::Url::parse(url)
                .map_err(|_| UnifiedError::bad_request("feed_url is invalid"))?;
            let Some(host) = parsed.host_str() else {
                return Err(UnifiedError::bad_request("feed_url must have a host"));
            };
            validate_provider_url(url, &[host.to_string()])?;
        }
        let now = Utc::now();
        let config = HolidaySyncConfig {
            id: Uuid::new_v4(),
            tenant_id: caller.tenant_id.clone(),
            company_id: caller.company_id.clone(),
            provider_id: provider.id,
            fallback_provider_ids,
            countries,
            years,
            schedule,
            feed_url,
            enabled: body.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        let view = config.public_view();
        state.configs.insert(config.id, config.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_CONFIG_CREATED,
                json!({
                    "config_id": config.id,
                    "tenant_id": config.tenant_id,
                    "provider_id": config.provider_id,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let config = load_config_for_tenant(&state, &caller, config_id)?;
        Ok(config.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateConfigRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let mut config = load_config_for_tenant(&state, &caller, config_id)?;
        let body = body.into_inner();
        if let Some(provider_id) = body.provider_id {
            let provider_id = provider_id.trim().to_string();
            let provider = load_enabled_provider(&state, &provider_id)?;
            config.provider_id = provider.id;
            config.fallback_provider_ids.retain(|id| *id != config.provider_id);
            if provider.kind != ProviderKind::GenericIcal {
                config.feed_url = None;
            }
        }
        if let Some(fallback_ids) = body.fallback_provider_ids {
            config.fallback_provider_ids =
                validate_fallback_chain(&state, &config.provider_id, &fallback_ids)?;
        }
        if let Some(countries) = body.countries {
            config.countries = validate_config_countries(&state, &countries)?;
        }
        if let Some(years) = body.years {
            config.years = validate_years(&years)?;
        }
        if let Some(schedule) = body.schedule {
            config.schedule = validate_schedule(&schedule)?;
        }
        if let Some(feed_url) = body.feed_url {
            let provider = load_enabled_provider(&state, &config.provider_id)?;
            if feed_url.trim().is_empty() {
                config.feed_url = None;
            } else {
                if provider.kind != ProviderKind::GenericIcal {
                    return Err(UnifiedError::bad_request(
                        "feed_url only applies to the generic_ical provider",
                    ));
                }
                let url = feed_url.trim().to_string();
                let parsed = url::Url::parse(&url)
                    .map_err(|_| UnifiedError::bad_request("feed_url is invalid"))?;
                let Some(host) = parsed.host_str() else {
                    return Err(UnifiedError::bad_request("feed_url must have a host"));
                };
                validate_provider_url(&url, &[host.to_string()])?;
                config.feed_url = Some(url);
            }
        }
        let provider = load_enabled_provider(&state, &config.provider_id)?;
        if provider.kind == ProviderKind::GenericIcal && config.feed_url.is_none() {
            return Err(UnifiedError::bad_request(
                "The generic_ical provider needs a feed_url on the sync config",
            ));
        }
        if let Some(enabled) = body.enabled {
            config.enabled = enabled;
        }
        config.updated_at = Utc::now();
        let view = config.public_view();
        state.configs.insert(config.id, config.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_CONFIG_UPDATED,
                json!({
                    "config_id": config.id,
                    "tenant_id": config.tenant_id,
                    "provider_id": config.provider_id,
                }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let config = load_config_for_tenant(&state, &caller, config_id)?;
        state.configs.remove(&config.id);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_CONFIG_DELETED,
                json!({
                    "config_id": config.id,
                    "tenant_id": config.tenant_id,
                }),
            );
        }
        Ok(json!({ "deleted": true, "config_id": config.id }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// Handlers: sync triggers and runs (tenant business data).

pub(crate) async fn sync_config(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_SYNC)?;
        let config_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid config id"))?;
        let config = load_config_for_tenant(&state, &caller, config_id)?;
        if !config.enabled {
            return Err(UnifiedError::bad_request(
                "Sync config is disabled; enable it before syncing",
            ));
        }
        // Manual syncs run inline only: the run below is the one
        // and only sync. Enqueueing a scheduler job as well would
        // run the same sync a second time via the
        // `holidays.sync_requested` listener, so no job is enqueued
        // here and the summary reports scheduler_status "manual".
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let bus = ctx.hooks.clone();
        let summary = run_sync_for_config(&state, &bus, &secret, &config, "manual").await;
        Ok::<Value, UnifiedError>(json!({
            "config_id": summary.config_id,
            "tenant_id": summary.tenant_id,
            "trigger": summary.trigger,
            "attempted": summary.attempted,
            "succeeded": summary.succeeded,
            "failed": summary.failed,
            "holidays_found": summary.holidays_found,
            "holidays_stored": summary.holidays_stored,
            "scheduler_status": summary.scheduler_status,
            "runs": summary.runs,
        }))
    })()
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn sync_all_configs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_SYNC)?;
        let configs: Vec<HolidaySyncConfig> = state
            .configs
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id && entry.enabled)
            .map(|entry| entry.clone())
            .collect();
        // Manual syncs run inline only (see `sync_config`): no
        // scheduler job is enqueued, so the same configs are never
        // synced a second time via `holidays.sync_all_requested`.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let bus = ctx.hooks.clone();
        let mut summaries: Vec<Value> = Vec::new();
        let mut attempted: usize = 0;
        let mut succeeded: usize = 0;
        let mut failed: usize = 0;
        let mut holidays_found: i64 = 0;
        let mut holidays_stored: i64 = 0;
        for config in configs {
            let summary = run_sync_for_config(&state, &bus, &secret, &config, "sync_all").await;
            attempted += summary.attempted;
            succeeded += summary.succeeded;
            failed += summary.failed;
            holidays_found += summary.holidays_found;
            holidays_stored += summary.holidays_stored;
            summaries.push(json!({
                "config_id": summary.config_id,
                "trigger": summary.trigger,
                "attempted": summary.attempted,
                "succeeded": summary.succeeded,
                "failed": summary.failed,
                "holidays_found": summary.holidays_found,
                "holidays_stored": summary.holidays_stored,
                "scheduler_status": summary.scheduler_status,
                "runs": summary.runs,
            }));
        }
        Ok::<Value, UnifiedError>(json!({
            "tenant_id": caller.tenant_id,
            "trigger": "sync_all",
            "configs_synced": summaries.len(),
            "attempted": attempted,
            "succeeded": succeeded,
            "failed": failed,
            "holidays_found": holidays_found,
            "holidays_stored": holidays_stored,
            "scheduler_status": "manual",
            "summaries": summaries,
        }))
    })()
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn list_sync_runs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .runs
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .map(|entry| entry.public_view())
            .collect();
        if let Some(config_id) = params.filters.get("config_id") {
            items.retain(|item| {
                item.get("config_id").and_then(Value::as_str) == Some(config_id.as_str())
            });
        }
        if let Some(provider_id) = params.filters.get("provider_id") {
            items.retain(|item| {
                item.get("provider_id").and_then(Value::as_str) == Some(provider_id.as_str())
            });
        }
        if let Some(country) = params.filters.get("country") {
            let wanted = country.trim().to_ascii_uppercase();
            items.retain(|item| {
                item.get("country").and_then(Value::as_str) == Some(wanted.as_str())
            });
        }
        if let Some(year) = params.filters.get("year") {
            items.retain(|item| {
                item.get("year").and_then(Value::as_i64)
                    == year.trim().parse::<i64>().ok()
            });
        }
        if let Some(status) = params.filters.get("status") {
            items.retain(|item| item.get("status").and_then(Value::as_str) == Some(status.as_str()));
        }
        items.sort_by(|a, b| {
            b.get("started_at")
                .and_then(Value::as_str)
                .cmp(&a.get("started_at").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_sync_run(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let run_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid sync run id"))?;
        let Some(run) = state.runs.get(&run_id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Sync run"));
        };
        if run.tenant_id != caller.tenant_id {
            return Err(not_found_for_tenant("Sync run"));
        }
        Ok(run.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}
