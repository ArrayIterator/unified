// Studio - Caller identity, tenant and permission guards.

use actix_web::HttpRequest;
use serde_json::json;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::tenancy::CallerScope;
use unified_core::tenancy::TenantContext as TenancyContext;

// Caller identity and capability checks.

pub(crate) struct Caller {
    pub(crate) actor: String,
    role: String,
    /// The tenant context the request resolved to. Kept on the
    /// caller so handlers can read the server-side company scope
    /// without re-resolving the tenant.
    pub(crate) tenant: TenancyContext,
}

/// Maximum length of a company id taken from a header or hook
/// answer, mirroring the files module's bound.
const MAX_COMPANY_LEN: usize = 128;

/// Resolve the tenant context for a request. THE single
/// convergence point for tenant-scoped data access in this file
/// (see `unified_core::tenancy`): this function returns the tenancy
/// `TenantContext` (tenant id as Uuid + schema name derived only
/// by `tenancy::schema_name()`), and the Diesel persistence wiring
/// - when it lands - takes the Uuid from here into
/// `tenancy::with_tenant_connection`. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means the request is refused
/// here, never served from `public`.
///
/// Company scoping is enforced at the handler level on top of
/// this guard (see `resolve_caller_company` below and
/// `CallerScope` in `unified_core::tenancy`): the caller's company
/// is resolved from server-side sources only (verified caller,
/// tenant context, `user.company_requested` hook) and a body/query
/// `company_id` that disagrees with it is rejected; list endpoints
/// only return rows of the caller's company plus global rows.
/// Platform is an explicit, positively-decided scope (control-plane
/// request by a verified admin with no resolvable company) - a
/// caller with no resolvable company is unresolved and reads
/// global rows only, never any company's rows.
///
/// RESIDUAL (still open, integrator work): the in-memory DashMap
/// stores in this module carry no `tenant_id` on their records,
/// so there is no per-record tenant filtering beyond the company
/// check above. That is safe only as long as company ids are
/// globally unique (one company belongs to one tenant, which the
/// company module guarantees with UUID ids): two tenants that
/// ever shared one `company_id` string would see each other's
/// rows in the shared in-memory maps. Full tenant partitioning
/// needs a store-shape change (tenant id on every record + a
/// migration column) and the Diesel persistence wiring through
/// `tenancy::with_tenant_connection`.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved context
    // is kept on the caller: it carries the server-side company
    // scope (`TenantContext.company_id`) that
    // `resolve_caller_company` verifies request company ids
    // against, and it is bound for the Diesel persistence layer
    // (see `resolve_tenant`).
    let tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller {
        actor,
        role,
        tenant,
    })
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed.
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

pub(crate) fn require_permission(
    ctx: &AppContext,
    call: &Caller,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, &call.role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{}\" does not hold permission \"{}\"",
            call.role, permission
        )))
    }
}

pub(crate) fn guard(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let call = caller(req)?;
    require_permission(ctx, &call, permission)?;
    Ok(call)
}

// Company scoping.
//
// The caller's company is resolved from server-side sources only
// - never from a request body or query filter, which are
// client-asserted. A `company_id` carried in the body or query is
// verified against the scope resolved here instead (see
// `CallerScope::require_company_scope` and
// `CallerScope::scoped_read_company`). This is the same
// verified-caller-then-tenant-then-hook shape the files module uses
// (`resolve_caller_company` there). Record access itself is the
// shared `CallerScope` in `unified_core::tenancy`: platform is an
// explicitly-decided scope, and an unresolved caller reads global
// rows only.

/// Resolve the company scope the caller acts under, from
/// server-side sources only, most authoritative first:
///
/// 1. the verified caller's company
///    (`caller::caller_from(req).company_id`);
/// 2. the tenant context the middleware resolved for this request
///    (`TenantContext.company_id`);
/// 3. the `user.company_requested` hook answered by the users
///    module from its own store for this caller.
///
/// A body/query `company_id` is NEVER a source, and neither is the
/// `X-Company-Id` header: both are client-asserted, so trusting
/// either would let any JWT caller claim another company. Returns
/// a `CallerScope`: `Company` when a company resolves, `Platform`
/// only when no company resolves AND the request is a control-plane
/// request AND the verified caller holds an admin role
/// (`CallerScope::decide`), `Unresolved` otherwise. Fails closed
/// with 403 when two sources resolve to different non-empty
/// companies: the request is refused rather than served under
/// either scope.
pub(crate) fn resolve_caller_company(
    ctx: &AppContext,
    req: &HttpRequest,
    caller: &Caller,
) -> UnifiedResult<CallerScope> {
    let mut candidates: Vec<String> = Vec::new();
    if let Some(company) = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_LEN)
    {
        candidates.push(company);
    }
    if let Some(company) = caller.tenant.company_id.clone() {
        let company = company.trim().to_string();
        if !company.is_empty() && company.len() <= MAX_COMPANY_LEN {
            candidates.push(company);
        }
    }
    let hook = ctx
        .hooks
        .emit("user.company_requested", json!({ "user_id": caller.actor }));
    if let Some(company) = hook
        .get("company_id")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= MAX_COMPANY_LEN)
    {
        candidates.push(company.to_string());
    }
    let mut resolved: Option<String> = None;
    for candidate in candidates {
        match &resolved {
            None => resolved = Some(candidate),
            Some(seen) if seen == &candidate => {}
            Some(_) => {
                return Err(UnifiedError::forbidden(
                    "Company scope mismatch: the verified caller, tenant context, and the caller's company do not agree",
                ));
            }
        }
    }
    let is_admin = caller.role == "admin" || caller.role == "super_admin";
    Ok(CallerScope::decide(resolved, req, is_admin))
}

/// Permission guard plus caller-company resolution in one step,
/// following the files module's `authorize()` shape. Returns the
/// caller and the company scope they act under (see `CallerScope`).
pub(crate) fn authorize(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<(Caller, CallerScope)> {
    let call = guard(ctx, req, permission)?;
    let company = resolve_caller_company(ctx, req, &call)?;
    Ok((call, company))
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;
    use std::sync::Arc;
    use unified_core::config::AppConfig;
    use unified_core::hooks::{HookBus, HookContext};
    use unified_core::middleware::{TenantContext as MwTenantContext, TenantKind};
    use unified_core::modules::ModuleRegistry;
    use uuid::Uuid;

    fn test_ctx() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn test_ctx_with_hook(company: &str) -> AppContext {
        let hooks = Arc::new(HookBus::new());
        let company = company.to_string();
        hooks.register(
            "user.company_requested",
            "test",
            Arc::new(move |ctx: &mut HookContext| {
                ctx.set("company_id", serde_json::json!(company));
            }),
        );
        AppContext::new(AppConfig::default(), hooks, Arc::new(ModuleRegistry::new()))
    }

    fn test_caller(company: Option<&str>) -> Caller {
        Caller {
            actor: "tester".to_string(),
            role: "admin".to_string(),
            tenant: TenancyContext {
                tenant_id: Uuid::new_v4(),
                schema_name: "tenant_test".to_string(),
                company_id: company.map(str::to_string),
                site_id: None,
            },
        }
    }

    /// A request that resolved to a tenant host (middleware tenant
    /// context attached), so it is NOT a control-plane request.
    fn tenant_host_request() -> HttpRequest {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut().insert(MwTenantContext {
            tenant_id: Uuid::new_v4().to_string(),
            company_id: String::new(),
            site_id: String::new(),
            kind: TenantKind::Subdomain,
            host: "tenant.example.test".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        });
        req
    }

    #[test]
    fn can_access_record_platform_scope_is_explicit() {
        // Platform reads any record - but only as the explicitly
        // decided `Platform` scope, never from an absent company.
        assert!(CallerScope::Platform.can_access_record("company-a"));
        assert!(CallerScope::Platform.can_access_record(""));
    }

    #[test]
    fn can_access_record_company_caller_reads_own_and_global_only() {
        let scope = CallerScope::Company("company-a".to_string());
        assert!(scope.can_access_record("company-a"));
        assert!(scope.can_access_record(""));
        assert!(!scope.can_access_record("company-b"));
    }

    #[test]
    fn can_access_record_unresolved_caller_reads_global_only() {
        assert!(CallerScope::Unresolved.can_access_record(""));
        assert!(!CallerScope::Unresolved.can_access_record("company-a"));
        assert!(!CallerScope::Unresolved.can_access_record("company-b"));
    }

    #[test]
    fn require_company_scope_platform_caller_may_write_any_scope() {
        assert!(
            CallerScope::Platform
                .require_company_scope("company-a")
                .is_ok()
        );
        assert!(CallerScope::Platform.require_company_scope("").is_ok());
    }

    #[test]
    fn require_company_scope_company_caller_writes_own_company_only() {
        let scope = CallerScope::Company("company-a".to_string());
        assert!(scope.require_company_scope("company-a").is_ok());
        // Another company is a mismatch, and the global scope is
        // platform-only: writing it would affect every company.
        assert!(scope.require_company_scope("company-b").is_err());
        assert!(scope.require_company_scope("").is_err());
    }

    #[test]
    fn require_company_scope_unresolved_caller_writes_nothing() {
        assert!(
            CallerScope::Unresolved
                .require_company_scope("company-a")
                .is_err()
        );
        assert!(CallerScope::Unresolved.require_company_scope("").is_err());
    }

    #[test]
    fn scoped_read_company_defaults_to_caller_company_and_rejects_others() {
        let scope = CallerScope::Company("company-a".to_string());
        assert_eq!(scope.scoped_read_company("").unwrap(), "company-a");
        assert_eq!(scope.scoped_read_company("company-a").unwrap(), "company-a");
        assert!(scope.scoped_read_company("company-b").is_err());
        assert_eq!(
            CallerScope::Platform
                .scoped_read_company("company-b")
                .unwrap(),
            "company-b"
        );
        assert_eq!(CallerScope::Platform.scoped_read_company("").unwrap(), "");
        // Unresolved reads global rows only.
        assert_eq!(CallerScope::Unresolved.scoped_read_company("").unwrap(), "");
        assert!(
            CallerScope::Unresolved
                .scoped_read_company("company-a")
                .is_err()
        );
    }

    #[test]
    fn resolve_caller_company_prefers_tenant_company() {
        let ctx = test_ctx();
        let req = TestRequest::default().to_http_request();
        let caller = test_caller(Some("company-a"));
        assert_eq!(
            resolve_caller_company(&ctx, &req, &caller).unwrap(),
            CallerScope::Company("company-a".to_string())
        );
    }

    #[test]
    fn resolve_caller_company_falls_back_to_hook() {
        let ctx = test_ctx_with_hook("company-a");
        let req = TestRequest::default().to_http_request();
        let caller = test_caller(None);
        assert_eq!(
            resolve_caller_company(&ctx, &req, &caller).unwrap(),
            CallerScope::Company("company-a".to_string())
        );
    }

    #[test]
    fn resolve_caller_company_rejects_mismatched_sources() {
        let ctx = test_ctx_with_hook("company-b");
        let req = TestRequest::default().to_http_request();
        let caller = test_caller(Some("company-a"));
        assert!(resolve_caller_company(&ctx, &req, &caller).is_err());
    }

    #[test]
    fn resolve_caller_company_unresolved_on_tenant_host_without_company() {
        // No company anywhere, admin caller, but the request
        // resolved to a tenant host: NOT platform - platform needs
        // a control-plane request. Unresolved reads global only.
        let ctx = test_ctx();
        let req = tenant_host_request();
        let caller = test_caller(None);
        let scope = resolve_caller_company(&ctx, &req, &caller).unwrap();
        assert_eq!(scope, CallerScope::Unresolved);
        assert!(!scope.can_access_record("company-a"));
    }
}
