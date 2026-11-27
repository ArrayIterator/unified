//! Auth Manager - Password login: user loading, scopes, brute-force-safe failures.

use crate::auth::{password as auth_password, totp as auth_totp, CsrfManager};
use unified_core::context::AppContext;
use unified_core::error::UnifiedError;
use unified_core::hardening::{shared_state, trusted_client_ip_for_http_request};
use unified_core::hooks::events;
use unified_core::json as api_json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::db::models::User;
use unified_core::db::DbPool;
use actix_web::http::header;
use actix_web::{web, HttpRequest, HttpResponse};
use base64::Engine;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use super::{providers::*, jwt::*, state::*, handlers_providers::*, tokens::*, recovery::*, totp::*, google::*, module::*};
use unified_core::error::UnifiedResult;

// Password login helpers
//
// `POST /api/v1/auth-manager/jwt/issue` is the password login
// endpoint: it verifies the caller's email + Argon2 password (+ TOTP
// when enabled) against the users table and only then mints a token.
// It never mints a token for an arbitrary user id, and the scopes on
// the token come only from the user's database roles -- callers can
// never request scopes. A failed login feeds the hardening
// brute-force engine (per IP and per account) and a successful one
// resets it.

/// Minutes a pending TOTP enrolment stays usable.
pub(crate) const PENDING_TOTP_TTL_MINUTES: i64 = 15;

/// One pending TOTP enrolment: a generated secret awaiting proof of
/// possession through `totp.confirm`.
#[derive(Debug, Clone)]
pub struct PendingTotp {
    pub secret: String,
    pub created_at: DateTime<Utc>,
}

/// Syntactically valid Argon2id PHC string for an unknown password,
/// used to verify a password for an unknown or disabled account so
/// the timing matches a real verification and the account's existence
/// is never revealed by response time.
pub(crate) const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";

/// Client IP for brute-force accounting: the trusted client IP
/// (forwarded headers only from configured trusted proxies).
pub(crate) fn client_ip_text(req: &HttpRequest, ctx: &AppContext) -> Option<String> {
    let settings = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .security
        .clone();
    trusted_client_ip_for_http_request(req, &settings).map(|ip| ip.to_string())
}

/// The single generic login failure: the same message whether the
/// account does not exist, is disabled, or the password is wrong, so
/// the response never reveals which one it was.
pub(crate) fn login_failed_error() -> UnifiedError {
    UnifiedError::unauthorized("Invalid email or password")
        .with_metadata("kind", json!("invalid_credentials"))
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

/// Mintable scopes from one role permission map
/// (`{resource: [actions]}`): one `resource:action` scope per action.
/// A literal `"*": ["*"]` entry mints the wildcard scope `*`. Only
/// strings read from the database are ever minted.
pub(crate) fn scopes_from_permissions(permissions: &Value) -> Vec<String> {
    let mut scopes = Vec::new();
    let Some(map) = permissions.as_object() else {
        return scopes;
    };
    for (resource, actions) in map {
        let Some(actions) = actions.as_array() else {
            continue;
        };
        for action in actions {
            let Some(action) = action.as_str() else {
                continue;
            };
            if resource == "*" && action == "*" {
                scopes.push("*".to_string());
            } else if !resource.is_empty() && !action.is_empty() {
                scopes.push(format!("{resource}:{action}"));
            }
        }
    }
    scopes
}

/// One row of `roles.permissions`, read as text: Diesel here has no
/// `serde_json` feature, so a `Value` cannot be loaded from the
/// `jsonb` column directly (same shape as the dashboards module).
#[derive(Debug, QueryableByName)]
pub(crate) struct PermissionRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub(crate) permissions: String,
}

/// The user row plus the scopes their database roles grant, for a
/// login attempt. `needs_rehash` is true when the stored Argon2 hash
/// was produced with parameters other than the current defaults.
pub(crate) struct LoginUser {
    pub(crate) user: User,
    pub(crate) scopes: Vec<String>,
    pub(crate) needs_rehash: bool,
}

/// Load a user by (lowercased) email with the scopes their roles
/// grant. Returns `Ok(None)` when no such user exists.
pub(crate) async fn load_login_user(pool: DbPool, email: &str) -> UnifiedResult<Option<LoginUser>> {
    let email = email.to_string();
    run_blocking(pool, move |connection| {
        use unified_core::db::schema::user_roles::dsl as user_roles_dsl;
        use unified_core::db::schema::users::dsl as users_dsl;
        let user = users_dsl::users
            .filter(users_dsl::email.eq(&email))
            .select(User::as_select())
            .first::<User>(connection)
            .optional()
            .map_err(UnifiedError::from)?;
        let Some(user) = user else {
            return Ok(None);
        };
        let needs_rehash = auth_password::needs_rehash(&user.password_hash);
        let role_ids: Vec<Uuid> = user_roles_dsl::user_roles
            .filter(user_roles_dsl::user_id.eq(user.id))
            .select(user_roles_dsl::role_id)
            .load::<Uuid>(connection)
            .map_err(UnifiedError::from)?;
        let mut scopes: Vec<String> = Vec::new();
        if !role_ids.is_empty() {
            let rows = diesel::sql_query(
                "SELECT permissions::text AS permissions FROM roles WHERE id = ANY($1)",
            )
            .bind::<diesel::sql_types::Array<diesel::sql_types::Uuid>, _>(role_ids)
            .load::<PermissionRow>(connection)
            .map_err(UnifiedError::from)?;
            for row in rows {
                if let Ok(value) = serde_json::from_str::<Value>(&row.permissions) {
                    scopes.extend(scopes_from_permissions(&value));
                }
            }
        }
        scopes.sort();
        scopes.dedup();
        Ok(Some(LoginUser {
            user,
            scopes,
            needs_rehash,
        }))
    })
    .await
}

/// Load one user row by id.
pub(crate) async fn load_user_by_id(pool: DbPool, user_id: Uuid) -> UnifiedResult<Option<User>> {
    run_blocking(pool, move |connection| {
        use unified_core::db::schema::users::dsl as users_dsl;
        users_dsl::users
            .find(user_id)
            .select(User::as_select())
            .first::<User>(connection)
            .optional()
            .map_err(UnifiedError::from)
    })
    .await
}

/// Re-hash a password with the current Argon2 parameters after a
/// successful login whose stored hash was stale. Best effort: the
/// caller ignores a failure so a rehash problem never blocks a valid
/// login.
pub(crate) async fn rehash_password(pool: DbPool, user_id: Uuid, password: &str) -> UnifiedResult<()> {
    let hash = auth_password::hash(password)?;
    run_blocking(pool, move |connection| {
        use unified_core::db::schema::users::dsl as users_dsl;
        diesel::update(users_dsl::users.find(user_id))
            .set(users_dsl::password_hash.eq(hash))
            .execute(connection)
            .map_err(UnifiedError::from)?;
        Ok(())
    })
    .await
}

/// Re-encrypt a legacy plaintext TOTP secret with the current
/// encryption format after a successful login verified it. Best
/// effort: the caller ignores a failure so an upgrade problem never
/// blocks a valid login; the plaintext row keeps working until the
/// next login retries the upgrade.
pub(crate) async fn upgrade_totp_secret(
    pool: DbPool,
    user_id: Uuid,
    encrypted_secret: String,
) -> UnifiedResult<()> {
    run_blocking(pool, move |connection| {
        use unified_core::db::schema::users::dsl as users_dsl;
        diesel::update(users_dsl::users.find(user_id))
            .set(users_dsl::totp_secret.eq(Some(encrypted_secret)))
            .execute(connection)
            .map_err(UnifiedError::from)?;
        Ok(())
    })
    .await
}

/// Try to consume one recovery code for `user_id`. Returns true only
/// when an unused code matched; a consumed code is marked used.
/// Codes are verified against their Argon2id PHC hash (see
/// `recovery_code_hash`); a stored value that is not a valid PHC
/// string never verifies.
pub(crate) fn try_consume_recovery_code(
    state: &AuthManagerState,
    user_id: &str,
    code: &str,
) -> bool {
    let normalized = normalize_recovery_code(code);
    if normalized.len() != 12 {
        return false;
    }
    let Some(mut records) = state.recovery_codes.get_mut(user_id) else {
        return false;
    };
    for record in records.iter_mut() {
        if !record.used && auth_password::verify(&normalized, &record.code_hash) {
            record.used = true;
            record.used_at = Some(Utc::now());
            return true;
        }
    }
    false
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
pub(crate) fn extract_bearer_token_with_source(
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
    let header = req.headers().get(header::COOKIE)?.to_str().ok()?;
    for part in header.split(';') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('=') {
            if key.trim() == session_cookie_name && !value.trim().is_empty() {
                return Some((value.trim().to_string(), AuthSource::Cookie));
            }
        }
    }
    None
}

/// Extract a Bearer token from the `Authorization` header, falling
/// back to the session cookie (`app.session_name`).
pub(crate) fn extract_bearer_token(req: &HttpRequest, session_cookie_name: &str) -> Option<String> {
    extract_bearer_token_with_source(req, session_cookie_name).map(|(token, _)| token)
}

/// Header carrying the CSRF token on state-changing requests that
/// authenticate with the session cookie.
pub(crate) const CSRF_TOKEN_HEADER: &str = "X-CSRF-Token";

/// Require a valid CSRF token for a cookie-authenticated request.
/// Uses the shared `crate::auth::CsrfManager` (HMAC-bound, expiring
/// tokens); no parallel implementation lives in this module.
/// Bearer-authenticated callers are exempt: browsers never attach an
/// `Authorization` header automatically, so a cross-site request
/// cannot borrow a Bearer token.
fn require_csrf_token(req: &HttpRequest, ctx: &AppContext, raw_token: &str) -> UnifiedResult<()> {
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
        Err(UnifiedError::forbidden(
            "A valid X-CSRF-Token header is required for cookie-authenticated state-changing requests; fetch one from GET /api/v1/security/csrf-token",
        ))
    }
}

/// Verify the caller's token and return the caller user id (`sub`),
/// scopes, how the token was presented, and the raw token. Before
/// verifying, the persisted revocations are hydrated into memory
/// once per process, so a token revoked before a restart stays
/// revoked.
async fn caller_auth(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<(String, Vec<String>, AuthSource, String)> {
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        state.ensure_revoked_tokens_hydrated(pool).await;
    }
    let session_name = {
        let config = ctx.config.read().expect("config lock poisoned");
        config.app.session_name.clone()
    };
    let (token, source) =
        extract_bearer_token_with_source(req, &session_name).ok_or_else(|| {
            UnifiedError::unauthorized("Authentication required: provide a Bearer token")
        })?;
    let claims = verify_token(state, ctx, &token)?;
    Ok((claims.sub, claims.scopes, source, token))
}

/// Verify the caller's Bearer token and return the caller user id
/// (`sub`) and scopes. Used to authorize self-service recovery-code
/// and TOTP endpoints, and read-only Google / provider endpoints.
pub(crate) async fn caller_from_request(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<(String, Vec<String>)> {
    let (caller_id, scopes, _, _) = caller_auth(state, ctx, req).await?;
    Ok((caller_id, scopes))
}

/// Verify the caller for a state-changing endpoint: as
/// `caller_from_request`, plus a CSRF-token check when the caller
/// authenticated with the session cookie (which browsers send
/// automatically). Bearer-authenticated callers are exempt.
pub(crate) async fn caller_from_request_write(
    state: &AuthManagerState,
    ctx: &AppContext,
    req: &HttpRequest,
) -> UnifiedResult<(String, Vec<String>)> {
    let (caller_id, scopes, source, token) = caller_auth(state, ctx, req).await?;
    if source == AuthSource::Cookie {
        require_csrf_token(req, ctx, &token)?;
    }
    Ok((caller_id, scopes))
}

/// True when the caller's scopes carry an admin capability: the
/// wildcard scope, or the admin/users management scopes minted from
/// database roles. Provider administration and other admin-only
/// endpoints gate on this.
pub(crate) fn caller_is_admin(scopes: &[String]) -> bool {
    scopes
        .iter()
        .any(|scope| scope == "*" || scope == "admin:access" || scope == "users:manage")
}

/// True when the caller may manage `target_user_id`: the account is
/// their own, or they hold an admin capability.
pub(crate) fn caller_may_manage_user(scopes: &[String], caller_id: &str, target_user_id: &str) -> bool {
    caller_id == target_user_id || caller_is_admin(scopes)
}

// JWT handlers

#[derive(Debug, Deserialize)]
pub struct IssueTokenInput {
    pub email: String,
    pub password: String,
    /// TOTP code, required when the account has TOTP enabled.
    #[serde(default)]
    pub totp_code: Option<String>,
    /// Single-use recovery code, accepted instead of a TOTP code when
    /// the account has TOTP enabled.
    #[serde(default)]
    pub recovery_code: Option<String>,
    pub expires_in_seconds: Option<u64>,
}

#[derive(Debug, Deserialize)]
pub struct TokenInput {
    pub token: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scopes_from_permissions_cases() {
        let permissions =
            serde_json::json!({"articles": ["read", "write"], "*": ["*"]});
        let mut scopes = scopes_from_permissions(&permissions);
        scopes.sort();
        assert_eq!(scopes, vec!["*", "articles:read", "articles:write"]);
        assert!(scopes_from_permissions(&serde_json::json!("not-an-object")).is_empty());
        assert!(scopes_from_permissions(&serde_json::json!({"a": "not-an-array"})).is_empty());
        assert!(scopes_from_permissions(&serde_json::json!({"": ["read"]})).is_empty());
        assert!(scopes_from_permissions(&serde_json::json!({"a": [""]})).is_empty());
        assert!(scopes_from_permissions(&serde_json::json!({})).is_empty());
    }

    #[test]
    fn caller_is_admin_cases() {
        assert!(caller_is_admin(&["*".to_string()]));
        assert!(caller_is_admin(&["admin:access".to_string()]));
        assert!(caller_is_admin(&["users:manage".to_string()]));
        assert!(caller_is_admin(&[
            "articles:read".to_string(),
            "users:manage".to_string()
        ]));
        assert!(!caller_is_admin(&["articles:read".to_string()]));
        assert!(!caller_is_admin(&[]));
        // A scope that merely contains the admin name is not admin.
        assert!(!caller_is_admin(&["admin:access:read".to_string()]));
        assert!(!caller_is_admin(&["users:manage:other".to_string()]));
    }

    #[test]
    fn caller_may_manage_user_cases() {
        let admin = vec!["admin:access".to_string()];
        let plain: Vec<String> = vec![];
        assert!(caller_may_manage_user(&plain, "user-1", "user-1"));
        assert!(!caller_may_manage_user(&plain, "user-1", "user-2"));
        assert!(caller_may_manage_user(&admin, "user-1", "user-2"));
    }
}
