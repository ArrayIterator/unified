//! HTTP handlers: countries.

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
// Handlers: countries (platform reference data).
// ---------------------------------------------------------------------------

pub(crate) async fn list_countries(
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
            .countries
            .iter()
            .map(|entry| {
                json!({
                    "code": entry.code,
                    "name": entry.name,
                    "enabled": entry.enabled,
                    "created_at": entry.created_at.to_rfc3339(),
                    "updated_at": entry.updated_at.to_rfc3339(),
                })
            })
            .collect();
        if let Some(search) = params.search.as_deref().filter(|text| !text.trim().is_empty()) {
            let needle = search.trim().to_ascii_lowercase();
            items.retain(|item| {
                item.get("code")
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
        items.sort_by(|a, b| {
            a.get("code")
                .and_then(Value::as_str)
                .cmp(&b.get("code").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_country(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateCountryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let code = normalize_country_code(&body.code)?;
        if state.countries.contains_key(&code) {
            return Err(UnifiedError::conflict(format!(
                "Country \"{code}\" already exists"
            )));
        }
        let name = body.name.trim().to_string();
        if name.is_empty() || name.len() > 100 {
            return Err(UnifiedError::bad_request(
                "Country name must be 1-100 characters",
            ));
        }
        let now = Utc::now();
        let country = HolidayCountry {
            code: code.clone(),
            name: name.clone(),
            enabled: body.enabled.unwrap_or(true),
            created_at: now,
            updated_at: now,
        };
        state.countries.insert(code.clone(), country);
        Ok(json!({
            "code": code,
            "name": name,
            "enabled": body.enabled.unwrap_or(true),
            "created_at": now.to_rfc3339(),
            "updated_at": now.to_rfc3339(),
        }))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_country(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateCountryRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let code = normalize_country_code(&path.into_inner())?;
        let body = body.into_inner();
        let Some(mut country) = state.countries.get(&code).map(|entry| entry.clone()) else {
            return Err(not_found_for_tenant("Country"));
        };
        if let Some(name) = body.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 100 {
                return Err(UnifiedError::bad_request(
                    "Country name must be 1-100 characters",
                ));
            }
            country.name = name;
        }
        if let Some(enabled) = body.enabled {
            country.enabled = enabled;
        }
        country.updated_at = Utc::now();
        let view = json!({
            "code": country.code,
            "name": country.name,
            "enabled": country.enabled,
            "created_at": country.created_at.to_rfc3339(),
            "updated_at": country.updated_at.to_rfc3339(),
        });
        state.countries.insert(code, country);
        Ok(view)
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn delete_country(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = platform_caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let code = normalize_country_code(&path.into_inner())?;
        if !state.countries.contains_key(&code) {
            return Err(not_found_for_tenant("Country"));
        }
        let referenced = state
            .configs
            .iter()
            .any(|entry| entry.countries.contains(&code));
        if referenced {
            return Err(UnifiedError::conflict(
                "Country is selected by a sync config; remove it from the config first",
            ));
        }
        state.countries.remove(&code);
        Ok(json!({ "deleted": true, "code": code }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}
