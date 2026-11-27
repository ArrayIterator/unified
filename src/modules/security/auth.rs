// security -- Caller authentication and capability enforcement.

use crate::auth::CsrfManager;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json::error_response;
use unified_core::db::DbPool;
use unified_core::db::models::User;
use actix_web::HttpRequest;
use actix_web::HttpResponse;
use actix_web::http::header;
use base64::Engine;
use diesel::prelude::*;
use hmac::Mac;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::constants::*;
use super::types::*;

// Caller authentication and capability enforcement.
//
// Every endpoint in this module requires a valid Auth Manager JWT
// (Bearer header or session cookie) and a security capability:
// `security:view` for reads, `security:manage` for writes. The
// wildcard scope `*` and `admin:access` grant both, matching the
// dashboards module's capability semantics. Scopes are trustworthy
// here because the Auth Manager mints them only from the caller's
// database roles (see `issue_token`); a token can never carry a
// capability its owner does not hold in the database.
//
// Defence in depth: when a database pool is attached, the token's
// user must still exist and be active; a deleted or disabled user's
// token stops working immediately instead of living until expiry.
// Without a pool there is no user row to check, so the token scopes
// are the only check -- deployments without a database must not
// expose this surface.
//
// CSRF: a session cookie is sent automatically by browsers, so a
// state-changing endpoint authenticated with the cookie also
// requires a CSRF token (header `X-CSRF-Token`, bound to the
// caller's session token and issued by `GET /api/v1/security/
// csrf-token`). Bearer tokens are never attached automatically by
// browsers, so Bearer-authenticated requests are exempt.

/// Maximum clock skew (seconds) tolerated between the token issuer
/// and this server when checking `iat` / `nbf`.
pub(crate) const JWT_MAX_CLOCK_SKEW_SECS: i64 = 300;

/// The issuer expected on Auth Manager JWTs. Must match the `jwt`
/// provider's `issuer` setting in the Auth Manager (default
/// `"unified"`): the Auth Manager writes this claim at issuance and
/// this verifier rejects anything else, so a token minted by a
/// foreign issuer can never authenticate here. If an operator ever
/// customizes the Auth Manager issuer, this must move into shared
/// application config instead of living here.
pub(crate) const EXPECTED_JWT_ISSUER: &str = "unified";

/// Claims of an Auth Manager JWT, mirroring the issuer exactly.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct JwtClaims {
    pub(crate) sub: String,
    pub(crate) iss: String,
    pub(crate) iat: i64,
    pub(crate) exp: i64,
    #[allow(dead_code)]
    pub(crate) jti: String,
    #[serde(default)]
    pub(crate) nbf: Option<i64>,
    #[serde(default)]
    pub(crate) scopes: Vec<String>,
}

/// How the caller presented its token. Bearer tokens are never sent
/// automatically by browsers, so only cookie authentication needs a
/// CSRF check on state-changing endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AuthSource {
    Bearer,
    Cookie,
}

/// Extract a Bearer token from the `Authorization` header, falling
/// back to the session cookie (`app.session_name`), and report how
/// it was presented.
pub(crate) fn extract_token_with_source(
    req: &HttpRequest,
    session_cookie_name: &str,
) -> Option<(String, AuthSource)> {
    if let Some(header_value) = req.headers().get(header::AUTHORIZATION) {
        if let Ok(value) = header_value.to_str() {
            let trimmed = value.trim();
            if let Some(token) = trimmed
                .strip_prefix("Bearer ")
                .or_else(|| trimmed.strip_prefix("bearer "))
            {
                if !token.trim().is_empty() {
                    return Some((token.trim().to_string(), AuthSource::Bearer));
                }
            }
        }
    }
    cookie_value(req, session_cookie_name).map(|token| (token, AuthSource::Cookie))
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

/// Verify an Auth Manager JWT: three base64url segments, `alg`
/// `HS256`, HMAC-SHA256 over `header.payload` with signing key
/// `"{secret_key}:{salt_key}"`, an unexpired `exp`, an `iss` equal to
/// `expected_issuer`, and an `iat` / `nbf` that is not in the future
/// (allowing a small clock-skew window). Every failure is a 401.
pub(crate) fn verify_jwt(
    token: &str,
    secret_key: &str,
    salt_key: &str,
    expected_issuer: &str,
) -> UnifiedResult<JwtClaims> {
    if secret_key.is_empty() || salt_key.is_empty() {
        return Err(UnifiedError::unauthorized(
            "Security authentication is not configured: app.secret_key and app.salt_key must both be set",
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
        return Err(UnifiedError::unauthorized("Token algorithm must be HS256"));
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
    if claims.exp <= chrono::Utc::now().timestamp() {
        return Err(UnifiedError::unauthorized("Token has expired"));
    }
    // The issuer must be the Auth Manager's own: a token minted by
    // a foreign issuer (even a correctly signed one under a shared
    // key) is not trusted here.
    if claims.iss != expected_issuer {
        return Err(UnifiedError::unauthorized("Token issuer is not trusted"));
    }
    // A token issued in the future (or not yet valid) must not be
    // accepted: the issuer's clock and this server's clock may drift
    // a little, so a small skew window is tolerated.
    let now = chrono::Utc::now().timestamp();
    if claims.iat > now + JWT_MAX_CLOCK_SKEW_SECS {
        return Err(UnifiedError::unauthorized("Token was issued in the future"));
    }
    if let Some(nbf) = claims.nbf {
        if nbf > now + JWT_MAX_CLOCK_SKEW_SECS {
            return Err(UnifiedError::unauthorized("Token is not yet valid"));
        }
    }
    Ok(claims)
}

/// True when the token scope set grants `capability`. The wildcard
/// scope `*` grants everything; `admin:access` is the admin
/// baseline and grants every security capability.
pub(crate) fn has_security_capability(scopes: &[String], capability: &str) -> bool {
    scopes
        .iter()
        .any(|scope| scope == "*" || scope == "admin:access" || scope == capability)
}

/// Run blocking Diesel work off the async worker threads.
pub(crate) async fn run_blocking<F, T>(pool: DbPool, work: F) -> UnifiedResult<T>
where
    F: FnOnce(&mut diesel::PgConnection) -> UnifiedResult<T> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(move || {
        let mut connection = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot borrow a database connection: {error}"))
        })?;
        work(&mut connection)
    })
    .await
    .map_err(|error| UnifiedError::internal(format!("Database task failed: {error}")))?
}

/// True when the user exists and is active. Without a database pool
/// there is no row to check, so this returns true and the token
/// scopes are the only check (see the section header).
pub(crate) async fn db_user_is_active(ctx: &AppContext, user_id: Uuid) -> UnifiedResult<bool> {
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => return Ok(true),
    };
    run_blocking(pool, move |connection| {
        use unified_core::db::schema::users::dsl as users_dsl;
        let user = users_dsl::users
            .find(user_id)
            .select(User::as_select())
            .first::<User>(connection)
            .optional()
            .map_err(UnifiedError::from)?;
        Ok(user.is_some_and(|user| user.is_active))
    })
    .await
}

/// Authenticate the caller and enforce that the token grants at
/// least one of `capabilities`. Returns the raw token and how it was
/// presented. Returns an error `HttpResponse` (standard envelope) on
/// failure; handlers return it directly.
async fn authenticate(
    req: &HttpRequest,
    ctx: &AppContext,
    capabilities: &[&str],
) -> Result<(AuthSource, String), HttpResponse> {
    let debug = ctx.is_debug();
    let fail = |error: UnifiedError| error_response(&error, debug);
    let (secret_key, salt_key, session_name) = {
        let config = ctx.config.read().expect("config lock poisoned");
        (
            config.app.secret_key.clone(),
            config.app.salt_key.clone(),
            config.app.session_name.clone(),
        )
    };
    let (token, source) = extract_token_with_source(req, &session_name).ok_or_else(|| {
        fail(UnifiedError::unauthorized(
            "Authentication required: provide a Bearer token",
        ))
    })?;
    let claims = verify_jwt(&token, &secret_key, &salt_key, EXPECTED_JWT_ISSUER).map_err(fail)?;
    let user_id = Uuid::parse_str(claims.sub.trim()).map_err(|_| {
        fail(
            UnifiedError::unauthorized("Token subject is not a user id")
                .with_metadata("kind", json!("invalid_subject")),
        )
    })?;
    if !capabilities
        .iter()
        .any(|capability| has_security_capability(&claims.scopes, capability))
    {
        let wanted = capabilities.join("\" or \"");
        let metadata = if capabilities.len() == 1 {
            json!(capabilities[0])
        } else {
            json!(capabilities)
        };
        return Err(fail(
            UnifiedError::forbidden(format!(
                "Capability \"{wanted}\" is required for this security endpoint"
            ))
            .with_metadata("capability", metadata),
        ));
    }
    match db_user_is_active(ctx, user_id).await {
        Ok(true) => Ok((source, token)),
        Ok(false) => Err(fail(
            UnifiedError::unauthorized("Authenticated user does not exist or is disabled")
                .with_metadata("kind", json!("unknown_user")),
        )),
        Err(error) => Err(fail(error)),
    }
}

/// Enforce authentication plus one security capability for a
/// handler. Returns an error `HttpResponse` (standard envelope) on
/// failure; handlers return it directly.
pub(crate) async fn require_security_capability(
    req: &HttpRequest,
    ctx: &AppContext,
    capability: &str,
) -> Result<(), HttpResponse> {
    authenticate(req, ctx, &[capability]).await.map(|_| ())
}

/// Header carrying the CSRF token on state-changing requests that
/// authenticate with the session cookie.
pub(crate) const CSRF_TOKEN_HEADER: &str = "X-CSRF-Token";

/// True when the request carries a CSRF token (header
/// `X-CSRF-Token`) that validates against `manager` for the session
/// bound to `session_id`. The session id here is the caller's raw
/// session token (the JWT), which only the caller and this server
/// know, so a cross-site request cannot present a matching token.
pub(crate) fn csrf_header_valid(
    req: &HttpRequest,
    manager: &CsrfManager,
    session_id: &str,
) -> bool {
    match req
        .headers()
        .get(CSRF_TOKEN_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        Some(token) if !token.trim().is_empty() => manager.validate(token.trim(), session_id),
        _ => false,
    }
}

/// Require a valid CSRF token for a cookie-authenticated request.
/// Uses the shared `crate::auth::CsrfManager` (HMAC-bound, expiring
/// tokens); no parallel implementation lives in this module.
fn require_csrf_token(
    req: &HttpRequest,
    ctx: &AppContext,
    raw_token: &str,
) -> Result<(), HttpResponse> {
    let debug = ctx.is_debug();
    let config = ctx.config.read().expect("config lock poisoned").clone();
    let manager = CsrfManager::from_config(&config);
    if csrf_header_valid(req, &manager, raw_token) {
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

/// Enforce authentication plus one security capability for a
/// state-changing handler. Bearer-authenticated callers are exempt
/// from the CSRF check: browsers never attach an `Authorization`
/// header automatically, so a cross-site request cannot borrow a
/// Bearer token. Cookie-authenticated callers must also present a
/// valid `X-CSRF-Token` header, because the session cookie alone is
/// sent automatically and `SameSite=Lax` is the only other defence.
pub(crate) async fn require_security_capability_write(
    req: &HttpRequest,
    ctx: &AppContext,
    capability: &str,
) -> Result<(), HttpResponse> {
    let (source, token) = authenticate(req, ctx, &[capability]).await?;
    if source == AuthSource::Cookie {
        require_csrf_token(req, ctx, &token)?;
    }
    Ok(())
}

/// Authenticate a caller for CSRF-token issuance (either security
/// capability suffices: a `security:manage`-only caller must be able
/// to fetch the token its writes require). Returns the caller's raw
/// session token, which the issued CSRF token is bound to.
pub(crate) async fn authenticate_for_csrf_token(
    req: &HttpRequest,
    ctx: &AppContext,
) -> Result<String, HttpResponse> {
    let (_source, token) =
        authenticate(req, ctx, &[CAP_SECURITY_VIEW, CAP_SECURITY_MANAGE]).await?;
    Ok(token)
}

// Tests for the scope check in this file.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::CsrfManager;
    use actix_web::test::TestRequest;
    use base64::Engine as _;
    use hmac::Mac as _;
    use std::time::Duration;

    type TestHmac = hmac::Hmac<sha2::Sha256>;

    fn scopes(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// Mint an HS256 JWT with the given times, signed with
    /// `"{secret}:{salt}"`, mirroring the Auth Manager issuer.
    fn mint_token(
        secret: &str,
        salt: &str,
        iss: &str,
        iat: i64,
        exp: i64,
        nbf: Option<i64>,
    ) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(r#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = serde_json::json!({
            "sub": "00000000-0000-0000-0000-000000000001",
            "iss": iss,
            "iat": iat,
            "exp": exp,
            "jti": "test-jti",
            "scopes": ["security:view"],
            "nbf": nbf,
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

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    #[test]
    fn wildcard_scope_grants_everything() {
        assert!(has_security_capability(&scopes(&["*"]), "security:view"));
    }

    #[test]
    fn admin_access_grants_everything() {
        assert!(has_security_capability(
            &scopes(&["admin:access"]),
            "security:manage"
        ));
    }

    #[test]
    fn exact_scope_grants_matching_capability() {
        assert!(has_security_capability(
            &scopes(&["security:view"]),
            "security:view"
        ));
    }

    #[test]
    fn other_scopes_do_not_grant() {
        assert!(!has_security_capability(
            &scopes(&["security:view"]),
            "security:manage"
        ));
    }

    #[test]
    fn empty_scopes_grant_nothing() {
        assert!(!has_security_capability(&[], "security:view"));
    }

    #[test]
    fn verify_jwt_accepts_current_token() {
        let token = mint_token("secret", "salt", "unified", now() - 60, now() + 3600, None);
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_ok());
    }

    #[test]
    fn verify_jwt_rejects_expired_token() {
        let token = mint_token("secret", "salt", "unified", now() - 3600, now() - 10, None);
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_err());
    }

    #[test]
    fn verify_jwt_rejects_future_iat() {
        let token = mint_token(
            "secret",
            "salt",
            "unified",
            now() + 3600,
            now() + 7200,
            None,
        );
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_err());
    }

    #[test]
    fn verify_jwt_accepts_iat_within_clock_skew() {
        let token = mint_token("secret", "salt", "unified", now() + 240, now() + 3600, None);
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_ok());
    }

    #[test]
    fn verify_jwt_rejects_future_nbf() {
        let token = mint_token(
            "secret",
            "salt",
            "unified",
            now() - 60,
            now() + 3600,
            Some(now() + 3600),
        );
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_err());
    }

    #[test]
    fn verify_jwt_accepts_nbf_within_clock_skew() {
        let token = mint_token(
            "secret",
            "salt",
            "unified",
            now() - 60,
            now() + 3600,
            Some(now() + 240),
        );
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_ok());
    }

    #[test]
    fn verify_jwt_rejects_wrong_issuer() {
        let token = mint_token("secret", "salt", "attacker", now() - 60, now() + 3600, None);
        assert!(verify_jwt(&token, "secret", "salt", "unified").is_err());
    }

    #[test]
    fn extract_token_prefers_bearer_over_cookie() {
        let req = TestRequest::default()
            .insert_header(("Authorization", "Bearer bearer-token"))
            .insert_header(("Cookie", "vault=cookie-token"))
            .to_http_request();
        assert_eq!(
            extract_token_with_source(&req, "vault"),
            Some(("bearer-token".to_string(), AuthSource::Bearer))
        );
    }

    #[test]
    fn extract_token_falls_back_to_cookie() {
        let req = TestRequest::default()
            .insert_header(("Cookie", "vault=cookie-token"))
            .to_http_request();
        assert_eq!(
            extract_token_with_source(&req, "vault"),
            Some(("cookie-token".to_string(), AuthSource::Cookie))
        );
    }

    #[test]
    fn extract_token_returns_none_without_credentials() {
        let req = TestRequest::default().to_http_request();
        assert!(extract_token_with_source(&req, "vault").is_none());
    }

    fn csrf_manager() -> CsrfManager {
        CsrfManager::new(b"test-secret", Duration::from_secs(1800))
    }

    #[test]
    fn csrf_header_valid_accepts_token_bound_to_session() {
        let manager = csrf_manager();
        let token = manager.generate("session-1");
        let req = TestRequest::default()
            .insert_header((CSRF_TOKEN_HEADER, token))
            .to_http_request();
        assert!(csrf_header_valid(&req, &manager, "session-1"));
    }

    #[test]
    fn csrf_header_valid_rejects_token_bound_to_another_session() {
        let manager = csrf_manager();
        let token = manager.generate("session-1");
        let req = TestRequest::default()
            .insert_header((CSRF_TOKEN_HEADER, token))
            .to_http_request();
        assert!(!csrf_header_valid(&req, &manager, "session-2"));
    }

    #[test]
    fn csrf_header_valid_rejects_missing_and_tampered_tokens() {
        let manager = csrf_manager();
        let bare = TestRequest::default().to_http_request();
        assert!(!csrf_header_valid(&bare, &manager, "session-1"));
        let bogus = TestRequest::default()
            .insert_header((CSRF_TOKEN_HEADER, "bogus"))
            .to_http_request();
        assert!(!csrf_header_valid(&bogus, &manager, "session-1"));
    }
}
