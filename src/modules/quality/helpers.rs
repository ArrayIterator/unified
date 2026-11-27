//! quality module - Caller, permissions, text checks and response helpers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::constants::MAX_SLUG_LEN;
use super::constants::PERM_QUALITY_INSPECT;
use super::constants::PERM_QUALITY_MANAGE;

// Helpers

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the events module, plus
/// the tenant id resolved from the request (never from a client
/// header). Every record this module creates carries that tenant
/// id, and every read filters on it through `tenant_matches`.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
    pub(crate) tenant_id: String,
}

/// Tenant scoping rule for stored records, mirroring the calendar
/// module: a record scoped to a different tenant is never visible
/// to this caller; a record with no tenant id (created before
/// tenant scoping, or by an event that carried no tenant) stays
/// visible until it is backfilled, so old data is not silently
/// hidden by the migration.
pub(crate) fn tenant_matches(record_tenant: &Option<String>, tenant_id: &str) -> bool {
    match record_tenant {
        Some(owner) => owner == tenant_id,
        None => true,
    }
}

/// True when the role holds the manage capability: a supervisor
/// for the purposes of inspector-assignment and CAPA checks.
pub(crate) fn is_supervisor(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_QUALITY_MANAGE)
}

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
/// The tenant id resolved here is carried on `Caller::tenant_id`
/// and stored on every record this module creates; reads filter
/// on it through `tenant_matches`, so one tenant's inspections,
/// alerts, CAPAs, points, and criteria are never visible to
/// another tenant. Records created before tenant scoping carry
/// `tenant_id: None` and stay visible (see `tenant_matches`).
pub(crate) fn resolve_tenant(req: &HttpRequest) -> UnifiedResult<TenancyContext> {
    TenancyContext::from_http_request(req)
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    // Tenant first: no resolved tenant context means fail closed,
    // never fall back to the public schema. The resolved tenant id
    // is kept on the caller so every handler can stamp it on new
    // records and filter reads by it.
    let tenant = resolve_tenant(req)?;
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller {
        actor,
        role,
        tenant_id: tenant.tenant_id.to_string(),
    })
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

/// True when the caller may inspect: the manage capability or the
/// inspector capability.
pub(crate) fn can_inspect(ctx: &AppContext, role: &str) -> bool {
    check_capability(ctx, role, PERM_QUALITY_MANAGE)
        || check_capability(ctx, role, PERM_QUALITY_INSPECT)
}

pub(crate) fn require_inspect(ctx: &AppContext, role: &str) -> UnifiedResult<()> {
    if can_inspect(ctx, role) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(format!(
            "Role \"{role}\" does not hold permission \"{PERM_QUALITY_MANAGE}\" or \"{PERM_QUALITY_INSPECT}\""
        )))
    }
}

pub(crate) fn parse_uuid(raw: &str, what: &str) -> UnifiedResult<Uuid> {
    Uuid::parse_str(raw).map_err(|_| UnifiedError::bad_request(format!("Invalid {what} id: {raw}")))
}

pub(crate) fn require_text(value: &str, field: &str, max: usize) -> UnifiedResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    if trimmed.len() > max {
        return Err(UnifiedError::bad_request(format!(
            "{field} must be at most {max} characters"
        )));
    }
    Ok(trimmed.to_string())
}

pub(crate) fn optional_text(
    value: &Option<String>,
    field: &str,
    max: usize,
) -> UnifiedResult<Option<String>> {
    match value {
        Some(raw) => Ok(Some(require_text(raw, field, max)?)),
        None => Ok(None),
    }
}

/// Validate a value against a settings list. The list is the live
/// admin-configured one - never a constant in this file.
pub(crate) fn require_in_list(
    value: &str,
    field: &str,
    allowed: &[String],
) -> UnifiedResult<String> {
    let checked = require_text(value, field, MAX_SLUG_LEN)?;
    if !allowed.iter().any(|item| item == &checked) {
        return Err(UnifiedError::bad_request(format!(
            "{field} \"{checked}\" is not allowed; allowed values: {}",
            allowed.join(", ")
        )));
    }
    Ok(checked)
}

/// Validate a whole list of values against a settings list.
pub(crate) fn require_all_in_list(
    values: &[String],
    field: &str,
    allowed: &[String],
) -> UnifiedResult<Vec<String>> {
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        out.push(require_in_list(value, field, allowed)?);
    }
    Ok(out)
}

/// Validate an admin-supplied settings list: non-empty, each item
/// a short slug, no duplicates.
pub(crate) fn require_settings_list(values: &[String], field: &str) -> UnifiedResult<Vec<String>> {
    if values.is_empty() {
        return Err(UnifiedError::bad_request(format!(
            "{field} must not be empty"
        )));
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let checked = require_text(value, field, MAX_SLUG_LEN)?;
        if out.contains(&checked) {
            return Err(UnifiedError::bad_request(format!(
                "{field} contains duplicate value \"{checked}\""
            )));
        }
        out.push(checked);
    }
    Ok(out)
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

pub(crate) fn opt_string(data: &Value, key: &str) -> Option<String> {
    data.get(key)
        .and_then(|value| value.as_str())
        .map(str::to_string)
}

pub(crate) fn parse_date_filter(raw: &str, field: &str) -> UnifiedResult<DateTime<Utc>> {
    raw.parse::<DateTime<Utc>>()
        .map_err(|_| UnifiedError::bad_request(format!("{field} must be an RFC 3339 timestamp")))
}

#[cfg(test)]
mod tests {
    use super::optional_text;
    use super::parse_uuid;
    use super::require_in_list;
    use super::require_text;
    use super::tenant_matches;
    use uuid::Uuid;

    #[test]
    fn require_text_trims() {
        assert_eq!(require_text("  hello  ", "name", 10).unwrap(), "hello");
    }

    #[test]
    fn require_text_rejects_empty_and_oversized() {
        assert!(require_text("", "name", 10).is_err());
        assert!(require_text("   ", "name", 10).is_err());
        assert!(require_text("toolongvalue", "name", 5).is_err());
    }

    #[test]
    fn optional_text_passes_through() {
        assert_eq!(
            optional_text(&Some("  x  ".to_string()), "note", 10).unwrap(),
            Some("x".to_string())
        );
        assert_eq!(optional_text(&None, "note", 10).unwrap(), None);
    }

    #[test]
    fn require_in_list_accepts_member() {
        let allowed = vec!["a".to_string(), "b".to_string()];
        assert_eq!(require_in_list("b", "kind", &allowed).unwrap(), "b");
        assert!(require_in_list("c", "kind", &allowed).is_err());
    }

    #[test]
    fn parse_uuid_accepts_valid() {
        let id = Uuid::new_v4();
        assert_eq!(parse_uuid(&id.to_string(), "id").unwrap(), id);
    }

    #[test]
    fn parse_uuid_rejects_garbage() {
        assert!(parse_uuid("not-a-uuid", "id").is_err());
    }

    #[test]
    fn tenant_matches_own_tenant() {
        assert!(tenant_matches(&Some("tenant-a".to_string()), "tenant-a"));
    }

    #[test]
    fn tenant_matches_rejects_other_tenant() {
        assert!(!tenant_matches(&Some("tenant-a".to_string()), "tenant-b"));
    }

    #[test]
    fn tenant_matches_legacy_unscoped_record_stays_visible() {
        assert!(tenant_matches(&None, "tenant-a"));
        assert!(tenant_matches(&None, "tenant-b"));
    }
}
