// Shared helpers: callers, validation, pagination.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::HttpRequest;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

pub(crate) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(crate) fn now() -> DateTime<Utc> {
    Utc::now()
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

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai and events modules.
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

/// Ask the roles module whether `role` holds `permission` through
/// the `role.check` hook. Fails closed: no answer means not
/// allowed (same fail-closed shape as the ai module).
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
// ---------------------------------------------------------------------------
// Tenant edge guard (C3 pilot 4, gate-only).
// ---------------------------------------------------------------------------

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` / `authorize` or any
/// capability check.
///
/// This module's records carry no `company_id` / `tenant_id` scope
/// field, so the guard is a fail-closed gate only: no per-record
/// company check is applied. Real partitioning of these records
/// needs a `tenant_id` on the record (C11 follow-up).
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// Company scope of the verified caller: the tenant context's
/// company first, the server-resolved caller company second; a
/// mismatch between the two is a 403. The `X-Company-Id` header is
/// never read. Resolved in every gated handler so a tenant/caller
/// mismatch fails closed even though these records carry no company
/// field to filter on (gate-only; see `require_tenant`).
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

pub(crate) fn valid_slug(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

pub(crate) fn valid_field_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
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

pub(crate) fn validate_module_list(modules: &[String]) -> UnifiedResult<Vec<String>> {
    let mut out: Vec<String> = Vec::new();
    for module_id in modules {
        let cleaned = module_id.trim().to_string();
        if !valid_slug(&cleaned) {
            return Err(UnifiedError::bad_request(format!(
                "Module id \"{module_id}\" must be a lowercase slug"
            )));
        }
        if !out.contains(&cleaned) {
            out.push(cleaned);
        }
    }
    Ok(out)
}

pub(crate) fn sort_desc_by_created<T>(items: &mut Vec<T>, created_at: fn(&T) -> DateTime<Utc>) {
    items.sort_by(|a, b| created_at(b).cmp(&created_at(a)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_slug_accepts_lowercase_digits_dash_underscore() {
        assert!(valid_slug("consent-type_1"));
        assert!(valid_slug("a"));
        assert!(valid_slug(&"x".repeat(128)));
    }

    #[test]
    fn valid_slug_rejects_empty_upper_space_and_too_long() {
        assert!(!valid_slug(""));
        assert!(!valid_slug("UPPER"));
        assert!(!valid_slug("has space"));
        assert!(!valid_slug("has.dot"));
        assert!(!valid_slug(&"x".repeat(129)));
    }

    #[test]
    fn valid_field_name_accepts_lowercase_digits_underscore() {
        assert!(valid_field_name("full_name"));
        assert!(valid_field_name("field1"));
        assert!(valid_field_name(&"x".repeat(64)));
    }

    #[test]
    fn valid_field_name_rejects_dash_upper_and_too_long() {
        assert!(!valid_field_name(""));
        assert!(!valid_field_name("has-dash"));
        assert!(!valid_field_name("UPPER"));
        assert!(!valid_field_name(&"x".repeat(65)));
    }

    #[test]
    fn require_text_trims_and_enforces_bounds() {
        assert_eq!(require_text("  hello  ", "label", 10).unwrap(), "hello");
        assert!(require_text("", "label", 10).is_err());
        assert!(require_text("   ", "label", 10).is_err());
        assert!(require_text("toolongvalue", "label", 5).is_err());
    }
}
