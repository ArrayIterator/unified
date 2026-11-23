// Inbox - Caller identity and permission guards.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::{self, TenantContext};
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module.
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
/// or the resolved tenant id is not a UUID (403). Every
/// tenant-data handler in this module calls this FIRST, before
/// caller identity or any capability check, so an unresolved
/// request never reaches a store or an emit. There is no fallback
/// to the `public` schema and no default tenant (same shape as the
/// library module's `require_tenant`; see
/// `unified_core::tenancy` (`TENANT_SCOPE_CONTRACT`)).
///
/// The verified caller's server-resolved company id, when
/// present, is verified against the resolved tenant context: a
/// malformed value is rejected (400), and a value that does not
/// match the context's company id is rejected (403). The
/// middleware only resolves a tenant when it could verify a
/// company id, so a resolved context always carries one; a
/// context without a verifiable company id fails closed rather
/// than trusting any client-asserted claim. The `X-Company-Id`
/// header is never read here.
pub(crate) fn require_tenant(req: &HttpRequest) -> UnifiedResult<TenantContext> {
    let tenant = tenancy::TenantContext::from_http_request(req)?;
    if let Some(claimed) = resolve_company_id(req)? {
        let context_company = tenant
            .company_id
            .as_deref()
            .and_then(|raw| Uuid::parse_str(raw).ok())
            .ok_or_else(|| {
                UnifiedError::forbidden(
                    "Tenant context carries no verifiable company id; refusing the company-scoped request",
                )
            })?;
        if claimed != context_company {
            return Err(UnifiedError::forbidden(
                "Verified caller company does not match the resolved tenant's company",
            ));
        }
    }
    Ok(tenant)
}

/// Owning company, resolved from the verified caller's
/// server-resolved company when present; `Ok(None)` when the
/// caller has none. A present but malformed value is rejected
/// (400) instead of silently ignored, so a typo'd company claim
/// can never widen access.
pub(crate) fn resolve_company_id(req: &HttpRequest) -> UnifiedResult<Option<Uuid>> {
    let Some(raw) = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(None);
    };
    Uuid::parse_str(raw.trim())
        .map(Some)
        .map_err(|_| UnifiedError::bad_request(format!("Invalid company id: {raw}")))
}

/// True when a stored record belongs to the caller's tenant
/// scope: the tenant id must match exactly, and the company must
/// match whenever both the record and the scope carry one. A
/// nil-tenant record (unscoped ingestion) never matches a real
/// tenant, so it is never visible via the API.
pub(crate) fn tenant_visible(
    record_tenant: Uuid,
    record_company: Option<&str>,
    scope: &TenantContext,
) -> bool {
    if record_tenant != scope.tenant_id {
        return false;
    }
    match (record_company, scope.company_id.as_deref()) {
        (Some(record), Some(scope_company)) => record == scope_company,
        _ => true,
    }
}

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the events module's capability check).
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

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!("{field} must not be empty")));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(value: &Option<String>, field: &str, max: usize) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
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


#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::middleware::{TenantContext as ResolvedTenantContext, TenantKind};
    use actix_web::test::TestRequest;
    use serde_json::json;

    /// Map a stable test name to a fixed UUID, so a test user keeps
    /// one identity everywhere: a UUID string parses as-is, any
    /// other name hashes (FNV-1a folded into the low 64 bits, with
    /// the version/variant bits set so the result is a valid v4
    /// UUID). Two different names never map to the same UUID.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        if let Ok(parsed) = uuid::Uuid::parse_str(name) {
            return parsed;
        }
        let mut hasher = DefaultHasher::new();
        name.hash(&mut hasher);
        let low = hasher.finish();
        uuid::Uuid::from_u128(
            ((low as u128) << 64) | 0x0000_0000_0000_4000_8000_0000_0000_0000,
        )
    }

    /// A request that passed tenant resolution: the middleware
    /// `TenantContext` is attached to the request extensions, with
    /// the tenant id itself as the company id (the tenants-module
    /// convention). The caller is the verified tester with the
    /// admin role, attached the way the auth middleware would, and
    /// carries `verified_company` as its server-resolved company
    /// (absent = no company resolved). `forged_header`, when given,
    /// sets a forged `X-Company-Id` header: it must never affect
    /// the outcome, because the code under test reads the verified
    /// caller, not the header.
    fn tenant_request(
        tenant_id: Uuid,
        verified_company: Option<&str>,
        forged_header: Option<&str>,
    ) -> HttpRequest {
        let mut request = TestRequest::default();
        if let Some(header) = forged_header {
            request = request.insert_header(("x-company-id", header));
        }
        let req = request.to_http_request();
        {
            use actix_web::HttpMessage;
            let mut verified = unified_core::auth::Caller::verified(
                test_user_uuid("tester"),
                Vec::new(),
            )
            .with_roles(vec!["admin".to_string()]);
            if let Some(company) = verified_company {
                verified = verified.with_company(company.to_string());
            }
            req.extensions_mut().insert(verified);
        }
        req.extensions_mut().insert(ResolvedTenantContext {
            tenant_id: tenant_id.to_string(),
            company_id: tenant_id.to_string(),
            site_id: Uuid::now_v7().to_string(),
            kind: TenantKind::Subdomain,
            host: "tenant.example.com".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        });
        req
    }

    #[test]
    fn require_tenant_rejects_request_without_tenant_context() {
        let req = TestRequest::default().to_http_request();
        assert!(require_tenant(&req).is_err());
    }

    #[test]
    fn require_tenant_accepts_resolved_tenant() {
        let tenant_id = Uuid::now_v7();
        let req = tenant_request(tenant_id, None, None);
        let scope = require_tenant(&req).expect("resolved tenant is accepted");
        assert_eq!(scope.tenant_id, tenant_id);
    }

    #[test]
    fn require_tenant_accepts_matching_verified_caller_company() {
        let tenant_id = Uuid::now_v7();
        let req = tenant_request(tenant_id, Some(&tenant_id.to_string()), None);
        assert!(require_tenant(&req).is_ok());
    }

    #[test]
    fn require_tenant_rejects_mismatched_verified_caller_company() {
        let tenant_id = Uuid::now_v7();
        let other_company = Uuid::now_v7();
        let req = tenant_request(tenant_id, Some(&other_company.to_string()), None);
        assert!(require_tenant(&req).is_err());
    }

    #[test]
    fn require_tenant_rejects_malformed_verified_caller_company() {
        let tenant_id = Uuid::now_v7();
        let req = tenant_request(tenant_id, Some("not-a-uuid"), None);
        assert!(require_tenant(&req).is_err());
    }

    #[test]
    fn require_tenant_ignores_a_forged_company_header() {
        let tenant_id = Uuid::now_v7();
        let other_company = Uuid::now_v7();
        let req = tenant_request(
            tenant_id,
            Some(&tenant_id.to_string()),
            Some(&other_company.to_string()),
        );
        assert!(require_tenant(&req).is_ok());
    }

    #[test]
    fn tenant_visible_matches_same_tenant_and_company() {
        let tenant_id = Uuid::now_v7();
        let scope = tenancy::TenantContext {
            tenant_id,
            schema_name: tenancy::schema_name(tenant_id),
            company_id: Some(tenant_id.to_string()),
            site_id: None,
        };
        assert!(tenant_visible(
            tenant_id,
            Some(&tenant_id.to_string()),
            &scope
        ));
        assert!(tenant_visible(tenant_id, None, &scope));
    }

    #[test]
    fn tenant_visible_rejects_other_tenant_and_other_company() {
        let tenant_id = Uuid::now_v7();
        let other = Uuid::now_v7();
        let scope = tenancy::TenantContext {
            tenant_id,
            schema_name: tenancy::schema_name(tenant_id),
            company_id: Some(tenant_id.to_string()),
            site_id: None,
        };
        assert!(!tenant_visible(other, Some(&tenant_id.to_string()), &scope));
        assert!(!tenant_visible(tenant_id, Some(&other.to_string()), &scope));
        // Unscoped (nil-tenant) records are never visible.
        assert!(!tenant_visible(Uuid::nil(), None, &scope));
    }

    fn params(page: u32, per_page: u32) -> PaginationParams {
        PaginationParams {
            page: Some(page),
            per_page: Some(per_page),
            ..Default::default()
        }
    }

    #[test]
    fn paginate_first_page() {
        let page = paginate(vec![1, 2, 3], &params(1, 2));
        assert_eq!(page.items, vec![1, 2]);
        assert_eq!(page.pagination.total, 3);
        assert_eq!(page.pagination.total_pages, 2);
    }

    #[test]
    fn paginate_beyond_end_is_empty() {
        let page = paginate(vec![1, 2, 3], &params(3, 2));
        assert!(page.items.is_empty());
        assert_eq!(page.pagination.total, 3);
    }

    #[test]
    fn require_text_trims() {
        assert_eq!(require_text("  hi  ", "body", 10).unwrap(), "hi");
    }

    #[test]
    fn require_text_rejects_empty() {
        assert!(require_text("  ", "body", 10).is_err());
    }

    #[test]
    fn parse_uuid_round_trip() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "thread").unwrap(), id);
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        assert!(parse_uuid("nope", "thread").is_err());
    }
}
