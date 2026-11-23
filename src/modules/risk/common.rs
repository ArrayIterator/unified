// risk — common.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::store::*;
use super::handlers_risks::*;
use super::handlers_plans::*;
use super::handlers_templates::*;
use super::handlers_instances::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

pub(crate) fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

/// Viewer identity from the request headers (`X-User-Id` /
/// `X-Role`), exactly like the forms and appointments modules.
/// Query parameters are never read for identity: they are logged,
/// cached, and bookmarked, so a claimed role in the query string
/// must not decide what the caller may do.
pub(crate) fn viewer_of(req: &HttpRequest) -> (Option<String>, Option<String>) {
    let verified = unified_core::caller::caller_from(req);
    if verified.is_authenticated() {
        (
            Some(verified.user_id.clone()),
            verified.roles.first().cloned(),
        )
    } else {
        (None, None)
    }
}

pub(crate) fn require_capability(
    state: &RiskState,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    match role {
        Some(role) if state.check_capability(role, permission) => Ok(()),
        _ => Err(UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 3).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `viewer_of`/`actor_of` or any
/// capability check.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// — a request must not carry one company in its tenant context and
/// another in its verified identity. The `X-Company-Id` header is
/// NEVER trusted here: only the verified caller (company resolved
/// from the database at authentication time) and the
/// middleware-resolved tenant context are sources.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company))
            if tenant_company != verified_company =>
        {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified),
    }
}

/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// When no caller scope is known, a body company is refused: an
/// unresolved caller may not claim a company (the shared
/// `CallerScope::Unresolved` rule); omitting the company stores
/// a global row.
pub(crate) fn require_company_match(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &Option<String>,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (body_company_id, caller) {
        (Some(body), Some(caller_company)) if body != caller_company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (Some(body), Some(_)) => Ok(Some(body.clone())),
        (Some(body), None) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (None, caller) => Ok(caller.clone()),
    }
}

/// True when a stored record belongs to the caller's company
/// scope. With no caller scope (an unresolved caller) only
/// global records match: unscoped records are visible, every
/// company-owned record is denied (the shared
/// `CallerScope::Unresolved` rule). With a scope, only records
/// carrying exactly that company match; unscoped (legacy) records
/// are hidden from scoped callers.
pub(crate) fn in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> bool {
    match caller_company {
        None => record_company_id.is_none(),
        Some(company) => record_company_id.as_deref() == Some(company.as_str()),
    }
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    caller_company: &Option<String>,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, caller_company) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Load a risk and fail closed (403) when it is outside the
/// caller's company scope. Plans and tasks carry no company of
/// their own — they are scoped through their parent risk — so
/// every handler that works through a parent id calls one of
/// these helpers first.
pub(crate) fn require_risk_in_scope(
    state: &RiskState,
    risk_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<Risk> {
    let risk = state.get_risk(risk_id)?;
    require_record_in_company_scope(&risk.company_id, scope)?;
    Ok(risk)
}

/// Load a mitigation plan and fail closed when its parent risk is
/// outside the caller's company scope.
pub(crate) fn require_plan_risk_in_scope(
    state: &RiskState,
    plan_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<MitigationPlan> {
    let plan = state.get_plan(plan_id)?;
    require_risk_in_scope(state, &plan.risk_id, scope)?;
    Ok(plan)
}

/// Load a mitigation task and fail closed when its parent risk is
/// outside the caller's company scope.
pub(crate) fn require_task_risk_in_scope(
    state: &RiskState,
    task_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<MitigationTask> {
    let task = state
        .tasks
        .get(task_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Mitigation task not found"))?;
    require_risk_in_scope(state, &task.risk_id, scope)?;
    Ok(task)
}

/// Load a checklist instance and fail closed (403) when it is
/// outside the caller's company scope. Instance items carry no
/// company of their own — they are scoped through their parent
/// instance.
pub(crate) fn require_instance_in_scope(
    state: &RiskState,
    instance_id: &str,
    scope: &Option<String>,
) -> UnifiedResult<ChecklistInstance> {
    let instance = state
        .instances
        .get(instance_id)
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Checklist instance not found"))?;
    require_record_in_company_scope(&instance.company_id, scope)?;
    Ok(instance)
}

/// Actor identity from the request headers (`X-User-Id` /
/// `X-Role`). See `viewer_of`: query parameters are never read
/// for identity.
pub(crate) fn actor_of(req: &HttpRequest) -> (Option<String>, Option<String>) {
    viewer_of(req)
}

pub(crate) fn valid_rating(value: u8) -> bool {
    (1..=5).contains(&value)
}

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, false),
    }
}
#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;
    use actix_web::test::TestRequest;

    #[test]
    fn ratings_accept_only_one_to_five() {
        assert!(valid_rating(1));
        assert!(valid_rating(5));
        assert!(!valid_rating(0));
        assert!(!valid_rating(6));
        assert!(!valid_rating(255));
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_then_verified_caller() {
        use actix_web::HttpMessage;
        let build = |tenant_company: &str, caller_company: Option<&str>| {
            let req = actix_web::test::TestRequest::default().to_http_request();
            let verified = match caller_company {
                Some(company) => unified_core::auth::Caller::verified(
                    uuid::Uuid::now_v7(),
                    Vec::new(),
                )
                .with_company(company.to_string()),
                None => {
                    unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                }
            };
            req.extensions_mut().insert(verified);
            req.extensions_mut()
                .insert(unified_core::middleware::TenantContext {
                    tenant_id: uuid::Uuid::now_v7().to_string(),
                    company_id: tenant_company.to_string(),
                    site_id: String::new(),
                    kind: unified_core::middleware::TenantKind::Subdomain,
                    host: "tenant.example.test".to_string(),
                    scope: None,
                    product_id: None,
                    package_id: None,
                    product_subdomain_id: None,
                });
            req
        };
        let req = build("acme", Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some("acme".to_string())
        );
        let req = build("", Some("acme"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some("acme"));
        let req = build("acme", Some("other-company"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn in_company_scope_hides_unscoped_records_from_scoped_callers() {
        let scope = Some("acme".to_string());
        assert!(in_company_scope(&Some("acme".to_string()), &scope));
        assert!(!in_company_scope(&Some("other".to_string()), &scope));
        assert!(!in_company_scope(&None, &scope));
        assert!(in_company_scope(&None, &None));
        assert!(!in_company_scope(&Some("other".to_string()), &None));
    }

    #[test]
    fn require_company_match_for_refuses_body_company_from_unresolved_callers() {
        let body = Some("c1".to_string());
        assert!(require_company_match_for(&None, &body).is_err());
        assert!(require_company_match_for(&None, &None).unwrap().is_none());
        assert_eq!(
            require_company_match_for(&Some("c1".to_string()), &body).unwrap(),
            Some("c1".to_string())
        );
        assert!(
            require_company_match_for(&Some("c1".to_string()), &Some("c2".to_string())).is_err()
        );
    }

    #[test]
    fn actor_identity_comes_from_the_verified_caller_not_query_params() {
        let req = verified_request("user-1", "admin");
        assert_eq!(
            actor_of(&req),
            (
                Some(test_user_uuid("user-1").to_string()),
                Some("admin".to_string())
            )
        );
        assert_eq!(
            viewer_of(&req),
            (
                Some(test_user_uuid("user-1").to_string()),
                Some("admin".to_string())
            )
        );
        // A role claimed only in the query string is ignored:
        // query params are logged, cached, and bookmarked.
        let req = TestRequest::default()
            .uri("/api/v1/risk/risks?viewer_id=spoofed&viewer_role=admin")
            .to_http_request();
        assert_eq!(actor_of(&req), (None, None));
        assert_eq!(viewer_of(&req), (None, None));
    }
}
