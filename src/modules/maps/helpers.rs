// Map & Tagging — caller, capability, and tenant helpers.
//
// Mirrors `widgets::helpers` exactly (same fail-closed shapes):
// the caller is the server-verified JWT identity (never a header),
// capabilities come from the `role.check` hook, and tenant scope
// comes from the middleware-resolved tenant context.

use actix_web::HttpRequest;
use serde_json::json;
use unified_core::caller;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};

/// The verified caller, reduced to what this module needs.
#[derive(Debug, Clone)]
pub(crate) struct Caller {
    pub actor: String,
    pub role: String,
}

/// The server-verified caller: 401 when no token was verified for
/// this request. The user id is the JWT subject; the role is the
/// primary server-resolved role (never a header value).
pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
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

/// Fail closed with 403 when `role` does not hold `permission`.
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

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected and never falls back
/// to the `public` schema. This is the C3 guard: it runs FIRST in
/// every handler, before caller identity or any capability check.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree.
/// The `X-Company-Id` header is NEVER trusted here.
pub(crate) fn caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<Option<String>> {
    let verified = caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty());
    match (&tenant.company_id, &verified) {
        (Some(tenant_company), Some(verified_company)) if tenant_company != verified_company => {
            Err(UnifiedError::forbidden(
                "Tenant context company and verified caller company disagree",
            ))
        }
        (Some(tenant_company), _) => Ok(Some(tenant_company.clone())),
        (None, verified_company) => Ok(verified_company.clone()),
    }
}

/// A write requires a resolved company scope.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}
