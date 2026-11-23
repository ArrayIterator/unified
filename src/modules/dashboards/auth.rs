// Dashboards - Navigation building, JWT auth and capability guards.

use crate::auth::CsrfManager;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::error_response;
use actix_web::http::header;
use actix_web::{HttpMessage, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::Utc;
use hmac::Mac;
use serde_json::{json, Value};
use std::collections::HashSet;
use uuid::Uuid;

use super::{CAP_MODULES_MANAGE, CAP_USER_ACCESS, CallerContext, CallerIdentity, DashboardsState,
    HmacSha256, JwtClaims, NAV_DEFS, NavItem, effective_scope, resolve_capabilities};

/// Build the navigation for one scope, filtered by the caller's
/// capabilities. Admin navigation additionally lists every registered
/// module (from `ctx.modules.summary()`) under the Modules section, so
/// the nav is built from the live module registry, not a hard-coded
/// list. `current` marks the item whose href matches the current path.
pub(crate) fn nav_items(
    ctx: &AppContext,
    state: &DashboardsState,
    scope: &str,
    capabilities: &HashSet<String>,
    current: Option<&str>,
) -> Vec<NavItem> {
    let mut items: Vec<NavItem> = NAV_DEFS
        .iter()
        .filter(|def| def.scope == scope)
        .filter(|def| has_capability(capabilities, def.capability))
        .map(|def| NavItem {
            id: def.id.to_string(),
            label: def.label.to_string(),
            href: state.expand_href(def.href),
            icon: def.icon,
            capability: def.capability,
            section: def.section,
            active: current.is_some_and(|path| path == state.expand_href(def.href)),
        })
        .collect();
    if scope == "admin" && has_capability(capabilities, CAP_MODULES_MANAGE) {
        for module in ctx.modules.summary() {
            let href = format!("/{}/modules/{}", state.admin_prefix(), module.id);
            items.push(NavItem {
                id: format!("module.{}", module.id),
                label: module.name.to_string(),
                href: href.clone(),
                icon: "icon-layers",
                capability: CAP_MODULES_MANAGE,
                section: "modules",
                active: current.is_some_and(|path| path == href),
            });
        }
    }
    items
}

/// True when the capability set grants `capability`. The wildcard
/// scope `*` grants everything; `user:access` is the authenticated
/// baseline and is always present in a resolved set.
pub(crate) fn has_capability(capabilities: &HashSet<String>, capability: &str) -> bool {
    capabilities.contains("*") || capabilities.contains(capability)
}

/// The issuer expected on Auth Manager JWTs. Must match the `jwt`
/// provider's `issuer` setting in the Auth Manager (default
/// `"unified"`); see `security::auth::EXPECTED_JWT_ISSUER`. The
/// Auth Manager writes this claim at issuance and this verifier
/// rejects anything else, so a token minted by a foreign issuer can
/// never authenticate here.
pub(crate) const EXPECTED_JWT_ISSUER: &str = "unified";

/// How the caller presented its token. Bearer tokens are never sent
/// automatically by browsers, so only cookie authentication needs a
/// CSRF check on state-changing endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthSource {
    Bearer,
    Cookie,
}

/// Header carrying the CSRF token on state-changing requests that
/// authenticate with the session cookie.
pub(crate) const CSRF_TOKEN_HEADER: &str = "X-CSRF-Token";

/// Extract a Bearer token from the `Authorization` header, falling
/// back to the session cookie (`app.session_name`), and report how
/// it was presented.
pub(crate) fn extract_token_with_source(
    req: &HttpRequest,
    session_cookie_name: &str,
) -> Option<(String, AuthSource)> {
    if let Some(header) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(value) = header.to_str() {
            let trimmed = value.trim();
            if let Some(token) = trimmed.strip_prefix("Bearer ") {
                if !token.trim().is_empty() {
                    return Some((token.trim().to_string(), AuthSource::Bearer));
                }
            }
            if let Some(token) = trimmed.strip_prefix("bearer ") {
                if !token.trim().is_empty() {
                    return Some((token.trim().to_string(), AuthSource::Bearer));
                }
            }
        }
    }
    cookie_value(req, session_cookie_name).map(|token| (token, AuthSource::Cookie))
}

/// Extract a Bearer token from the `Authorization` header, falling
/// back to the session cookie (`app.session_name`).
pub(crate) fn extract_token(req: &HttpRequest, session_cookie_name: &str) -> Option<String> {
    extract_token_with_source(req, session_cookie_name).map(|(token, _)| token)
}

/// Read one cookie value from the raw `Cookie` header.
pub(crate) fn cookie_value(req: &HttpRequest, name: &str) -> Option<String> {
    let header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim() == name && !value.trim().is_empty() {
                return Some(value.trim().to_string());
            }
        }
    }
    None
}

/// Verify an Auth Manager JWT. This mirrors
/// `AuthManagerState::verify_token` exactly: three base64url segments
/// without padding, HMAC-SHA256 over `header.payload`, signing key
/// `"{secret_key}:{salt_key}"` from the application configuration.
/// Returns the claims on success; every failure is a 401.
pub(crate) fn verify_jwt(token: &str, secret_key: &str, salt_key: &str) -> UnifiedResult<JwtClaims> {
    if secret_key.is_empty() || salt_key.is_empty() {
        return Err(UnifiedError::unauthorized(
            "Dashboard authentication is not configured: app.secret_key and app.salt_key must both be set",
        )
        .with_metadata("kind", json!("auth_not_configured")));
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(UnifiedError::unauthorized(
            "Token must have three base64url segments",
        ));
    }
    let header_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[0])
        .map_err(|_| UnifiedError::unauthorized("Token header is not valid base64url"))?;
    let header: Value = serde_json::from_slice(&header_bytes)
        .map_err(|_| UnifiedError::unauthorized("Token header is not valid JSON"))?;
    if header.get("alg").and_then(|value| value.as_str()) != Some("HS256") {
        return Err(UnifiedError::unauthorized(
            "Token algorithm must be HS256",
        ));
    }
    let signing_key = format!("{secret_key}:{salt_key}");
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let provided = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[2])
        .map_err(|_| UnifiedError::unauthorized("Token signature is not valid base64url"))?;
    let mut mac = HmacSha256::new_from_slice(signing_key.as_bytes())
        .map_err(|error| UnifiedError::internal(format!("HMAC initialisation failed: {error}")))?;
    mac.update(signing_input.as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| UnifiedError::unauthorized("Token signature is invalid"))?;
    let payload_bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(parts[1])
        .map_err(|_| UnifiedError::unauthorized("Token payload is not valid base64url"))?;
    let claims: JwtClaims = serde_json::from_slice(&payload_bytes)
        .map_err(|_| UnifiedError::unauthorized("Token payload is not valid JSON"))?;
    if claims.exp <= Utc::now().timestamp() {
        return Err(UnifiedError::unauthorized("Token has expired"));
    }
    // The issuer must be the Auth Manager's own: a token minted by
    // a foreign issuer (even a correctly signed one under a shared
    // key) is not trusted here.
    if claims.iss != EXPECTED_JWT_ISSUER {
        return Err(UnifiedError::unauthorized("Token issuer is not trusted"));
    }
    Ok(claims)
}

/// Require a valid CSRF token for a cookie-authenticated request.
/// Uses the shared `crate::auth::CsrfManager` (HMAC-bound, expiring
/// tokens). Bearer-authenticated callers are exempt: browsers never
/// attach an `Authorization` header automatically, so a cross-site
/// request cannot borrow a Bearer token.
fn require_csrf_token(
    req: &HttpRequest,
    ctx: &AppContext,
    raw_token: &str,
) -> Result<(), HttpResponse> {
    let debug = ctx.is_debug();
    let config = ctx.config.read().expect("config lock poisoned").clone();
    let manager = CsrfManager::from_config(&config);
    let valid = req
        .headers()
        .get(CSRF_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|token| !token.trim().is_empty() && manager.validate(token.trim(), raw_token));
    if valid {
        Ok(())
    } else {
        Err(error_response(
            &UnifiedError::forbidden(
                "A valid X-CSRF-Token header is required for cookie-authenticated state-changing requests; fetch one from GET /api/v1/security/csrf-token",
            ),
            debug,
        ))
    }
}

/// Authenticate a request: Bearer header or session cookie, verified
/// as an Auth Manager JWT whose `sub` is the user's UUID. View-as
/// sessions are applied here: when the real caller has an active,
/// unexpired view-as session, the effective identity is the target
/// user. Expired sessions are removed and reported as expired.
/// Returns the identity, how the token was presented, and the raw
/// token (the CSRF token for cookie-authenticated writes is bound to
/// it).
pub(crate) fn authenticate_with_source(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
) -> UnifiedResult<(CallerIdentity, AuthSource, String)> {
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
    let claims = verify_jwt(&token, &secret_key, &salt_key)?;
    let user_id = Uuid::parse_str(claims.sub.trim()).map_err(|_| {
        UnifiedError::unauthorized("Token subject is not a user id")
            .with_metadata("kind", json!("invalid_subject"))
    })?;
    let mut identity = CallerIdentity::direct(user_id, claims.scopes);
    if let Some(session) = state.take_expired_or_active_view_as(user_id, ctx) {
        identity.user_id = session.target_user_id;
        identity.impersonating = true;
        identity.impersonated_by = Some(user_id);
        identity.view_as_expires_at = Some(session.expires_at);
    }
    Ok((identity, source, token))
}

/// Authenticate a request: Bearer header or session cookie, verified
/// as an Auth Manager JWT whose `sub` is the user's UUID. View-as
/// sessions are applied here: when the real caller has an active,
/// unexpired view-as session, the effective identity is the target
/// user. Expired sessions are removed and reported as expired.
pub(crate) fn authenticate(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
) -> UnifiedResult<CallerIdentity> {
    authenticate_with_source(req, ctx, state).map(|(identity, _, _)| identity)
}

/// Read the identity the middleware stored in the request extensions,
/// if any.
pub(crate) fn identity_from_extensions(req: &HttpRequest) -> Option<CallerIdentity> {
    req.extensions().get::<CallerIdentity>().cloned()
}

// ---------------------------------------------------------------------------
// Layer 2 — per-handler guards (defence in depth over the middleware)
// ---------------------------------------------------------------------------

/// Resolve the caller for a handler: prefer the identity the
/// middleware stored in the request extensions, otherwise
/// re-authenticate from the request itself, so a handler stays
/// protected even when the middleware is bypassed or misconfigured.
/// Returns an error `HttpResponse` (standard envelope) on failure.
pub(crate) async fn require_identity(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
) -> Result<CallerIdentity, HttpResponse> {
    if let Some(identity) = identity_from_extensions(req) {
        return Ok(identity);
    }
    let debug = ctx.is_debug();
    authenticate(req, ctx, state).map_err(|error| error_response(&error, debug))
}

/// Enforce one capability for a handler. `CAP_USER_ACCESS` is the
/// authenticated baseline and always passes. Returns the caller
/// context (identity, resolved capabilities, profile) on success, or
/// a 401 / 403 `HttpResponse` in the standard envelope.
pub(crate) async fn require_capability(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    capability: &str,
) -> Result<CallerContext, HttpResponse> {
    let debug = ctx.is_debug();
    let identity = require_identity(req, ctx, state).await?;
    let (capabilities, profile) = resolve_capabilities(ctx, &identity)
        .await
        .map_err(|error| error_response(&error, debug))?;
    if capability != CAP_USER_ACCESS && !has_capability(&capabilities, capability) {
        let (scope, _) = effective_scope(&capabilities);
        return Err(error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this dashboard section"
            ))
            .with_metadata("capability", json!(capability))
            .with_metadata("scope", json!(scope)),
            debug,
        ));
    }
    Ok(CallerContext {
        identity,
        capabilities,
        profile,
    })
}

/// The same check, but against the REAL authenticated user instead of
/// the effective (possibly impersonated) identity. Used by the
/// view-as stop endpoint: while impersonating, the effective identity
/// is the target user, who must never be able to end (or extend) the
/// admin's session — only the admin can.
pub(crate) async fn require_real_capability(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    capability: &str,
) -> Result<CallerContext, HttpResponse> {
    let debug = ctx.is_debug();
    let identity = require_identity(req, ctx, state).await?;
    let real = CallerIdentity::direct(identity.real_user_id, identity.token_scopes.clone());
    let (capabilities, profile) = resolve_capabilities(ctx, &real)
        .await
        .map_err(|error| error_response(&error, debug))?;
    if !has_capability(&capabilities, capability) {
        return Err(error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this dashboard section"
            ))
            .with_metadata("capability", json!(capability)),
            debug,
        ));
    }
    Ok(CallerContext {
        identity,
        capabilities,
        profile,
    })
}

/// As `require_capability`, for state-changing handlers: the caller
/// is always re-authenticated from the request itself, because the
/// identity the middleware stores in the request extensions carries
/// no presentation source, and a cookie-authenticated caller must
/// also present a valid `X-CSRF-Token` header. Bearer-authenticated
/// callers are exempt from the CSRF check.
pub(crate) async fn require_capability_write(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    capability: &str,
) -> Result<CallerContext, HttpResponse> {
    let debug = ctx.is_debug();
    let (identity, source, token) =
        authenticate_with_source(req, ctx, state).map_err(|error| error_response(&error, debug))?;
    let (capabilities, profile) = resolve_capabilities(ctx, &identity)
        .await
        .map_err(|error| error_response(&error, debug))?;
    if capability != CAP_USER_ACCESS && !has_capability(&capabilities, capability) {
        let (scope, _) = effective_scope(&capabilities);
        return Err(error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this dashboard section"
            ))
            .with_metadata("capability", json!(capability))
            .with_metadata("scope", json!(scope)),
            debug,
        ));
    }
    if source == AuthSource::Cookie {
        require_csrf_token(req, ctx, &token)?;
    }
    Ok(CallerContext {
        identity,
        capabilities,
        profile,
    })
}

/// As `require_real_capability`, for state-changing handlers: the
/// caller is re-authenticated from the request itself and a
/// cookie-authenticated caller must also present a valid
/// `X-CSRF-Token` header. Used by the view-as stop endpoint.
pub(crate) async fn require_real_capability_write(
    req: &HttpRequest,
    ctx: &AppContext,
    state: &DashboardsState,
    capability: &str,
) -> Result<CallerContext, HttpResponse> {
    let debug = ctx.is_debug();
    let (identity, source, token) =
        authenticate_with_source(req, ctx, state).map_err(|error| error_response(&error, debug))?;
    let real = CallerIdentity::direct(identity.real_user_id, identity.token_scopes.clone());
    let (capabilities, profile) = resolve_capabilities(ctx, &real)
        .await
        .map_err(|error| error_response(&error, debug))?;
    if !has_capability(&capabilities, capability) {
        return Err(error_response(
            &UnifiedError::forbidden(format!(
                "Capability \"{capability}\" is required for this dashboard section"
            ))
            .with_metadata("capability", json!(capability)),
            debug,
        ));
    }
    if source == AuthSource::Cookie {
        require_csrf_token(req, ctx, &token)?;
    }
    Ok(CallerContext {
        identity,
        capabilities,
        profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn caps(items: &[&str]) -> HashSet<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test]
    fn has_capability_wildcard_grants_everything() {
        assert!(has_capability(&caps(&["*"]), "orders.view"));
    }

    #[test]
    fn has_capability_matches_exact() {
        assert!(has_capability(&caps(&["orders.view"]), "orders.view"));
        assert!(!has_capability(&caps(&["orders.view"]), "orders.manage"));
    }

    #[test]
    fn has_capability_does_not_expand_prefix_wildcards() {
        assert!(!has_capability(&caps(&["orders.*"]), "orders.view"));
        assert!(has_capability(&caps(&["orders:manage"]), "orders:manage"));
    }

    #[test]
    fn has_capability_empty_set_grants_nothing() {
        assert!(!has_capability(&caps(&[]), "orders.view"));
    }
}
