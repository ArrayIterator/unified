// routes module — Redirect helpers.
//
// Split from the former single-file module `src/modules/routes.rs`.
// The module root (`routes.rs`) re-exports the public items, so the
// public API is unchanged.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::routes::{
    OverrideSource, RedirectEntry, RouteChange, RouteEntry, RouteRegistry,
    ROUTE_CHANGED_EVENT,
};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, RwLock};

// ---------------------------------------------------------------------------
// Redirect helper for the server wiring.
//
// `resolve_redirect` is the function the HTTP server calls before
// its normal routes: when it returns `Some(target)`, the server
// answers 301 with a `Location: <target>` header and never reaches
// the normal router. Because every stored redirect target is a
// route's current effective path, a redirect never points at another
// old path and redirects never chain. See the server.rs snippet in
// the coordinator report for the exact installation point.
// ---------------------------------------------------------------------------

/// Permission strings for this module's HTTP handlers.
pub(crate) const PERM_ROUTES_VIEW: &str = "routes.view";
pub(crate) const PERM_ROUTES_MANAGE: &str = "routes.manage";

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the ai module.
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
/// (same fail-closed shape as the ai module's capability check).
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

/// Resolve an incoming path to its redirect target, if the path is a
/// recorded old path of a renamed route.
pub fn resolve_redirect(registry: &RouteRegistry, path: &str) -> Option<String> {
    registry.resolve_redirect(path)
}

/// Build a 301 response for an old path, or `None` when the path is
/// not a recorded old path.
pub fn redirect_response(registry: &RouteRegistry, path: &str) -> Option<HttpResponse> {
    let target = registry.resolve_redirect(path)?;
    Some(
        HttpResponse::MovedPermanently()
            .insert_header(("Location", target))
            .finish(),
    )
}

/// Resolve a named route to its current effective path. Handlers and
/// templates must use this instead of embedding a literal path.
pub fn route_url(registry: &RouteRegistry, name: &str) -> String {
    registry.route_url(name)
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
    fn resolve_redirect_returns_none_on_a_fresh_registry() {
        let registry = RouteRegistry::new();
        assert_eq!(resolve_redirect(&registry, "/login"), None);
        assert_eq!(resolve_redirect(&registry, "/no-such-path"), None);
    }

    #[test]
    fn route_url_returns_the_default_path() {
        let registry = RouteRegistry::new();
        assert_eq!(route_url(&registry, "login"), "/login");
    }

    #[test]
    fn set_override_redirects_the_old_path() {
        let registry = RouteRegistry::new();
        registry
            .set_override("login", "/sign-in", OverrideSource::Settings)
            .unwrap();
        assert_eq!(
            resolve_redirect(&registry, "/login"),
            Some("/sign-in".to_string())
        );
    }

    #[test]
    fn set_override_changes_the_effective_url() {
        let registry = RouteRegistry::new();
        registry
            .set_override("login", "/sign-in", OverrideSource::Settings)
            .unwrap();
        assert_eq!(route_url(&registry, "login"), "/sign-in");
    }

    #[test]
    fn set_override_rejects_unknown_routes() {
        let registry = RouteRegistry::new();
        assert!(
            registry
                .set_override("no-such-route", "/x", OverrideSource::Settings)
                .is_err()
        );
    }

    #[test]
    fn caller_requires_a_verified_caller() {
        use actix_web::test::TestRequest;
        let req = verified_request("user-1", "admin");
        let caller = caller(&req).unwrap();
        assert_eq!(caller.actor, test_user_uuid("user-1").to_string());
        assert_eq!(caller.role, "admin");
        assert!(caller(&TestRequest::default().to_http_request()).is_err());
        assert!(caller(
            &TestRequest::default().to_http_request()
        )
        .is_err());
    }

    #[test]
    fn capability_check_fails_closed_without_a_role_check_listener() {
        use unified_core::config::AppConfig;
        use unified_core::hooks::HookBus;
        use unified_core::modules::ModuleRegistry;
        let ctx = AppContext::new(
            AppConfig::default(),
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        );
        assert!(!check_capability(&ctx, "admin", PERM_ROUTES_VIEW));
        assert!(!check_capability(&ctx, "admin", PERM_ROUTES_MANAGE));
        assert!(require_permission(&ctx, "admin", PERM_ROUTES_VIEW).is_err());
    }
}
