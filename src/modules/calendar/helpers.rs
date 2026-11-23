//! calendar module - Caller, permissions and response helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::json;

/// Constant-time string comparison, so token verification does
/// not leak how many leading characters matched.
pub(crate) fn constant_time_eq(expected: &str, actual: &str) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .as_bytes()
        .iter()
        .zip(actual.as_bytes().iter())
        .fold(0u8, |acc, (left, right)| acc | (left ^ right))
        == 0
}

// ---------------------------------------------------------------------------
// Helpers: caller, capabilities, validation.
// ---------------------------------------------------------------------------

/// The tenant context for one request, resolved ONLY through
/// `unified_core::tenancy::TenantContext::from_http_request`
/// (aliased `TenancyContext` — the batch-17 tenancy plumbing,
/// parsed uuidv7 tenant id plus the `schema_name` derived ONLY by
/// `tenancy::schema_name()`). This is the single convergence point:
/// nothing else in this file derives a schema name, and no other
/// function resolves a tenant. Fail closed:
/// `from_http_request` answers 401 when the request did not
/// resolve to a tenant and 403 when the resolved tenant id is not
/// a valid UUID; no resolved tenant means no calendar access at
/// all, never a silent fall back to `public`. OPEN: control-plane
/// requests with no tenant get 403 here — serving the calendar on
/// the control plane needs an explicit policy decision, never a
/// silent `public` fallback.
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

/// The server-verified caller (see `unified_core::caller::Caller`):
/// the user id identifies the actor, the server-resolved role is the
/// role slug or id the capability check runs against.
/// resolved tenant context, never from a client header.
/// `schema_name` is carried (not just derived on demand) so the
/// future Diesel persistence layer takes it from exactly one place.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
    pub(crate) tenant_id: String,
    /// Derived ONLY by `tenancy::schema_name()` (see
    /// `resolve_tenant`). Unread until the Diesel persistence layer
    /// lands and takes it into `tenancy::with_tenant_connection`.
    #[allow(dead_code)]
    pub(crate) schema_name: String,
    /// Company id from the tenant context; empty means unscoped.
    pub(crate) company_id: Option<String>,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id.to_string(),
        schema_name: tenant.schema_name.clone(),
        company_id: tenant.company_id.clone(),
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

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 200)
}

pub(crate) fn created<T: Serialize>(data: T) -> HttpResponse {
    json::success_response(data, 201)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    json::error_response(&error, debug)
}

pub(crate) fn paginate<T: Serialize + Clone>(
    items: Vec<T>,
    params: &PaginationParams,
) -> PageResult<T> {
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
