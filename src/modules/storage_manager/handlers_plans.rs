// Plan handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::PaginationParams;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

use super::*;

// ---------------------------------------------------------------------------
// Plans.
// ---------------------------------------------------------------------------

pub(crate) async fn list_plans(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<PlanView> = state
        .plans
        .iter()
        .map(|entry| PlanView::from(entry.value()))
        .collect();
    if let Some(filter) = params.filters.get("is_public") {
        let want = filter == "true";
        items.retain(|plan| plan.is_public == want);
    }
    if let Some(filter) = params.filters.get("enabled") {
        let want = filter == "true";
        items.retain(|plan| plan.enabled == want);
    }
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|plan| {
            plan.name.to_lowercase().contains(&needle)
                || plan.description.to_lowercase().contains(&needle)
        });
    }
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    ok(paginate(items, &params))
}

pub(crate) async fn list_public_plans(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<PlanView> = state
        .plans
        .iter()
        .map(|entry| PlanView::from(entry.value()))
        .filter(|plan| plan.is_public && plan.enabled)
        .collect();
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|plan| {
            plan.name.to_lowercase().contains(&needle)
                || plan.description.to_lowercase().contains(&needle)
        });
    }
    items.sort_by(|a, b| {
        a.sort_order
            .cmp(&b.sort_order)
            .then_with(|| a.name.cmp(&b.name))
    });
    ok(paginate(items, &params))
}

pub(crate) async fn create_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<CreatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanView> = (|| {
        let input = body.into_inner();
        let name = require_text(&input.name, "name", 128)?;
        if state
            .plans
            .iter()
            .any(|entry| entry.value().name == name)
        {
            return Err(UnifiedError::conflict(format!(
                "Storage plan \"{name}\" already exists"
            )));
        }
        let description = optional_text(&input.description, "description", 2048)?
            .unwrap_or_default();
        if input.quota_bytes < 0 {
            return Err(UnifiedError::bad_request(
                "quota_bytes must not be negative",
            ));
        }
        let max_files = input.max_files.unwrap_or(0);
        if max_files < 0 {
            return Err(UnifiedError::bad_request(
                "max_files must not be negative (use 0 for unlimited)",
            ));
        }
        let included = validate_provider_kind_list(&input.included_provider_kinds)?;
        let product_id = optional_text(&input.product_id, "product_id", 128)?;
        let billing_cycle = validate_billing_cycle(&input.billing_cycle)?;
        let now = Utc::now();
        let plan = StoragePlan {
            id: Uuid::now_v7(),
            name,
            description,
            quota_bytes: input.quota_bytes,
            max_files,
            included_provider_kinds: included,
            product_id,
            billing_cycle,
            is_public: input.is_public.unwrap_or(false),
            sort_order: input.sort_order.unwrap_or(0),
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            events::PLAN_CREATED,
            json!({
                "plan_id": plan.id.to_string(),
                "name": plan.name,
                "quota_bytes": plan.quota_bytes,
                "product_id": plan.product_id,
                "created_by": found.actor,
            }),
        );
        Ok(PlanView::from(&plan))
    })();
    match result {
        Ok(view) => created(view),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn get_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_READ) {
        return err_response(error, debug);
    }
    json::respond(load_plan(&state, &id).map(|plan| PlanView::from(&plan)), debug)
}

pub(crate) async fn update_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<UpdatePlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanView> = (|| {
        let input = body.into_inner();
        let mut plan = load_plan(&state, &id)?;
        if let Some(name) = &input.name {
            let name = require_text(name, "name", 128)?;
            if state
                .plans
                .iter()
                .any(|entry| entry.value().id != plan.id && entry.value().name == name)
            {
                return Err(UnifiedError::conflict(format!(
                    "Storage plan \"{name}\" already exists"
                )));
            }
            plan.name = name;
        }
        if let Some(description) = &input.description {
            plan.description = require_text(description, "description", 2048)?;
        }
        if let Some(quota_bytes) = input.quota_bytes {
            if quota_bytes < 0 {
                return Err(UnifiedError::bad_request(
                    "quota_bytes must not be negative",
                ));
            }
            plan.quota_bytes = quota_bytes;
        }
        if let Some(max_files) = input.max_files {
            if max_files < 0 {
                return Err(UnifiedError::bad_request(
                    "max_files must not be negative (use 0 for unlimited)",
                ));
            }
            plan.max_files = max_files;
        }
        if let Some(kinds) = &input.included_provider_kinds {
            plan.included_provider_kinds = validate_provider_kind_list(kinds)?;
        }
        if let Some(product_id) = &input.product_id {
            plan.product_id = match product_id {
                Some(value) => Some(require_text(value, "product_id", 128)?),
                None => None,
            };
        }
        if let Some(cycle) = &input.billing_cycle {
            plan.billing_cycle = match cycle {
                Some(value) => validate_billing_cycle(&Some(value.clone()))?,
                None => None,
            };
        }
        if let Some(is_public) = input.is_public {
            plan.is_public = is_public;
        }
        if let Some(sort_order) = input.sort_order {
            plan.sort_order = sort_order;
        }
        if let Some(enabled) = input.enabled {
            plan.enabled = enabled;
        }
        plan.updated_at = Utc::now();
        state.plans.insert(plan.id, plan.clone());
        state.emit(
            events::PLAN_UPDATED,
            json!({
                "plan_id": plan.id.to_string(),
                "name": plan.name,
                "updated_by": found.actor,
            }),
        );
        Ok(PlanView::from(&plan))
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<Value> = (|| {
        let plan = load_plan(&state, &id)?;
        if state
            .plan_assignments
            .iter()
            .any(|entry| entry.value().plan_id == plan.id && entry.value().status == "active")
        {
            return Err(UnifiedError::conflict(format!(
                "Storage plan \"{}\" cannot be deleted while buyers hold an active assignment to it",
                plan.name
            )));
        }
        state.plans.remove(&plan.id);
        state.emit(
            events::PLAN_DELETED,
            json!({
                "plan_id": plan.id.to_string(),
                "name": plan.name,
                "deleted_by": found.actor,
            }),
        );
        Ok(json!({ "deleted": true, "plan_id": plan.id }))
    })();
    json::respond(result, debug)
}

/// Assign a plan to a buyer, or change the buyer's current plan.
/// Changing a plan re-checks current usage against the new quota
/// and refuses with 409 when usage exceeds it. A company-scoped
/// assignment requires the module entitlement for that company;
/// a plan with a `product_id` additionally requires the product
/// entitlement for the buyer.
pub(crate) async fn assign_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<AssignPlanRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanAssignment> = (|| {
        let input = body.into_inner();
        let plan = load_plan(&state, &id)?;
        if !plan.enabled {
            return Err(UnifiedError::conflict(format!(
                "Storage plan \"{}\" is disabled and cannot be assigned",
                plan.name
            )));
        }
        // Tenant scoping: the assignment's company is verified
        // against the caller's scope — a scoped caller assigns for
        // their own company (an omitted company adopts it); an
        // unresolved caller may only assign personally.
        let body_company = optional_text(&input.company_id, "company_id", 128)?;
        let company_id = require_company_match_for(&_scope, &body_company)?;
        let user_id = valid_scope_id(&input.user_id)?;
        if let Some(expires_at) = input.expires_at {
            if expires_at <= Utc::now() {
                return Err(UnifiedError::bad_request(
                    "expires_at must be in the future",
                ));
            }
        }
        if let Some(company) = &company_id {
            require_storage_module_entitlement(&ctx, company)?;
        }
        check_plan_product_entitlement(&ctx, &plan, &user_id, company_id.as_deref())?;
        // The quota re-check runs before any mutation (and before
        // any map guard is taken), so a refusal leaves the old
        // plan in place.
        check_plan_change_quota(&state, &plan, company_id.as_deref(), &user_id)?;
        let now = Utc::now();
        // Assignments are keyed by id and cancelled rows are
        // retained: an active assignment is updated in place (same
        // id, plan changed), while a buyer with no active
        // assignment — including one whose only row is cancelled —
        // gets a fresh row with a new id. The cancelled row stays
        // resolvable and its history is preserved.
        if let Some(active) =
            active_assignment_for_buyer(&state, company_id.as_deref(), &user_id)
        {
            let mut existing = state
                .plan_assignments
                .get_mut(&active.id)
                .expect("active assignment vanished");
            let old_plan_id = existing.plan_id;
            existing.plan_id = plan.id;
            existing.expires_at = input.expires_at;
            existing.assigned_by = found.actor.clone();
            existing.assigned_at = now;
            existing.updated_at = now;
            let assignment = (*existing).clone();
            drop(existing);
            state.emit(
                events::PLAN_CHANGED,
                json!({
                    "assignment_id": assignment.id.to_string(),
                    "old_plan_id": old_plan_id.to_string(),
                    "plan_id": plan.id.to_string(),
                    "company_id": company_id,
                    "user_id": user_id,
                    "changed_by": found.actor,
                }),
            );
            Ok(assignment)
        } else {
            let assignment = PlanAssignment {
                id: Uuid::now_v7(),
                plan_id: plan.id,
                company_id: company_id.clone(),
                user_id: user_id.clone(),
                status: "active".to_string(),
                assigned_by: found.actor.clone(),
                assigned_at: now,
                expires_at: input.expires_at,
                cancelled_at: None,
                cancel_reason: None,
                created_at: now,
                updated_at: now,
            };
            state
                .plan_assignments
                .insert(assignment.id, assignment.clone());
            state.emit(
                events::PLAN_ASSIGNED,
                json!({
                    "assignment_id": assignment.id.to_string(),
                    "plan_id": plan.id.to_string(),
                    "company_id": company_id,
                    "user_id": user_id,
                    "assigned_by": found.actor,
                }),
            );
            Ok(assignment)
        }
    })();
    match result {
        Ok(assignment) => created(assignment),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn list_plan_assignments(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<PlanAssignment> = state
        .plan_assignments
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: a scoped caller lists only their own
    // company's assignments; an unresolved caller lists only
    // personal assignments.
    items.retain(|item| in_company_scope(&item.company_id, &_scope));
    if let Some(filter) = params.filters.get("company_id") {
        items.retain(|item| item.company_id.as_deref() == Some(filter.as_str()));
    }
    if let Some(filter) = params.filters.get("user_id") {
        items.retain(|item| item.user_id == *filter);
    }
    if let Some(filter) = params.filters.get("plan_id") {
        items.retain(|item| item.plan_id.to_string() == *filter);
    }
    if let Some(filter) = params.filters.get("status") {
        items.retain(|item| item.status == *filter);
    }
    items.sort_by(|a, b| b.assigned_at.cmp(&a.assigned_at));
    ok(paginate(items, &params))
}

pub(crate) async fn get_plan_assignment(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let result: UnifiedResult<PlanAssignment> = (|| {
        let assignment = load_assignment(&state, &id)?;
        require_record_in_company_scope(&assignment.company_id, &_scope)?;
        Ok(assignment)
    })();
    json::respond(result, debug)
}

pub(crate) async fn cancel_plan_assignment(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<String>,
    body: web::Json<CancelAssignmentRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<PlanAssignment> = (|| {
        let input = body.into_inner();
        let mut assignment = load_assignment(&state, &id)?;
        require_record_writable_in_company_scope(&assignment.company_id, &_scope)?;
        if assignment.status != "active" {
            return Err(UnifiedError::conflict(format!(
                "Plan assignment {} is already {} and cannot be cancelled",
                assignment.id, assignment.status
            )));
        }
        assignment.status = "cancelled".to_string();
        assignment.cancelled_at = Some(Utc::now());
        assignment.cancel_reason =
            optional_text(&input.reason, "reason", 512)?;
        assignment.updated_at = Utc::now();
        // Keyed by assignment id: the cancelled row is retained in
        // place, never overwritten by a later re-assignment.
        state.plan_assignments.insert(assignment.id, assignment.clone());
        state.emit(
            events::PLAN_CANCELLED,
            json!({
                "assignment_id": assignment.id.to_string(),
                "plan_id": assignment.plan_id.to_string(),
                "company_id": assignment.company_id,
                "user_id": assignment.user_id,
                "reason": assignment.cancel_reason,
                "cancelled_by": found.actor,
            }),
        );
        Ok(assignment)
    })();
    json::respond(result, debug)
}

/// A buyer's current plan plus usage against its quota. The buyer
/// is the caller (`X-User-Id`) unless an admin (holding
/// `storage_manager.manage_plans`) asks for another buyer.
pub(crate) async fn get_my_plan(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    query: web::Query<HashMap<String, String>>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_READ) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let is_admin = is_platform_admin(&ctx, &found.role, &req, PERM_MANAGE_PLANS);
    let user_id = if is_admin {
        query
            .get("user_id")
            .map(|value| value.to_string())
            .unwrap_or_else(|| found.actor.clone())
    } else {
        found.actor.clone()
    };
    // Non-admins cannot pick another company via the query string:
    // their company comes only from the verified caller scope (see
    // `resolve_my_plan_company`).
    let company_id = resolve_my_plan_company(
        is_admin,
        query.get("company_id").map(String::as_str),
        _scope.as_deref(),
    );
    let result: UnifiedResult<Value> = (|| {
        let user_id = valid_scope_id(&user_id)?;
        let company_id = match &company_id {
            Some(value) => Some(valid_scope_id(value)?),
            None => None,
        };
        let assignment = state
            .plan_assignments
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| {
                item.company_id.as_deref() == company_id.as_deref() && item.user_id == user_id
            })
            .max_by(|a, b| a.assigned_at.cmp(&b.assigned_at));
        let plan = match &assignment {
            Some(item) if item.status == "active" => state
                .plans
                .get(&item.plan_id)
                .map(|entry| PlanView::from(entry.value())),
            _ => None,
        };
        // An expired assignment reports as expired instead of
        // silently looking like no plan at all.
        let status = match &assignment {
            Some(item) if item.status == "active"
                && item
                    .expires_at
                    .is_some_and(|expires_at| expires_at <= Utc::now()) =>
            {
                "expired"
            }
            Some(item) => item.status.as_str(),
            None => "none",
        };
        let (bytes, files) = buyer_usage(&state, company_id.as_deref(), &user_id);
        let (quota_bytes, max_files) = match &plan {
            Some(view) => (Some(view.quota_bytes), Some(view.max_files)),
            None => (None, None),
        };
        Ok(json!({
            "company_id": company_id,
            "user_id": user_id,
            "assignment": assignment,
            "plan": plan,
            "status": status,
            "usage": {
                "bytes_stored": bytes,
                "files_stored": files,
                "quota_bytes": quota_bytes,
                "max_files": max_files,
                "bytes_remaining": quota_bytes.map(|quota| (quota - bytes).max(0)),
                "files_remaining": max_files.map(|max| if max == 0 { Value::Null } else { Value::from((max - files).max(0)) }).unwrap_or(Value::Null),
                "over_quota": quota_bytes.is_some_and(|quota| bytes > quota)
                    || max_files.is_some_and(|max| max > 0 && files > max),
            },
        }))
    })();
    json::respond(result, debug)
}

/// Record a metering report for one buyer and emit it for billing.
/// The current month is read from live usage (recorded hub objects
/// plus BYO objects); a past month is read from the daily rollup
/// (`period_usage_from_daily`), so a backdated report states that
/// period's usage instead of today's.
pub(crate) async fn report_metering(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    body: web::Json<ReportMeteringRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    let found = match authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        Ok(found) => found,
        Err(error) => return err_response(error, debug),
    };
    let result: UnifiedResult<MeteringReport> = (|| {
        let input = body.into_inner();
        // Tenant scoping: the report's company is verified against
        // the caller's scope — a scoped caller reports for their
        // own company (an omitted company adopts it); an unresolved
        // caller may only report personally.
        let body_company = optional_text(&input.company_id, "company_id", 128)?;
        let company_id = require_company_match_for(&_scope, &body_company)?;
        let user_id = valid_scope_id(&input.user_id)?;
        let period = match &input.period {
            Some(raw) => {
                let trimmed = raw.trim();
                let valid = trimmed.len() == 7
                    && trimmed.as_bytes().get(4) == Some(&b'-')
                    && trimmed[..4].chars().all(|c| c.is_ascii_digit())
                    && trimmed[5..].chars().all(|c| c.is_ascii_digit())
                    && (1..=12).contains(&trimmed[5..].parse::<u32>().unwrap_or(0));
                if !valid {
                    return Err(UnifiedError::bad_request(
                        "period must be YYYY-MM",
                    ));
                }
                trimmed.to_string()
            }
            None => Utc::now().format("%Y-%m").to_string(),
        };
        let plan_id = active_assignment_for_buyer(&state, company_id.as_deref(), &user_id)
            .map(|assignment| assignment.plan_id);
        // Usage is period-bound, never "current usage labelled as
        // that period": the current month uses live usage (hub +
        // BYO objects); a past period is read from the daily rollup
        // (see `period_usage_from_daily`), so a backdated report
        // states that period's usage.
        let current_period = Utc::now().format("%Y-%m").to_string();
        let (bytes, files) = if period == current_period {
            buyer_usage(&state, company_id.as_deref(), &user_id)
        } else {
            period_usage_from_daily(&state, company_id.as_deref(), &user_id, &period)
        };
        let report = MeteringReport {
            id: Uuid::now_v7(),
            company_id: company_id.clone(),
            user_id: user_id.clone(),
            plan_id,
            period: period.clone(),
            bytes_stored: bytes,
            files_stored: files,
            reported_by: found.actor.clone(),
            reported_at: Utc::now(),
        };
        state
            .metering_reports
            .insert(report.id, report.clone());
        emit_metering(&state, &report, "plan_metering");
        Ok(report)
    })();
    match result {
        Ok(report) => created(report),
        Err(error) => err_response(error, debug),
    }
}

pub(crate) async fn list_metering(
    state: web::Data<StorageManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();

    let _tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return err_response(error, debug),
    };
    let _scope = match caller_company(&_tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return err_response(error, debug),
    };
    if let Err(error) = authorize(&ctx, &req, PERM_MANAGE_PLANS) {
        return err_response(error, debug);
    }
    let params = params.into_inner();
    let mut items: Vec<MeteringReport> = state
        .metering_reports
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Tenant scoping: a scoped caller lists only their own
    // company's reports; an unresolved caller lists only personal
    // reports.
    items.retain(|item| in_company_scope(&item.company_id, &_scope));
    if let Some(filter) = params.filters.get("company_id") {
        items.retain(|item| item.company_id.as_deref() == Some(filter.as_str()));
    }
    if let Some(filter) = params.filters.get("user_id") {
        items.retain(|item| item.user_id == *filter);
    }
    if let Some(filter) = params.filters.get("period") {
        items.retain(|item| item.period == *filter);
    }
    if let Some(filter) = params.filters.get("plan_id") {
        items.retain(|item| item.plan_id.map(|id| id.to_string()) == Some(filter.clone()));
    }
    items.sort_by(|a, b| b.reported_at.cmp(&a.reported_at));
    ok(paginate(items, &params))
}
