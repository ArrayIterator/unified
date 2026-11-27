//! Handler-facing view of the server-verified caller (inspection C1 fix).
//!
//! The SINGLE source of caller identity is [`crate::auth::Caller`]:
//! the global auth middleware verifies the Auth Manager JWT /
//! session token once and inserts exactly one `auth::Caller` into
//! the request extensions. This module converts that verified
//! identity into the [`Caller`] view handlers already use (string
//! user id, primary role, company id) - it never reads a request
//! header, and it never trusts any other extension type. The
//! `X-User-Id` / `X-Role` / `X-User-Roles` / `X-Company-Id`
//! headers are self-asserted: any unauthenticated client can send
//! `X-Role: admin` and pass every capability check, which is the C1
//! finding. The only place that may still read an identity header
//! is the auth middleware's token path itself (the `Authorization`
//! Bearer token / session cookie), and only to *verify* it, never
//! to trust a claimed role.
//!
//! How the conversion handles the server-resolved values (and
//! its one remaining limit, not hidden here):
//! * Roles: the auth middleware resolves the user's role slugs
//!   from the database (`user_roles` / `roles`) at authentication
//!   time and attaches them to `auth::Caller`; this view copies
//!   them through, so [`Caller::role`] returns the primary
//!   server-resolved role and capability checks through the
//!   `role.check` hook answer from the roles module's store. When
//!   no database pool is attached (or the lookup fails), roles
//!   stay empty and the checks fail closed exactly as before -
//!   the actor identity (`user_id`) is real and verified either
//!   way. Token scopes (`auth::Caller::has_scope`) remain the
//!   capability source for scope-gated routes; both are minted
//!   from the same database roles, so the two answers agree.
//! * Company: the auth middleware resolves the user's company id
//!   from the database (`users.company_id`) at authentication
//!   time and attaches it to `auth::Caller`; this view copies it
//!   through, so [`Caller::company_id`] is the server-resolved
//!   company - the first source in the company resolution order
//!   (verified Caller company, then tenant-context company, then
//!   the `user.company_requested` hook answer). When the user has
//!   no company, or the lookup could not run or failed, it stays
//!   `None` and company scoping fails closed; tenant scoping
//!   still comes from the resolved `TenantContext`
//!   (`resolve_tenant`), as before.
//!
//! The dashboards module already carries its own verified identity
//! (`CallerIdentity`, inserted by its middleware after JWT
//! verification); that type stays as-is for dashboards routes, but
//! new code must use this module (and through it, `auth::Caller`)
//! so there is one extension type for the whole server.

use actix_web::{HttpMessage, HttpRequest};

use crate::auth::Caller as AuthCaller;
use crate::error::{UnifiedError, UnifiedResult};

/// The server-verified caller, as handlers see it: a view converted
/// from the [`AuthCaller`] the auth middleware attached to the
/// request extensions. `authenticated` is false exactly when no
/// token was verified for this request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Caller {
    /// Verified user id (the JWT `sub`, as a string). Empty when anonymous.
    pub user_id: String,
    /// Roles resolved server-side for this user by the auth
    /// middleware (from the database at authentication time).
    /// Empty for an anonymous caller and when no pool is attached
    /// or the lookup failed; an empty list grants nothing.
    pub roles: Vec<String>,
    /// Raw capability strings from the JWT `scopes` claim.
    pub scopes: Vec<String>,
    /// Company the verified caller acts for, resolved server-side
    /// by the auth middleware from the `users` table at
    /// authentication time. `None` when the user has no company,
    /// or when the lookup could not run or failed.
    pub company_id: Option<String>,
    /// Tenant the verified caller acts for, when resolved.
    pub tenant_id: Option<String>,
    /// True only when a token was verified for this request.
    pub authenticated: bool,
    /// True while an admin is viewing as another user (mirrors the
    /// dashboards view-as session): `user_id` is the target.
    pub impersonating: bool,
    /// The real authenticated user id while impersonating.
    pub impersonated_by: Option<String>,
}

impl Caller {
    /// An unauthenticated caller: no user, no roles, no scopes.
    /// Every capability check fails closed for this caller.
    pub fn anonymous() -> Self {
        Self::default()
    }

    /// A verified caller with a user id and server-resolved roles.
    pub fn verified(user_id: impl Into<String>, roles: Vec<String>) -> Self {
        Self {
            user_id: user_id.into(),
            roles,
            authenticated: true,
            ..Self::default()
        }
    }

    /// A verified caller built from a JWT: `sub` plus the token's
    /// scopes claim. Roles are resolved separately by the middleware
    /// (never taken from the token or a header).
    pub fn direct(user_id: impl Into<String>, scopes: Vec<String>) -> Self {
        Self {
            user_id: user_id.into(),
            scopes,
            authenticated: true,
            ..Self::default()
        }
    }

    /// Build the handler-facing view from the middleware's verified
    /// [`AuthCaller`]. The user id is the token subject as a string;
    /// scopes, tenant id, the server-resolved roles and the
    /// server-resolved company id carry over.
    pub fn from_auth(verified: &AuthCaller) -> Self {
        Self {
            user_id: verified
                .user_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            roles: verified.roles.clone(),
            scopes: verified.scopes.clone(),
            company_id: verified.company_id.clone(),
            tenant_id: verified.tenant_id.clone(),
            authenticated: verified.authenticated,
            impersonating: false,
            impersonated_by: None,
        }
    }

    /// True only when a token was verified for this request.
    pub fn is_authenticated(&self) -> bool {
        self.authenticated
    }

    /// The primary role: the first server-resolved role, or
    /// `"anonymous"` when no role was resolved (anonymous caller,
    /// no database pool, or a failed lookup). Never a header value.
    pub fn role(&self) -> &str {
        self.roles
            .first()
            .map(String::as_str)
            .unwrap_or("anonymous")
    }

    /// The acting user id (alias for `user_id`).
    pub fn actor(&self) -> &str {
        &self.user_id
    }

    /// True when the caller holds `role` (server-resolved only).
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|held| held == role)
    }

    /// True when the caller holds an admin role.
    pub fn is_admin(&self) -> bool {
        self.has_role("admin") || self.has_role("super_admin")
    }
}

/// Read the server-verified caller: convert the [`AuthCaller`] the
/// auth middleware attached to the request extensions into the
/// handler-facing [`Caller`] view. Returns an anonymous caller when
/// the middleware attached nothing - never headers, never a default
/// role. Only `auth::Caller` is ever read from extensions; a
/// `caller::Caller` value in extensions is not trusted.
pub fn caller_from(req: &HttpRequest) -> Caller {
    req.extensions()
        .get::<AuthCaller>()
        .map(Caller::from_auth)
        .unwrap_or_else(Caller::anonymous)
}

/// Read the server-verified caller, failing closed with 401 when no
/// token was verified for this request (or the verified token
/// carries no subject). Use this at the top of any handler that
/// must not run unauthenticated.
pub fn require_caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let caller = caller_from(req);
    if caller.is_authenticated() && !caller.user_id.is_empty() {
        Ok(caller)
    } else {
        Err(UnifiedError::unauthorized(
            "Authentication required: no verified caller for this request",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn with_company_flows_through_from_auth() {
        // Modelled on `auth::with_roles_attaches_server_resolved_roles`:
        // a company attached to the verified caller at auth time
        // must reach the handler-facing view unchanged.
        let verified = AuthCaller::verified(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            vec!["store:manage".to_string()],
        )
        .with_roles(vec!["admin".to_string()])
        .with_company("company-1".to_string())
        .with_tenant("tenant-1".to_string());
        let caller = Caller::from_auth(&verified);
        assert!(caller.is_authenticated());
        assert_eq!(caller.company_id.as_deref(), Some("company-1"));
        assert_eq!(caller.tenant_id.as_deref(), Some("tenant-1"));
        assert!(caller.has_role("admin"));
        // Without a resolved company the view carries none - never
        // an invented one.
        let unresolved = AuthCaller::verified(
            Uuid::parse_str("00000000-0000-0000-0000-000000000001").unwrap(),
            vec![],
        );
        assert_eq!(Caller::from_auth(&unresolved).company_id, None);
        assert_eq!(Caller::from_auth(&AuthCaller::anonymous()).company_id, None);
    }
}
