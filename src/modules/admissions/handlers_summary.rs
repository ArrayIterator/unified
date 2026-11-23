// admissions module — Period summary report.
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

use super::constants::PERM_ADMISSIONS_READ;
use super::helpers::{caller, caller_company, require_permission, require_tenant};
use super::state::AdmissionsState;
use super::types::{Application, ChecklistItem};

// ---------------------------------------------------------------------------
// Handlers: period summary report
// ---------------------------------------------------------------------------

pub(crate) async fn period_summary(
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
        let period_id = path.as_str().to_string();
        let period = state.period(&period_id)?;
        let applications: Vec<Application> = state
            .applications
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|application| application.period_id == period_id)
            .collect();
        let mut by_status: std::collections::HashMap<String, u64> =
            std::collections::HashMap::new();
        for application in &applications {
            *by_status.entry(application.status.clone()).or_insert(0) += 1;
        }
        let mut quota_usage: Vec<Value> = Vec::new();
        for quota in state.quotas.iter() {
            let quota = quota.value();
            if quota.period_id != period_id {
                continue;
            }
            let accepted = state.accepted_count(&period_id, &quota.program_id, &quota.level, None);
            quota_usage.push(json!({
                "program_id": quota.program_id,
                "level": quota.level,
                "seats": quota.seats,
                "accepted": accepted,
                "free": quota.seats.saturating_sub(accepted),
            }));
        }
        let waiting = state
            .waitlist
            .iter()
            .filter(|entry| {
                entry.value().period_id == period_id && entry.value().status == "waiting"
            })
            .count() as u64;
        let published_results = state
            .results
            .iter()
            .filter(|entry| entry.value().period_id == period_id && entry.value().published)
            .count() as u64;
        let checklist_items: Vec<ChecklistItem> = state
            .checklist
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.period_id == period_id)
            .collect();
        let required_items = checklist_items.iter().filter(|item| item.required).count() as u64;
        let verified_items = checklist_items
            .iter()
            .filter(|item| item.required && item.status == "verified")
            .count() as u64;
        Ok(json!({
            "period": period,
            "application_count": applications.len() as u64,
            "by_status": by_status,
            "quota_usage": quota_usage,
            "waitlist_waiting": waiting,
            "results_published": published_results,
            "required_checklist_items": required_items,
            "verified_checklist_items": verified_items,
        }))
    })();
    json::respond(result, debug)
}
