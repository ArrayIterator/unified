// Library - Caller identity, tenant and permission guards.

use actix_web::HttpRequest;
use serde_json::json;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::tenancy::{self, CallerScope};
use uuid::Uuid;

use super::{PERM_LIBRARY_CATALOG, PERM_LIBRARY_CIRCULATE, PERM_LIBRARY_MANAGE};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Constant-time string comparison, so token verification does not
/// leak how many leading characters matched.
pub(crate) fn constant_time_eq(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(actual.as_bytes().iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the e-learning module.
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

/// Resolve the tenant for this request, failing closed when the
/// tenant-resolution middleware attached no tenant context (401)
/// or the resolved tenant id is not a uuidv7 UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant. See
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`).
///
/// Diesel persistence is not wired yet (the DashMap stores are
/// authoritative; migration `2026-11-22-000091_library` is the
/// persistence target), so there is no `DbPool` in this module
/// and `with_tenant_connection` has no call site here yet — when
/// the Diesel writers land, all data access goes through
/// `tenancy::with_tenant_connection(pool, tenant.tenant_id, ...)`.
/// Until then, company scoping is enforced at the handler level:
/// `require_company_match` rejects a body `company_id` outside
/// the caller's scope, `require_record_in_company_scope` rejects
/// single-record access outside it, and list handlers filter to
/// the caller's company (`caller_company` / `in_company_scope`,
/// both backed by the shared `CallerScope`).
/// Residual: the maps themselves carry no `tenant_id`, so records
/// are partitioned by `company_id`, not by tenant; a tenant whose
/// context carries no company scope is `Unresolved` and sees
/// global rows only, and hook listeners (no `HttpRequest`) carry
/// no scope at all. Full per-tenant isolation lands with the
/// Diesel writers and a `tenant_id` on every record.
pub(crate) fn require_tenant(req: &HttpRequest) -> UnifiedResult<tenancy::TenantContext> {
    tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts under, as the shared
/// `CallerScope` (see `unified_core::tenancy`). Precedence, most
/// authoritative first: the verified caller's company
/// (`caller::caller_from(req).company_id`), then the tenant
/// context's company. Fails closed (403) when both are present and
/// disagree — a request must not carry one company for the caller
/// and another in its tenant context.
///
/// The `X-Company-Id` header is NOT a source: it is
/// client-asserted, so trusting it would let any JWT caller claim
/// another company (the old header fallback was that hole). The
/// `user.company_requested` hook is not consulted here either:
/// this function takes no `AppContext`, and the tenant context —
/// required first by every handler in this module — is the
/// authoritative source. A caller with no resolvable company is
/// `Unresolved` and reads global rows only; platform is an
/// explicitly-decided scope (`CallerScope::decide`: control-plane
/// request by a verified admin), never the absence of a company.
pub(crate) fn caller_company(
    tenant: &tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<CallerScope> {
    let verified = unified_core::caller::caller_from(req);
    let verified_company = verified
        .company_id
        .clone()
        .filter(|value| !value.is_empty());
    match (&verified_company, &tenant.company_id) {
        (Some(verified), Some(tenant_company)) if verified != tenant_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(verified), _) => Ok(CallerScope::Company(verified.clone())),
        (None, Some(tenant_company)) => Ok(CallerScope::Company(tenant_company.clone())),
        (None, None) => Ok(CallerScope::decide(None, req, verified.is_admin())),
    }
}

/// Verify a request body's `company_id` against the caller's
/// company scope and return the company id to store. Fails closed
/// (403) when the body names a company other than the caller's.
/// When the body omits the company, the caller's scope is adopted
/// so the record is never stored unscoped under a scoped caller.
/// A platform caller may store any body company (or none); an
/// unresolved caller may not claim a company at all and stores a
/// global row when the body omits one.
pub(crate) fn require_company_match(
    tenant: &tenancy::TenantContext,
    req: &HttpRequest,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    let caller = caller_company(tenant, req)?;
    require_company_match_for(&caller, body_company_id)
}

/// `require_company_match` against an already-resolved caller
/// scope, for handlers that computed `caller_company` first.
pub(crate) fn require_company_match_for(
    caller: &CallerScope,
    body_company_id: &Option<String>,
) -> UnifiedResult<Option<String>> {
    match (caller, body_company_id) {
        (CallerScope::Platform, body) => Ok(body.clone()),
        (CallerScope::Company(company), Some(body)) if body != company => Err(
            UnifiedError::forbidden("company_id does not match the caller's company scope"),
        ),
        (CallerScope::Company(company), _) => Ok(Some(company.clone())),
        (CallerScope::Unresolved, Some(_)) => Err(UnifiedError::forbidden(
            "company_id does not match the caller's company scope",
        )),
        (CallerScope::Unresolved, None) => Ok(None),
    }
}

/// True when a stored record belongs to the caller's company
/// scope, under the shared `CallerScope` rule: platform reads any
/// record; a company caller reads their own company's rows plus
/// global (unscoped) rows; an unresolved caller reads global rows
/// ONLY. There is no "no scope matches everything" arm: with no
/// resolved company, company-owned records do NOT match — the old
/// no-scope-matches-everything shape let any caller without a company read
/// every company's records.
pub(crate) fn in_company_scope(record_company_id: &Option<String>, scope: &CallerScope) -> bool {
    scope.can_access_record(record_company_id.as_deref().unwrap_or(""))
}

/// Fail closed (403) when a stored record is outside the caller's
/// company scope. Single-record reads and mutations call this
/// right after loading the record; list handlers use
/// `in_company_scope` as a filter instead.
pub(crate) fn require_record_in_company_scope(
    record_company_id: &Option<String>,
    scope: &CallerScope,
) -> UnifiedResult<()> {
    if in_company_scope(record_company_id, scope) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "The record belongs to a different company",
        ))
    }
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the e-learning module).
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
    role: &str,
    permission: &str,
) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

/// True when the caller may circulate (loans / returns / holds):
/// the circulate capability or the manage capability.
pub(crate) fn can_circulate(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_LIBRARY_CIRCULATE)
        || check_capability(ctx, role, PERM_LIBRARY_MANAGE)
}

pub(crate) fn require_circulate(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_circulate(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_LIBRARY_CIRCULATE}\" or \"{PERM_LIBRARY_MANAGE}\""
        )))
    }
}

/// True when the caller may edit the catalog: the catalog
/// capability or the manage capability.
pub(crate) fn can_catalog(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_LIBRARY_CATALOG)
        || check_capability(ctx, role, PERM_LIBRARY_MANAGE)
}

pub(crate) fn require_catalog(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_catalog(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_LIBRARY_CATALOG}\" or \"{PERM_LIBRARY_MANAGE}\""
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::HttpMessage;
    use actix_web::test::TestRequest;
    use unified_core::middleware::{TenantContext as MwTenantContext, TenantKind};

    fn tenant_with_company(company: Option<&str>) -> tenancy::TenantContext {
        tenancy::TenantContext {
            tenant_id: Uuid::new_v4(),
            schema_name: "tenant_test".to_string(),
            company_id: company.map(str::to_string),
            site_id: None,
        }
    }

    /// Attach the server-verified caller the auth middleware would
    /// attach for this user and role to a request.
    fn attach_verified_caller(req: HttpRequest, role: &str) -> HttpRequest {
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(
                Uuid::from_u128(0x0000_0000_0000_4000_8000_0000_0000_0001),
                Vec::new(),
            )
            .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// A request carrying the server-verified caller the auth
    /// middleware would attach for this role.
    fn verified_request(role: &str) -> HttpRequest {
        attach_verified_caller(TestRequest::default().to_http_request(), role)
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
    fn caller_company_prefers_tenant_over_header() {
        let company = Uuid::new_v4().to_string();
        let tenant = tenant_with_company(Some(&company));
        let req = TestRequest::default()
            .insert_header(("x-company-id", company.clone()))
            .to_http_request();
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            CallerScope::Company(company)
        );
    }

    #[test]
    fn caller_company_ignores_header_without_tenant_company() {
        // The `X-Company-Id` header is client-asserted and is NOT
        // a scope source: with no tenant company it resolves to
        // `Unresolved`, never to the header's company.
        let company = Uuid::new_v4().to_string();
        let tenant = tenant_with_company(None);
        let req = TestRequest::default()
            .insert_header(("x-company-id", company))
            .to_http_request();
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            CallerScope::Unresolved
        );
    }

    #[test]
    fn caller_company_is_unresolved_without_tenant_company() {
        let tenant = tenant_with_company(None);
        let req = TestRequest::default().to_http_request();
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            CallerScope::Unresolved
        );
    }

    #[test]
    fn caller_company_platform_needs_control_plane_admin() {
        // No company, control-plane request, verified admin: Platform.
        let tenant = tenant_with_company(None);
        let req = verified_request("admin");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            CallerScope::Platform
        );
        // Same admin on a tenant host: Unresolved, not Platform.
        let req = attach_verified_caller(tenant_host_request(), "admin");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            CallerScope::Unresolved
        );
        // Non-admin on a control-plane request: Unresolved too.
        let req = verified_request("viewer");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            CallerScope::Unresolved
        );
    }

    #[test]
    fn require_company_match_rejects_cross_company_body() {
        let company = Uuid::new_v4().to_string();
        let tenant = tenant_with_company(Some(&company));
        let req = TestRequest::default().to_http_request();
        let other = Some(Uuid::new_v4().to_string());
        assert!(require_company_match(&tenant, &req, &other).is_err());
    }

    #[test]
    fn require_company_match_adopts_caller_scope_when_body_omitted() {
        let company = Uuid::new_v4().to_string();
        let tenant = tenant_with_company(Some(&company));
        let req = TestRequest::default().to_http_request();
        assert_eq!(
            require_company_match(&tenant, &req, &None).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn require_company_match_accepts_matching_body() {
        let company = Uuid::new_v4().to_string();
        let tenant = tenant_with_company(Some(&company));
        let req = TestRequest::default().to_http_request();
        assert_eq!(
            require_company_match(&tenant, &req, &Some(company.clone())).unwrap(),
            Some(company)
        );
    }

    #[test]
    fn require_company_match_unresolved_caller_claims_no_company() {
        let tenant = tenant_with_company(None);
        let req = TestRequest::default().to_http_request();
        // Naming a company is refused; omitting one stores global.
        assert!(require_company_match(&tenant, &req, &Some("company-a".to_string())).is_err());
        assert_eq!(require_company_match(&tenant, &req, &None).unwrap(), None);
    }

    #[test]
    fn in_company_scope_follows_shared_caller_scope() {
        let company = CallerScope::Company("company-a".to_string());
        assert!(in_company_scope(&Some("company-a".to_string()), &company));
        assert!(!in_company_scope(&Some("company-b".to_string()), &company));
        // A company caller reads global (unscoped) rows too, per
        // the shared `CallerScope` rule.
        assert!(in_company_scope(&None, &company));
        // Platform reads any record.
        assert!(in_company_scope(
            &Some("company-b".to_string()),
            &CallerScope::Platform
        ));
        assert!(in_company_scope(&None, &CallerScope::Platform));
        // Unresolved reads global rows ONLY: company-owned records
        // do NOT match (the old no-scope-matches-everything behaviour is gone).
        assert!(in_company_scope(&None, &CallerScope::Unresolved));
        assert!(!in_company_scope(
            &Some("company-b".to_string()),
            &CallerScope::Unresolved
        ));
    }

    #[test]
    fn require_record_in_company_scope_rejects_other_company() {
        let company = CallerScope::Company("company-a".to_string());
        assert!(require_record_in_company_scope(&Some("company-a".to_string()), &company).is_ok());
        assert!(require_record_in_company_scope(&Some("company-b".to_string()), &company).is_err());
        assert!(require_record_in_company_scope(&None, &company).is_ok());
        assert!(
            require_record_in_company_scope(
                &Some("company-a".to_string()),
                &CallerScope::Unresolved
            )
            .is_err()
        );
        assert!(require_record_in_company_scope(&None, &CallerScope::Unresolved).is_ok());
    }
}
