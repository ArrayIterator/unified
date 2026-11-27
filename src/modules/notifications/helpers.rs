// notifications module -- Shared helpers: callers, permissions, responses, and lookups.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use uuid::Uuid;

use super::*;

// Helpers

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

pub(crate) fn respond_created<T: Serialize>(result: UnifiedResult<T>, debug: bool) -> HttpResponse {
    match result {
        Ok(data) => json::success_response(data, 201),
        Err(error) => json::error_response(&error, debug),
    }
}

// Caller identity and capabilities (N1).
//
// Every HTTP handler authenticates the caller from the
// server-verified `unified_core::caller::Caller` the auth middleware
// inserts into request extensions after verifying the JWT/session -
// never from the `X-User-Id` / `X-Role` headers: no verified caller,
// no access. The acting user id is the identity all per-user reads
// and writes are scoped to; a caller may only reach another user's
// data when their role holds the admin capability below (checked
// through the `role.check` hook, failing closed -- no answer means
// not allowed). The admin role's `"*": ["*"]` wildcard grants it;
// grant it to any other role through the roles module to delegate
// notification administration.

/// Admin capability: cross-user access to notifications, delivery
/// attempts and preferences, and administration of templates and
/// audiences.
pub(crate) const PERM_NOTIFICATIONS_MANAGE: &str = "notifications.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, required for capability checks), exactly
/// like the supervision and BI modules.
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

/// Authenticate the caller and report whether they hold the admin
/// capability. Handlers call this first and fail the request when it
/// errors, so no handler ever runs unauthenticated.
pub(crate) fn authenticate(ctx: &AppContext, req: &HttpRequest) -> UnifiedResult<(Caller, bool)> {
    let caller = caller(req)?;
    let is_admin = check_capability(ctx, &caller.role, PERM_NOTIFICATIONS_MANAGE);
    Ok((caller, is_admin))
}
// Tenant edge guard (C3 pilot 4, gate-only).

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

/// Resolve the user id a caller may act on: an explicit request
/// value when one is given, otherwise the acting caller. A non-admin
/// caller may only act on their own id; anything else is forbidden.
pub(crate) fn resolve_actor_user_id(
    requested: Option<&str>,
    actor: &str,
    is_admin: bool,
) -> UnifiedResult<String> {
    match requested.map(str::trim).filter(|value| !value.is_empty()) {
        Some(user_id) if user_id != actor && !is_admin => Err(UnifiedError::forbidden(
            "Cannot access another user's notifications",
        )),
        Some(user_id) => Ok(user_id.to_string()),
        None => Ok(actor.to_string()),
    }
}

/// Substitute `{{ name }}` and `{{ user.name }}` placeholders in a
/// template. Values come from the variables map (dotted paths resolve
/// into nested objects). Unknown placeholders are left untouched.
/// Templates are admin-authored, so values are substituted as plain
/// text without HTML escaping.
pub(crate) fn render_template_text(template: &str, variables: &HashMap<String, Value>) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        match after.find("}}") {
            Some(end) => {
                let key = after[..end].trim();
                match lookup_variable(variables, key) {
                    Some(value) => out.push_str(&value_as_text(value)),
                    None => out.push_str(&format!("{{{{{key}}}}}")),
                }
                rest = &after[end + 2..];
            }
            None => {
                out.push_str(&rest[start..]);
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn lookup_variable<'a>(variables: &'a HashMap<String, Value>, path: &str) -> Option<&'a Value> {
    let mut current: Option<&Value> = None;
    for (index, part) in path.split('.').enumerate() {
        current = if index == 0 {
            variables.get(part)
        } else {
            current.and_then(|value| value.get(part))
        };
        if current.is_none() {
            return None;
        }
    }
    current
}

pub(crate) fn value_as_text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(text) => text.clone(),
        Value::Bool(flag) => flag.to_string(),
        Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

pub(crate) fn validate_channels(channels: &[String]) -> UnifiedResult<()> {
    if channels.is_empty() {
        return Err(UnifiedError::bad_request(
            "At least one channel must be enabled",
        ));
    }
    for channel in channels {
        if !NOTIFICATION_CHANNELS.contains(&channel.as_str()) {
            return Err(UnifiedError::bad_request(format!(
                "Channels must be a subset of: {}",
                NOTIFICATION_CHANNELS.join(", ")
            )));
        }
    }
    Ok(())
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

    #[test]
    fn render_template_substitutes_variables() {
        let mut vars = HashMap::new();
        vars.insert("name".to_string(), serde_json::json!("Ada"));
        assert_eq!(
            render_template_text("Hello {{name}}!", &vars),
            "Hello Ada!"
        );
    }

    #[test]
    fn render_template_supports_nested_paths_and_numbers() {
        let mut vars = HashMap::new();
        vars.insert("user".to_string(), serde_json::json!({"name": "Ada"}));
        vars.insert("count".to_string(), serde_json::json!(3));
        assert_eq!(
            render_template_text("{{user.name}} has {{count}} messages.", &vars),
            "Ada has 3 messages."
        );
    }

    #[test]
    fn render_template_keeps_unknown_and_unclosed() {
        let vars = HashMap::new();
        assert_eq!(
            render_template_text("{{missing}} stays", &vars),
            "{{missing}} stays"
        );
        assert_eq!(
            render_template_text("Hello {{name", &vars),
            "Hello {{name"
        );
    }

    #[test]
    fn value_as_text_covers_json_scalars() {
        assert_eq!(value_as_text(&serde_json::json!(null)), "");
        assert_eq!(value_as_text(&serde_json::json!("x")), "x");
        assert_eq!(value_as_text(&serde_json::json!(true)), "true");
        assert_eq!(value_as_text(&serde_json::json!(42)), "42");
        assert_eq!(value_as_text(&serde_json::json!([1, 2])), "[1,2]");
    }

    #[test]
    fn validate_channels_requires_known_channels() {
        assert!(validate_channels(&[]).is_err());
        assert!(validate_channels(&["smoke".to_string()]).is_err());
        assert!(validate_channels(&["in_app".to_string(), "email".to_string()]).is_ok());
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        let no_headers = TestRequest::default().to_http_request();
        assert!(caller(&no_headers).is_err());
        let no_role = TestRequest::default().to_http_request();
        assert!(caller(&no_role).is_err());
        let no_user = TestRequest::default().to_http_request();
        assert!(caller(&no_user).is_err());
        let blank_user = TestRequest::default().to_http_request();
        assert!(caller(&blank_user).is_err());
    }

    #[test]
    fn caller_reads_actor_and_role() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-1", "member");
        let caller = caller(&req).expect("headers are present");
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "member");
    }

    #[test]
    fn resolve_actor_user_id_scopes_to_the_caller() {
        assert_eq!(
            resolve_actor_user_id(None, "user-1", false).unwrap(),
            "user-1"
        );
        assert_eq!(
            resolve_actor_user_id(Some("user-1"), "user-1", false).unwrap(),
            "user-1"
        );
        // An admin may name another user; a non-admin may not.
        assert_eq!(
            resolve_actor_user_id(Some("user-2"), "user-1", true).unwrap(),
            "user-2"
        );
        assert!(resolve_actor_user_id(Some("user-2"), "user-1", false).is_err());
        // A blank request value falls back to the caller.
        assert_eq!(
            resolve_actor_user_id(Some("   "), "user-1", false).unwrap(),
            "user-1"
        );
    }
}
