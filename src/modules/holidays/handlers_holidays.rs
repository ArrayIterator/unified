//! HTTP handlers: holidays and overrides.

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

// Handlers: holidays (shared reference data, tenant-scoped reads).

pub(crate) async fn list_holidays(
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
        // A tenant only ever sees the countries its configs select.
        let selected = state.tenant_countries(&caller.tenant_id);
        let mut items: Vec<Value> = state
            .holidays
            .iter()
            .filter(|entry| selected.contains(&entry.country))
            .map(|entry| entry.public_view())
            .collect();
        if let Some(country) = params.filters.get("country") {
            let wanted = normalize_country_code(country)?;
            if !selected.contains(&wanted) {
                return Err(not_found_for_tenant("Country"));
            }
            items.retain(|item| item.get("country").and_then(Value::as_str) == Some(wanted.as_str()));
        }
        if let Some(year) = params.filters.get("year") {
            let wanted = year
                .trim()
                .parse::<i32>()
                .map_err(|_| UnifiedError::bad_request("Year filter must be a number"))?;
            items.retain(|item| {
                item.get("date")
                    .and_then(Value::as_str)
                    .and_then(|date| date.get(..4))
                    .and_then(|text| text.parse::<i32>().ok())
                    == Some(wanted)
            });
        }
        if let Some(source) = params.filters.get("source") {
            items.retain(|item| item.get("source").and_then(Value::as_str) == Some(source.as_str()));
        }
        if let Some(provider_id) = params.filters.get("provider_id") {
            items.retain(|item| {
                item.get("provider_id").and_then(Value::as_str) == Some(provider_id.as_str())
            });
        }
        if let Some(search) = params.search.as_deref().filter(|text| !text.trim().is_empty()) {
            let needle = search.trim().to_ascii_lowercase();
            items.retain(|item| {
                item.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|text| text.to_ascii_lowercase().contains(&needle))
            });
        }
        items.sort_by(|a, b| {
            a.get("date")
                .and_then(Value::as_str)
                .cmp(&b.get("date").and_then(Value::as_str))
                .then_with(|| {
                    a.get("name")
                        .and_then(Value::as_str)
                        .cmp(&b.get("name").and_then(Value::as_str))
                })
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_is_holiday(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<IsHolidayQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let query = query.into_inner();
        let date = parse_date(&query.date)?;
        let country = normalize_country_code(&query.country)?;
        Ok(answer_is_holiday(&state, &caller.tenant_id, &country, date))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn get_non_working_days(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    query: web::Query<NonWorkingDaysQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let query = query.into_inner();
        let start = parse_date(&query.start)?;
        let end = parse_date(&query.end)?;
        let country = normalize_country_code(&query.country)?;
        let (start, end) = if end < start { (end, start) } else { (start, end) };
        let span = end.signed_duration_since(start).num_days();
        if span > MAX_NON_WORKING_RANGE_DAYS {
            return Err(UnifiedError::bad_request(format!(
                "Date range must span at most {MAX_NON_WORKING_RANGE_DAYS} days"
            )));
        }
        let selected = state.tenant_countries(&caller.tenant_id);
        if !selected.contains(&country) {
            return Ok(json!({
                "start": start.to_string(),
                "end": end.to_string(),
                "country": country,
                "in_scope": false,
                "dates": [],
            }));
        }
        let mut dates: Vec<Value> = Vec::new();
        let mut day = start;
        while day <= end {
            if let Some(override_entry) = state.tenant_override(&caller.tenant_id, &country, day) {
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
        Ok(json!({
            "start": start.to_string(),
            "end": end.to_string(),
            "country": country,
            "in_scope": true,
            "dates": dates,
        }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

// Handlers: manual overrides (tenant business data).

fn load_override_for_tenant(
    state: &HolidaysState,
    caller: &Caller,
    override_id: Uuid,
) -> UnifiedResult<HolidayOverride> {
    let Some(override_entry) = state.overrides.get(&override_id).map(|entry| entry.clone()) else {
        return Err(not_found_for_tenant("Override"));
    };
    if override_entry.tenant_id != caller.tenant_id {
        return Err(not_found_for_tenant("Override"));
    }
    Ok(override_entry)
}

pub(crate) async fn list_overrides(
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
            .overrides
            .iter()
            .filter(|entry| entry.tenant_id == caller.tenant_id)
            .map(|entry| entry.public_view())
            .collect();
        if let Some(country) = params.filters.get("country") {
            let wanted = normalize_country_code(country)?;
            items.retain(|item| item.get("country").and_then(Value::as_str) == Some(wanted.as_str()));
        }
        if let Some(kind) = params.filters.get("override_kind") {
            items.retain(|item| {
                item.get("override_kind").and_then(Value::as_str) == Some(kind.as_str())
            });
        }
        if let Some(year) = params.filters.get("year") {
            let wanted = year
                .trim()
                .parse::<i32>()
                .map_err(|_| UnifiedError::bad_request("Year filter must be a number"))?;
            items.retain(|item| {
                item.get("date")
                    .and_then(Value::as_str)
                    .and_then(|date| date.get(..4))
                    .and_then(|text| text.parse::<i32>().ok())
                    == Some(wanted)
            });
        }
        items.sort_by(|a, b| {
            a.get("date")
                .and_then(Value::as_str)
                .cmp(&b.get("date").and_then(Value::as_str))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => ok(page),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn create_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    body: web::Json<CreateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let body = body.into_inner();
        let country = normalize_country_code(&body.country)?;
        let date = parse_date(&body.date)?;
        let name = body.name.trim().to_string();
        if name.is_empty() || name.len() > 200 {
            return Err(UnifiedError::bad_request(
                "Override name must be 1-200 characters",
            ));
        }
        let override_kind = body
            .override_kind
            .as_deref()
            .unwrap_or("day_off")
            .trim()
            .to_string();
        if !matches!(override_kind.as_str(), "day_off" | "working_day") {
            return Err(UnifiedError::bad_request(
                "override_kind must be \"day_off\" or \"working_day\"",
            ));
        }
        if state
            .tenant_override(&caller.tenant_id, &country, date)
            .is_some()
        {
            return Err(UnifiedError::conflict(
                "This tenant already has an override for that country and date",
            ));
        }
        let note = body.note.map(|text| text.trim().to_string()).filter(|text| !text.is_empty());
        if let Some(note) = note.as_deref() {
            if note.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Override note must be at most 500 characters",
                ));
            }
        }
        let now = Utc::now();
        let override_entry = HolidayOverride {
            id: Uuid::new_v4(),
            tenant_id: caller.tenant_id.clone(),
            company_id: caller.company_id.clone(),
            country,
            date,
            name,
            override_kind,
            note,
            created_by: caller.actor.clone(),
            created_at: now,
            updated_at: now,
        };
        let view = override_entry.public_view();
        // Atomic check-and-insert: the pre-check above is only a
        // fast path, the lock in `insert_override_atomic` is what
        // keeps two concurrent creates from both inserting.
        if !state.insert_override_atomic(override_entry.clone()) {
            return Err(UnifiedError::conflict(
                "This tenant already has an override for that country and date",
            ));
        }
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_OVERRIDE_CREATED,
                json!({
                    "override_id": override_entry.id,
                    "tenant_id": override_entry.tenant_id,
                    "country": override_entry.country,
                    "date": override_entry.date.to_string(),
                    "override_kind": override_entry.override_kind,
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

pub(crate) async fn get_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_READ)?;
        let override_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid override id"))?;
        let override_entry = load_override_for_tenant(&state, &caller, override_id)?;
        Ok(override_entry.public_view())
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}

pub(crate) async fn update_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdateOverrideRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let override_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid override id"))?;
        let mut override_entry = load_override_for_tenant(&state, &caller, override_id)?;
        let body = body.into_inner();
        if let Some(name) = body.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 200 {
                return Err(UnifiedError::bad_request(
                    "Override name must be 1-200 characters",
                ));
            }
            override_entry.name = name;
        }
        if let Some(kind) = body.override_kind {
            let kind = kind.trim().to_string();
            if !matches!(kind.as_str(), "day_off" | "working_day") {
                return Err(UnifiedError::bad_request(
                    "override_kind must be \"day_off\" or \"working_day\"",
                ));
            }
            override_entry.override_kind = kind;
        }
        if let Some(note) = body.note {
            let note = note.trim().to_string();
            if note.len() > 500 {
                return Err(UnifiedError::bad_request(
                    "Override note must be at most 500 characters",
                ));
            }
            override_entry.note = if note.is_empty() { None } else { Some(note) };
        }
        override_entry.updated_at = Utc::now();
        let view = override_entry.public_view();
        state.overrides.insert(override_entry.id, override_entry.clone());
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_OVERRIDE_UPDATED,
                json!({
                    "override_id": override_entry.id,
                    "tenant_id": override_entry.tenant_id,
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

pub(crate) async fn delete_override(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<HolidaysState>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOLIDAYS_MANAGE)?;
        let override_id = Uuid::parse_str(path.into_inner().trim())
            .map_err(|_| UnifiedError::bad_request("Invalid override id"))?;
        let override_entry = load_override_for_tenant(&state, &caller, override_id)?;
        state.overrides.remove(&override_entry.id);
        if let Some(bus) = state.bus() {
            bus.emit(
                EVENT_OVERRIDE_DELETED,
                json!({
                    "override_id": override_entry.id,
                    "tenant_id": override_entry.tenant_id,
                }),
            );
        }
        Ok(json!({ "deleted": true, "override_id": override_entry.id }))
    })();
    match result {
        Ok(view) => ok(view),
        Err(error) => err(error, debug),
    }
}
