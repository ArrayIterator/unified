// Whitelabel module - shared helpers.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::{error_response, success_response};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use serde::Serialize;
use serde_json::{json, Value};

use super::constants::{MAX_COMPANY_ID_LEN, MAX_TENANT_ID_LEN};

// ---------------------------------------------------------------------------
// Caller identity + capability checks (portals / ai module shape)
// ---------------------------------------------------------------------------

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

/// The caller's company, resolved server-side from the verified
/// caller's server-resolved company id. The `X-Company-Id` header
/// is NEVER trusted here: it is client-asserted, so trusting it
/// would let any JWT caller claim another company's branding.
/// No resolvable company is unauthorized: branding is always
/// company-scoped, so there is no safe unscoped fallback.
pub(crate) fn caller_company(req: &HttpRequest) -> UnifiedResult<String> {
    let company = unified_core::caller::caller_from(req)
        .company_id
        .filter(|value| !value.is_empty())
        .ok_or_else(|| UnifiedError::unauthorized("No company for the verified caller"))?;
    if company.chars().count() > MAX_COMPANY_ID_LEN {
        return Err(UnifiedError::bad_request(
            "Company id must be at most 128 characters",
        ));
    }
    Ok(company)
}

/// Optional tenant inside the company, from `X-Tenant-Id`. Absent
/// means the company default profile (no tenant override).
pub(crate) fn caller_tenant(req: &HttpRequest) -> UnifiedResult<Option<String>> {
    let tenant = req
        .headers()
        .get("x-tenant-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if let Some(tenant) = &tenant {
        if tenant.chars().count() > MAX_TENANT_ID_LEN {
            return Err(UnifiedError::bad_request(
                "X-Tenant-Id must be at most 128 characters",
            ));
        }
    }
    Ok(tenant)
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

pub(crate) fn require_permission(ctx: &AppContext, role: &str, permission: &str) -> UnifiedResult<()> {
    if check_capability(ctx, role, permission) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{permission}\""
        )))
    }
}

pub(crate) fn parse_company_id(raw: &str) -> UnifiedResult<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_COMPANY_ID_LEN {
        return Err(UnifiedError::bad_request(
            "company_id must not be empty and must be at most 128 characters",
        ));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn page_of<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> Value {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items: Vec<T> = if start < items.len() {
        items[start..end].to_vec()
    } else {
        Vec::new()
    };
    PageResult::new(page_items, total, &pagination).to_json()
}

pub(crate) fn ok<T: Serialize>(data: T) -> HttpResponse {
    success_response(data, 200)
}

pub(crate) fn err(error: UnifiedError, debug: bool) -> HttpResponse {
    error_response(&error, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verified_request(company: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        if !company.is_empty() {
            req.extensions_mut().insert(
                unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                    .with_company(company.to_string()),
            );
        }
        req
    }

    #[test]
    fn caller_company_uses_the_verified_caller_company() {
        let req = verified_request("company-a");
        assert_eq!(caller_company(&req).unwrap(), "company-a");
    }

    #[test]
    fn caller_company_ignores_a_forged_company_header() {
        let req = actix_web::test::TestRequest::default()
            .insert_header(("x-company-id", "company-forged"))
            .to_http_request();
        assert!(caller_company(&req).is_err());
    }

    #[test]
    fn caller_company_is_unauthorized_without_a_verified_company() {
        let req = verified_request("");
        assert!(caller_company(&req).is_err());
    }
}
