//! Planner - HTTP handlers for allocation plans.

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
use super::{constants::*, helpers::*, types::*, state::*, handlers_pools::*, forecast::*, events::*, module::*};

// Plan handlers.

/// After a plan changes state, check the pool for overcommit and emit
/// `planner.overcommit` with the numbers when allocations exceed
/// capacity. Returns the detail when overcommitted, so the caller can
/// attach it to the response as a warning.
///
/// Deliberate policy (inspection wave 6): overcommit only warns, it
/// never blocks approval or activation. The route docs therefore say
/// "overcommit warned" (the earlier wording implied a blocking
/// check): a pool may legitimately run overcommitted (burst headroom, planned
/// scale-out), and the warning plus the `planner.overcommit` event
/// give operators the numbers to act on instead of refusing the
/// transition outright.
pub(crate) fn check_overcommit(
    ctx: &unified_core::context::AppContext,
    state: &PlannerState,
    pool: &CapacityPool,
) -> Option<OvercommitDetail> {
    let detail = state.overcommit_detail(pool)?;
    ctx.hooks.emit(
        PLANNER_OVERCOMMIT,
        serde_json::to_value(&detail).unwrap_or(Value::Null),
    );
    Some(detail)
}

/// List allocation plans. Requires `planner.view`.
pub(crate) async fn list_plans(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    let result: UnifiedResult<PageResult<PlanView>> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let mut items: Vec<PlanView> = Vec::new();
        for entry in state.plans.iter() {
            let plan = entry.value().clone();
            // Company scope first: a scoped caller sees only their
            // own company's plans; an unresolved caller sees global
            // plans only.
            if !in_company_scope(&plan.company_id, &scope) {
                continue;
            }
            if !matches_search(
                &format!(
                    "{} {} {}",
                    plan.name,
                    plan.project_name.clone().unwrap_or_default(),
                    plan.company_id.clone().unwrap_or_default()
                ),
                &params,
            ) {
                continue;
            }
            if let Some(status) = params.filters.get("status") {
                if plan.status != *status {
                    continue;
                }
            }
            if let Some(pool_id) = params.filters.get("pool_id") {
                if plan.pool_id != *pool_id {
                    continue;
                }
            }
            if let Some(company_id) = params.filters.get("company_id") {
                if plan.company_id.as_deref() != Some(company_id.as_str()) {
                    continue;
                }
            }
            items.push(state.plan_view(&plan)?);
        }
        items.sort_by(|a, b| a.plan.created_at.cmp(&b.plan.created_at));
        if params.normalized().order == "desc" {
            items.reverse();
        }
        Ok(paginate(items, &params))
    })();
    json::respond(result, ctx.is_debug())
}

/// Create an allocation plan (draft). Requires `planner.manage`.
pub(crate) async fn create_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let pool = state.pool(&input.pool_id)?;
        let name = require_name(&input.name)?;
        input.reserved.validate()?;
        if input.reserved.is_empty() {
            return Err(UnifiedError::bad_request(
                "reserved must request at least one resource above zero",
            ));
        }
        let starts_at = parse_datetime(&input.starts_at, "starts_at")?;
        let ends_at = parse_datetime(&input.ends_at, "ends_at")?;
        if ends_at <= starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        // An omitted body company adopts the caller scope, so a
        // plan is never stored unscoped under a scoped caller; an
        // unresolved caller may not name a company.
        let company_id = require_company_match(
            &_tenant,
            &req,
            &input
                .company_id
                .clone()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
        )?;
        let timestamp = now();
        let plan = AllocationPlan {
            id: new_id(),
            pool_id: pool.id,
            name,
            company_id,
            project_name: input
                .project_name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()),
            reserved: input.reserved,
            starts_at,
            ends_at,
            status: "draft".to_string(),
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.plans.insert(plan.id.clone(), plan.clone());
        state.plan_view(&plan)
    })();
    respond_created(result, ctx.is_debug())
}

/// Get one allocation plan. Requires `planner.view`.
pub(crate) async fn get_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let plan = state.plan(path.as_str())?;
        require_record_in_company_scope(&plan.company_id, &scope)?;
        state.plan_view(&plan)
    })();
    json::respond(result, ctx.is_debug())
}

/// Update a draft allocation plan. Requires `planner.manage`.
pub(crate) async fn update_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let input = body.into_inner();
        let mut plan = state.plan(path.as_str())?;
        require_record_writable_in_company_scope(&plan.company_id, &scope)?;
        if plan.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only a draft plan can be updated; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        if let Some(name) = input.name {
            plan.name = require_name(&name)?;
        }
        if let Some(company_id) = input.company_id {
            let next = company_id
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
            // `Some(None)` is an explicit clear and stays a clear;
            // a newly named company must equal the caller scope.
            plan.company_id = match next {
                Some(value) => require_company_match(&_tenant, &req, &Some(value))?,
                None => None,
            };
        }
        if let Some(project_name) = input.project_name {
            plan.project_name = project_name
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        if let Some(reserved) = input.reserved {
            reserved.validate()?;
            if reserved.is_empty() {
                return Err(UnifiedError::bad_request(
                    "reserved must request at least one resource above zero",
                ));
            }
            plan.reserved = reserved;
        }
        if let Some(starts_at) = input.starts_at {
            plan.starts_at = parse_datetime(&starts_at, "starts_at")?;
        }
        if let Some(ends_at) = input.ends_at {
            plan.ends_at = parse_datetime(&ends_at, "ends_at")?;
        }
        if plan.ends_at <= plan.starts_at {
            return Err(UnifiedError::bad_request("ends_at must be after starts_at"));
        }
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        state.plan_view(&plan)
    })();
    json::respond(result, ctx.is_debug())
}

/// Delete a draft or expired allocation plan. Requires `planner.manage`.
pub(crate) async fn delete_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let plan = state.plan(path.as_str())?;
        require_record_writable_in_company_scope(&plan.company_id, &scope)?;
        if !matches!(plan.status.as_str(), "draft" | "expired") {
            return Err(UnifiedError::conflict(format!(
                "Only a draft or expired plan can be deleted; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        state.plans.remove(&plan.id);
        Ok(json!({ "deleted": true, "id": plan.id }))
    })();
    json::respond(result, ctx.is_debug())
}

/// Approve a draft plan. Requires `planner.manage`. Overcommit is
/// warned, not blocking (see `check_overcommit`).
pub(crate) async fn approve_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let mut plan = state.plan(path.as_str())?;
        require_record_writable_in_company_scope(&plan.company_id, &scope)?;
        if plan.status != "draft" {
            return Err(UnifiedError::conflict(format!(
                "Only a draft plan can be approved; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        if plan.is_past_end(now()) {
            return Err(UnifiedError::conflict(format!(
                "Plan \"{}\" already ended on {}; it cannot be approved",
                plan.id,
                plan.ends_at.to_rfc3339()
            )));
        }
        plan.status = "approved".to_string();
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        let pool = state.pool(&plan.pool_id)?;
        let warning = check_overcommit(&ctx, &state, &pool);
        ctx.hooks.emit(
            PLANNER_PLAN_APPROVED,
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        let mut view = state.plan_view(&plan)?;
        view.overcommit_warning = warning;
        Ok(view)
    })();
    json::respond(result, ctx.is_debug())
}

/// Activate an approved plan. Requires `planner.manage`. Overcommit
/// is warned, not blocking (see `check_overcommit`).
pub(crate) async fn activate_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let mut plan = state.plan(path.as_str())?;
        require_record_writable_in_company_scope(&plan.company_id, &scope)?;
        if plan.status != "approved" {
            return Err(UnifiedError::conflict(format!(
                "Only an approved plan can be activated; plan \"{}\" is {}",
                plan.id, plan.status
            )));
        }
        if plan.is_past_end(now()) {
            return Err(UnifiedError::conflict(format!(
                "Plan \"{}\" already ended on {}; it cannot be activated",
                plan.id,
                plan.ends_at.to_rfc3339()
            )));
        }
        plan.status = "active".to_string();
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        let pool = state.pool(&plan.pool_id)?;
        let warning = check_overcommit(&ctx, &state, &pool);
        ctx.hooks.emit(
            PLANNER_PLAN_ACTIVATED,
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        let mut view = state.plan_view(&plan)?;
        view.overcommit_warning = warning;
        Ok(view)
    })();
    json::respond(result, ctx.is_debug())
}

/// Expire a plan and release its capacity. Requires `planner.manage`.
pub(crate) async fn expire_plan(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<PlannerState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    let result: UnifiedResult<PlanView> = (|| {
        let _tenant = require_tenant(&req)?;
        let found = caller(&req)?;
        require_permission(&ctx, &found.role, PERM_PLANNER_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let mut plan = state.plan(path.as_str())?;
        require_record_writable_in_company_scope(&plan.company_id, &scope)?;
        if plan.status == "expired" {
            return Err(UnifiedError::conflict(format!(
                "Plan \"{}\" is already expired",
                plan.id
            )));
        }
        plan.status = "expired".to_string();
        plan.updated_at = now();
        state.plans.insert(plan.id.clone(), plan.clone());
        ctx.hooks.emit(
            PLANNER_PLAN_EXPIRED,
            serde_json::to_value(&plan).unwrap_or(Value::Null),
        );
        state.plan_view(&plan)
    })();
    json::respond(result, ctx.is_debug())
}
