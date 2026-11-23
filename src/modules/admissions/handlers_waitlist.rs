// admissions module — Waitlist handlers.
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

use super::constants::{PERM_ADMISSIONS_APPLY, PERM_ADMISSIONS_MANAGE, PERM_ADMISSIONS_READ};
use super::helpers::{
    caller, caller_company, check_capability, load_application_for_read, now, paginate, require_id,
    require_permission, require_tenant,
};
use super::state::AdmissionsState;
use super::types::{PromoteWaitlist, WaitlistEntry};

// ---------------------------------------------------------------------------
// Handlers: waitlist
// ---------------------------------------------------------------------------

pub(crate) async fn list_waitlist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<PageResult<WaitlistEntry>> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let params = query.into_inner();
        let entries = state.waiting_entries(&period_id);
        let items: Vec<WaitlistEntry> = entries
            .into_iter()
            .filter(|entry| {
                params
                    .filters
                    .get("status")
                    .is_none_or(|status| entry.status == *status)
            })
            .collect();
        Ok(paginate(items, &params))
    })();
    match result {
        Ok(page) => json::success_response(page, 200),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn promote_waitlist(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<AdmissionsState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PromoteWaitlist>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let _company = caller_company(&_tenant, &req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE)?;
        let period_id = path.as_str().to_string();
        state.period(&period_id)?;
        let input = body.into_inner();
        let entries = state.waiting_entries(&period_id);
        let entry = match input.application_id {
            Some(application_id) => {
                let application_id = require_id(&application_id, "application_id")?;
                entries
                    .into_iter()
                    .find(|entry| entry.application_id == application_id)
                    .ok_or_else(|| {
                        UnifiedError::not_found(
                            "No waiting waitlist entry for this application in this period",
                        )
                    })?
            }
            None => entries
                .into_iter()
                .next()
                .ok_or_else(|| UnifiedError::not_found("Waitlist is empty"))?,
        };
        let application = state.application(&entry.application_id)?;
        if !state.seat_available(&application)? {
            return Err(UnifiedError::conflict(
                "No seat is free for this program and level yet",
            ));
        }
        let promoted = state.promote_waitlist_entry(&entry, &application)?;
        Ok(json!({ "application": promoted }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn decline_waitlist_entry(
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
        require_permission(&ctx, &caller.role, PERM_ADMISSIONS_READ)?;
        let entry_id = path.as_str().to_string();
        let entry = state
            .waitlist
            .get(&entry_id)
            .map(|value| value.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Waitlist entry not found"))?;
        let application = load_application_for_read(&state, &ctx, &caller, &entry.application_id)?;
        let is_manager = check_capability(&ctx, &caller.role, PERM_ADMISSIONS_MANAGE);
        let is_owner = application.applicant_user_id == caller.actor
            && check_capability(&ctx, &caller.role, PERM_ADMISSIONS_APPLY);
        if !is_manager && !is_owner {
            return Err(UnifiedError::forbidden(
                "Only the applicant or an admissions manager may decline a waitlist entry",
            ));
        }
        if entry.status != "waiting" {
            return Err(UnifiedError::conflict(format!(
                "A waitlist entry in status \"{}\" cannot be declined",
                entry.status
            )));
        }
        // Record the result first: a published result locks its
        // decision, so a refusal here must happen before the entry
        // and the application are mutated.
        state.upsert_result(&application, "rejected", &caller.actor, None)?;
        if let Some(mut record) = state.waitlist.get_mut(&entry_id) {
            record.status = "declined".to_string();
            record.updated_at = now();
        }
        if let Some(mut record) = state.applications.get_mut(&application.id) {
            if record.status == "waitlisted" {
                record.status = "rejected".to_string();
                record.updated_at = now();
            }
        }
        Ok(json!({ "declined": true, "id": entry_id }))
    })();
    json::respond(result, debug)
}
