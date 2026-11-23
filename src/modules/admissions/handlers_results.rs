// admissions module — Result handlers.
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

use super::constants::{PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ, PERM_ADMISSIONS_READ_ALL};
use super::helpers::{
    caller, caller_company, check_capability, now, paginate, require_permission, require_tenant,
};
use super::state::AdmissionsState;
use super::types::AdmissionResult;

// ---------------------------------------------------------------------------
// Handlers: results
// ---------------------------------------------------------------------------

pub(crate) async fn list_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<AdmissionResult>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let read_all = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_READ_ALL);
        let mut items: Vec<AdmissionResult> = state
            .results
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|result| {
                result.period_id == period_id
                    && (read_all || result.published)
                    && params
                        .filters
                        .get("decision")
                        .is_none_or(|decision| result.decision == *decision)
                    && params
                        .filters
                        .get("published")
                        .is_none_or(|published| result.published.to_string() == *published)
            })
            .collect();
        items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn publish_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        if period.status != "closed" {
            return Err(UnifiedError::conflict(
                "Results can only be published for a closed period",
            ));
        }
        let mut published: Vec<AdmissionResult> = Vec::new();
        for mut entry in state.results.iter_mut() {
            let result = entry.value_mut();
            if result.period_id == period_id && !result.published {
                result.published = true;
                result.published_at = Some(now());
                result.updated_at = now();
                published.push(result.clone());
            }
        }
        for result in &published {
            if let Some(application) = state
                .applications
                .get(&result.application_id)
                .map(|value| value.value().clone())
            {
                state.notify_result(&application, &result.decision);
            }
        }
        let count = published.len() as u64;
        state.emit(
            "admissions.results_published",
            json!({ "period_id": period_id, "result_count": count }),
        );
        Ok(json!({ "published": count, "period_id": period_id }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn unpublish_results(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let mut unpublished: u64 = 0;
        for mut entry in state.results.iter_mut() {
            let result = entry.value_mut();
            if result.period_id == period_id && result.published {
                result.published = false;
                result.published_at = None;
                result.updated_at = now();
                unpublished += 1;
            }
        }
        state.emit(
            "admissions.results_unpublished",
            json!({ "period_id": period_id }),
        );
        Ok(json!({ "unpublished": unpublished, "period_id": period_id }))
    })();
    json::respond(result, debug)
}
