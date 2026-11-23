// admissions module — Quota handlers.
//
// Split from the former single-file module `src/modules/admissions.rs`.
// The module root (`admissions.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::{MAX_SHORT, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ};
use super::helpers::{
    caller, caller_company, matches_search, new_id, now, paginate, require_id, require_permission,
    require_tenant, require_text,
};
use super::state::AdmissionsState;
use super::types::{CreateQuota, PeriodQuota, UpdateQuota};
use super::state::AdmissionsState;

// ---------------------------------------------------------------------------
// Handlers: quotas
// ---------------------------------------------------------------------------

pub(crate) async fn list_quotas(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<PeriodQuota>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let mut items: Vec<PeriodQuota> = state
            .quotas
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|quota| {
                quota.period_id == period_id
                    && matches_search(&format!("{} {}", quota.program_id, quota.level), &params)
                    && params
                        .filters
                        .get("program_id")
                        .is_none_or(|program| quota.program_id == *program)
                    && params
                        .filters
                        .get("level")
                        .is_none_or(|level| quota.level == *level)
            })
            .collect();
        items.sort_by(|a, b| {
            a.program_id
                .cmp(&b.program_id)
                .then_with(|| a.level.cmp(&b.level))
        });
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn create_quota(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<CreateQuota>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PeriodQuota> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Quotas cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let program_id = require_id(&input.program_id, "program_id")?;
        let level = require_text(&input.level, "level", MAX_SHORT)?;
        if input.seats == 0 {
            return Err(UnifiedError::bad_request("seats must be at least 1"));
        }
        if state.quotas.iter().any(|entry| {
            let quota = entry.value();
            quota.period_id == period_id
                && quota.program_id == program_id
                && quota.level == level
        }) {
            return Err(UnifiedError::conflict(format!(
                "A quota already exists for program \"{program_id}\" level \"{level}\" in this period",
            )));
        }
        let quota = PeriodQuota {
            id: new_id(),
            period_id,
            program_id,
            level,
            seats: input.seats,
            created_at: now(),
            updated_at: now(),
        };
        state.quotas.insert(quota.id.clone(), quota.clone());
        Ok(quota)
    })();
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn update_quota(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<UpdateQuota>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PeriodQuota> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, quota_id) = path.into_inner();
        let period = state.period(&period_id)?;
        if period.status == "archived" {
            return Err(UnifiedError::conflict(
                "Quotas cannot be changed on an archived period",
            ));
        }
        let input = body.into_inner();
        let mut quota = state
            .quotas
            .get(&quota_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quota not found"))?;
        if quota.period_id != period_id {
            return Err(UnifiedError::not_found("Quota not found"));
        }
        if let Some(seats) = input.seats {
            if seats == 0 {
                return Err(UnifiedError::bad_request("seats must be at least 1"));
            }
            let taken = state.accepted_count(&period_id, &quota.program_id, &quota.level, None);
            if seats < taken {
                return Err(UnifiedError::conflict(format!(
                    "seats cannot drop below the {taken} already accepted for this program and level",
                )));
            }
            quota.seats = seats;
        }
        quota.updated_at = now();
        state.quotas.insert(quota.id.clone(), quota.clone());
        Ok(quota)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_quota(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let (period_id, quota_id) = path.into_inner();
        let quota = state
            .quotas
            .get(&quota_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Quota not found"))?;
        if quota.period_id != period_id {
            return Err(UnifiedError::not_found("Quota not found"));
        }
        if state.accepted_count(&period_id, &quota.program_id, &quota.level, None) > 0 {
            return Err(UnifiedError::conflict(
                "Quota still has accepted applications; lower the seats instead of deleting it",
            ));
        }
        state
            .quotas
            .remove(quota_id.as_str())
            .map(|_| json!({ "deleted": true, "id": quota_id }))
            .ok_or_else(|| UnifiedError::not_found("Quota not found"))
    })();
    json::respond(result, debug)
}
