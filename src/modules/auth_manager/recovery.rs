//! Auth Manager - Recovery codes: generation, verification, and lifecycle.

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
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use super::{providers::*, jwt::*, state::*, handlers_providers::*, login::*, tokens::*, totp::*, google::*, module::*};
use unified_core::error::UnifiedResult;

// ---------------------------------------------------------------------------
// Two-factor recovery codes
//
// A set of 10 single-use codes per user, offered as a TOTP alternative
// at login when the authenticator device is unavailable. Only Argon2id
// PHC hashes are stored (see `recovery_code_hash`); the plaintext codes
// are returned exactly once, in the generation response, and are never
// logged or returned again.
// A code is consumed (marked used) only where it stands in for a
// login factor -- password login (`issue_token`) and TOTP disable --
// through `try_consume_recovery_code`. The standalone verify endpoint
// below is deliberately read-only: it checks a code without consuming
// it, and requires the caller to be authenticated as that user (or an
// admin), so a read-only check can never burn a single-use code and
// can never be used to probe another user's codes. Regenerating a
// set invalidates every previously issued code for that user.
// ---------------------------------------------------------------------------

/// How many recovery codes one generated set contains.
pub const RECOVERY_CODE_COUNT: usize = 10;

/// One stored recovery code: hash only, never the plaintext.
#[derive(Debug, Clone)]
pub struct RecoveryCodeRecord {
    pub code_hash: String,
    pub used: bool,
    pub created_at: DateTime<Utc>,
    pub used_at: Option<DateTime<Utc>>,
}

/// Argon2id PHC hash of a normalized recovery code, using the same
/// parameters as password hashing (`auth::password`). A fast
/// unsalted hash (plain SHA-256) must never store a login factor:
/// the code alphabet is only 32 characters, so a stolen hash table
/// would fall to offline guessing. Argon2id makes each guess cost
/// the full memory-hard verification, exactly like a password.
pub(crate) fn recovery_code_hash(code: &str) -> UnifiedResult<String> {
    auth_password::hash(code)
}

/// Normalize a submitted code: strip dashes and whitespace, uppercase.
pub(crate) fn normalize_recovery_code(code: &str) -> String {
    code.chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .map(|ch| ch.to_ascii_uppercase())
        .collect()
}

/// Read-only recovery-code check: true when an unused code matches,
/// without marking it used. Verification must never consume a code;
/// consumption happens only at actual login / TOTP-disable time in
/// `try_consume_recovery_code`. Each stored record is verified with
/// Argon2id (PHC); a non-PHC stored value never verifies.
pub(crate) fn recovery_code_is_valid(
    state: &AuthManagerState,
    user_id: &str,
    code: &str,
) -> bool {
    let normalized = normalize_recovery_code(code);
    if normalized.len() != 12 {
        return false;
    }
    let Some(records) = state.recovery_codes.get(user_id) else {
        return false;
    };
    records
        .iter()
        .any(|record| !record.used && auth_password::verify(&normalized, &record.code_hash))
}

/// Generate one random recovery code, formatted `XXXX-XXXX-XXXX`
/// from an unambiguous alphabet (no `0`/`O`, `1`/`I`, `l`).
pub(crate) fn generate_recovery_code() -> String {
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = rand::random::<[u8; 12]>();
    let chars: Vec<char> = bytes
        .iter()
        .map(|byte| CHARSET[(*byte as usize) % CHARSET.len()] as char)
        .collect();
    let joined: String = chars.into_iter().collect();
    format!("{}-{}-{}", &joined[0..4], &joined[4..8], &joined[8..12])
}

pub(crate) fn validate_recovery_user_id(user_id: &str) -> UnifiedResult<String> {
    let user_id = user_id.trim();
    if user_id.is_empty() {
        return Err(UnifiedError::bad_request("user_id must not be empty"));
    }
    if user_id.chars().count() > 256 {
        return Err(UnifiedError::bad_request(
            "user_id must be at most 256 characters",
        ));
    }
    Ok(user_id.to_string())
}

pub(crate) fn remaining_recovery_codes(state: &AuthManagerState, user_id: &str) -> usize {
    state
        .recovery_codes
        .get(user_id)
        .map(|entry| entry.value().iter().filter(|record| !record.used).count())
        .unwrap_or(0)
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecoveryCodeUserInput {
    pub user_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VerifyRecoveryCodeInput {
    pub user_id: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RecoveryCodeUserQuery {
    pub user_id: String,
}

pub(crate) async fn generate_recovery_codes(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<RecoveryCodeUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // The plaintext codes are returned exactly once: only the account
    // owner (or an admin) may generate them. This used to be
    // unauthenticated for any user id, which let anyone mint and read
    // another user's recovery codes.
    let (caller_id, scopes) = match caller_from_request_write(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !caller_may_manage_user(&scopes, &caller_id, input.user_id.trim()) {
        return api_json::error_response(
            &UnifiedError::forbidden("Recovery codes can only be managed for your own account"),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&input.user_id)?;
        let previous = state
            .recovery_codes
            .get(&user_id)
            .map(|entry| entry.value().len())
            .unwrap_or(0);
        let now = Utc::now();
        let mut plaintext = Vec::with_capacity(RECOVERY_CODE_COUNT);
        let mut records = Vec::with_capacity(RECOVERY_CODE_COUNT);
        for _ in 0..RECOVERY_CODE_COUNT {
            let code = generate_recovery_code();
            plaintext.push(code.clone());
            records.push(RecoveryCodeRecord {
                code_hash: recovery_code_hash(&normalize_recovery_code(&code))?,
                used: false,
                created_at: now,
                used_at: None,
            });
        }
        state.recovery_codes.insert(user_id.clone(), records);
        ctx.hooks.emit(
            "auth.recovery_code.generated",
            json!({ "user_id": user_id, "count": RECOVERY_CODE_COUNT }),
        );
        Ok(json!({
            "user_id": user_id,
            // Shown exactly once: these codes are never stored in
            // plaintext and cannot be listed again.
            "codes": plaintext,
            "count": RECOVERY_CODE_COUNT,
            "remaining": RECOVERY_CODE_COUNT,
            "invalidated_previous": previous,
            "warning": "Store these codes in a safe place. Each code can be used once and will never be shown again.",
        }))
    })();
    match result {
        Ok(data) => api_json::success_response(data, 201),
        Err(error) => api_json::error_response(&error, debug),
    }
}

pub(crate) async fn verify_recovery_code(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<VerifyRecoveryCodeInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    // Verification is read-only and account-scoped: only the account
    // owner (or an admin) may check a code, and a successful check
    // never consumes it -- the code is consumed only when it is
    // actually used as a login factor (password login, TOTP disable).
    let (caller_id, scopes) = match caller_from_request_write(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !caller_may_manage_user(&scopes, &caller_id, input.user_id.trim()) {
        return api_json::error_response(
            &UnifiedError::forbidden("Recovery codes can only be verified for your own account"),
            debug,
        );
    }
    // A recovery code is a login factor: brute-force guessing feeds
    // the same lockout engine as password login, and a locked IP or
    // account gets 429 before any code is checked.
    let lockout_user_id = input.user_id.trim().to_string();
    let ip = client_ip_text(&req, &ctx);
    if !lockout_user_id.is_empty() {
        let hardening = shared_state();
        let ip_locked = ip
            .as_deref()
            .is_some_and(|ip| hardening.is_locked("ip", ip));
        if ip_locked || hardening.is_locked("account", &lockout_user_id) {
            return api_json::error_response(
                &UnifiedError::new(
                    "Too many failed attempts; try again later",
                    429,
                )
                .with_metadata("reason", json!("account_locked")),
                debug,
            );
        }
    }
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&input.user_id)?;
        // Read-only check: the code stays usable after this call.
        if !recovery_code_is_valid(&state, &user_id, &input.code) {
            return Err(UnifiedError::unauthorized(
                "Recovery code is invalid or has already been used",
            ));
        }
        let remaining = remaining_recovery_codes(&state, &user_id);
        ctx.hooks.emit(
            "auth.recovery_code.verified",
            json!({ "user_id": user_id, "remaining": remaining }),
        );
        Ok(json!({
            "valid": true,
            "user_id": user_id,
            "remaining": remaining,
        }))
    })();
    match result {
        Ok(data) => api_json::success_response(data, 200),
        Err(error) => {
            shared_state().record_login_failure(ip.as_deref(), Some(&lockout_user_id));
            ctx.hooks.emit(
                events::AUTH_FAILED,
                json!({
                    "provider": "recovery-code",
                    "ip": ip,
                    "user_id": lockout_user_id,
                    "reason": error.message,
                }),
            );
            api_json::error_response(&error, debug)
        }
    }
}

pub(crate) async fn regenerate_recovery_codes(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<RecoveryCodeUserInput>,
) -> HttpResponse {
    // Regeneration is generation with invalidation: the new set
    // replaces the old one, so every previously issued code stops
    // working immediately.
    generate_recovery_codes(req, state, ctx, input).await
}

pub(crate) async fn remaining_recovery_codes_handler(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    query: web::Query<RecoveryCodeUserQuery>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (caller_id, scopes) = match caller_from_request(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !caller_may_manage_user(&scopes, &caller_id, query.user_id.trim()) {
        return api_json::error_response(
            &UnifiedError::forbidden("Recovery codes can only be managed for your own account"),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&query.user_id)?;
        let total = state
            .recovery_codes
            .get(&user_id)
            .map(|entry| entry.value().len())
            .unwrap_or(0);
        Ok(json!({
            "user_id": user_id,
            "remaining": remaining_recovery_codes(&state, &user_id),
            "total": total,
            "has_codes": total > 0,
        }))
    })();
    api_json::respond(result, debug)
}

pub(crate) async fn revoke_recovery_codes(
    req: HttpRequest,
    state: web::Data<AuthManagerState>,
    ctx: web::Data<Arc<AppContext>>,
    input: web::Json<RecoveryCodeUserInput>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let (caller_id, scopes) = match caller_from_request_write(&state, &ctx, &req).await {
        Ok(caller) => caller,
        Err(error) => return api_json::error_response(&error, debug),
    };
    if !caller_may_manage_user(&scopes, &caller_id, input.user_id.trim()) {
        return api_json::error_response(
            &UnifiedError::forbidden("Recovery codes can only be managed for your own account"),
            debug,
        );
    }
    let result: UnifiedResult<Value> = (|| {
        let user_id = validate_recovery_user_id(&input.user_id)?;
        let removed = state
            .recovery_codes
            .remove(&user_id)
            .map(|(_, records)| records.len())
            .unwrap_or(0);
        ctx.hooks.emit(
            "auth.recovery_code.revoked",
            json!({ "user_id": user_id, "revoked_count": removed }),
        );
        Ok(json!({
            "user_id": user_id,
            "revoked": true,
            "revoked_count": removed,
        }))
    })();
    api_json::respond(result, debug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_recovery_code_cases() {
        assert_eq!(normalize_recovery_code("abcd-efgh ijkl"), "ABCDEFGHIJKL");
        assert_eq!(normalize_recovery_code("a-b c"), "ABC");
        assert_eq!(normalize_recovery_code(""), "");
    }

    #[test]
    fn recovery_code_hash_is_argon2id_phc() {
        let hash = recovery_code_hash("ABCDEFGHIJKL").expect("hashes");
        assert!(hash.starts_with("$argon2id$"));
        assert!(auth_password::verify("ABCDEFGHIJKL", &hash));
        assert!(!auth_password::verify("ABCDEFGHIJKM", &hash));
        // Fresh salt: the same code hashes differently each time.
        let other = recovery_code_hash("ABCDEFGHIJKL").expect("hashes");
        assert_ne!(hash, other);
        assert!(auth_password::verify("ABCDEFGHIJKL", &other));
    }

    #[test]
    fn generate_recovery_code_shape() {
        let code = generate_recovery_code();
        assert_eq!(code.len(), 14);
        assert_eq!(&code[4..5], "-");
        assert_eq!(&code[9..10], "-");
        for ch in code.chars().filter(|c| *c != '-') {
            assert!(
                "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".contains(ch),
                "unexpected char {ch}"
            );
        }
        assert_ne!(generate_recovery_code(), generate_recovery_code());
    }

    #[test]
    fn recovery_code_is_valid_is_read_only() {
        let state = AuthManagerState::default();
        let code = generate_recovery_code();
        state.recovery_codes.insert(
            "user-1".to_string(),
            vec![RecoveryCodeRecord {
                code_hash: recovery_code_hash(&normalize_recovery_code(&code)).expect("hashes"),
                used: false,
                created_at: Utc::now(),
                used_at: None,
            }],
        );
        // Verification succeeds without consuming the code.
        assert!(recovery_code_is_valid(&state, "user-1", &code));
        assert!(recovery_code_is_valid(&state, "user-1", &code));
        assert_eq!(remaining_recovery_codes(&state, "user-1"), 1);
        // A wrong code never validates, and a used code stops validating.
        assert!(!recovery_code_is_valid(&state, "user-1", "AAAA-AAAA-AAAA"));
        assert!(!recovery_code_is_valid(&state, "user-2", &code));
        // Consumption still works exactly once, at login / TOTP-disable time.
        assert!(try_consume_recovery_code(&state, "user-1", &code));
        assert!(!recovery_code_is_valid(&state, "user-1", &code));
        assert!(!try_consume_recovery_code(&state, "user-1", &code));
        assert_eq!(remaining_recovery_codes(&state, "user-1"), 0);
    }

    #[test]
    fn validate_recovery_user_id_cases() {
        assert_eq!(
            validate_recovery_user_id("  user-1  ").expect("trimmed"),
            "user-1"
        );
        assert!(validate_recovery_user_id("").is_err());
        assert!(validate_recovery_user_id("   ").is_err());
        assert!(validate_recovery_user_id(&"x".repeat(257)).is_err());
        assert!(validate_recovery_user_id(&"x".repeat(256)).is_ok());
    }
}
