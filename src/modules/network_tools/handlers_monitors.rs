//! HTTP handlers for uptime monitors, events and certificate watches.

use unified_core::context::{AppContext};
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{Utc};
use serde_json::{json, Value};
use std::sync::{Arc};
use uuid::Uuid;
use super::types::{CreateCertWatchRequest, CreateUptimeMonitorRequest, NetworkToolCertWatch, NetworkToolUptimeEvent, NetworkToolUptimeMonitor, RATE_LIMIT_RUNS_PER_MINUTE, UpdateUptimeMonitorRequest};
use super::store::{cert_watches_store, db_delete_cert_watch, db_delete_monitor, db_load_cert_watches, db_load_monitors, db_pool, db_upsert_cert_watch, db_upsert_monitor, uptime_events_store, uptime_monitors_store};
use super::helpers::{allow_private_probes, check_rate_limit, company_filter, has_capability, matches_search, paginate, parse_company_id, require_capability, require_owner_or_manage, tool_actor, viewer_of};
use super::validation::validate_domain;
use super::tools_monitor::{check_cert_watches, evaluate_monitor};

// ---------------------------------------------------------------------------
// Uptime monitors (CRUD + trigger + events).
// ---------------------------------------------------------------------------

pub(crate) async fn list_uptime_monitors(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let company_id = company_filter(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let mut items: Vec<NetworkToolUptimeMonitor> = match db_pool(&ctx) {
            Some(pool) => db_load_monitors(&pool).unwrap_or_else(|_| {
                uptime_monitors_store()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect()
            }),
            None => uptime_monitors_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(viewer_id) = &viewer_id {
            items.retain(|monitor| &monitor.user_id == viewer_id);
        } else if !has_capability(viewer_role.as_deref(), "network_tools:manage") {
            items.clear();
        }
        if let Some(company) = &company_id {
            items.retain(|monitor| monitor.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true";
            items.retain(|monitor| monitor.enabled == wanted);
        }
        items.retain(|monitor| {
            matches_search(&format!("{} {}", monitor.name, monitor.url), &params)
        });
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_uptime_monitor(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateUptimeMonitorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let company_id = company_filter(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let input = body.into_inner();
        let name = input.name.trim().to_string();
        if name.is_empty() || name.len() > 128 {
            return Err(UnifiedError::bad_request(
                "Monitor name must be 1-128 characters",
            ));
        }
        // Validate the URL shape now (without probing it); the SSRF
        // guard runs again on every probe.
        let parsed = input
            .url
            .trim()
            .parse::<reqwest::Url>()
            .map_err(|_| UnifiedError::bad_request("Monitor URL is not a valid URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(UnifiedError::bad_request(
                "Monitor URL must use http or https",
            ));
        }
        if input.interval_seconds < 60 || input.interval_seconds > 86400 {
            return Err(UnifiedError::bad_request(
                "Monitor interval must be 60-86400 seconds",
            ));
        }
        if input.expected_status < 100 || input.expected_status > 599 {
            return Err(UnifiedError::bad_request(
                "Expected status must be a valid HTTP status code",
            ));
        }
        if input.failure_threshold < 1 || input.failure_threshold > 10 {
            return Err(UnifiedError::bad_request(
                "Failure threshold must be 1-10",
            ));
        }
        let now = Utc::now();
        let monitor = NetworkToolUptimeMonitor {
            id: Uuid::new_v4(),
            user_id: viewer_id.unwrap_or_else(|| "anonymous".to_string()),
            company_id: parse_company_id(input.company_id.or(company_id)),
            name,
            url: input.url.trim().to_string(),
            interval_seconds: input.interval_seconds,
            expected_status: input.expected_status,
            failure_threshold: input.failure_threshold,
            consecutive_failures: 0,
            enabled: true,
            last_status: None,
            last_checked_at: None,
            created_at: now,
            updated_at: now,
        };
        uptime_monitors_store().insert(monitor.id, monitor.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_monitor(&pool, &monitor);
        }
        Ok(json!(monitor))
    })();
    json::respond(result, debug)
}

pub(crate) async fn get_uptime_monitor(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        let monitor = uptime_monitors_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Uptime monitor {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &monitor.user_id)?;
        Ok(json!(monitor))
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_uptime_monitor(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
    body: web::Json<UpdateUptimeMonitorRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let mut monitor = uptime_monitors_store()
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Uptime monitor {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &monitor.user_id)?;
        let input = body.into_inner();
        if let Some(name) = input.name {
            let name = name.trim().to_string();
            if name.is_empty() || name.len() > 128 {
                return Err(UnifiedError::bad_request(
                    "Monitor name must be 1-128 characters",
                ));
            }
            monitor.name = name;
        }
        if let Some(url) = input.url {
            let parsed = url
                .trim()
                .parse::<reqwest::Url>()
                .map_err(|_| UnifiedError::bad_request("Monitor URL is not a valid URL"))?;
            if !matches!(parsed.scheme(), "http" | "https") {
                return Err(UnifiedError::bad_request(
                    "Monitor URL must use http or https",
                ));
            }
            monitor.url = url.trim().to_string();
        }
        if let Some(interval) = input.interval_seconds {
            if !(60..=86400).contains(&interval) {
                return Err(UnifiedError::bad_request(
                    "Monitor interval must be 60-86400 seconds",
                ));
            }
            monitor.interval_seconds = interval;
        }
        if let Some(expected) = input.expected_status {
            if !(100..=599).contains(&expected) {
                return Err(UnifiedError::bad_request(
                    "Expected status must be a valid HTTP status code",
                ));
            }
            monitor.expected_status = expected;
        }
        if let Some(threshold) = input.failure_threshold {
            if !(1..=10).contains(&threshold) {
                return Err(UnifiedError::bad_request(
                    "Failure threshold must be 1-10",
                ));
            }
            monitor.failure_threshold = threshold;
        }
        if let Some(enabled) = input.enabled {
            monitor.enabled = enabled;
        }
        monitor.updated_at = Utc::now();
        uptime_monitors_store().insert(monitor.id, monitor.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_monitor(&pool, &monitor);
        }
        Ok(json!(monitor))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_uptime_monitor(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let owner = uptime_monitors_store()
            .get(&id)
            .map(|entry| entry.value().user_id.clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Uptime monitor {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &owner)?;
        uptime_monitors_store().remove(&id);
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_delete_monitor(&pool, id);
        }
        Ok(json!({ "deleted": id }))
    })();
    json::respond(result, debug)
}

/// Trigger one evaluation of a monitor now. This is the endpoint the
/// scheduler arm (or an operator) calls; it also answers the
/// `network_tools.uptime.check_requested` hook event (see the hook
/// handler below, which acknowledges the request synchronously and
/// points the caller at this endpoint for the async probe).
pub(crate) async fn check_uptime_monitor(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _params = query.into_inner();
    let (actor, viewer_role) = tool_actor(&req);
    let allow_private = allow_private_probes(viewer_role.as_deref());
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:manage")
        .and_then(|_| check_rate_limit(&actor, "uptime:check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), debug);
    }
    let id = path.into_inner();
    let result = evaluate_monitor(&ctx, id, &actor, allow_private).await;
    json::respond(result, debug)
}

pub(crate) async fn list_uptime_events(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (_, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let id = path.into_inner();
        if !uptime_monitors_store().contains_key(&id) {
            return Err(UnifiedError::not_found(format!(
                "Uptime monitor {id} not found"
            )));
        }
        let mut items: Vec<NetworkToolUptimeEvent> = uptime_events_store()
            .iter()
            .filter(|entry| entry.value().monitor_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        if let Some(status) = params.filters.get("status") {
            items.retain(|event| event.status == *status);
        }
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

// ---------------------------------------------------------------------------
// Certificate watches (CRUD + check).
// ---------------------------------------------------------------------------

pub(crate) async fn list_cert_watches(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let company_id = company_filter(&params);
        require_capability(viewer_role.as_deref(), "network_tools:view")?;
        let mut items: Vec<NetworkToolCertWatch> = match db_pool(&ctx) {
            Some(pool) => db_load_cert_watches(&pool).unwrap_or_else(|_| {
                cert_watches_store()
                    .iter()
                    .map(|entry| entry.value().clone())
                    .collect()
            }),
            None => cert_watches_store()
                .iter()
                .map(|entry| entry.value().clone())
                .collect(),
        };
        if let Some(viewer_id) = &viewer_id {
            items.retain(|watch| &watch.user_id == viewer_id);
        } else if !has_capability(viewer_role.as_deref(), "network_tools:manage") {
            items.clear();
        }
        if let Some(company) = &company_id {
            items.retain(|watch| watch.company_id == parse_company_id(Some(company.clone())));
        }
        if let Some(enabled) = params.filters.get("enabled") {
            let wanted = enabled == "true";
            items.retain(|watch| watch.enabled == wanted);
        }
        items.retain(|watch| matches_search(&watch.domain, &params));
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn create_cert_watch(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    body: web::Json<CreateCertWatchRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        let company_id = company_filter(&params);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let input = body.into_inner();
        let domain = validate_domain(&input.domain)?;
        if cert_watches_store()
            .iter()
            .any(|entry| entry.value().domain == domain)
        {
            return Err(UnifiedError::bad_request(format!(
                "A certificate watch already exists for \"{domain}\""
            )));
        }
        let now = Utc::now();
        let watch = NetworkToolCertWatch {
            id: Uuid::new_v4(),
            user_id: viewer_id.unwrap_or_else(|| "anonymous".to_string()),
            company_id: parse_company_id(input.company_id.or(company_id)),
            domain,
            enabled: true,
            last_days_remaining: None,
            last_checked_at: None,
            alerted_thresholds: Vec::new(),
            created_at: now,
            updated_at: now,
        };
        cert_watches_store().insert(watch.id, watch.clone());
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_upsert_cert_watch(&pool, &watch);
        }
        Ok(json!(watch))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_cert_watch(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
    path: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _params = query.into_inner();
        let (viewer_id, viewer_role) = viewer_of(&req);
        require_capability(viewer_role.as_deref(), "network_tools:manage")?;
        let id = path.into_inner();
        let owner = cert_watches_store()
            .get(&id)
            .map(|entry| entry.value().user_id.clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Certificate watch {id} not found")))?;
        require_owner_or_manage(viewer_id.as_deref(), viewer_role.as_deref(), &owner)?;
        cert_watches_store().remove(&id);
        if let Some(pool) = db_pool(&ctx) {
            let _ = db_delete_cert_watch(&pool, id);
        }
        Ok(json!({ "deleted": id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn check_cert_watches_handler(
    req: HttpRequest,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let _params = query.into_inner();
    let (actor, viewer_role) = tool_actor(&req);
    if let Err(error) = require_capability(viewer_role.as_deref(), "network_tools:manage")
        .and_then(|_| check_rate_limit(&actor, "cert:check", RATE_LIMIT_RUNS_PER_MINUTE))
    {
        return json::respond(Err::<Value, UnifiedError>(error), debug);
    }
    let result = check_cert_watches(&ctx, &actor).await;
    json::respond(result, debug)
}
