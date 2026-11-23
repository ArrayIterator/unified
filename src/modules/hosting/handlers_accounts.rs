// hosting module — Account handlers.
//
// Split from the former single-file module `src/modules/hosting.rs`.
// The module root (`hosting.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::Domain;
use crate::modules::company::company_exists;
use crate::modules::network::DnsVerifier;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

use super::types::{HostingAccount, HostingState};
use super::requests::{CreateAccountRequest, UpdateAccountRequest};
use super::views::AccountRequirementReport;
use super::helpers::{PERM_HOSTING_MANAGE, PERM_HOSTING_VIEW, allowed_transition, caller, caller_company, require_company_for_write, check_account_requirements, in_company_scope, paginate, require_company_match_uuid, require_company_match_uuid_update, require_permission, require_record_in_company_scope, require_tenant, validate_usage_against_plan};

// ---------------------------------------------------------------------------
// Account handlers.
// ---------------------------------------------------------------------------

pub(crate) async fn list_accounts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let scope = match caller_company(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_HOSTING_VIEW))
    {
        return json::error_response(&error, debug);
    }
    let mut items: Vec<HostingAccount> = state
        .accounts
        .iter()
        .map(|entry| entry.value().clone())
        .collect();
    // Company scope first: a scoped caller sees only their own
    // company's accounts; an unresolved caller sees global ones only.
    items.retain(|account| in_company_scope(&account.company_id, &scope));
    if let Some(search) = &params.search {
        let needle = search.to_lowercase();
        items.retain(|account| account.domain.to_lowercase().contains(&needle));
    }
    if let Some(status) = params.filters.get("status") {
        items.retain(|account| account.status == *status);
    }
    if let Some(plan_id) = params.filters.get("plan_id") {
        if let Ok(wanted) = plan_id.parse::<Uuid>() {
            items.retain(|account| account.plan_id == wanted);
        }
    }
    if let Some(company_id) = params.filters.get("company_id") {
        if let Ok(wanted) = company_id.parse::<Uuid>() {
            items.retain(|account| account.company_id == Some(wanted));
        }
    }
    if let Some(ssl_state) = params.filters.get("ssl_state") {
        items.retain(|account| account.ssl_state == *ssl_state);
    }
    let sort = params.allowed_sort(&["domain", "status", "created_at"], "created_at");
    match sort.as_str() {
        "domain" => items.sort_by(|a, b| a.domain.cmp(&b.domain)),
        "status" => items.sort_by(|a, b| a.status.cmp(&b.status)),
        _ => items.sort_by(|a, b| a.created_at.cmp(&b.created_at)),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    json::success_response(paginate(items, &params).to_json(), 200)
}

pub(crate) async fn create_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    body: web::Json<CreateAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingAccount> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE)?;
        let company_id = require_company_match_uuid(&_tenant, &req, &body.company_id)?;
        let plan = state
            .plans
            .get(&body.plan_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))?;
        if !plan.is_active {
            return Err(UnifiedError::conflict(
                "Hosting plan is not active; activate the plan before creating accounts on it",
            ));
        }
        let syntax = DnsVerifier::check_domain_syntax(&body.domain);
        if !syntax.valid {
            return Err(UnifiedError::bad_request(format!(
                "Invalid domain: {}",
                syntax.issues.join("; ")
            )));
        }
        let domain = syntax
            .punycode
            .clone()
            .unwrap_or_else(|| body.domain.trim().to_lowercase());
        if let Some(company_id) = company_id {
            if !company_exists(&ctx, company_id) {
                return Err(UnifiedError::not_found("Company not found"));
            }
        }
        let store_order_id = body
            .store_order_id
            .clone()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let disk_used_mb = body.disk_used_mb.unwrap_or(0);
        let bandwidth_used_mb = body.bandwidth_used_mb.unwrap_or(0);
        validate_usage_against_plan(&plan, disk_used_mb, bandwidth_used_mb)?;
        if state.accounts.iter().any(|entry| {
            entry.value().domain == domain && entry.value().status != "cancelled"
        }) {
            return Err(UnifiedError::conflict(format!(
                "Hosting account for domain \"{domain}\" already exists"
            )));
        }
        let now = Utc::now();
        let account = HostingAccount {
            id: Uuid::now_v7(),
            plan_id: plan.id,
            domain,
            company_id,
            status: "pending".to_string(),
            ssl_state: "none".to_string(),
            disk_used_mb,
            bandwidth_used_mb,
            store_order_id,
            created_at: now,
            updated_at: now,
        };
        state.accounts.insert(account.id, account.clone());
        ctx.hooks.emit(
            "hosting.account.created",
            serde_json::to_value(&account).unwrap_or(Value::Null),
        );
        Ok(account)
    })();
    match result {
        Ok(account) => json::success_response(account, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

pub(crate) async fn get_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingAccount> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let account = state
            .accounts
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        require_record_in_company_scope(&account.company_id, &scope)?;
        Ok(account)
    })();
    json::respond(result, debug)
}

pub(crate) async fn update_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<UpdateAccountRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<HostingAccount> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        let mut account = state
            .accounts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        require_record_writable_in_company_scope(&account.company_id, &scope)?;
        if account.status == "cancelled" {
            return Err(UnifiedError::conflict(
                "Cancelled hosting accounts cannot be updated",
            ));
        }
        let mut plan = state
            .plans
            .get(&account.plan_id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::internal("Hosting plan for this account is missing"))?;
        if let Some(plan_id) = body.plan_id {
            if plan_id != account.plan_id {
                if account.status != "pending" {
                    return Err(UnifiedError::conflict(
                        "The plan of a hosting account can only be changed while it is pending",
                    ));
                }
                plan = state
                    .plans
                    .get(&plan_id)
                    .map(|entry| entry.value().clone())
                    .ok_or_else(|| UnifiedError::not_found("Hosting plan not found"))?;
                if !plan.is_active {
                    return Err(UnifiedError::conflict(
                        "Hosting plan is not active",
                    ));
                }
                account.plan_id = plan_id;
            }
        }
        if let Some(Some(company_id)) = &body.company_id {
            if !company_exists(&ctx, *company_id) {
                return Err(UnifiedError::not_found("Company not found"));
            }
        }
        // `None` keeps the current company, `Some(None)` is an
        // explicit clear and stays a clear, `Some(Some(id))` must
        // equal the caller scope.
        account.company_id =
            require_company_match_uuid_update(&_tenant, &req, &body.company_id, &account.company_id)?;
        let disk_used_mb = body.disk_used_mb.unwrap_or(account.disk_used_mb);
        let bandwidth_used_mb = body
            .bandwidth_used_mb
            .unwrap_or(account.bandwidth_used_mb);
        validate_usage_against_plan(&plan, disk_used_mb, bandwidth_used_mb)?;
        account.disk_used_mb = disk_used_mb;
        account.bandwidth_used_mb = bandwidth_used_mb;
        account.updated_at = Utc::now();
        state.accounts.insert(account.id, account.clone());
        ctx.hooks.emit(
            "hosting.account.updated",
            serde_json::to_value(&account).unwrap_or(Value::Null),
        );
        Ok(account)
    })();
    json::respond(result, debug)
}

pub(crate) async fn delete_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE)?;
        let scope = require_company_for_write(&_tenant, &req)?;
        let id = id.into_inner();
        let account = state
            .accounts
            .get(&id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        require_record_writable_in_company_scope(&account.company_id, &scope)?;
        if account.status != "pending" && account.status != "cancelled" {
            return Err(UnifiedError::conflict(
                "Only pending or cancelled hosting accounts can be deleted; suspend or cancel an active account first",
            ));
        }
        state.accounts.remove(&id);
        Ok(json!({ "deleted": true, "id": id }))
    })();
    json::respond(result, debug)
}

/// Shared status-transition handler for activate / suspend /
/// unsuspend / cancel. Activation additionally runs the full
/// requirement check first and is refused (422) while any check
/// fails; the report is carried in the error metadata.
pub(crate) fn transition_account(
    ctx: &AppContext,
    state: &Arc<HostingState>,
    scope: &Option<String>,
    id: Uuid,
    target: &str,
) -> UnifiedResult<HostingAccount> {
    let mut account = state
        .accounts
        .get(&id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
    require_record_in_company_scope(&account.company_id, scope)?;
    if !allowed_transition(&account.status, target) {
        return Err(UnifiedError::conflict(format!(
            "Hosting account cannot move from \"{}\" to \"{}\"",
            account.status, target
        )));
    }
    if target == "active" && account.status == "pending" {
        // Activation: requirements must all pass first.
        let report = check_account_requirements(state, ctx, &account);
        ctx.hooks.emit(
            "hosting.requirements.checked",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        if !report.can_activate {
            return Err(UnifiedError::unprocessable(format!(
                "Hosting account cannot be activated: requirements are {}",
                report.status
            ))
            .with_metadata("account_id", json!(account.id))
            .with_metadata(
                "requirements",
                serde_json::to_value(&report).unwrap_or(Value::Null),
            ));
        }
        if report
            .checks
            .iter()
            .any(|check| check.code == "ssl_ready" && check.status == "ok")
        {
            account.ssl_state = "active".to_string();
        }
    }
    account.status = target.to_string();
    account.updated_at = Utc::now();
    state.accounts.insert(account.id, account.clone());
    let event = match (account.status.as_str(), target) {
        ("active", _) => "hosting.account.activated",
        ("suspended", _) => "hosting.account.suspended",
        ("cancelled", _) => "hosting.account.cancelled",
        _ => "hosting.account.updated",
    };
    ctx.hooks.emit(
        event,
        serde_json::to_value(&account).unwrap_or(Value::Null),
    );
    Ok(account)
}

pub(crate) async fn activate_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let scope = match require_company_for_write(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE))
    {
        return json::error_response(&error, debug);
    }
    json::respond(transition_account(&ctx, &state, &scope, id.into_inner(), "active"), debug)
}

pub(crate) async fn suspend_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let scope = match require_company_for_write(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE))
    {
        return json::error_response(&error, debug);
    }
    json::respond(
        transition_account(&ctx, &state, &scope, id.into_inner(), "suspended"),
        debug,
    )
}

pub(crate) async fn unsuspend_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let scope = match require_company_for_write(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE))
    {
        return json::error_response(&error, debug);
    }
    json::respond(transition_account(&ctx, &state, &scope, id.into_inner(), "active"), debug)
}

pub(crate) async fn cancel_account(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let tenant = match require_tenant(&req) {
        Ok(tenant) => tenant,
        Err(error) => return json::error_response(&error, debug),
    };
    let scope = match require_company_for_write(&tenant, &req) {
        Ok(scope) => scope,
        Err(error) => return json::error_response(&error, debug),
    };
    if let Err(error) = caller(&req)
        .and_then(|caller| require_permission(&ctx, &caller.role, PERM_HOSTING_MANAGE))
    {
        return json::error_response(&error, debug);
    }
    json::respond(
        transition_account(&ctx, &state, &scope, id.into_inner(), "cancelled"),
        debug,
    )
}

pub(crate) async fn get_account_requirements(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<HostingState>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<AccountRequirementReport> = (|| {
        let _tenant = require_tenant(&req)?;
        let caller = caller(&req)?;
        require_permission(&ctx, &caller.role, PERM_HOSTING_VIEW)?;
        let scope = caller_company(&_tenant, &req)?;
        let account = state
            .accounts
            .get(&id.into_inner())
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found("Hosting account not found"))?;
        require_record_in_company_scope(&account.company_id, &scope)?;
        let report = check_account_requirements(&state, &ctx, &account);
        ctx.hooks.emit(
            "hosting.requirements.checked",
            serde_json::to_value(&report).unwrap_or(Value::Null),
        );
        Ok(report)
    })();
    json::respond(result, debug)
}
