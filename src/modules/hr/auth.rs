//! Caller identity, capability checks and small HTTP helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Ask the roles module whether `role` holds `permission` through the
/// `role.check` hook. Fails closed: no answer means not allowed.
pub(crate) fn check_capability(ctx: &AppContext, role: &str, permission: &str) -> bool {
    let context = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    context
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller holds any HR capability (read-all, manage, or
/// payroll). Used wherever salary amounts may be shown.
pub(crate) fn is_hr_capable(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_HR_READ_ALL)
        || check_capability(ctx, role, PERM_HR_MANAGE)
        || check_capability(ctx, role, PERM_HR_PAYROLL)
}

/// True when the caller may manage payroll: `hr:payroll`, or
/// `hr:manage` (managing HR includes payroll administration).
pub(crate) fn can_payroll(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_HR_PAYROLL) || check_capability(ctx, role, PERM_HR_MANAGE)
}

/// True when `caller` may read `employee`'s records: HR read-all, or
/// the employee themself.
pub(crate) fn can_read_employee(ctx: &AppContext, caller: &Caller, employee: &Employee) -> bool {
    check_capability(ctx, &caller.role, PERM_HR_READ_ALL) || employee.user_id == caller.actor
}

/// Enforce read access for one employee record, hiding the not-found
/// case: a caller who may not read the record gets 404, so employee
/// ids cannot be probed.
pub(crate) fn require_read_employee(
    ctx: &AppContext,
    caller: &Caller,
    employee: &Employee,
) -> UnifiedResult<()> {
    if can_read_employee(ctx, caller, employee) {
        Ok(())
    } else {
        Err(UnifiedError::not_found("Employee not found"))
    }
}

/// Enforce read access for one employee id: 404 when the employee
/// does not exist or the caller may not read it.
pub(crate) fn load_employee_for_read(
    state: &HrState,
    ctx: &AppContext,
    caller: &Caller,
    employee_id: &str,
) -> UnifiedResult<Employee> {
    let employee = state
        .employees
        .get(employee_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Employee not found"))?;
    require_read_employee(ctx, caller, &employee)?;
    Ok(employee)
}

// Tenant edge guard (C3 pilot 4, group C).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// HR (employees, positions, contracts, leave, payroll, reviews) records carry no company field, so the guard is a
/// fail-closed gate only: real per-company partitioning needs a
/// `tenant_id` on the records (C11 follow-up), and no per-record
/// company check is applied here.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

#[cfg(test)]
mod tenant_guard_tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach.
    fn tenant_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: "acme".to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request();
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }
}
