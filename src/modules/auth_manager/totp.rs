//! Auth Manager - TOTP enrolment, confirmation, and disable handlers.

use crate::auth::{password as auth_password, totp as auth_totp};
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
use super::{providers::*, jwt::*, state::*, handlers_providers::*, login::*, tokens::*, recovery::*, google::*, module::*};

// TOTP enrolment
//
// TOTP secrets live in the `users` table (`totp_secret`,
// `totp_enabled`) -- the columns already existed, but no endpoint ever
// wrote them, so TOTP could never be enabled through the API and the
// `auth::totp` helpers were unwired. These three endpoints close
// that gap: `setup` generates a secret and holds it pending (never
// persisted), `confirm` activates it after the user proves possession
// with a valid code, and `disable` clears it after a password check
// plus a valid code or recovery code. All three require the caller's
// own valid JWT. Confirmed secrets are stored encrypted at rest
// (`auth::totp::encrypt_secret`); a legacy plaintext row still
// verifies and is re-encrypted on the next successful password
// login (see `issue_token`).

#[derive(Debug, Deserialize)]
pub(crate) struct TotpCodeInput {
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct TotpDisableInput {
    pub password: String,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub recovery_code: Option<String>,
}

/// True when a pending TOTP enrolment created at `created_at` has
/// outlived `PENDING_TOTP_TTL_MINUTES` at `now`.
pub(crate) fn is_pending_totp_expired(created_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    created_at + chrono::Duration::minutes(PENDING_TOTP_TTL_MINUTES) < now
}

/// Reject a TOTP attempt with 429 when the caller IP or account is
/// locked by the hardening brute-force engine, mirroring the
/// password-login check in `issue_token`. Returns the client IP for
/// failure/success accounting on success.
fn totp_lockout_check(
    req: &HttpRequest,
    ctx: &AppContext,
    caller_id: &str,
) -> Result<Option<String>, UnifiedError> {
    let ip = client_ip_text(req, ctx);
    let hardening = shared_state();
    let locked = ip
        .as_deref()
        .is_some_and(|ip| hardening.is_locked("ip", ip))
        || hardening.is_locked("account", caller_id);
    if locked {
        return Err(UnifiedError::new(
            "Too many failed attempts; try again later",
            429,
        )
        .with_metadata("reason", json!("account_locked")));
    }
    Ok(ip)
}

/// Start TOTP enrolment: generate a secret, hold it pending for
/// `PENDING_TOTP_TTL_MINUTES`, and return the secret plus the
/// `otpauth://` provisioning URI (shown once, for a QR code). The
/// secret is not active until `totp.confirm` proves possession.
pub(crate) async fn totp_setup(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (caller_id, _) = match caller_from_request_write(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if ctx.db.read().expect("db lock poisoned").is_none() {
        return api_json::error_response(
            &UnifiedError::new(
                "Database is not configured: TOTP enrolment needs a database connection",
                503,
            )
            .with_metadata("kind", json!("database_not_configured")),
            debug,
        );
    }
    let secret = auth_totp::generate_secret();
    let uri = auth_totp::provisioning_uri(&secret, &caller_id, "Unified");
    state.pending_totp.insert(
        caller_id.clone(),
        PendingTotp {
            secret: secret.clone(),
            created_at: Utc::now(),
        },
    );
    api_json::success_response(
        json!({
            "user_id": caller_id,
            "secret": secret,
            "provisioning_uri": uri,
            "issuer": "Unified",
            "expires_in_minutes": PENDING_TOTP_TTL_MINUTES,
            "warning": "Store this secret in your authenticator app now. It is shown exactly once and is not active until confirmed.",
        }),
        200,
    )
}

/// Confirm TOTP enrolment: a valid code for the pending secret
/// activates TOTP on the account. A wrong code leaves the pending
/// secret in place so a typo does not force a restart; an expired
/// pending secret must be set up again. Wrong codes feed the
/// hardening lockout engine, so a stolen session cannot brute-force
/// the 6-digit code. The pending secret is only removed after the
/// database write succeeds, so a failed write never loses the
/// enrolment.
pub(crate) async fn totp_confirm(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<TotpCodeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (caller_id, _) = match caller_from_request_write(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let ip = match totp_lockout_check(&req, &ctx, &caller_id) {
        Ok(ip) => ip,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let Some(pending) = state
        .pending_totp
        .get(&caller_id)
        .map(|entry| entry.value().clone())
    else {
        return api_json::error_response(
            &UnifiedError::bad_request("No TOTP setup is pending for this account; call totp/setup first"),
            debug,
        );
    };
    if is_pending_totp_expired(pending.created_at, Utc::now()) {
        state.pending_totp.remove(&caller_id);
        return api_json::error_response(
            &UnifiedError::bad_request("The pending TOTP setup has expired; call totp/setup again"),
            debug,
        );
    }
    if !auth_totp::verify(&pending.secret, input.code.trim()) {
        shared_state().record_login_failure(ip.as_deref(), Some(&caller_id));
        ctx.hooks.emit(
            events::AUTH_FAILED,
            json!({
                "provider": "totp",
                "ip": ip,
                "user_id": caller_id,
                "reason": "invalid_totp_code",
            }),
        );
        return api_json::error_response(
            &UnifiedError::unauthorized("The TOTP code is invalid"),
            debug,
        );
    }
    let Ok(user_id) = Uuid::parse_str(&caller_id) else {
        return api_json::error_response(
            &UnifiedError::unauthorized("Token subject is not a user id"),
            debug,
        );
    };
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            return api_json::error_response(
                &UnifiedError::new(
                    "Database is not configured: TOTP enrolment needs a database connection",
                    503,
                )
                .with_metadata("kind", json!("database_not_configured")),
                debug,
            );
        }
    };
    // The secret is encrypted before it touches the database, so a
    // stolen users table never yields a usable authenticator secret.
    let encrypted = {
        let config = ctx.config.read().expect("config lock poisoned");
        let key = auth_totp::totp_encryption_key(&config.app.secret_key, &config.app.salt_key);
        auth_totp::encrypt_secret(&pending.secret, &key)
    };
    let Some(secret) = encrypted else {
        return api_json::error_response(
            &UnifiedError::internal("Cannot encrypt the TOTP secret for storage"),
            debug,
        );
    };
    let stored = run_blocking(pool, move |connection| {
        use unified_core::db::schema::users::dsl as users_dsl;
        diesel::update(users_dsl::users.find(user_id))
            .set((
                users_dsl::totp_secret.eq(Some(secret)),
                users_dsl::totp_enabled.eq(true),
            ))
            .execute(connection)
            .map_err(UnifiedError::from)?;
        Ok(())
    })
    .await;
    match stored {
        Ok(()) => {
            // Only now, with the secret persisted, is the pending
            // enrolment removed: a failed write above leaves it in
            // place so the user can retry instead of starting over.
            state.pending_totp.remove(&caller_id);
            shared_state().record_login_success(ip.as_deref(), Some(&caller_id));
            ctx.hooks.emit(
                "auth.totp.enabled",
                json!({ "user_id": caller_id }),
            );
            api_json::success_response(
                json!({ "user_id": caller_id, "totp_enabled": true }),
                200,
            )
        }
        Err(error) => api_json::error_response(&error, debug),
    }
}

/// Disable TOTP: requires the account password plus a valid TOTP
/// code (or a single-use recovery code, consumed on use), so a
/// stolen session token alone cannot strip the second factor.
/// Wrong passwords and wrong codes feed the hardening lockout engine
/// (429 when locked), mirroring password login.
pub(crate) async fn totp_disable(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<TotpDisableInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (caller_id, _) = match caller_from_request_write(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let ip = match totp_lockout_check(&req, &ctx, &caller_id) {
        Ok(ip) => ip,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let Ok(user_id) = Uuid::parse_str(&caller_id) else {
        return api_json::error_response(
            &UnifiedError::unauthorized("Token subject is not a user id"),
            debug,
        );
    };
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            return api_json::error_response(
                &UnifiedError::new(
                    "Database is not configured: TOTP management needs a database connection",
                    503,
                )
                .with_metadata("kind", json!("database_not_configured")),
                debug,
            );
        }
    };
    let user = match load_user_by_id(pool.clone(), user_id).await {
        Ok(user) => user,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let Some(user) = user else {
        return api_json::error_response(
            &UnifiedError::unauthorized("Authenticated user does not exist"),
            debug,
        );
    };
    if input.password.is_empty() || !auth_password::verify(&input.password, &user.password_hash)
    {
        shared_state().record_login_failure(ip.as_deref(), Some(&caller_id));
        ctx.hooks.emit(
            events::AUTH_FAILED,
            json!({
                "provider": "totp",
                "ip": ip,
                "user_id": caller_id,
                "reason": "invalid_password",
            }),
        );
        return api_json::error_response(
            &UnifiedError::unauthorized("Invalid password"),
            debug,
        );
    }
    // The stored secret is decrypted first: confirmed secrets are
    // stored encrypted at rest, and a legacy plaintext row still
    // verifies (it is re-encrypted on the next successful login).
    let totp_key = {
        let config = ctx.config.read().expect("config lock poisoned");
        auth_totp::totp_encryption_key(&config.app.secret_key, &config.app.salt_key)
    };
    let totp_ok = match (&user.totp_secret, &input.code) {
        (Some(stored), Some(code)) => auth_totp::decrypt_secret(stored, &totp_key)
            .is_some_and(|secret| auth_totp::verify(&secret, code)),
        _ => false,
    };
    let recovery_ok = match &input.recovery_code {
        Some(code) => try_consume_recovery_code(&state, &caller_id, code),
        None => false,
    };
    if !totp_ok && !recovery_ok {
        shared_state().record_login_failure(ip.as_deref(), Some(&caller_id));
        ctx.hooks.emit(
            events::AUTH_FAILED,
            json!({
                "provider": "totp",
                "ip": ip,
                "user_id": caller_id,
                "reason": "totp_required",
            }),
        );
        return api_json::error_response(
            &UnifiedError::unauthorized(
                "A valid TOTP code or recovery code is required to disable two-factor authentication",
            )
            .with_metadata("kind", json!("totp_required")),
            debug,
        );
    }
    let cleared = run_blocking(pool, move |connection| {
        use unified_core::db::schema::users::dsl as users_dsl;
        diesel::update(users_dsl::users.find(user_id))
            .set((
                users_dsl::totp_secret.eq(None::<String>),
                users_dsl::totp_enabled.eq(false),
            ))
            .execute(connection)
            .map_err(UnifiedError::from)?;
        Ok(())
    })
    .await;
    match cleared {
        Ok(()) => {
            shared_state().record_login_success(ip.as_deref(), Some(&caller_id));
            ctx.hooks.emit(
                "auth.totp.disabled",
                json!({ "user_id": caller_id }),
            );
            api_json::success_response(
                json!({ "user_id": caller_id, "totp_enabled": false }),
                200,
            )
        }
        Err(error) => api_json::error_response(&error, debug),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_pending_totp_expired_cases() {
        let created = Utc::now();
        assert!(!is_pending_totp_expired(created, created));
        assert!(!is_pending_totp_expired(
            created,
            created + chrono::Duration::minutes(PENDING_TOTP_TTL_MINUTES)
        ));
        assert!(is_pending_totp_expired(
            created,
            created + chrono::Duration::minutes(PENDING_TOTP_TTL_MINUTES + 1)
        ));
        // A pending enrolment created in the future is not expired.
        assert!(!is_pending_totp_expired(
            created + chrono::Duration::minutes(5),
            created
        ));
    }
}
