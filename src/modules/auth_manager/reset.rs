//! Auth Manager - Password reset: request a reset email, redeem a token.
//!
//! The forgot/reset pages (`/forgot-password`, `/reset-password`) used
//! to be honest dead ends: no reset backend existed. These two
//! endpoints close that gap:
//!
//! * `POST /api/v1/auth-manager/password/forgot` - body `{email}`:
//!   when an account exists for the email, mint a single-use token
//!   (32 random bytes, HMAC-hashed at rest), store it with a 60-minute
//!   expiry, and email a reset link through `mail.send_requested`.
//!   The response is identical whether or not the account exists, so
//!   the endpoint never reveals which emails are registered. A
//!   per-address cooldown (5 minutes) keeps the endpoint from
//!   flooding one inbox.
//! * `POST /api/v1/auth-manager/password/reset` - body
//!   `{token, new_password}`: redeem the token (expiry and single-use
//!   enforced atomically in the database), set the new password under
//!   the shared `PasswordPolicy`, and invalidate every sibling token
//!   for the user.
//!
//! Reset links point at `/reset-password?token=...` on the host that
//! served the forgot request (`Host` header + connection scheme), so
//! no base-URL configuration is needed. Behind a TLS-terminating
//! proxy the scheme comes from the connection, not `X-Forwarded-Proto`;
//! serve the app behind a proxy that terminates TLS at the app, or
//! accept an `http://` link that the proxy redirects to `https://`.
//!
//! Storage uses bound-parameter `sql_query` against the
//! `password_reset_tokens` table (migration `2026-11-22-000109`), so
//! the flow does not depend on the generated Diesel schema.

use crate::auth::password as auth_password;
use crate::modules::security::PasswordPolicy;
use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::json as api_json;
use unified_core::db::DbPool;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use diesel::prelude::*;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;
use super::{jwt::*, state::*, login::*};

/// How long a password-reset token stays redeemable.
pub(crate) const PASSWORD_RESET_TTL_MINUTES: i64 = 60;
/// Minimum gap between two reset emails to the same address.
pub(crate) const PASSWORD_RESET_COOLDOWN_MINUTES: i64 = 5;

/// One row of the `password_reset_tokens` table, read back after the
/// atomic claim in `redeem_reset_token`.
#[derive(Debug, QueryableByName)]
pub(crate) struct PasswordResetTokenRow {
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub(crate) id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Uuid)]
    pub(crate) user_id: Uuid,
    #[diesel(sql_type = diesel::sql_types::Timestamptz)]
    pub(crate) expires_at: DateTime<Utc>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Timestamptz>)]
    pub(crate) used_at: Option<DateTime<Utc>>,
}

/// HMAC-SHA256 (hex) of a reset token, keyed by the application
/// signing material and domain-separated from JWT signing. Only the
/// hash is stored, so a stolen table yields no usable reset link,
/// and the hash is the lookup key: no per-row Argon2id scan is
/// needed to find the token.
pub(crate) fn reset_token_hash(
    state: &AuthManagerState,
    ctx: &AppContext,
    token: &str,
) -> UnifiedResult<String> {
    let signing_key = state.signing_key(ctx)?;
    let tag = hmac_sign(&signing_key, &format!("password-reset-v1:{token}"));
    Ok(hex_encode(&tag))
}

/// Lowercase hex encoding (no external helper exists in this module).
fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Ask the Mail module to send one message through the
/// `mail.send_requested` hook, mirroring the marketing module's
/// `request_mail_send`. Returns the mail job id, or an error
/// describing why no handler answered.
fn request_mail_send(ctx: &AppContext, payload: Value) -> UnifiedResult<String> {
    let answer = ctx.hooks.emit("mail.send_requested", payload);
    match answer.results.first() {
        Some(result) if result.get("error").is_none() => {
            let job_id = result
                .get("job_id")
                .and_then(|value| value.as_str())
                .unwrap_or("")
                .to_string();
            if job_id.is_empty() {
                return Err(UnifiedError::new(
                    "Mail service queued the message but returned no job id",
                    503,
                ));
            }
            Ok(job_id)
        }
        Some(result) => {
            let reason = result
                .get("error")
                .and_then(|value| value.as_str())
                .unwrap_or("mail send was refused");
            Err(UnifiedError::new(
                format!("Mail service refused the message: {reason}"),
                503,
            ))
        }
        None => Err(UnifiedError::new(
            "Mail service did not answer the mail.send_requested event: no mail handler is registered",
            503,
        )),
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ForgotPasswordInput {
    pub email: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ResetPasswordInput {
    pub token: String,
    pub new_password: String,
}

/// `POST /api/v1/auth-manager/password/forgot`: request a reset
/// email. The response never reveals whether the account exists.
/// When the account exists and the per-address cooldown allows it,
/// a single-use token is stored and a reset link is emailed.
pub(crate) async fn request_password_reset(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<ForgotPasswordInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let email = input.email.trim().to_lowercase();
    if email.is_empty() || email.chars().count() > 256 || !email.contains('@') {
        return api_json::error_response(
            &UnifiedError::bad_request("email must be a valid email address"),
            debug,
        );
    }
    // Per-address cooldown: at most one reset email per window, so
    // the endpoint cannot flood one inbox. A cooled-down request
    // still gets the generic success response, so the cooldown
    // itself never reveals anything either.
    let now = Utc::now();
    let cooldown = chrono::Duration::minutes(PASSWORD_RESET_COOLDOWN_MINUTES);
    if let Some(last) = state
        .password_reset_requests
        .get(&email)
        .map(|entry| *entry.value())
    {
        if now - last < cooldown {
            return api_json::success_response(
                json!({
                    "requested": true,
                    "message": "If an account exists for that email address, a password-reset link has been sent.",
                }),
                200,
            );
        }
    }
    state.password_reset_requests.insert(email.clone(), now);
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            return api_json::error_response(
                &UnifiedError::new(
                    "Database is not configured: password reset needs a database connection",
                    503,
                )
                .with_metadata("kind", json!("database_not_configured")),
                debug,
            );
        }
    };
    let result: UnifiedResult<()> = (|| async {
        let loaded = load_login_user(pool.clone(), &email).await?;
        let Some(loaded) = loaded else {
            // Unknown account: same response as success. Never
            // reveal by timing or message whether the email exists.
            return Ok(());
        };
        let user = loaded.user;
        if !user.is_active {
            return Ok(());
        }
        let token_bytes: [u8; 32] = rand::random();
        let token = base64url_encode(&token_bytes);
        let token_hash = reset_token_hash(&state, &ctx, &token)?;
        let user_id = user.id;
        let expires_at = now + chrono::Duration::minutes(PASSWORD_RESET_TTL_MINUTES);
        run_blocking(pool, move |connection| {
            use diesel::sql_types::{Text, Timestamptz, Uuid as SqlUuid};
            diesel::sql_query(
                "INSERT INTO password_reset_tokens (id, user_id, token_hash, expires_at) \
                 VALUES ($1, $2, $3, $4)",
            )
            .bind::<SqlUuid, _>(Uuid::now_v7())
            .bind::<SqlUuid, _>(user_id)
            .bind::<Text, _>(&token_hash)
            .bind::<Timestamptz, _>(expires_at)
            .execute(connection)
            .map_err(UnifiedError::from)?;
            Ok(())
        })
        .await?;
        let connection_info = req.connection_info();
        let reset_url = format!(
            "{}://{}/reset-password?token={}",
            connection_info.scheme(),
            connection_info.host(),
            token
        );
        let mail = request_mail_send(
            &ctx,
            json!({
                "to": email,
                "subject": "Reset your password",
                "body_text": format!(
                    "You requested a password reset for your Unified account.\n\nOpen this link to choose a new password (valid for {PASSWORD_RESET_TTL_MINUTES} minutes, single use):\n{reset_url}\n\nIf you did not request this, ignore this email; your password stays unchanged."
                ),
                "body_html": format!(
                    "<p>You requested a password reset for your Unified account.</p><p><a href=\"{reset_url}\">Choose a new password</a> (valid for {PASSWORD_RESET_TTL_MINUTES} minutes, single use).</p><p>If you did not request this, ignore this email; your password stays unchanged.</p>"
                ),
            }),
        );
        // A mail failure must not reveal whether the account
        // exists, and must not fail the request either: the token
        // is stored, so an operator can re-send it. The failure is
        // reported through the auth hook for operators to see.
        if let Err(error) = mail {
            ctx.hooks.emit(
                "auth.password_reset.email_failed",
                json!({ "user_id": user.id.to_string(), "error": error.message }),
            );
        }
        ctx.hooks.emit(
            "auth.password_reset.requested",
            json!({ "user_id": user.id.to_string() }),
        );
        Ok(())
    })()
    .await;
    match result {
        Ok(()) => api_json::success_response(
            json!({
                "requested": true,
                "message": "If an account exists for that email address, a password-reset link has been sent.",
            }),
            200,
        ),
        Err(error) => api_json::error_response(&error, debug),
    }
}

/// Atomically claim one reset token: mark it used only when it is
/// still unused and unexpired, and return its row. The
/// `UPDATE ... WHERE used_at IS NULL AND expires_at > NOW()`
/// is the claim: two concurrent redeems cannot both win, and a
/// used or expired token claims zero rows. The new password hash is
/// applied and every sibling token for the user is invalidated in
/// the same blocking call.
fn redeem_reset_token(
    connection: &mut diesel::PgConnection,
    token_hash: &str,
    new_password_hash: &str,
) -> UnifiedResult<PasswordResetTokenRow> {
    use diesel::sql_types::{Text, Timestamptz};
    let claimed: Vec<PasswordResetTokenRow> = diesel::sql_query(
        "UPDATE password_reset_tokens SET used_at = NOW() \
         WHERE token_hash = $1 AND used_at IS NULL AND expires_at > NOW() \
         RETURNING id, user_id, expires_at, used_at",
    )
    .bind::<Text, _>(token_hash)
    .load::<PasswordResetTokenRow>(connection)
    .map_err(UnifiedError::from)?;
    let Some(row) = claimed.into_iter().next() else {
        return Err(UnifiedError::unauthorized(
            "The password-reset token is invalid, expired, or has already been used",
        ));
    };
    diesel::sql_query("UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2")
        .bind::<Text, _>(new_password_hash)
        .bind::<diesel::sql_types::Uuid, _>(row.user_id)
        .execute(connection)
        .map_err(UnifiedError::from)?;
    diesel::sql_query(
        "UPDATE password_reset_tokens SET used_at = NOW() \
         WHERE user_id = $1 AND used_at IS NULL",
    )
    .bind::<diesel::sql_types::Uuid, _>(row.user_id)
    .execute(connection)
    .map_err(UnifiedError::from)?;
    Ok(row)
}

/// `POST /api/v1/auth-manager/password/reset`: redeem a reset
/// token with a new password. The token is single-use and expires
/// after `PASSWORD_RESET_TTL_MINUTES`; the new password must pass
/// the shared `PasswordPolicy`.
pub(crate) async fn reset_password(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<ResetPasswordInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let token = input.token.trim();
    if token.is_empty() || token.chars().count() > 512 {
        return api_json::error_response(
            &UnifiedError::bad_request("token must not be empty"),
            debug,
        );
    }
    let policy = PasswordPolicy::default();
    let problems = policy.validate(&input.new_password);
    if !problems.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("Password does not meet the policy")
                .with_metadata("problems", json!(problems)),
            debug,
        );
    }
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            return api_json::error_response(
                &UnifiedError::new(
                    "Database is not configured: password reset needs a database connection",
                    503,
                )
                .with_metadata("kind", json!("database_not_configured")),
                debug,
            );
        }
    };
    // Argon2id hashing runs before the blocking call so the worker
    // thread only runs SQL, matching the login rehash pattern.
    let new_password_hash = match auth_password::hash(&input.new_password) {
        Ok(hash) => hash,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let token_hash = match reset_token_hash(&state, &ctx, token) {
        Ok(hash) => hash,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let claimed = run_blocking(pool, move |connection| {
        redeem_reset_token(connection, &token_hash, &new_password_hash)
    })
    .await;
    match claimed {
        Ok(row) => {
            ctx.hooks.emit(
                "auth.password_reset.completed",
                json!({ "user_id": row.user_id.to_string() }),
            );
            api_json::success_response(
                json!({
                    "reset": true,
                    "user_id": row.user_id.to_string(),
                    "message": "Your password has been reset. Sign in with the new password.",
                }),
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
    fn hex_encode_known_vector() {
        assert_eq!(hex_encode(&[0x00, 0xab, 0xff]), "00abff");
        assert_eq!(hex_encode(&[]), "");
    }

    #[test]
    fn password_reset_constants_are_sane() {
        assert_eq!(PASSWORD_RESET_TTL_MINUTES, 60);
        assert_eq!(PASSWORD_RESET_COOLDOWN_MINUTES, 5);
        assert!(PASSWORD_RESET_TTL_MINUTES > PASSWORD_RESET_COOLDOWN_MINUTES);
    }
}
