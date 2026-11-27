// Caller authentication (inspection C1, 2026-11-22).
//
// The server-verified identity of the caller of an HTTP request.
// A `Caller` is built ONLY by the global auth middleware
// (`crate::http::auth::AuthMiddleware`) from a verified Auth Manager
// JWT; it is carried in the Actix request extensions, and handlers
// read it through `caller_from` below.
//
// This type is deliberately the ONLY source of caller identity for
// handlers. The `X-User-Id` / `X-Role` request headers are
// self-asserted - any unauthenticated caller could send
// `X-Role: admin` and pass every capability check - so no code in
// this crate, or in any handler, may derive identity from headers.
// `caller_from` never reads a header. The per-module `caller()`
// helpers that still read headers are being migrated to this type
// (separate builder); until that migration lands they must be
// treated as untrusted input, not as authentication.

use actix_web::{HttpMessage, HttpRequest};
use serde::Serialize;
use uuid::Uuid;

/// How the caller presented its token. Bearer tokens are never sent
/// automatically by browsers, so only cookie authentication needs a
/// CSRF check on state-changing endpoints (see the security
/// module's `require_security_capability_write`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CallerAuthSource {
    #[serde(rename = "bearer")]
    Bearer,
    #[serde(rename = "cookie")]
    Cookie,
}

/// One route's authentication requirement, declared on `RouteInfo`
/// (`crate::modules::RouteInfo::auth`) so the route catalog states
/// what a route needs instead of leaving it implicit.
///
/// The default is `Authenticated`: fail closed. A route is `Public`
/// only when its owner module declares it so AND the global auth
/// middleware's allowlist admits it; the middleware is the
/// enforcement point, this field is the declaration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum RouteAuth {
    /// Any caller with a valid token. This is the default.
    #[serde(rename = "authenticated")]
    Authenticated,
    /// A valid token whose scopes grant this capability (or the
    /// wildcard scope `*`, or `admin:access`). Capability
    /// enforcement lives in the handler (via `Caller::has_scope`);
    /// the middleware enforces authentication only.
    #[serde(rename = "capability")]
    Capability(&'static str),
    /// Genuinely public: no token is required. Reserved for the
    /// login/token-issue flow, the public pages, the module-declared
    /// public surface (marked `PUBLIC:` / `TOKEN-GATED:` in the
    /// route catalog), and provider webhook receivers that verify
    /// their own HMAC/shared-secret signature. Never use it for a
    /// mutating endpoint that has no other credential check.
    #[serde(rename = "public")]
    Public,
}

impl Default for RouteAuth {
    fn default() -> Self {
        RouteAuth::Authenticated
    }
}

impl RouteAuth {
    /// True only for `RouteAuth::Public`.
    pub fn is_public(&self) -> bool {
        matches!(self, RouteAuth::Public)
    }

    /// The required capability for `RouteAuth::Capability`, else
    /// `None` (`Authenticated` needs a valid token but no
    /// particular scope; `Public` needs nothing).
    pub fn required_capability(&self) -> Option<&'static str> {
        match self {
            RouteAuth::Capability(capability) => Some(capability),
            _ => None,
        }
    }
}

/// Server-verified caller identity, carried in request extensions
/// by the auth middleware.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Caller {
    /// The verified token subject. `None` for an anonymous caller
    /// (no token, or a public route visited without one). Never
    /// populated from a request header.
    pub user_id: Option<Uuid>,
    /// Capabilities the Auth Manager minted into the token, from
    /// the user's database roles only. A token can never carry a
    /// capability its owner does not hold.
    pub scopes: Vec<String>,
    /// Role slugs resolved server-side for this user, from the
    /// `user_roles` / `roles` tables at authentication time (see
    /// `with_roles`). Empty for an anonymous caller, and empty when
    /// the lookup could not run or failed - an empty list grants
    /// nothing, so a failed lookup fails closed. Never populated
    /// from the token or a request header: a token carries
    /// capabilities, not role slugs.
    pub roles: Vec<String>,
    /// Tenant the request resolved to, when the tenant-resolution
    /// middleware ran first and resolved one. `None` on the
    /// control plane and when no tenant was resolved.
    pub tenant_id: Option<String>,
    /// Company the verified caller acts for, resolved server-side
    /// from the `users` table at authentication time (see
    /// `with_company`). `None` when the user has no company, or
    /// when the lookup could not run or failed - `None` grants no
    /// company, so a failed lookup fails closed. Never populated
    /// from the token or a request header: a token carries
    /// capabilities, not a company id.
    pub company_id: Option<String>,
    /// True only when a token was presented and verified. An
    /// anonymous `Caller` always has this set to `false`.
    pub authenticated: bool,
    /// How the token was presented, when one was presented.
    pub auth_source: Option<CallerAuthSource>,
}

impl Caller {
    /// A verified caller: a valid token whose subject is `user_id`
    /// and whose scopes are `scopes`.
    pub fn verified(user_id: Uuid, scopes: Vec<String>) -> Self {
        Self {
            user_id: Some(user_id),
            scopes,
            roles: Vec::new(),
            tenant_id: None,
            company_id: None,
            authenticated: true,
            auth_source: None,
        }
    }

    /// An unauthenticated caller: no verified identity at all.
    /// This is what `caller_from` returns when the middleware
    /// attached nothing (public route, or a request that never
    /// passed the middleware).
    pub fn anonymous() -> Self {
        Self {
            user_id: None,
            scopes: Vec::new(),
            roles: Vec::new(),
            tenant_id: None,
            company_id: None,
            authenticated: false,
            auth_source: None,
        }
    }

    /// Attach the resolved tenant id (filled in by the auth
    /// middleware from the tenant context).
    pub fn with_tenant(mut self, tenant_id: String) -> Self {
        self.tenant_id = Some(tenant_id);
        self
    }

    /// Record how the token was presented.
    pub fn with_auth_source(mut self, source: CallerAuthSource) -> Self {
        self.auth_source = Some(source);
        self
    }

    /// Attach the role slugs resolved for this user at
    /// authentication time. The auth middleware resolves them from
    /// the database (`user_roles` / `roles`); a caller built
    /// without this call simply holds no roles, which grants
    /// nothing.
    pub fn with_roles(mut self, roles: Vec<String>) -> Self {
        self.roles = roles;
        self
    }

    /// Attach the company id resolved for this user at
    /// authentication time. The auth middleware resolves it from
    /// the database (`users.company_id`); a caller built without
    /// this call simply has no company, which grants no company
    /// scope. Never populated from a token or a request header.
    pub fn with_company(mut self, company_id: String) -> Self {
        self.company_id = Some(company_id);
        self
    }

    /// True when the caller holds the server-resolved role `role`.
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|held| held == role)
    }

    /// True when this caller was authenticated by a verified token.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// True when the token scopes grant `scope`. The wildcard scope
    /// `*` grants everything; `admin:access` is the admin baseline
    /// and grants every capability. This mirrors the security
    /// module's `has_security_capability`, which is the same rule
    /// for the same scope vocabulary. An anonymous caller is
    /// granted nothing.
    pub fn has_scope(&self, scope: &str) -> bool {
        if !self.authenticated {
            return false;
        }
        self.scopes
            .iter()
            .any(|granted| granted == "*" || granted == "admin:access" || granted == scope)
    }

    /// True when the caller holds the admin baseline
    /// (`admin:access` or the wildcard scope).
    pub fn is_admin(&self) -> bool {
        self.has_scope("admin:access")
    }
}

/// The central helper every handler uses to learn who is calling.
///
/// Reads the server-verified `Caller` the auth middleware attached
/// to the request extensions. When the middleware attached nothing
/// - a public route, or a request that never passed the middleware
/// - returns an unauthenticated (anonymous) `Caller`.
///
/// This function NEVER reads request headers. Any identity derived
/// from `X-User-Id` / `X-Role` / `X-Session-Id` headers is
/// self-asserted and must not be trusted.
pub fn caller_from(req: &HttpRequest) -> Caller {
    req.extensions()
        .get::<Caller>()
        .cloned()
        .unwrap_or_else(Caller::anonymous)
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    fn verified_caller() -> Caller {
        Caller::verified(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            vec!["store:manage".to_string()],
        )
    }

    #[test]
    fn anonymous_caller_has_no_identity_and_no_scopes() {
        let caller = Caller::anonymous();
        assert!(!caller.is_authenticated());
        assert_eq!(caller.user_id, None);
        assert_eq!(caller.tenant_id, None);
        assert_eq!(caller.company_id, None);
        assert!(!caller.has_scope("store:manage"));
        assert!(!caller.is_admin());
    }

    #[test]
    fn verified_caller_grants_its_exact_scope_only() {
        let caller = verified_caller();
        assert!(caller.is_authenticated());
        assert!(caller.has_scope("store:manage"));
        assert!(!caller.has_scope("security:manage"));
    }

    #[test]
    fn wildcard_scope_grants_everything() {
        let caller = Caller::verified(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            vec!["*".to_string()],
        );
        assert!(caller.has_scope("anything:at:all"));
        assert!(caller.is_admin());
    }

    #[test]
    fn admin_access_grants_every_capability() {
        let caller = Caller::verified(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            vec!["admin:access".to_string()],
        );
        assert!(caller.has_scope("security:manage"));
        assert!(caller.is_admin());
    }

    #[test]
    fn with_tenant_attaches_the_tenant_id() {
        let caller = verified_caller().with_tenant("tenant-1".to_string());
        assert_eq!(caller.tenant_id.as_deref(), Some("tenant-1"));
    }

    #[test]
    fn with_company_attaches_the_server_resolved_company() {
        let caller = verified_caller().with_company("company-1".to_string());
        assert_eq!(caller.company_id.as_deref(), Some("company-1"));
        // A verified caller with no company resolved yet has none.
        assert_eq!(verified_caller().company_id, None);
        assert_eq!(Caller::anonymous().company_id, None);
    }

    #[test]
    fn with_roles_attaches_server_resolved_roles() {
        let caller = verified_caller().with_roles(vec!["admin".to_string(), "editor".to_string()]);
        assert!(caller.has_role("admin"));
        assert!(caller.has_role("editor"));
        assert!(!caller.has_role("ghost"));
        assert!(!caller.has_role("anonymous"));
        // An anonymous caller holds no role, resolved or otherwise.
        assert!(!Caller::anonymous().has_role("admin"));
        // Roles are independent of scopes: a verified caller with
        // no roles resolved yet holds none.
        assert!(!verified_caller().has_role("admin"));
    }

    #[test]
    fn caller_from_returns_the_attached_caller() {
        let req = TestRequest::default().to_http_request();
        req.extensions_mut().insert(verified_caller());
        let caller = caller_from(&req);
        assert!(caller.is_authenticated());
        assert_eq!(
            caller.user_id,
            Some(Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap())
        );
    }

    #[test]
    fn caller_from_returns_anonymous_without_an_attached_caller() {
        // No extensions inserted: this is the public-route /
        // never-passed-the-middleware case, and it must never
        // panic or read a header.
        let req = TestRequest::default()
            .insert_header(("X-User-Id", "00000000-0000-0000-0000-000000000001"))
            .insert_header(("X-Role", "admin"))
            .to_http_request();
        let caller = caller_from(&req);
        assert!(!caller.is_authenticated());
        assert_eq!(caller.user_id, None);
    }

    #[test]
    fn route_auth_defaults_to_authenticated() {
        assert_eq!(RouteAuth::default(), RouteAuth::Authenticated);
        assert!(!RouteAuth::default().is_public());
        assert_eq!(RouteAuth::Public.is_public(), true);
        assert_eq!(
            RouteAuth::Capability("store:manage").required_capability(),
            Some("store:manage")
        );
        assert_eq!(RouteAuth::Authenticated.required_capability(), None);
    }
}
