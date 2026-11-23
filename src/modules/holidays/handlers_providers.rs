//! HTTP handlers: providers.

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

// ---------------------------------------------------------------------------
// Handlers: providers (platform reference data).
// ---------------------------------------------------------------------------

pub(crate) async fn list_providers(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<Value>> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let params = query.into_inner();
        let mut items: Vec<Value> = state
            .providers
            .iter()
            .map(|entry| entry.public_view())
            .collect();
        if let Some(search) = params.search.as_deref().filter(|text| !text.trim().is_empty()) {
            let needle = search.trim().to_ascii_lowercase();
            items.retain(|item| {
                item.get("id")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
                    || item
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
            });
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled.trim() == "true";
            items.retain(|item| {
                item.get("enabled").and_then(Value::as_bool).unwrap_or(false) == wanted
            });
        }
        if let Some(kind) = params.filters.get("kind") {
            items.retain(|item| item.get("kind").and_then(Value::as_str) == Some(kind.as_str()));
        }
        items.sort_by(|a, b| {
            a.get("id")
                .and_then(Value::as_str)
                .cmp(&b.get("id").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let id = validate_provider_id(&body.id)?;
        if state.providers.contains_key(&id) {
            return Err(UnifiedError::conflict(format!(
                "Provider \"{id}\" already exists"
            )));
        }
        let Some(kind) = ProviderKind::from_str(body.kind.trim()) else {
            return Err(UnifiedError::bad_request(format!(
                "Unknown provider kind \"{}\" (nager_date, calendarific, abstract_api, google_ical, generic_ical)",
                body.kind
            )));
        };
        let name = body.name.trim().to_string();
        if name.is_empty() || name.len() > 100 {
            return Err(UnifiedError::bad_request(
                "Provider name must be 1-100 characters",
            ));
        }
        let base_url = body.base_url.trim().to_string();
        if kind == ProviderKind::GenericIcal {
            if !base_url.is_empty() {
                return Err(UnifiedError::bad_request(
                    "A generic_ical provider has no base URL: the feed URL lives on the sync config",
                ));
            }
        } else if base_url.is_empty() || base_url.len() > 500 {
            return Err(UnifiedError::bad_request(
                "Provider base URL must be 1-500 characters",
            ));
        }
        let mut allowed_hosts: Vec<String> = body
            .allowed_hosts
            .iter()
            .map(|host| host.trim().to_ascii_lowercase())
            .filter(|host| !host.is_empty())
            .collect();
        allowed_hosts.sort();
        allowed_hosts.dedup();
        if kind != ProviderKind::GenericIcal && allowed_hosts.is_empty() {
            return Err(UnifiedError::bad_request(
                "Provider allowed_hosts must list at least one host",
            ));
        }
        if kind != ProviderKind::GenericIcal {
            let probe = format!("{}/", base_url.trim_end_matches('/'));
            validate_provider_url(&probe, &allowed_hosts).map_err(|_| {
                UnifiedError::bad_request(
                    "Provider base URL host is not on allowed_hosts, is not https, or resolves to a private address",
                )
            })?;
        }
        let api_key_required = body.api_key_required.unwrap_or_else(|| kind.key_required());
        if api_key_required && !kind.key_required() {
            return Err(UnifiedError::bad_request(
                "Only calendarific and abstract_api providers take an API key",
            ));
        }
        let min_interval_secs = body.min_interval_secs.unwrap_or(1);
        if min_interval_secs > 3600 {
            return Err(UnifiedError::bad_request(
                "min_interval_secs must be between 0 and 3600",
            ));
        }
        let max_requests_per_minute = body.max_requests_per_minute.unwrap_or(60);
        if !(1..=1000).contains(&max_requests_per_minute) {
            return Err(UnifiedError::bad_request(
                "max_requests_per_minute must be between 1 and 1000",
            ));
        }
        let mut calendar_ids: HashMap<String, String> = HashMap::new();
        if let Some(ids) = body.calendar_ids {
            if kind != ProviderKind::GoogleIcal {
                return Err(UnifiedError::bad_request(
                    "calendar_ids only apply to the google_ical provider",
                ));
            }
            for (raw_code, calendar_id) in ids {
                let code = normalize_country_code(&raw_code)?;
                let calendar_id = calendar_id.trim().to_string();
                if calendar_id.is_empty() || calendar_id.len() > 200 {
                    return Err(UnifiedError::bad_request(
                        "Google calendar id must be 1-200 characters",
                    ));
                }
                calendar_ids.insert(code, calendar_id);
            }
        }
        let now = Utc::now();
        let mut provider = HolidayProvider {
            id: id.clone(),
            name,
            kind,
            base_url,
            allowed_hosts,
            api_key_required,
            api_key_encrypted: None,
            key_fingerprint: None,
            min_interval_secs,
            max_requests_per_minute,
            calendar_ids,
            enabled: body.enabled.unwrap_or(true),
            builtin: false,
            created_at: now,
            updated_at: now,
        };
        if let Some(api_key) = body.api_key.as_deref().filter(|key| !key.is_empty()) {
            if !kind.key_required() {
                return Err(UnifiedError::bad_request(
                    "This provider kind does not take an API key",
                ));
            }
            let secret = encryption_secret(&ctx)?;
            provider.api_key_encrypted = Some(encrypt_api_key(&secret, api_key));
            provider.key_fingerprint = Some(key_fingerprint(api_key));
        } else if api_key_required {
            return Err(UnifiedError::bad_request(
                "This provider needs an API key: pass api_key once (write-only, stored encrypted)",
            ));
        }
        let view = provider.public_view();
        state.providers.insert(id.clone(), provider);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_PROVIDER_CREATED,
                json!({ "provider_id": id, "kind": kind.as_str() }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let id = path.into_inner();
        let Some(provider) = state.providers.get(&id) else {
            return Err(not_found_for_tenant("Provider"));
        };
        Ok(provider.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let id = path.into_inner();
        let body = body.into_inner();
        let Some(mut provider) = state.providers.get(&id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Provider"));
        };
        if let Some(name) = body.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "Provider name must be 1-100 characters",
                ));
            }
            provider.name = name;
        }
        if let Some(hosts) = body.allowed_hosts {
            let mut allowed_hosts: Vec<String> = hosts
                .iter()
                .map(|host| host.trim().to_ascii_lowercase())
                .filter(|host| !host.is_empty())
                .collect();
            allowed_hosts.sort();
            allowed_hosts.dedup();
            if provider.kind != ProviderKind::GenericIcal && allowed_hosts.is_empty() {
                return Err(UnifiedError::bad_request(
                    "Provider allowed_hosts must list at least one host",
                ));
            }
            if provider.kind != ProviderKind::GenericIcal && !provider.base_url.is_empty() {
                let probe = format!("{}/", provider.base_url.trim_end_matches('/'));
                validate_provider_url(&probe, &allowed_hosts).map_err(|_| {
                    UnifiedError::bad_request(
                        "Provider base URL host is not on allowed_hosts, is not https, or resolves to a private address",
                    )
                })?;
            }
            provider.allowed_hosts = allowed_hosts;
        }
        if let Some(min_interval_secs) = body.min_interval_secs {
            if min_interval_secs > 3600 {
                return Err(UnifiedError::bad_request(
                    "min_interval_secs must be between 0 and 3600",
                ));
            }
            provider.min_interval_secs = min_interval_secs;
        }
        if let Some(max_requests_per_minute) = body.max_requests_per_minute {
            if !(1..=1000).contains(&max_requests_per_minute) {
                return Err(UnifiedError::bad_request(
                    "max_requests_per_minute must be between 1 and 1000",
                ));
            }
            provider.max_requests_per_minute = max_requests_per_minute;
        }
        if let Some(ids) = body.calendar_ids {
            if provider.kind != ProviderKind::GoogleIcal {
                return Err(UnifiedError::bad_request(
                    "calendar_ids only apply to the google_ical provider",
                ));
            }
            let mut calendar_ids: HashMap<String, String> = HashMap::new();
            for (raw_code, calendar_id) in ids {
                let code = normalize_country_code(&raw_code)?;
                let calendar_id = calendar_id.trim().to_string();
                if calendar_id.is_empty() || calendar_id.len() > 200 {
                    return Err(UnifiedError::bad_request(
                        "Google calendar id must be 1-200 characters",
                    ));
                }
                calendar_ids.insert(code, calendar_id);
            }
            provider.calendar_ids = calendar_ids;
        }
        if let Some(api_key) = body.api_key {
            if !provider.kind.key_required() {
                return Err(UnifiedError::bad_request(
                    "This provider kind does not take an API key",
                ));
            }
            if api_key.is_empty() {
                provider.api_key_encrypted = None;
                provider.key_fingerprint = None;
            } else {
                let secret = encryption_secret(&ctx)?;
                provider.api_key_encrypted = Some(encrypt_api_key(&secret, &api_key));
                provider.key_fingerprint = Some(key_fingerprint(&api_key));
            }
        }
        if let Some(enabled) = body.enabled {
            provider.enabled = enabled;
        }
        provider.updated_at = Utc::now();
        let view = provider.public_view();
        state.providers.insert(id.clone(), provider);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_PROVIDER_UPDATED,
                json!({ "provider_id": id }),
            );
        }
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let id = path.into_inner();
        let Some(provider) = state.providers.get(&id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Provider"));
        };
        if provider.builtin {
            return Err(UnifiedError::conflict(
                "Built-in providers cannot be deleted; disable them instead",
            ));
        }
        let referenced = state.configs.iter().any(|entry| {
            entry.provider_id == id || entry.fallback_provider_ids.contains(&id)
        });
        if referenced {
            return Err(UnifiedError::conflict(
                "Provider is referenced by a sync config; remove it from the config first",
            ));
        }
        state.providers.remove(&id);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_PROVIDER_DELETED,
                json!({ "provider_id": id }),
            );
        }
        Ok(json!({ "deleted": true, "provider_id": id }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn test_provider(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TestProviderRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result = (|| async {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_SYNC)?;
        let id = path.into_inner();
        let body = body.into_inner();
        let Some(provider) = state.providers.get(&id).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Provider"));
        };
        if !provider.enabled {
            return Err(UnifiedError::bad_request(format!(
                "Provider \"{id}\" is disabled"
            )));
        }
        let country = match body.country {
            Some(raw) => normalize_country_code(&raw)?,
            None => "ID".to_string(),
        };
        let year = body.year.unwrap_or_else(|| Utc::now().year());
        if !(MIN_SYNC_YEAR..=MAX_SYNC_YEAR).contains(&year) {
            return Err(UnifiedError::bad_request(format!(
                "Year must be between {MIN_SYNC_YEAR} and {MAX_SYNC_YEAR}"
            )));
        }
        // A throwaway config carries the test feed URL for a
        // generic_ical provider; it is never stored.
        let probe_config = HolidaySyncConfig {
            id: Uuid::new_v4(),
            tenant_id: String::new(),
            company_id: String::new(),
            provider_id: provider.id.clone(),
            fallback_provider_ids: Vec::new(),
            countries: vec![country.clone()],
            years: vec![year],
            schedule: DEFAULT_SCHEDULE.to_string(),
            feed_url: body.feed_url,
            enabled: true,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        // A keyed provider fails closed here when no secret is
        // configured (see `provider_key`); keyless providers test
        // fine without one.
        let secret = encryption_secret(&ctx).unwrap_or_default();
        let started = Utc::now();
        let fetched = fetch_holidays_for_provider(
            &state,
            &provider,
            &probe_config,
            &country,
            year,
            &secret,
        )
        .await?;
        let finished = Utc::now();
        Ok::<Value, UnifiedError>(json!({
            "provider_id": provider.id,
            "kind": provider.kind.as_str(),
            "country": country,
            "year": year,
            "reachable": true,
            "holidays_found": fetched.len(),
            "sample": fetched.iter().take(3).map(|item| json!({
                "date": item.date.to_string(),
                "name": item.name,
            })).collect::<Vec<Value>>(),
            "started_at": started.to_rfc3339(),
            "finished_at": finished.to_rfc3339(),
        }))
    })()
    .await;
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}
