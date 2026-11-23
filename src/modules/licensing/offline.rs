//! Licensing - Offline tokens: issue, verify, list, revoke.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{
    Module, ModuleRegistry, MODULE_ENTITLEMENT_DENIED_EVENT, MODULE_ENTITLEMENT_GRANTED_EVENT,
    RouteInfo,
};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::db::models::{License, LicenseActivation, NewLicense, NewLicenseActivation};
use crate::modules::apikeys::{
    authenticate_key, key_has_scope, note_used, AuthenticatedKey,
};
use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpRequest, HttpResponse};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use diesel::prelude::*;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use rand::Rng;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};
use uuid::Uuid;
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, helpers::*, product_handlers::*, verify::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// SaaS: offline tokens.
// ---------------------------------------------------------------------------

/// Signed offline-token payload. `token_id` is the server-side
/// record id; the token itself is never stored.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct OfflineTokenPayload {
    pub(crate) license_id: Uuid,
    pub(crate) product: String,
    pub(crate) expires_at: DateTime<Utc>,
    pub(crate) issued_at: DateTime<Utc>,
    pub(crate) token_id: Uuid,
}

pub(crate) fn constant_time_equal_bytes(expected: &[u8], actual: &[u8]) -> bool {
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Sign an offline-token payload:
/// `base64url(payload_json) + "." + base64url(HMAC-SHA256(payload_b64))`.
pub(crate) fn sign_offline_token(ctx: &AppContext, payload: &OfflineTokenPayload) -> UnifiedResult<String> {
    let payload_json = serde_json::to_string(payload)
        .map_err(|error| UnifiedError::internal(format!("Cannot encode offline token: {error}")))?;
    let payload_b64 = URL_SAFE_NO_PAD.encode(payload_json.as_bytes());
    let mut mac = HmacSha256::new_from_slice(&hmac_secret(ctx))
        .map_err(|_| UnifiedError::internal("Cannot initialise offline token signing"))?;
    mac.update(payload_b64.as_bytes());
    let signature_b64 = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    Ok(format!("{payload_b64}.{signature_b64}"))
}

/// Verify an offline token's signature and expiry WITHOUT a
/// database lookup. A revoked recorded token is NOT rejected here:
/// revocation is checked by the caller against the in-memory
/// record, and only when a record exists (see the trust model).
pub(crate) fn verify_offline_token_signature(
    ctx: &AppContext,
    token: &str,
) -> UnifiedResult<OfflineTokenPayload> {
    let (payload_b64, signature_b64) = token.split_once('.').ok_or_else(|| {
        UnifiedError::bad_request("Offline token must look like <payload>.<signature>")
    })?;
    let mut mac = HmacSha256::new_from_slice(&hmac_secret(ctx))
        .map_err(|_| UnifiedError::internal("Cannot initialise offline token verification"))?;
    mac.update(payload_b64.as_bytes());
    let expected = mac.finalize().into_bytes();
    let presented = URL_SAFE_NO_PAD
        .decode(signature_b64)
        .map_err(|_| UnifiedError::unauthorized("Offline token signature is not valid base64"))?;
    if !constant_time_equal_bytes(&expected, &presented) {
        return Err(UnifiedError::unauthorized(
            "Offline token signature verification failed",
        ));
    }
    let payload_json = URL_SAFE_NO_PAD
        .decode(payload_b64)
        .map_err(|_| UnifiedError::bad_request("Offline token payload is not valid base64"))?;
    let payload: OfflineTokenPayload = serde_json::from_slice(&payload_json)
        .map_err(|_| UnifiedError::bad_request("Offline token payload is not valid JSON"))?;
    if payload.expires_at <= Utc::now() {
        return Err(UnifiedError::unprocessable("Offline token has expired"));
    }
    Ok(payload)
}

pub(crate) async fn issue_offline_token(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    body: web::Json<IssueOfflineTokenRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = require_caller_capability(&ctx, &req, "licensing:manage")?;
        let id = id.into_inner();
        let license = load_license(&ctx, id)?
            .ok_or_else(|| UnifiedError::not_found("License not found"))?;
        if license.status != "active" {
            return Err(UnifiedError::unprocessable(format!(
                "Offline tokens can only be issued for active licenses (status is {})",
                license.status
            )));
        }
        let product_max = product_settings_for(&license.product)
            .map(|settings| settings.offline_max_days)
            .unwrap_or(DEFAULT_OFFLINE_MAX_DAYS)
            .clamp(1, MAX_OFFLINE_TOKEN_DAYS);
        let requested = body.max_days.unwrap_or(product_max);
        if requested < 1 {
            return Err(UnifiedError::bad_request("max_days must be at least 1"));
        }
        let effective_days = requested.min(product_max).min(MAX_OFFLINE_TOKEN_DAYS);
        let now = Utc::now();
        let payload = OfflineTokenPayload {
            license_id: id,
            product: license.product.clone(),
            expires_at: now + chrono::Duration::days(effective_days),
            issued_at: now,
            token_id: Uuid::now_v7(),
        };
        let token = sign_offline_token(&ctx, &payload)?;
        license_offline_tokens_store().insert(
            payload.token_id,
            LicenseOfflineToken {
                token_id: payload.token_id,
                license_id: id,
                product: license.product.clone(),
                expires_at: payload.expires_at,
                issued_at: now,
                revoked: false,
                created_at: now,
            },
        );
        append_license_event(
            &ctx,
            id,
            "offline_token_issued",
            Some(caller.actor.as_str()),
            serde_json::json!({
                "token_id": payload.token_id,
                "expires_at": payload.expires_at,
                "max_days": effective_days,
            }),
        );
        Ok(serde_json::json!({
            "token": token,
            "token_id": payload.token_id,
            "license_id": id,
            "product": license.product,
            "expires_at": payload.expires_at,
            "max_days": effective_days,
            "offline": true,
            "trust": OFFLINE_TRUST_NOTE,
        }))
    })();
    json::respond(result, debug)
}

/// Verify a signed offline token without a database lookup.
/// Returns the public-verify shape with `"offline": true`;
/// activation counts are null because no lookup happens, and the
/// trust note is included in every response.
pub(crate) async fn offline_verify_handler(
    ctx: web::Data<Arc<AppContext>>,
    body: web::Json<OfflineVerifyRequest>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        if body.token.trim().is_empty() {
            return Err(UnifiedError::bad_request("token is required"));
        }
        let payload = match verify_offline_token_signature(&ctx, body.token.trim()) {
            Ok(payload) => payload,
            Err(error) => {
                return Ok(serde_json::json!({
                    "valid": false,
                    "reason": "invalid",
                    "license_id": Value::Null,
                    "product": Value::Null,
                    "activations_used": Value::Null,
                    "max_activations": Value::Null,
                    "expires_at": Value::Null,
                    "grace": false,
                    "days_in_grace": 0,
                    "offline": true,
                    "error": error.message,
                    "trust": OFFLINE_TRUST_NOTE,
                }));
            }
        };
        // Revocation only applies when this process still holds the
        // token record; a token issued before a restart has no record
        // and verifies on signature + expiry alone (trust model).
        let revoked = license_offline_tokens_store()
            .get(&payload.token_id)
            .map(|entry| entry.value().revoked)
            .unwrap_or(false);
        if revoked {
            return Ok(serde_json::json!({
                "valid": false,
                "reason": "revoked",
                "license_id": payload.license_id,
                "product": payload.product,
                "activations_used": Value::Null,
                "max_activations": Value::Null,
                "expires_at": payload.expires_at,
                "grace": false,
                "days_in_grace": 0,
                "offline": true,
                "token_id": payload.token_id,
                "trust": OFFLINE_TRUST_NOTE,
            }));
        }
        let _ = &body.instance_id;
        Ok(serde_json::json!({
            "valid": true,
            "reason": "valid",
            "license_id": payload.license_id,
            "product": payload.product,
            "activations_used": Value::Null,
            "max_activations": Value::Null,
            "expires_at": payload.expires_at,
            "grace": false,
            "days_in_grace": 0,
            "offline": true,
            "token_id": payload.token_id,
            "trust": OFFLINE_TRUST_NOTE,
        }))
    })();
    json::respond(result, debug)
}

pub(crate) async fn list_offline_tokens(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    id: web::Path<Uuid>,
    params: web::Query<PaginationParams>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        require_caller_capability(&ctx, &req, "licensing:view")?;
        let id = id.into_inner();
        if load_license(&ctx, id)?.is_none() {
            return Err(UnifiedError::not_found("License not found"));
        }
        let mut items: Vec<LicenseOfflineToken> = license_offline_tokens_store()
            .iter()
            .filter(|entry| entry.value().license_id == id)
            .map(|entry| entry.value().clone())
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        if params.normalized().order == "asc" {
            items.reverse();
        }
        Ok(paginate(items, &params).to_json())
    })();
    json::respond(result, debug)
}

pub(crate) async fn revoke_offline_token(
    ctx: web::Data<Arc<AppContext>>,
    req: HttpRequest,
    path: web::Path<(Uuid, Uuid)>,
) -> HttpResponse {
    let debug = ctx.is_debug();
    let result: UnifiedResult<Value> = (|| {
        let caller = require_caller_capability(&ctx, &req, "licensing:manage")?;
        let (license_id, token_id) = path.into_inner();
        let Some(mut record) = license_offline_tokens_store().get_mut(&token_id) else {
            return Err(UnifiedError::not_found("Offline token not found"));
        };
        if record.license_id != license_id {
            return Err(UnifiedError::not_found(
                "Offline token not found for this license",
            ));
        }
        record.revoked = true;
        append_license_event(
            &ctx,
            license_id,
            "offline_token_revoked",
            Some(caller.actor.as_str()),
            serde_json::json!({ "token_id": token_id }),
        );
        Ok(serde_json::json!({
            "token_id": token_id,
            "license_id": license_id,
            "revoked": true,
        }))
    })();
    json::respond(result, debug)
}
