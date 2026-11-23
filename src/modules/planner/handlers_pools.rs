//! Planner - HTTP handlers for capacity pools and utilisation samples.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{constants::*, helpers::*, types::*, state::*, forecast::*, handlers_plans::*, events::*, module::*};

// ---------------------------------------------------------------------------
// Pool handlers.
// ---------------------------------------------------------------------------

/// List capacity pools. Requires `planner.view`.
pub(crate) async fn list_pools(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let guard: UnifiedResult<()> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)
    })();
    if let Err(error) = guard {
        return json::error_response(&error, ctx.is_debug());
    }
    let params = query.into_inner();
    let mut items: Vec<PoolView> = state
        .pools
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|pool| {
            matches_search(&format!("{} {}", pool.name, pool.notes.clone().unwrap_or_default()), &params)
                && params
                    .filters
                    .get("provider_connection_id")
                    .is_none_or(|value| pool.provider_connection_id.as_deref() == Some(value.as_str()))
                && params
                    .filters
                    .get("cluster_id")
                    .is_none_or(|value| pool.cluster_id.as_deref() == Some(value.as_str()))
                && params.filters.get("overcommitted").is_none_or(|flag| {
                    let want = flag == "true" || flag == "1";
                    let allocated = state.allocated(&pool.id, now());
                    allocated.overcommitted_against(&pool.capacity) == want
                })
        })
        .map(|pool| state.pool_view(&pool))
        .collect();
    items.sort_by(|a, b| a.pool.created_at.cmp(&b.pool.created_at));
    if params.normalized().order == "desc" {
        items.reverse();
    }
    let page = paginate(items, &params);
    json::success_response(page, 200)
}

/// Create a capacity pool. Requires `planner.manage`.
pub(crate) async fn create_pool(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    body: web::Json<CreatePoolRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PoolView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let input = body.into_inner();
        let name = require_name(&input.name)?;
        input.capacity.validate()?;
        if input.capacity.is_empty() {
            return Err(UnifiedError::bad_request(
                "capacity must reserve at least one resource above zero",
            ));
        }
        let timestamp = now();
        let pool = CapacityPool {
            id: new_id(),
            name,
            provider_connection_id: input
                .provider_connection_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            cluster_id: input
                .cluster_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            capacity: input.capacity,
            notes: input.notes.map(|value| value.trim().to_string()).filter(|value| !value.is_empty()),
            last_sync_at: None,
            last_server_count: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.pools.insert(pool.id.clone(), pool.clone());
        ctx.hooks.emit(
            PLANNER_POOL_CREATED,
            serde_json::to_value(&pool).unwrap_or(Value::Null),
        );
        Ok(state.pool_view(&pool))
    })();
    respond_created(result, debug)
}

/// Get one capacity pool. Requires `planner.view`.
pub(crate) async fn get_pool(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PoolView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)?;
        let pool = state.pool(path.as_str())?;
        Ok(state.pool_view(&pool))
    })();
    json::respond(result, ctx.is_debug())
}

/// Update a capacity pool. Requires `planner.manage`.
pub(crate) async fn update_pool(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePoolRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PoolView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let input = body.into_inner();
        let mut pool = state.pool(path.as_str())?;
        if let Some(name) = input.name {
            pool.name = require_name(&name)?;
        }
        if let Some(provider_connection_id) = input.provider_connection_id {
            pool.provider_connection_id = provider_connection_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(cluster_id) = input.cluster_id {
            pool.cluster_id = cluster_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(capacity) = input.capacity {
            capacity.validate()?;
            if capacity.is_empty() {
                return Err(UnifiedError::bad_request(
                    "capacity must reserve at least one resource above zero",
                ));
            }
            pool.capacity = capacity;
        }
        if let Some(notes) = input.notes {
            pool.notes = notes.map(|value| value.trim().to_string()).filter(|value| !value.is_empty());
        }
        pool.updated_at = now();
        state.pools.insert(pool.id.clone(), pool.clone());
        Ok(state.pool_view(&pool))
    })();
    json::respond(result, ctx.is_debug())
}

/// Delete a capacity pool. Requires `planner.manage`.
pub(crate) async fn delete_pool(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let pool = state.pool(path.as_str())?;
        let live = state.live_plan_count(&pool.id, now());
        if live > 0 {
            return Err(UnifiedError::conflict(format!(
                "Capacity pool \"{}\" still has {live} live allocation plan(s); expire or delete them first",
                pool.id
            ))
            .with_metadata("live_plans", Value::from(live)));
        }
        state.pools.remove(&pool.id);
        state
            .samples
            .retain(|_, sample| sample.pool_id != pool.id);
        Ok(json!({ "deleted": true, "id": pool.id }))
    })();
    json::respond(result, ctx.is_debug())
}

// ---------------------------------------------------------------------------
// Utilisation handlers.
// ---------------------------------------------------------------------------

/// List recorded utilisation samples for a pool. Requires `planner.view`.
pub(crate) async fn list_utilisation(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<UsageSample>> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)?;
        let pool = state.pool(path.as_str())?;
        let mut items = state.samples_for_pool(&pool.id);
        items.retain(|sample| {
            params
                .filters
                .get("source")
                .is_none_or(|source| sample.source == *source)
        });
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, ctx.is_debug())
}

/// Record one utilisation sample for a pool. Requires
/// `planner.manage`: a sample feeds forecasts and overcommit math,
/// so anonymous writes are not accepted even for the
/// `provider-sync` / `service-monitor` sources (the caller still
/// identifies itself with the `X-User-Id` / `X-Role` headers).
pub(crate) async fn record_usage(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<RecordUsageRequest>,
) -> HttpResponse {
    let result: UnifiedResult<UsageSample> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let pool = state.pool(path.as_str())?;
        let input = body.into_inner();
        input.used.validate()?;
        let source = input
            .source
            .map(|value| value.trim().to_lowercase())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "api".to_string());
        if !matches!(source.as_str(), "api" | "provider-sync" | "service-monitor") {
            return Err(UnifiedError::bad_request(
                "source must be one of: api, provider-sync, service-monitor",
            ));
        }
        let recorded_at = match input.recorded_at {
            Some(value) => parse_datetime(&value, "recorded_at")?,
            None => now(),
        };
        if recorded_at > now() {
            return Err(UnifiedError::bad_request("recorded_at must not be in the future"));
        }
        let sample = UsageSample {
            id: new_id(),
            pool_id: pool.id,
            used: input.used,
            source,
            recorded_at,
            created_at: now(),
        };
        state.samples.insert(sample.id.clone(), sample.clone());
        Ok(sample)
    })();
    respond_created(result, ctx.is_debug())
}
