// Saved Views - Caller identity, capability and ownership checks.
//
// Mirrors the guard pattern used by webhooks, field_service and
// admissions: the caller identity comes from the `X-User-Id` and
// `X-Role` headers, and the capability is checked through the
// `role.check` hook, failing closed when no handler answers.
//
// Ownership: a saved view belongs to exactly one user. Reads are
// allowed for the owner and for any caller when the view is shared
// (`is_visible_to`); every mutation (update, delete, set-default)
// requires ownership.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use actix_web::HttpRequest;
use serde_json::json;

use super::{SavedView, is_visible_to};

// Permissions.

pub(crate) const PERM_SAVED_VIEWS_VIEW: &str = "saved_views.view";

pub(crate) const PERM_SAVED_VIEWS_MANAGE: &str = "saved_views.manage";

// Caller identity and capability checks.

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the webhooks module.
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
/// the `role.check` hook. Fails closed: no answer means not allowed
/// (same fail-closed shape as the webhooks capability check).
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

// Tenant edge guard (C3 pilot 4, group E).

/// Fail-closed tenant gate: resolves the tenant context the
/// tenant-resolution middleware attached to the request. A request
/// with no resolved tenant context is rejected (401) and never falls
/// back to the `public` schema (the shared `TenantContext` contract
/// in `unified_core::tenancy`). Every tenant-data handler in this
/// module calls this FIRST, before `caller` or any capability check.
///
/// Records in this module carry no company field, so the guard is a
/// fail-closed gate only - there is no per-record company check to
/// apply. Real partitioning needs a `tenant_id` on the record, a
/// C11 follow-up, not a C3 gate.
pub(crate) fn require_tenant(
    req: &HttpRequest,
) -> UnifiedResult<unified_core::tenancy::TenantContext> {
    unified_core::tenancy::TenantContext::from_http_request(req)
}

/// The company scope the caller acts in: the tenant context's
/// company first, the verified caller's server-resolved company
/// otherwise. Fails closed (403) when both are present and disagree
/// - a request must not carry one company in its tenant context and
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

/// Writes require a resolved company: a mutating handler with no
/// resolvable company scope is refused (403). Reads proceed on the
/// tenant gate plus the mismatch check above - records here carry
/// no company field, so there is nothing to partition on read.
pub(crate) fn require_caller_company(
    tenant: &unified_core::tenancy::TenantContext,
    req: &HttpRequest,
) -> UnifiedResult<String> {
    caller_company(tenant, req)?
        .ok_or_else(|| UnifiedError::forbidden("A write requires a resolved company scope"))
}

// Ownership and visibility checks.

/// Only the view's owner may change or delete it. A shared view is
/// readable by anyone, but ownership never transfers with sharing.
pub(crate) fn require_view_owner(view: &SavedView, actor: &str) -> UnifiedResult<()> {
    if view.user_id == actor {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Saved view belongs to another user",
        ))
    }
}

/// A view may be read when it is visible to the caller: their own
/// view, or any shared view. Anonymous callers see shared views only.
pub(crate) fn require_view_visible(
    view: &SavedView,
    requester_id: Option<&str>,
) -> UnifiedResult<()> {
    if is_visible_to(view, requester_id) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Saved view is not visible to this user",
        ))
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
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;
    use actix_web::test::TestRequest;
    use chrono::Utc;
    use serde_json::json;
    use std::sync::Arc;

    fn test_context() -> AppContext {
        AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn view(owner: &str, visibility: &str) -> SavedView {
        let now = Utc::now();
        SavedView {
            id: "v1".to_string(),
            user_id: owner.to_string(),
            module_id: "saved_views".to_string(),
            name: "Open tickets".to_string(),
            filter: json!({"status": "open"}),
            sort: None,
            order: Some("asc".to_string()),
            visibility: visibility.to_string(),
            is_default: false,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let req = verified_request("user-1", "admin");
        let found = caller(&req).unwrap();
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn caller_rejects_an_anonymous_request() {
        let req = TestRequest::default().to_http_request();
        let err = caller(&req).unwrap_err();
        assert_eq!(err.code, 401);
    }

    #[test]
    fn require_permission_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(!check_capability(&ctx, "admin", "saved_views.view"));
        assert!(require_permission(&ctx, "admin", "saved_views.view").is_err());
    }

    #[test]
    fn require_permission_accepts_when_role_check_allows() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            std::sync::Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        assert!(require_permission(&ctx, "admin", "saved_views.view").is_ok());
    }

    #[test]
    fn require_view_owner_allows_the_owner() {
        let v = view("user-1", "private");
        assert!(require_view_owner(&v, "user-1").is_ok());
    }

    #[test]
    fn require_view_owner_rejects_another_user() {
        let v = view("user-1", "private");
        let err = require_view_owner(&v, "user-2").unwrap_err();
        assert_eq!(err.code, 403);
    }

    #[test]
    fn require_view_owner_rejects_another_user_even_when_shared() {
        // Sharing grants read access, never ownership: a shared view
        // still cannot be changed or deleted by a non-owner.
        let v = view("user-1", "shared");
        assert!(require_view_owner(&v, "user-2").is_err());
    }

    #[test]
    fn require_view_visible_allows_owner_and_shared_for_others() {
        let own = view("user-1", "private");
        assert!(require_view_visible(&own, Some("user-1")).is_ok());
        let shared = view("user-1", "shared");
        assert!(require_view_visible(&shared, Some("user-2")).is_ok());
    }

    #[test]
    fn require_view_visible_rejects_private_view_for_others() {
        let v = view("user-1", "private");
        let err = require_view_visible(&v, Some("user-2")).unwrap_err();
        assert_eq!(err.code, 403);
    }

    #[test]
    fn require_view_visible_rejects_private_view_for_anonymous() {
        let v = view("user-1", "private");
        assert!(require_view_visible(&v, None).is_err());
    }

    #[test]
    fn require_view_visible_allows_shared_view_for_anonymous() {
        let v = view("user-1", "shared");
        assert!(require_view_visible(&v, None).is_ok());
    }

    /// Build a request carrying a resolved tenant context plus a
    /// verified caller, for the tenant-gate tests below.
    fn tenant_request(company_id: &str, caller_company: Option<&str>) -> HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        let verified = match caller_company {
            Some(company) => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new())
                .with_company(company.to_string()),
            None => unified_core::auth::Caller::verified(uuid::Uuid::now_v7(), Vec::new()),
        };
        req.extensions_mut().insert(verified);
        req.extensions_mut()
            .insert(unified_core::middleware::TenantContext {
                tenant_id: uuid::Uuid::now_v7().to_string(),
                company_id: company_id.to_string(),
                site_id: String::new(),
                kind: unified_core::middleware::TenantKind::Subdomain,
                host: "tenant.example.test".to_string(),
                scope: None,
                product_id: None,
                package_id: None,
                product_subdomain_id: None,
            });
        req
    }

    #[test]
    fn require_tenant_fails_closed_without_resolved_context() {
        let bare = actix_web::test::TestRequest::default().to_http_request();
        assert!(require_tenant(&bare).is_err());
    }

    #[test]
    fn caller_company_prefers_tenant_company() {
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(
            caller_company(&tenant, &req).unwrap(),
            Some(company.clone())
        );
        // No tenant company: the verified caller's company is the scope.
        let req = tenant_request("", Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(caller_company(&tenant, &req).unwrap(), Some(company));
    }

    #[test]
    fn caller_company_rejects_verified_caller_mismatch() {
        let req = tenant_request(
            &uuid::Uuid::now_v7().to_string(),
            Some(&uuid::Uuid::now_v7().to_string()),
        );
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(caller_company(&tenant, &req).is_err());
    }

    #[test]
    fn require_caller_company_refuses_writes_without_a_resolved_company() {
        let req = tenant_request("", None);
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert!(require_caller_company(&tenant, &req).is_err());
        let company = uuid::Uuid::now_v7().to_string();
        let req = tenant_request(&company, Some(&company));
        let tenant = require_tenant(&req).expect("tenant context must resolve");
        assert_eq!(require_caller_company(&tenant, &req).unwrap(), company);
    }
}
