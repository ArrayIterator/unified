// Global authentication middleware (inspection C1 + C2, 2026-11-22).
//
// Before this middleware existed there was no global auth: any
// unauthenticated caller could send `X-User-Id: <anyone>` +
// `X-Role: admin` and pass every capability check, because the
// per-module `caller()` helpers read those self-asserted headers
// and `src/http/server.rs` registered only security-headers,
// tenant-resolution, IP-filter, rate-limit and request-size
// middleware. This middleware closes that hole at the front door:
//
// * every request that is not on the public allowlist must carry
//   a valid Auth Manager JWT (Bearer header or session cookie),
//   verified with the ONE existing verifier -
//   `crate::modules::security::auth::verify_jwt`, the same HS256
//   HMAC-SHA256 check the security module uses for its own
//   endpoints (signing key `"{secret_key}:{salt_key}"`, pinned
//   `alg`, expiry and `iat`/`nbf` checks). No second verifier
//   lives here; the Auth Manager issues these tokens with the
//   same key (`AuthManagerState::signing_key`), so one verifier
//   covers both issuers.
// * a verified token becomes a server-verified `Caller`
//   (`unified_core::auth::Caller`) in the request extensions.
//   Handlers read it through `unified_core::auth::caller_from` -
//   never from headers.
// * the middleware also resolves the user's role slugs from the
//   database (`user_roles` / `roles`) and the user's company id
//   (`users.company_id`) and attaches both to the `Caller`, so
//   `role()` / `roles` return real values and the `role.check`
//   capability checks answer from the roles module's store instead
//   of failing closed on `"anonymous"` for every verified caller
// (inspection HIGH-1), and the handler-facing view
//   carries the server-resolved company (C3) instead of `None`.
//   The cost is three indexed lookups per authenticated request
//   (two for roles, one for the company); any failure - no pool
//   attached, no connection, any query error - resolves to no
//   roles and no company, so capability checks fail closed
//   exactly as they did before these lookups existed. A failed
//   lookup never fails authentication itself: the token is
//   already verified. Without a database pool (the in-memory
//   deployment) roles stay empty and the company stays `None`,
//   as before.
// * a verified token is also checked against the persisted
//   revocation store (`auth_revoked_tokens`, migration
//   `2026-11-22-000109`) - one indexed lookup per authenticated
//   request, on top of the role lookups - and a revoked token is
//   denied here, so logout and explicit revocation take effect on
//   every API route immediately, not only at the Auth Manager's
//   verify endpoint (inspection C13/H2, R2 HIGH-2). The check
//   consults the database table directly (the durable source of
//   truth) rather than the Auth Manager's in-memory map, which
//   this layer cannot reach and which can go stale for rows
//   another process wrote. Fail closed: when a pool is attached
//   and the lookup fails for any reason (no connection, query
//   error, table not migrated yet) authentication is denied - a
//   revocation status that cannot be determined is never treated
//   as "not revoked". Without a database pool (the in-memory
//   deployment) there is no revocation store to consult, so the
//   check is skipped and the token passes; revocation in that
//   deployment is best-effort only (the in-memory map the Auth
//   Manager keeps per process). Operators running with a database
//   must have migration `2026-11-22-000109` applied - it runs
//   automatically at startup when a database is configured.
// * invalid or missing tokens are rejected with the standard
//   error envelope (401, fail closed). The token itself is never
//   logged.
//
// What this middleware does NOT do (explicit follow-ups, not
// oversights):
// * Deleted/disabled-user checks: the security module re-checks
//   the user row on its own endpoints; this middleware does not
//   add a database round-trip to every request. Handlers that
//   need immediate deactivation enforcement must check.
// * Capability enforcement: the middleware enforces
//   authentication only. A route's required capability
//   (`RouteInfo::auth == RouteAuth::Capability(..)`) is enforced
//   in its handler via `Caller::has_scope` (the caller()
//   migration builder's job).
// * Tenant isolation (C3/C4) is a separate layer; this middleware
//   only copies the already-resolved tenant id onto the `Caller`.
//
// Priority 925: after tenant resolution (950) so the `Caller`
// can carry the resolved tenant id, before the IP filter (900)
// so unauthenticated requests are rejected before they consume
// rate-limit budget or reach module middleware.

use crate::modules::auth_manager::state::AuthManagerState;
use crate::modules::security::auth::{
    AuthSource as SecurityAuthSource, EXPECTED_JWT_ISSUER, extract_token_with_source, verify_jwt,
};
use actix_web::dev::ServiceRequest;
use actix_web::http::Method;
use actix_web::{HttpMessage, HttpRequest};
use std::sync::Arc;
use unified_core::auth::{Caller, CallerAuthSource, RouteAuth};
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json;
use unified_core::middleware::{Middleware, MiddlewareResult, NextFn, TenantContext};
use unified_core::modules::RouteInfo;
use uuid::Uuid;

/// A public route: (HTTP method, path template). The method is
/// `*` for any method. Templates use the Actix segment syntax:
/// `{name}` matches one path segment, `{name:.*}` matches the
/// rest of the path.
pub type PublicRoute = (&'static str, &'static str);

/// Routes that are public by construction, not by module
/// declaration. Each entry carries its reason in the comment
/// above it; nothing else may be added here without one.
///
/// These are also `RouteAuth::Public` in the route catalog where
/// a module declares them. The hardcoded copy exists so a catalog
/// regression (a module forgetting its `route_info`) can never
/// lock out login: without `jwt/issue` nobody can obtain a token
/// at all. The catalog remains the source of truth for every
/// other public route.
const HARDCODED_PUBLIC_ROUTES: &[PublicRoute] = &[
    // Liveness probe (also exempt from pause/maintenance in
    // server.rs). No data, no mutation.
    ("GET", "/health"),
    // ACME HTTP-01 challenge: the token is unguessable and
    // single-use, and blocking it would break certificate
    // issuance and renewal (see server.rs).
    ("GET", "/.well-known/acme-challenge/{token}"),
    // Fingerprinted static assets: public content by design,
    // immutable cache headers, no handler state.
    ("GET", "/assets/{path:.*}"),
    ("GET", "/theme/{id}/assets/{path:.*}"),
    // Login: password (+ TOTP/recovery code) is the credential
    // here; no token can exist yet. Issues the JWT every other
    // route requires.
    ("POST", "/api/v1/auth-manager/jwt/issue"),
    // Token verification endpoint (used by clients to check a
    // token they hold; takes the token itself as the credential).
    ("POST", "/api/v1/auth-manager/jwt/verify"),
    // Google OAuth entry points: the OAuth code/state flow is
    // the credential; no JWT can exist yet.
    ("GET", "/api/v1/auth-manager/google/authorize"),
    ("POST", "/api/v1/auth-manager/google/callback"),
    // SSO availability probe: reports only whether a Google
    // sign-in can start right now; no user data.
    ("GET", "/api/v1/auth-manager/google/sso/status"),
    // TOTP recovery-code check: the recovery code is the
    // credential, used during login when the second factor is
    // lost; no JWT can exist yet.
    ("POST", "/api/v1/auth-manager/recovery-codes/verify"),
    // Password recovery: the reset token (emailed link) is the
    // credential; no JWT can exist yet. Same class as the
    // recovery-code check above.
    ("POST", "/api/v1/auth-manager/password/forgot"),
    ("POST", "/api/v1/auth-manager/password/reset"),
];

/// True when `template` matches `path`. A `{name}` segment
/// matches exactly one path segment; a `{name:.*}` segment
/// matches the rest of the path (including slashes). Literal
/// segments must match exactly. A trailing slash on the request
/// path does not match a template without one.
pub fn template_matches(template: &str, path: &str) -> bool {
    let template_segments: Vec<&str> = template.split('/').collect();
    let path_segments: Vec<&str> = path.split('/').collect();
    let mut template_index = 0;
    let mut path_index = 0;
    while template_index < template_segments.len() {
        let template_segment = template_segments[template_index];
        if template_segment.starts_with('{') && template_segment.ends_with(".*}") {
            // Rest-of-path wildcard: matches whatever remains
            // (including nothing more).
            return true;
        }
        if path_index >= path_segments.len() {
            return false;
        }
        if template_segment.starts_with('{') && template_segment.ends_with('}') {
            // Single-segment parameter: matches any one segment.
        } else if template_segment != path_segments[path_index] {
            return false;
        }
        template_index += 1;
        path_index += 1;
    }
    path_index == path_segments.len()
}

/// True when the request path is public infrastructure that no
/// module declares in the route catalog (registered directly in
/// `server.rs`). Everything else public comes from the catalog.
fn is_infra_public(path: &str) -> bool {
    path == "/health"
        || path.starts_with("/.well-known/acme-challenge/")
        || path.starts_with("/assets/")
        || path.starts_with("/theme/") && path.contains("/assets/")
}

/// The public routes declared in the route catalog: every
/// `RouteInfo` whose `auth` is `RouteAuth::Public`, as
/// (method, path template). This is the module-declared public
/// surface - the `PUBLIC:` / `TOKEN-GATED:` /
/// `TOKEN-AUTHENTICATED:` routes, the public pages, the
/// signature-verified provider webhook receivers, and the public
/// storefront reads - and the ONLY catalog-derived source of
/// publicness. A route that no module declares public is
/// protected: undeclared routes fail closed, and a module that
/// wants a public route must declare it.
pub fn public_routes_from_catalog(catalog: &[RouteInfo]) -> Vec<PublicRoute> {
    catalog
        .iter()
        .filter(|route| route.auth == RouteAuth::Public)
        .map(|route| (route.method, route.path))
        .collect()
}

/// True when (`method`, `path`) matches an entry of the allowlist
/// (method `*` matches any method, templates match per
/// `template_matches`).
pub fn is_public_route(method: &Method, path: &str, public_routes: &[PublicRoute]) -> bool {
    if is_infra_public(path) {
        return true;
    }
    public_routes.iter().any(|(route_method, template)| {
        (*route_method == "*" || *route_method == method.as_str())
            && template_matches(template, path)
    })
}

/// Verify the caller's token and build the server-verified
/// `Caller`. This is the single verification path the middleware
/// uses: Bearer header first, session cookie second (see
/// `extract_token_with_source`), then the security module's
/// `verify_jwt`. Any failure is a 401 - fail closed - including
/// the case where authentication is not configured at all
/// (`verify_jwt` rejects empty keys). A verified token is then
/// checked against the persisted revocation store (see
/// `is_token_revoked`); a revoked token, or a token whose
/// revocation status cannot be determined while a pool is
/// attached, is denied the same way. The token value is never
/// logged. The verified caller's role slugs and company id are
/// resolved from the database and attached (see
/// `resolve_user_roles` and `resolve_user_company`); a failed
/// lookup attaches no roles and no company, it never fails
/// authentication.
pub fn authenticate_request(
    ctx: &AppContext,
    req: &HttpRequest,
    tenant: Option<&TenantContext>,
) -> UnifiedResult<Caller> {
    let (secret_key, salt_key, session_name) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.secret_key.clone(),
            config.app.salt_key.clone(),
            config.app.session_name.clone(),
        )
    };
    let (token, source) = extract_token_with_source(req, &session_name).ok_or_else(|| {
        UnifiedError::unauthorized("Authentication required: provide a Bearer token")
    })?;
    let claims = verify_jwt(&token, &secret_key, &salt_key, EXPECTED_JWT_ISSUER)?;
    // Revoked tokens are denied at the front door, not only at the
    // Auth Manager's verify endpoint: logout and explicit revoke
    // must take effect on every route immediately.
    match is_token_revoked(ctx, &claims.jti) {
        Ok(true) => {
            return Err(UnifiedError::unauthorized("token has been revoked"));
        }
        Ok(false) => {}
        Err(error) => {
            return Err(error);
        }
    }
    let user_id = Uuid::parse_str(claims.sub.trim())
        .map_err(|_| UnifiedError::unauthorized("Token subject is not a user id"))?;
    let mut caller = Caller::verified(user_id, claims.scopes);
    caller = caller.with_roles(resolve_user_roles(ctx, user_id));
    if let Some(company_id) = resolve_user_company(ctx, user_id) {
        caller = caller.with_company(company_id);
    }
    caller = caller.with_auth_source(match source {
        SecurityAuthSource::Bearer => CallerAuthSource::Bearer,
        SecurityAuthSource::Cookie => CallerAuthSource::Cookie,
    });
    if let Some(tenant) = tenant {
        caller = caller.with_tenant(tenant.tenant_id.clone());
    }
    Ok(caller)
}

/// Resolve the role slugs assigned to `user_id` from the database
/// (`user_roles` / `roles`), for the verified caller. Two indexed
/// lookups, only when a pool is attached. ANY failure - no pool,
/// no connection, any query error - resolves to no roles, so
/// capability checks fail closed exactly as they did before this
/// lookup existed. A failed lookup never fails authentication
/// itself: the token is already verified, and the actor identity
/// is real either way. Without a database pool (the in-memory
/// deployment) roles stay empty, as before.
fn resolve_user_roles(ctx: &AppContext, user_id: Uuid) -> Vec<String> {
    let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() else {
        return Vec::new();
    };
    let Ok(mut connection) = pool.get() else {
        return Vec::new();
    };
    crate::modules::users::db_role_slugs(&mut connection, user_id).unwrap_or_default()
}

/// Resolve the company id assigned to `user_id` from the database
/// (`users.company_id`), for the verified caller. One indexed
/// lookup, only when a pool is attached. ANY failure - no pool,
/// no connection, any query error - resolves to no company, so
/// company scoping fails closed exactly as it did before this
/// lookup existed (the caller simply has no company). A failed
/// lookup never fails authentication itself: the token is already
/// verified, and the actor identity is real either way. Without
/// a database pool (the in-memory deployment) the company stays
/// `None`, as before.
fn resolve_user_company(ctx: &AppContext, user_id: Uuid) -> Option<String> {
    let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() else {
        return None;
    };
    let Ok(mut connection) = pool.get() else {
        return None;
    };
    crate::modules::users::db_user_company(&mut connection, user_id)
        .ok()
        .flatten()
}

/// True when the token id `jti` is revoked in the persisted
/// revocation store (`auth_revoked_tokens`), consulted directly
/// from the database - the durable source of truth, shared with
/// the Auth Manager's verify path (see
/// `AuthManagerState::is_jti_revoked_in_db`). One indexed lookup,
/// only when a pool is attached. The error case is the fail-closed
/// one: when a pool is attached and the lookup fails (no
/// connection, query error, table not migrated yet) the caller
/// gets an error and authentication is denied - a revocation
/// status that cannot be determined is never treated as "not
/// revoked". Without a database pool there is no revocation store
/// to consult, so this returns `Ok(false)`: revocation cannot be
/// enforced in the in-memory deployment.
fn is_token_revoked(ctx: &AppContext, jti: &str) -> UnifiedResult<bool> {
    let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() else {
        return Ok(false);
    };
    AuthManagerState::is_jti_revoked_in_db(&pool, jti)
        .map_err(|_| UnifiedError::unauthorized("token revocation status could not be verified"))
}

/// Global authentication middleware. See the module header.
pub struct AuthMiddleware {
    ctx: Arc<AppContext>,
    /// Hardcoded public routes plus the public routes declared
    /// in the catalog at construction time.
    public_routes: Vec<PublicRoute>,
}

impl std::fmt::Debug for AuthMiddleware {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "AuthMiddleware ({} public routes)",
            self.public_routes.len()
        )
    }
}

impl AuthMiddleware {
    /// Build the middleware for `ctx`, snapshotting the public
    /// routes from the route catalog. The catalog is fixed at
    /// startup (modules register once), so a construction-time
    /// snapshot cannot go stale while the server runs.
    pub fn new(ctx: Arc<AppContext>) -> Self {
        let mut public_routes: Vec<PublicRoute> = HARDCODED_PUBLIC_ROUTES.to_vec();
        public_routes.extend(public_routes_from_catalog(&ctx.modules.route_catalog()));
        Self { ctx, public_routes }
    }

    /// The public-route allowlist this middleware enforces, for
    /// tests and for operators listing the unauthenticated
    /// surface. The hardcoded entries come first, then the
    /// catalog-derived ones.
    pub fn public_routes(&self) -> &[PublicRoute] {
        &self.public_routes
    }
}

impl Middleware for AuthMiddleware {
    /// After tenant resolution (950) so the `Caller` can carry
    /// the resolved tenant id, before the IP filter (900).
    fn priority(&self) -> isize {
        925
    }

    fn handle(&self, req: ServiceRequest, next: NextFn) -> MiddlewareResult {
        let ctx = self.ctx.clone();
        let debug = ctx.is_debug();
        let mut req = req;
        let method = req.method().clone();
        let path = req.path().to_string();
        let public = is_public_route(&method, &path, &self.public_routes);
        let tenant = TenantContext::from_service_request(&req);
        if public {
            // Public route: a valid token still attaches its
            // verified Caller (handlers read it through
            // `caller_from`); a missing or invalid token simply
            // leaves the caller anonymous. A stale cookie must
            // never break the login page or the public site, and
            // an anonymous caller holds no capability either way.
            if let Ok(caller) = authenticate_request(&ctx, req.request(), tenant.as_ref()) {
                req.extensions_mut().insert(caller);
            }
            return Box::pin(async move { next(req).await });
        }
        match authenticate_request(&ctx, req.request(), tenant.as_ref()) {
            Ok(caller) => {
                req.extensions_mut().insert(caller);
                Box::pin(async move { next(req).await })
            }
            Err(error) => {
                let response = json::error_response(&error, debug);
                Box::pin(async move { Ok(req.into_response(response)) })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;
    use base64::Engine as _;
    use hmac::Mac as _;
    use unified_core::auth::RouteAuth;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    fn test_context() -> AppContext {
        let mut config = AppConfig::default();
        config.app.secret_key = "test-secret".to_string();
        config.app.salt_key = "test-salt".to_string();
        config.app.session_name = "vault".to_string();
        AppContext::new(
            config,
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// Mint an HS256 JWT the way the Auth Manager issues one:
    /// signing key `"{secret}:{salt}"`, scopes from the caller.
    type TestHmac = hmac::Hmac<sha2::Sha256>;

    fn mint_token(secret: &str, salt: &str, sub: &str, scopes: &[&str]) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let now = chrono::Utc::now().timestamp();
        let payload = serde_json::json!({
            "sub": sub,
            "iss": "unified",
            "iat": now - 60,
            "exp": now + 3600,
            "jti": "test-jti",
            "scopes": scopes,
        });
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string());
        let signing_input = format!("{header}.{payload}");
        let mut mac = TestHmac::new_from_slice(format!("{secret}:{salt}").as_bytes())
            .expect("HMAC key is valid");
        mac.update(signing_input.as_bytes());
        let signature =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
        format!("{signing_input}.{signature}")
    }

    fn route(method: &'static str, path: &'static str, auth: RouteAuth) -> RouteInfo {
        RouteInfo {
            method,
            path,
            owner: "test",
            description: "test route",
            auth,
        }
    }

    #[test]
    fn template_matches_exact_and_parameterised_paths() {
        assert!(template_matches(
            "/api/v1/store/products",
            "/api/v1/store/products"
        ));
        assert!(!template_matches(
            "/api/v1/store/products",
            "/api/v1/store/products/123"
        ));
        assert!(template_matches(
            "/api/v1/store/products/{id}",
            "/api/v1/store/products/123"
        ));
        assert!(!template_matches(
            "/api/v1/store/products/{id}",
            "/api/v1/store/products"
        ));
        assert!(!template_matches(
            "/api/v1/store/products/{id}",
            "/api/v1/store/products/123/extra"
        ));
        assert!(template_matches(
            "/assets/{path:.*}",
            "/assets/app.abc123.js"
        ));
        assert!(template_matches("/assets/{path:.*}", "/assets/a/b/c.css"));
        assert!(template_matches(
            "/.well-known/acme-challenge/{token}",
            "/.well-known/acme-challenge/abc"
        ));
    }

    #[test]
    fn infra_public_covers_health_acme_and_assets_only() {
        assert!(is_infra_public("/health"));
        assert!(is_infra_public("/.well-known/acme-challenge/abc"));
        assert!(is_infra_public("/assets/app.js"));
        assert!(is_infra_public("/theme/default/assets/app.js"));
        assert!(!is_infra_public("/ws"));
        assert!(!is_infra_public("/api/v1/store/products"));
        assert!(!is_infra_public("/api/v1/mail/settings"));
    }

    #[test]
    fn catalog_public_routes_come_only_from_public_declarations() {
        let catalog = vec![
            route("GET", "/api/v1/announcements/public", RouteAuth::Public),
            route("POST", "/api/v1/store/products", RouteAuth::Authenticated),
            route("GET", "/api/v1/audit/entries", RouteAuth::Authenticated),
        ];
        let public = public_routes_from_catalog(&catalog);
        assert_eq!(public.len(), 1);
        assert_eq!(public[0], ("GET", "/api/v1/announcements/public"));
    }

    #[test]
    fn allowlist_admits_login_and_public_routes_but_not_c2_spots() {
        // The C2 named spots must never be public: store product
        // mutations, mail settings, audit log reads.
        let catalog = vec![
            route("POST", "/api/v1/store/products", RouteAuth::Authenticated),
            route(
                "PUT",
                "/api/v1/store/products/{id}",
                RouteAuth::Authenticated,
            ),
            route(
                "DELETE",
                "/api/v1/store/products/{id}",
                RouteAuth::Authenticated,
            ),
            route("POST", "/api/v1/store/checkout", RouteAuth::Authenticated),
            route("PUT", "/api/v1/mail/settings", RouteAuth::Authenticated),
            route("GET", "/api/v1/audit/entries", RouteAuth::Authenticated),
            route("GET", "/api/v1/announcements/public", RouteAuth::Public),
        ];
        let mut public: Vec<PublicRoute> = HARDCODED_PUBLIC_ROUTES.to_vec();
        public.extend(public_routes_from_catalog(&catalog));
        let post = Method::POST;
        let put = Method::PUT;
        let delete = Method::DELETE;
        let get = Method::GET;
        assert!(!is_public_route(&post, "/api/v1/store/products", &public));
        assert!(!is_public_route(
            &put,
            "/api/v1/store/products/123",
            &public
        ));
        assert!(!is_public_route(
            &delete,
            "/api/v1/store/products/123",
            &public
        ));
        assert!(!is_public_route(&post, "/api/v1/store/checkout", &public));
        assert!(!is_public_route(&put, "/api/v1/mail/settings", &public));
        assert!(!is_public_route(&get, "/api/v1/audit/entries", &public));
        assert!(is_public_route(
            &post,
            "/api/v1/auth-manager/jwt/issue",
            &public
        ));
        assert!(is_public_route(
            &get,
            "/api/v1/announcements/public",
            &public
        ));
        assert!(is_public_route(&get, "/health", &public));
    }

    #[test]
    fn authenticate_request_accepts_a_valid_token() {
        let ctx = test_context();
        let token = mint_token(
            "test-secret",
            "test-salt",
            "00000000-0000-0000-0000-000000000001",
            &["store:manage"],
        );
        let req = TestRequest::default()
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_http_request();
        let caller = authenticate_request(&ctx, &req, None).expect("valid token");
        assert!(caller.is_authenticated());
        assert_eq!(
            caller.user_id,
            Some(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
        );
        assert!(caller.has_scope("store:manage"));
        assert!(!caller.has_scope("security:manage"));
        assert_eq!(caller.auth_source, Some(CallerAuthSource::Bearer));
        // No database pool is attached to the test context, so no
        // roles can be resolved: the caller holds none and every
        // role-gated check fails closed for it, exactly as before
        // the resolution landed.
        assert!(caller.roles.is_empty());
        assert!(!caller.has_role("admin"));
    }

    #[test]
    fn is_token_revoked_is_skipped_without_a_database_pool() {
        // No pool is attached to the test context, so there is no
        // revocation store to consult: the check reports "not
        // revoked" instead of failing authentication. With a pool
        // attached the same helper consults `auth_revoked_tokens`
        // and fails closed on any lookup error (see its docs).
        let ctx = test_context();
        assert!(!is_token_revoked(&ctx, "any-jti").expect("no-pool check succeeds"));
    }

    #[test]
    fn authenticate_request_accepts_the_session_cookie() {
        let ctx = test_context();
        let token = mint_token(
            "test-secret",
            "test-salt",
            "00000000-0000-0000-0000-000000000002",
            &[],
        );
        let req = TestRequest::default()
            .insert_header(("Cookie", format!("vault={token}")))
            .to_http_request();
        let caller = authenticate_request(&ctx, &req, None).expect("valid cookie");
        assert!(caller.is_authenticated());
        assert_eq!(caller.auth_source, Some(CallerAuthSource::Cookie));
    }

    #[test]
    fn authenticate_request_rejects_missing_tampered_and_wrong_key_tokens() {
        let ctx = test_context();
        let bare = TestRequest::default().to_http_request();
        let missing = authenticate_request(&ctx, &bare, None);
        assert!(missing.is_err());
        assert_eq!(missing.unwrap_err().code, 401);

        let token = mint_token(
            "test-secret",
            "test-salt",
            "00000000-0000-0000-0000-000000000001",
            &[],
        );
        let tampered = TestRequest::default()
            .insert_header(("Authorization", format!("Bearer {token}x")))
            .to_http_request();
        assert_eq!(
            authenticate_request(&ctx, &tampered, None)
                .unwrap_err()
                .code,
            401
        );
        let wrong_key = mint_token(
            "other-secret",
            "test-salt",
            "00000000-0000-0000-0000-000000000001",
            &[],
        );
        let wrong = TestRequest::default()
            .insert_header(("Authorization", format!("Bearer {wrong_key}")))
            .to_http_request();
        assert_eq!(
            authenticate_request(&ctx, &wrong, None).unwrap_err().code,
            401
        );
    }

    #[test]
    fn authenticate_request_ignores_self_asserted_headers() {
        // The C1 hole, closed: X-User-Id / X-Role headers buy
        // nothing without a verified token.
        let ctx = test_context();
        let req = TestRequest::default()
            .insert_header(("X-User-Id", "00000000-0000-0000-0000-000000000001"))
            .insert_header(("X-Role", "admin"))
            .to_http_request();
        assert_eq!(
            authenticate_request(&ctx, &req, None).unwrap_err().code,
            401
        );
    }

    #[test]
    fn authenticate_request_attaches_the_resolved_tenant() {
        let ctx = test_context();
        let token = mint_token(
            "test-secret",
            "test-salt",
            "00000000-0000-0000-0000-000000000001",
            &[],
        );
        let req = TestRequest::default()
            .insert_header(("Authorization", format!("Bearer {token}")))
            .to_http_request();
        let tenant = TenantContext {
            tenant_id: "tenant-1".to_string(),
            company_id: "company-1".to_string(),
            site_id: "site-1".to_string(),
            kind: unified_core::middleware::TenantKind::Subdomain,
            host: "tenant.example.com".to_string(),
            scope: None,
            product_id: None,
            package_id: None,
            product_subdomain_id: None,
        };
        let caller = authenticate_request(&ctx, &req, Some(&tenant)).expect("valid");
        assert_eq!(caller.tenant_id.as_deref(), Some("tenant-1"));
    }
}
