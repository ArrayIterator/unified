// Capability checks and caller identity.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use serde::Serialize;
use serde_json::json;

use super::*;

// ---------------------------------------------------------------------------
// Capability checks and caller identity.
// ---------------------------------------------------------------------------

/// Fail when no role was supplied or the roles module does not grant
/// `permission` for it (the licensing module's
/// `require_licensing_capability` pattern: ask `role.check` on the
/// hook bus, fail closed on silence).
pub(crate) fn require_capability(
    ctx: &AppContext,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    let denied = || {
        UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))
    };
    let Some(role) = role.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(denied());
    };
    let outcome = ctx.hooks.emit(
        "role.check",
        json!({ "role": role, "permission": permission, "allowed": false }),
    );
    if outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(denied())
    }
}

/// Caller identity for the capability check only: `X-User-Id`
/// (actor) and `X-Role` (role slug), read from the request headers
/// and nowhere else. Query parameters (`?viewer_id=` /
/// `?viewer_role=`) are never identity: they are logged, cached,
/// and bookmarked, so a role slug named in a URL must not decide
/// what the caller may do. Either side may be absent, and every
/// downstream check fails closed on an absent side: no role means
/// no capability, and no viewer means no ownership. Tenant
/// identity is deliberately NOT part of this struct: it comes only
/// from `TenantScope::resolve` (the middleware-attached tenant
/// context), never from a caller-supplied `X-Company-Id` header.
/// See the module header for the known gap: role headers arrive as
/// trusted input until the auth extractor lands.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallerInfo {
    pub(crate) actor_id: Option<String>,
    pub(crate) role: Option<String>,
}

pub(crate) fn caller_of(req: &HttpRequest) -> CallerInfo {
    let verified = unified_core::caller::caller_from(req);
    CallerInfo {
        actor_id: verified
            .is_authenticated()
            .then(|| verified.user_id.clone()),
        role: verified.roles.first().cloned(),
    }
}

/// Admin caller identity: headers ONLY (`X-User-Id` actor,
/// `X-Role` role slug). Query parameters (`?viewer_role=` /
/// `?viewer_id=`) and body fields are never identity here: anyone
/// who can name a role slug in a URL could otherwise authorize
/// themselves. Both headers are required — the same rule as the
/// bank-transfer module's `admin_caller`: a caller missing either
/// header is anonymous and gets 401 before any capability is
/// consulted; a caller whose role lacks the capability gets 403
/// from `require_capability`, not from here.
pub(crate) fn admin_caller(req: &HttpRequest) -> UnifiedResult<CallerInfo> {
    let verified = unified_core::caller::require_caller(req)?;
    Ok(CallerInfo {
        actor_id: Some(verified.user_id.clone()),
        role: verified.roles.first().cloned(),
    })
}

// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 5, group A).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller_of` / `admin_caller` or
/// any capability check.
///
/// The tenant-dashboard, package-change, and AI handlers resolve
/// their company scope through `TenantScope::resolve` (which
/// requires a company in the context and refuses a requested
/// company that differs from it); the admin handlers in
/// `handlers_admin.rs` are the platform-admin surface and are
/// gate-only — they act on the body/query company by design, so no
/// caller-company match is applied there.
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
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Verified caller company does not match the tenant company scope",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified) => Ok(verified.clone()),
    }
}

/// Resolve the tenant namespace id for a grant row.
///
/// The tenants module uses the tenant id itself as the company id
/// (see `crate::modules::tenants::records`), so the resolution is:
/// an explicitly named tenant id must parse as a UUID; otherwise
/// the company id itself must parse as a UUID and IS the tenant
/// id. Anything else fails closed with 400 — the row is never
/// written with a defaulted or invented tenant id. There is no
/// separate company -> tenant lookup in the tree to consult.
pub(crate) fn resolve_grant_tenant_id(
    explicit: Option<&str>,
    company_id: &str,
) -> UnifiedResult<String> {
    if let Some(explicit) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        uuid::Uuid::parse_str(explicit).map_err(|_| {
            UnifiedError::bad_request("tenant_id must be a UUID (uuidv7 namespace id)")
        })?;
        return Ok(explicit.to_string());
    }
    if uuid::Uuid::parse_str(company_id.trim()).is_ok() {
        return Ok(company_id.trim().to_string());
    }
    Err(UnifiedError::bad_request(
        "tenant_id is required: company_id is not a tenant UUID, so no tenant namespace can be resolved for this grant",
    ))
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
    fn caller_of_reads_the_verified_caller_only() {
        let req = verified_request("user-1", "admin");
        let caller = caller_of(&req);
        assert_eq!(
            caller.actor_id.as_deref(),
            Some(test_user_uuid("user-1").to_string().as_str())
        );
        assert_eq!(caller.role.as_deref(), Some("admin"));
        // A role claimed only in the query string is ignored:
        // query parameters are logged, cached, and bookmarked.
        let req = TestRequest::default()
            .uri("/api/v1/entitlements/my-modules?viewer_id=spoofed&viewer_role=admin")
            .to_http_request();
        let caller = caller_of(&req);
        assert_eq!(caller.actor_id, None);
        assert_eq!(caller.role, None);
    }

    #[test]
    fn resolve_grant_tenant_id_prefers_explicit_uuid() {
        let tenant = uuid::Uuid::now_v7().to_string();
        let company = uuid::Uuid::now_v7().to_string();
        assert_eq!(
            resolve_grant_tenant_id(Some(&tenant), &company).unwrap(),
            tenant
        );
        assert!(resolve_grant_tenant_id(Some("not-a-uuid"), &company).is_err());
    }

    #[test]
    fn resolve_grant_tenant_id_falls_back_to_uuid_company_id() {
        let company = uuid::Uuid::now_v7().to_string();
        assert_eq!(resolve_grant_tenant_id(None, &company).unwrap(), company);
    }

    #[test]
    fn resolve_grant_tenant_id_fails_closed_for_non_uuid_company() {
        let error = resolve_grant_tenant_id(None, "acme").unwrap_err();
        assert_eq!(error.code, 400);
    }

    /// Request carrying the middleware-resolved tenant context the
    /// tenant-resolution middleware would attach, plus a verified
    /// caller (optionally with a server-resolved company).
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use unified_core::middleware::{TenantContext as ResolvedTenantContext, TenantKind};
        let req = TestRequest::default().to_http_request();
        req.extensions_mut().insert(ResolvedTenantContext {
            tenant_id: company_id.to_string(),
            company_id: company_id.to_string(),
            site_id: "site-1".to_string(),
            kind: TenantKind::Subdomain,
            host: "tenant.example.com".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        });
        let mut caller = unified_core::auth::Caller::verified(
            test_user_uuid("tester"),
            Vec::new(),
        )
        .with_roles(vec!["admin".to_string()]);
        if let Some(company) = caller_company {
            caller = caller.with_company(company.to_string());
        }
        req.extensions_mut().insert(caller);
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn require_tenant_resolves_from_middleware_context() {
        let req = tenant_request(&uuid::Uuid::now_v7().to_string(), None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(!tenant.tenant_id.to_string().is_empty());
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some("other"));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }
}
