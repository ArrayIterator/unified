//! Auth Manager - JWT issue, verify, and revoke handlers.

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
use super::{providers::*, jwt::*, state::*, handlers_providers::*, login::*, recovery::*, totp::*, google::*, module::*};

/// Maximum JWT lifetime the caller may request, in seconds (1
/// hour). `expires_in_seconds` is caller-chosen but server-capped:
/// a long-lived bearer token is a long-lived credential, and there
/// is no refresh-token flow that would justify more. Requests above
/// the cap are clamped down, never rejected, so old clients keep
/// working with a shorter token.
pub(crate) const MAX_JWT_TTL_SECS: u64 = 3600;

/// The issuer claim written into issued tokens and expected on
/// verified ones: the `jwt` provider's configured `issuer` setting,
/// defaulting to `"unified"`. Issuance and verification share this
/// helper so they can never disagree about the expected issuer.
pub(crate) fn jwt_issuer(state: &AuthManagerState) -> String {
    state
        .provider_info("jwt")
        .and_then(|info| {
            info.settings
                .get("issuer")
                .and_then(|value| value.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "unified".to_string())
}

/// Validate a decoded token's time and issuer claims, after the
/// signature has been checked. Rejects tokens from another issuer
/// and tokens whose `iat` lies in the future beyond the clock-skew
/// leeway, as well as expired tokens.
pub(crate) fn validate_token_claims(
    claims: &JwtClaims,
    expected_issuer: &str,
    now_unix: i64,
) -> Result<(), UnifiedError> {
    if claims.iss != expected_issuer {
        return Err(UnifiedError::unauthorized(
            "token issuer is not trusted",
        ));
    }
    if claims.iat > now_unix + JWT_CLOCK_SKEW_LEEWAY_SECS {
        return Err(UnifiedError::unauthorized(
            "token was issued in the future",
        ));
    }
    if claims.exp <= now_unix {
        return Err(UnifiedError::unauthorized("token has expired"));
    }
    Ok(())
}

/// Password login: verify `email` + Argon2 `password` (+ TOTP when
/// the account has it enabled) against the users table, then mint a
/// JWT whose scopes come only from the user's database roles.
///
/// This endpoint never mints a token for an arbitrary user id and
/// never accepts caller-supplied scopes: both used to be accepted
/// here with no credential at all, which let anyone mint a token for
/// any user with any scopes. The response never reveals whether the
/// account exists, is disabled, or the password was wrong. Failed
/// attempts feed the hardening brute-force engine (per IP and per
/// account) and a locked IP or account gets 429 before any password
/// check runs; a successful login resets both counters.
pub(crate) async fn issue_token(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<IssueTokenInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let input = input.into_inner();
    if !state.is_enabled("jwt") {
        return api_json::error_response(
            &UnifiedError::forbidden("the JWT provider is disabled"),
            debug,
        );
    }
    let email = input.email.trim().to_lowercase();
    if email.is_empty() || email.chars().count() > 256 {
        return api_json::error_response(
            &UnifiedError::bad_request("email must not be empty and at most 256 characters"),
            debug,
        );
    }
    if input.password.is_empty() || input.password.chars().count() > 128 {
        return api_json::error_response(
            &UnifiedError::bad_request("password must not be empty and at most 128 characters"),
            debug,
        );
    }
    let ip = client_ip_text(&req, &ctx);
    let hardening = shared_state();
    if let Some(ip_text) = &ip {
        if hardening.is_locked("ip", ip_text) || hardening.is_locked("account", &email) {
            return api_json::error_response(
                &UnifiedError::new(
                    "Too many failed login attempts; the account or address is temporarily locked",
                    429,
                )
                .with_metadata("reason", json!("account_locked")),
                debug,
            );
        }
    }
    let pool = match ctx.db.read().expect("db lock poisoned").clone() {
        Some(pool) => pool,
        None => {
            return api_json::error_response(
                &UnifiedError::new(
                    "Database is not configured: password login needs a database connection",
                    503,
                )
                .with_metadata("kind", json!("database_not_configured")),
                debug,
            );
        }
    };
    let loaded = match load_login_user(pool.clone(), &email).await {
        Ok(loaded) => loaded,
        Err(error) => return api_json::error_response(&error, debug),
    };
    // Unknown account: run a dummy verification so the timing matches
    // a real password check, then fail exactly like a wrong password.
    let Some(loaded) = loaded else {
        let _ = auth_password::verify(&input.password, DUMMY_PASSWORD_HASH);
        hardening.record_login_failure(ip.as_deref(), Some(&email));
        ctx.hooks.emit(
            events::AUTH_FAILED,
            json!({
                "provider": "jwt",
                "ip": ip,
                "account": email,
                "reason": "invalid_credentials",
            }),
        );
        return api_json::error_response(&login_failed_error(), debug);
    };
    let user = loaded.user;
    if !user.is_active || !auth_password::verify(&input.password, &user.password_hash) {
        hardening.record_login_failure(ip.as_deref(), Some(&email));
        ctx.hooks.emit(
            events::AUTH_FAILED,
            json!({
                "provider": "jwt",
                "ip": ip,
                "user_id": user.id.to_string(),
                "account": email,
                "reason": "invalid_credentials",
            }),
        );
        return api_json::error_response(&login_failed_error(), debug);
    }
    // Second factor when the account has TOTP enabled: a valid TOTP
    // code, or one single-use recovery code (consumed on use). The
    // stored secret is decrypted first: confirmed secrets are
    // stored encrypted at rest, and a legacy plaintext row still
    // verifies (it is re-encrypted below, after login succeeds).
    let totp_key = {
        let config = ctx.config.read().expect("config lock poisoned");
        auth_totp::totp_encryption_key(&config.app.secret_key, &config.app.salt_key)
    };
    if user.totp_enabled {
        let totp_ok = match (&user.totp_secret, &input.totp_code) {
            (Some(stored), Some(code)) => auth_totp::decrypt_secret(stored, &totp_key)
                .is_some_and(|secret| auth_totp::verify(&secret, code)),
            _ => false,
        };
        let recovery_ok = match &input.recovery_code {
            Some(code) => try_consume_recovery_code(&state, &user.id.to_string(), code),
            None => false,
        };
        if !totp_ok && !recovery_ok {
            hardening.record_login_failure(ip.as_deref(), Some(&email));
            ctx.hooks.emit(
                events::AUTH_FAILED,
                json!({
                    "provider": "jwt",
                    "ip": ip,
                    "user_id": user.id.to_string(),
                    "account": email,
                    "reason": "totp_required",
                }),
            );
            return api_json::error_response(
                &UnifiedError::unauthorized(
                    "Two-factor authentication is required: provide a valid TOTP code or recovery code",
                )
                .with_metadata("kind", json!("totp_required")),
                debug,
            );
        }
    }
    if loaded.needs_rehash {
        let _ = rehash_password(pool.clone(), user.id, &input.password).await;
    }
    // A legacy plaintext TOTP secret that just verified is
    // re-encrypted in place, so the plaintext row disappears
    // without forcing the user to re-enrol. Best-effort: a failed
    // upgrade never blocks the login.
    if user.totp_enabled {
        if let Some(stored) = &user.totp_secret {
            if !auth_totp::is_encrypted(stored) {
                if let Some(encrypted) = auth_totp::encrypt_secret(stored, &totp_key) {
                    let _ = upgrade_totp_secret(pool.clone(), user.id, encrypted).await;
                }
            }
        }
    }
    let user_id = user.id.to_string();
    let scopes = loaded.scopes;
    let ttl = input
        .expires_in_seconds
        .unwrap_or(3600)
        .clamp(60, MAX_JWT_TTL_SECS);
    let now = Utc::now();
    let issuer = jwt_issuer(&state);
    let claims = JwtClaims {
        sub: user_id.clone(),
        iss: issuer,
        iat: now.timestamp(),
        exp: (now + chrono::Duration::seconds(ttl as i64)).timestamp(),
        jti: Uuid::now_v7().to_string(),
        scopes: scopes.clone(),
    };
    let header = JwtHeader {
        alg: "HS256".to_string(),
        typ: "JWT".to_string(),
    };
    let header_part = base64url_encode(
        serde_json::to_string(&header)
            .expect("JWT header serializes")
            .as_bytes(),
    );
    let payload_part = base64url_encode(
        serde_json::to_string(&claims)
            .expect("JWT claims serialize")
            .as_bytes(),
    );
    let signing_key = match state.signing_key(&ctx) {
        Ok(key) => key,
        Err(error) => return api_json::error_response(&error, debug),
    };
    let signing_input = format!("{header_part}.{payload_part}");
    let signature = base64url_encode(&hmac_sign(&signing_key, &signing_input));
    let token = format!("{signing_input}.{signature}");
    hardening.record_login_success(ip.as_deref(), Some(&email));
    ctx.hooks.emit(
        events::AUTH_LOGIN,
        json!({
            "user_id": user_id,
            "provider": "jwt",
            "ip": ip,
            "jti": claims.jti,
        }),
    );
    api_json::success_response(
        json!({
            "token": token,
            "token_type": "Bearer",
            "expires_in": ttl,
            "expires_at": claims.exp,
            "user_id": user_id,
            "scopes": scopes,
        }),
        200,
    )
}

/// Decode and verify a token. Returns the claims on success, or a
/// `UnifiedError` describing why the token is unusable.
pub(crate) fn verify_token(
    state: &AuthManagerState,
    ctx: &AppContext,
    token: &str,
) -> Result<JwtClaims, UnifiedError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(UnifiedError::bad_request(
            "token must have three base64url segments",
        ));
    }
    // The header is part of the signed input, but the algorithm must
    // still be pinned: only HS256 is ever issued, and any other `alg`
    // (including `none`) is rejected before the signature is checked.
    let header_bytes = base64url_decode(parts[0])?;
    let header: Value = serde_json::from_slice(&header_bytes).map_err(|_| {
        UnifiedError::bad_request("token header is not valid JSON")
    })?;
    if header.get("alg").and_then(|value| value.as_str()) != Some("HS256") {
        return Err(UnifiedError::unauthorized(
            "token algorithm must be HS256",
        ));
    }
    let signing_input = format!("{}.{}", parts[0], parts[1]);
    let provided = base64url_decode(parts[2])?;
    let signing_key = state.signing_key(ctx)?;
    let mut mac = HmacSha256::new_from_slice(&signing_key)
        .expect("HMAC accepts any key length");
    mac.update(signing_input.as_bytes());
    if mac.verify_slice(&provided).is_err() {
        return Err(UnifiedError::unauthorized("token signature is invalid"));
    }
    let payload_bytes = base64url_decode(parts[1])?;
    let claims: JwtClaims = serde_json::from_slice(&payload_bytes).map_err(|_| {
        UnifiedError::bad_request("token payload is not valid JSON")
    })?;
    let now_unix = Utc::now().timestamp();
    validate_token_claims(&claims, &jwt_issuer(state), now_unix)?;
    if state.is_token_revoked(&claims.jti, now_unix) {
        return Err(UnifiedError::unauthorized("token has been revoked"));
    }
    // Second opinion from the durable revocation table: the
    // in-memory map above only knows rows it wrote itself (or the
    // once-per-process hydration), so a revocation written by
    // another process - logout, another instance - would be
    // invisible here until a restart. When a pool is attached the
    // table is consulted directly; a lookup failure fails closed
    // (the token cannot be proven unrevoked), and without a pool
    // the map above is the only store there is.
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        match AuthManagerState::is_jti_revoked_in_db(&pool, &claims.jti) {
            Ok(true) => {
                return Err(UnifiedError::unauthorized("token has been revoked"));
            }
            Ok(false) => {}
            Err(_) => {
                return Err(UnifiedError::unauthorized(
                    "token revocation status could not be verified",
                ));
            }
        }
    }
    Ok(claims)
}

pub(crate) async fn verify_token_handler(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<TokenInput>,
) -> HttpResponse {
    let token = input.token.trim();
    if token.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("token must not be empty"),
            ctx.is_debug(),
        );
    }
    // Hydrate the persisted revocations first, so a token revoked
    // before a restart is still reported revoked.
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        state.ensure_revoked_tokens_hydrated(pool).await;
    }
    match verify_token(&state, &ctx, token) {
        Ok(claims) => api_json::success_response(
            json!({
                "valid": true,
                "user_id": claims.sub,
                "issuer": claims.iss,
                "issued_at": claims.iat,
                "expires_at": claims.exp,
                "scopes": claims.scopes,
            }),
            200,
        ),
        Err(error) => {
            ctx.hooks.emit(
                events::AUTH_FAILED,
                json!({ "provider": "jwt", "reason": error.message }),
            );
            api_json::success_response(
                json!({ "valid": false, "reason": error.message }),
                200,
            )
        }
    }
}

pub(crate) async fn revoke_token_handler(
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<TokenInput>,
) -> HttpResponse {
    let token = input.token.trim();
    if token.is_empty() {
        return api_json::error_response(
            &UnifiedError::bad_request("token must not be empty"),
            ctx.is_debug(),
        );
    }
    // Hydrate the persisted revocations first, so an already-revoked
    // token is reported as such even after a restart.
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        state.ensure_revoked_tokens_hydrated(pool).await;
    }
    // Only a genuinely signed token can be revoked; a forged token must
    // not be able to poison the revocation list.
    let claims = match verify_token(&state, &ctx, token) {
        Ok(claims) => claims,
        Err(error) => {
            // An already-revoked token is still a signed token: report it
            // as already revoked instead of failing the request.
            if error.message == "token has been revoked" {
                return api_json::success_response(
                    json!({ "revoked": true, "already_revoked": true }),
                    200,
                );
            }
            return api_json::error_response(&error, ctx.is_debug());
        }
    };
    let already_revoked = state
        .revoked_tokens
        .insert(
            claims.jti.clone(),
            RevokedToken {
                revoked_at: Utc::now(),
                expires_at: claims.exp,
            },
        )
        .is_some();
    // Drop revocation entries whose tokens have already expired, so
    // the map stays bounded by live revoked tokens.
    state.prune_revoked_tokens(Utc::now().timestamp());
    // Persist the revocation so it survives a restart (best-effort:
    // the in-memory insert above already took effect for this
    // process, and a database failure must not fail the revoke).
    if let Some(pool) = ctx.db.read().expect("db lock poisoned").clone() {
        state
            .persist_revocation_async(
                pool,
                claims.jti.clone(),
                claims.sub.clone(),
                Utc::now(),
                claims.exp,
            )
            .await;
    }
    ctx.hooks.emit(
        "auth.token.revoked",
        json!({ "user_id": claims.sub, "jti": claims.jti }),
    );
    ctx.hooks.emit(
        events::AUTH_LOGOUT,
        json!({ "user_id": claims.sub, "provider": "jwt" }),
    );
    api_json::success_response(
        json!({
            "revoked": true,
            "already_revoked": already_revoked,
            "user_id": claims.sub,
            "jti": claims.jti,
        }),
        200,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use unified_core::config::AppConfig;
    use unified_core::hooks::HookBus;
    use unified_core::modules::ModuleRegistry;

    fn test_context() -> AppContext {
        let mut config = AppConfig::default();
        // Signing keys are mandatory (no ephemeral fallback): tests
        // must configure them, exactly like a started server.
        config.app.secret_key = "test-secret-key-that-is-long-enough".to_string();
        config.app.salt_key = "test-salt-key-long-enough".to_string();
        AppContext::new(
            config,
            Arc::new(HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    fn claims(iss: &str, iat: i64, exp: i64) -> JwtClaims {
        JwtClaims {
            sub: "user-1".to_string(),
            iss: iss.to_string(),
            iat,
            exp,
            jti: Uuid::now_v7().to_string(),
            scopes: vec![],
        }
    }

    fn mint_token(
        state: &AuthManagerState,
        ctx: &AppContext,
        iss: &str,
        iat: i64,
        exp: i64,
        jti: &str,
    ) -> String {
        let claims = JwtClaims {
            sub: "user-1".to_string(),
            iss: iss.to_string(),
            iat,
            exp,
            jti: jti.to_string(),
            scopes: vec![],
        };
        let header = JwtHeader {
            alg: "HS256".to_string(),
            typ: "JWT".to_string(),
        };
        let header_part = base64url_encode(
            serde_json::to_string(&header).expect("header serializes").as_bytes(),
        );
        let payload_part = base64url_encode(
            serde_json::to_string(&claims)
                .expect("claims serialize")
                .as_bytes(),
        );
        let signing_input = format!("{header_part}.{payload_part}");
        let signing_key = state
            .signing_key(ctx)
            .expect("test context configures signing keys");
        let signature = base64url_encode(&hmac_sign(&signing_key, &signing_input));
        format!("{signing_input}.{signature}")
    }

    #[test]
    fn validate_token_claims_cases() {
        let now = 1_800_000_000;
        // Valid claims pass.
        assert!(validate_token_claims(&claims("unified", now, now + 3600), "unified", now).is_ok());
        // A small future iat within the clock-skew leeway passes.
        assert!(validate_token_claims(
            &claims("unified", now + JWT_CLOCK_SKEW_LEEWAY_SECS, now + 3600),
            "unified",
            now
        )
        .is_ok());
        // An issuer that does not match the configured issuer is rejected.
        let error = validate_token_claims(&claims("evil-issuer", now, now + 3600), "unified", now)
            .expect_err("wrong issuer must be rejected");
        assert_eq!(error.message, "token issuer is not trusted");
        // An iat beyond the leeway is rejected.
        let error = validate_token_claims(
            &claims("unified", now + JWT_CLOCK_SKEW_LEEWAY_SECS + 1, now + 7200),
            "unified",
            now,
        )
        .expect_err("future iat must be rejected");
        assert_eq!(error.message, "token was issued in the future");
        // An expired token is rejected.
        let error = validate_token_claims(&claims("unified", now - 100, now), "unified", now)
            .expect_err("expired token must be rejected");
        assert_eq!(error.message, "token has expired");
    }

    #[test]
    fn verify_token_rejects_wrong_issuer_and_future_iat() {
        let state = AuthManagerState::default();
        let ctx = test_context();
        let now = Utc::now().timestamp();
        let wrong_issuer = mint_token(&state, &ctx, "attacker", now, now + 3600, "jti-issuer");
        let error =
            verify_token(&state, &ctx, &wrong_issuer).expect_err("wrong issuer must be rejected");
        assert_eq!(error.message, "token issuer is not trusted");
        let future_iat = mint_token(
            &state,
            &ctx,
            "unified",
            now + JWT_CLOCK_SKEW_LEEWAY_SECS + 300,
            now + 7200,
            "jti-future",
        );
        let error =
            verify_token(&state, &ctx, &future_iat).expect_err("future iat must be rejected");
        assert_eq!(error.message, "token was issued in the future");
        let valid = mint_token(&state, &ctx, "unified", now, now + 3600, "jti-valid");
        let verified = verify_token(&state, &ctx, &valid).expect("valid token verifies");
        assert_eq!(verified.sub, "user-1");
        assert_eq!(verified.iss, "unified");
    }

    #[test]
    fn verify_token_rejects_revoked_token() {
        let state = AuthManagerState::default();
        let ctx = test_context();
        let now = Utc::now().timestamp();
        let token = mint_token(&state, &ctx, "unified", now, now + 3600, "jti-revoked");
        assert!(verify_token(&state, &ctx, &token).is_ok());
        state.revoked_tokens.insert(
            "jti-revoked".to_string(),
            RevokedToken {
                revoked_at: Utc::now(),
                expires_at: now + 3600,
            },
        );
        let error = verify_token(&state, &ctx, &token).expect_err("revoked token must be rejected");
        assert_eq!(error.message, "token has been revoked");
    }
}
