//! Licensing - Shared helpers for the licensing-as-a-service surface.

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
use super::{keys::*, expiry::*, store::*, hooks::*, types::*, validation::*, handlers::*, products::*, product_handlers::*, verify::*, offline::*, distribution::*, releases::*, module::*};

// ---------------------------------------------------------------------------
// Small shared helpers (SaaS).
// ---------------------------------------------------------------------------

pub(crate) fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let digest = hasher.finalize();
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

/// Constant-time comparison, the same construction the store module
/// uses for its download signatures.
pub(crate) fn constant_time_equal(expected: &str, actual: &str) -> bool {
    let expected = expected.as_bytes();
    let actual = actual.as_bytes();
    if expected.len() != actual.len() {
        return false;
    }
    expected
        .iter()
        .zip(actual.iter())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// HMAC signing secret: `store.download_signing_secret` when it is
/// configured (so signed tokens and download links survive restarts),
/// otherwise a random per-process secret generated once. The store
/// module documents the same fallback for its own signed links.
pub(crate) fn hmac_secret(ctx: &AppContext) -> Vec<u8> {
    let configured = ctx
        .config
        .read()
        .expect("config lock poisoned")
        .store
        .download_signing_secret
        .clone();
    if !configured.trim().is_empty() {
        return configured.into_bytes();
    }
    static FALLBACK_SECRET: OnceLock<Vec<u8>> = OnceLock::new();
    FALLBACK_SECRET
        .get_or_init(|| {
            log::warn!(
                "store.download_signing_secret is not set; licensing offline tokens and signed download links use a random per-process secret and stop verifying after a restart"
            );
            let mut rng = rand::rng();
            (0..32).map(|_| rng.random::<u8>()).collect()
        })
        .clone()
}

/// Load one license by id: in-memory store first, database when a
/// pool is attached and the license is not in memory.
pub(crate) fn load_license(ctx: &AppContext, id: Uuid) -> UnifiedResult<Option<License>> {
    if let Some(license) = licenses_store().get(&id) {
        return Ok(Some(license.value().clone()));
    }
    if let Some(pool) = db_pool(ctx) {
        return db_find_license_by_id(&pool, id);
    }
    Ok(None)
}

/// Load one license by id, database first: the database rows are
/// authoritative when a pool is attached, so a license that only
/// exists there (for example after a restart, before anything
/// repopulated the in-memory store) is still found. The in-memory
/// store is only consulted when no pool exists. Mutating admin
/// handlers use this loader; read paths that tolerate a warm cache
/// use `load_license`.
pub(crate) fn load_license_db_first(
    ctx: &AppContext,
    id: Uuid,
) -> UnifiedResult<Option<License>> {
    if let Some(pool) = db_pool(ctx) {
        return db_find_license_by_id(&pool, id);
    }
    Ok(licenses_store()
        .get(&id)
        .map(|entry| entry.value().clone()))
}

/// Persist a license's mutable fields (product, seats, status,
/// expiry) to the database when a pool is attached, and always to
/// the in-memory store.
pub(crate) fn persist_license(ctx: &AppContext, license: &License) -> UnifiedResult<()> {
    if let Some(pool) = db_pool(ctx) {
        use unified_core::db::schema::licenses::dsl as lic_dsl;
        #[derive(AsChangeset)]
        #[diesel(table_name = unified_core::db::schema::licenses)]
        struct LicenseUpdate {
            product: String,
            max_activations: i32,
            status: String,
            expires_at: Option<DateTime<Utc>>,
        }
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        diesel::update(lic_dsl::licenses.filter(lic_dsl::id.eq(license.id)))
            .set(LicenseUpdate {
                product: license.product.clone(),
                max_activations: license.max_activations,
                status: license.status.clone(),
                expires_at: license.expires_at,
            })
            .execute(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot update license: {error}"))
            })?;
    }
    licenses_store().insert(license.id, license.clone());
    Ok(())
}

/// Product settings for a slug (case-insensitive), if configured.
pub(crate) fn product_settings_for(product: &str) -> Option<LicenseProduct> {
    let needle = product.trim().to_lowercase();
    license_products_store()
        .iter()
        .find(|entry| entry.value().product.to_lowercase() == needle)
        .map(|entry| entry.value().clone())
}

/// Grace period (days) for a product; 0 when the product has no
/// settings or configures no grace.
pub(crate) fn grace_days_for(product: &str) -> i64 {
    product_settings_for(product)
        .map(|settings| settings.grace_period_days.max(0))
        .unwrap_or(0)
}

/// Renewal-reminder windows (days) for a product; the documented
/// defaults when the product has no settings.
pub(crate) fn renewal_reminder_days_for(product: &str) -> Vec<i64> {
    match product_settings_for(product) {
        Some(settings) if !settings.renewal_reminder_days.is_empty() => {
            settings.renewal_reminder_days
        }
        _ => DEFAULT_RENEWAL_REMINDER_DAYS.to_vec(),
    }
}

/// `(in_grace, days_in_grace)` for a license: true when the license
/// is past `expires_at` but the overrun is within the product grace
/// period. A license already marked `expired` is not revived by
/// grace: grace only applies while the status is still `active`.
/// The boundary is exact to the second: the overrun in seconds must
/// be at most `grace_days * 86_400` (a whole-day comparison would
/// keep a license in grace for almost a day past its grace period).
/// `days_in_grace` is whole days elapsed since `expires_at` (0 on
/// the first day past expiry).
pub(crate) fn grace_state(license: &License) -> (bool, i64) {
    if license.status != "active" {
        return (false, 0);
    }
    let Some(expires_at) = license.expires_at else {
        return (false, 0);
    };
    let now = Utc::now();
    if expires_at > now {
        return (false, 0);
    }
    let grace_days = grace_days_for(&license.product);
    if grace_days <= 0 {
        return (false, 0);
    }
    let overdue_secs = (now - expires_at).num_seconds();
    if overdue_secs <= grace_days * 86_400 {
        (true, overdue_secs.div_euclid(86_400))
    } else {
        (false, 0)
    }
}

/// Strip a product's known key prefix (for example `UNF-`) from a
/// key; keys that do not carry the prefix are returned unchanged.
pub(crate) fn strip_product_prefix<'a>(key: &'a str, product: &str) -> &'a str {
    if let Some(settings) = product_settings_for(product) {
        if let Some(prefix) = &settings.key_prefix {
            let prefix = prefix.trim().to_uppercase();
            if !prefix.is_empty() {
                let prefixed = format!("{prefix}-");
                if let Some(rest) = key.strip_prefix(prefixed.as_str()) {
                    return rest;
                }
            }
        }
    }
    key
}

/// Checksum validation for a key that may carry a product prefix:
/// strip the product's known prefix (for example `UNF-`) and check
/// the remaining 16-character body. Keys for products without
/// settings, or without a prefix, use the plain checksum check.
pub(crate) fn license_key_checksum_valid(key: &str, product: &str) -> bool {
    key_checksum_valid(strip_product_prefix(key, product))
}

/// Generate a key honouring a product's format: an optional prefix
/// plus `segments` 4-character groups whose final character is the
/// checksum over the body characters. A `custom` format must also
/// match the product regex; generation retries until a candidate
/// matches (bounded), then fails instead of issuing a bad key.
pub fn generate_product_key(product: &LicenseProduct) -> UnifiedResult<String> {
    let prefix = product
        .key_prefix
        .clone()
        .unwrap_or_default()
        .trim()
        .to_uppercase();
    let segments = product.segments.clamp(1, 8) as usize;
    let body_chars = segments * 4;
    let pattern = match &product.key_pattern {
        Some(pattern) => Some(
            Regex::new(pattern)
                .map_err(|error| UnifiedError::bad_request(format!("Invalid key pattern: {error}")))?,
        ),
        None => None,
    };
    for _ in 0..32 {
        let mut indices: Vec<usize> = (0..body_chars - 1)
            .map(|_| rand::rng().random_range(0..KEY_ALPHABET.len()))
            .collect();
        let checksum = indices.iter().sum::<usize>() % KEY_ALPHABET.len();
        indices.push(checksum);
        let chars: Vec<char> = indices
            .iter()
            .map(|index| KEY_ALPHABET[*index] as char)
            .collect();
        let body: String = chars
            .chunks(4)
            .map(|chunk| chunk.iter().collect::<String>())
            .collect::<Vec<String>>()
            .join("-");
        let key = if prefix.is_empty() {
            body
        } else {
            format!("{prefix}-{body}")
        };
        if product.key_format == KEY_FORMAT_CUSTOM {
            match &pattern {
                Some(regex) if regex.is_match(&key) => return Ok(key),
                Some(_) => continue,
                None => {
                    return Err(UnifiedError::bad_request(
                        "Custom key format requires a key_pattern",
                    ))
                }
            }
        }
        return Ok(key);
    }
    Err(UnifiedError::bad_request(
        "Custom key pattern could not be satisfied by generated keys",
    ))
}

/// Append one hash-chained entry to the license event log and emit
/// `license.anchored` with `{license_id, event_id, hash}`. The
/// chain is tamper-evident: any edit to an earlier entry breaks
/// every later hash (see `verify_event_chain`). Appends for one
/// license are serialised on a per-license mutex: two concurrent
/// appends would otherwise read the same `prev_hash`, fork the
/// chain, and fail verification. The hook is emitted after the
/// mutex is released, so a listener can never deadlock against an
/// append for the same license.
pub(crate) fn append_license_event(
    ctx: &AppContext,
    license_id: Uuid,
    event: &str,
    actor: Option<&str>,
    details: Value,
) -> LicenseEvent {
    let entry = {
        let event_lock = event_lock_for(license_id);
        let _guard = event_lock
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let prev_hash = license_event_last_hash_store()
            .get(&license_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_else(|| "GENESIS".to_string());
        let details_json = serde_json::to_string(&details).unwrap_or_default();
        let canonical = format!(
            "{prev_hash}\n{license_id}\n{event}\n{}\n{details_json}",
            actor.unwrap_or("")
        );
        let hash = sha256_hex(canonical.as_bytes());
        let entry = LicenseEvent {
            id: Uuid::now_v7(),
            license_id,
            event: event.to_string(),
            actor: actor.map(str::to_string),
            details,
            prev_hash: prev_hash.clone(),
            hash: hash.clone(),
            created_at: Utc::now(),
        };
        license_events_store().insert(entry.id, entry.clone());
        license_event_last_hash_store().insert(license_id, hash.clone());
        entry
    };
    ctx.hooks.emit(
        LICENSE_ANCHORED,
        serde_json::json!({
            "license_id": license_id,
            "event_id": entry.id,
            "hash": entry.hash.clone(),
        }),
    );
    entry
}

/// Recompute a license's event chain in creation order. Returns
/// `(valid, broken_at)`: `broken_at` is the first entry id whose
/// stored hash does not match its recomputed hash, or whose
/// `prev_hash` does not match the previous entry.
pub(crate) fn verify_event_chain(license_id: Uuid) -> (bool, Option<Uuid>) {
    let mut entries: Vec<LicenseEvent> = license_events_store()
        .iter()
        .filter(|entry| entry.value().license_id == license_id)
        .map(|entry| entry.value().clone())
        .collect();
    entries.sort_by(|a, b| a.created_at.cmp(&b.created_at).then(a.id.cmp(&b.id)));
    let mut prev_hash = "GENESIS".to_string();
    for entry in &entries {
        if entry.prev_hash != prev_hash {
            return (false, Some(entry.id));
        }
        let details_json = serde_json::to_string(&entry.details).unwrap_or_default();
        let canonical = format!(
            "{prev_hash}\n{}\n{}\n{}\n{details_json}",
            entry.license_id,
            entry.event,
            entry.actor.clone().unwrap_or_default()
        );
        if sha256_hex(canonical.as_bytes()) != entry.hash {
            return (false, Some(entry.id));
        }
        prev_hash = entry.hash.clone();
    }
    (true, None)
}

/// Record one activation-history row (activate or deactivate).
pub(crate) fn record_activation_history(
    license_id: Uuid,
    action: &str,
    actor: Option<&str>,
    instance_id: &str,
    hostname: Option<&str>,
) {
    let id = Uuid::now_v7();
    license_activation_history_store().insert(
        id,
        LicenseActivationHistory {
            id,
            license_id,
            action: action.to_string(),
            actor: actor.map(str::to_string),
            instance_id: instance_id.to_string(),
            hostname: hostname.map(str::to_string),
            created_at: Utc::now(),
        },
    );
}

/// Revoke every recorded offline token of a license (server-side
/// only; already-issued offline verifications are unaffected).
pub(crate) fn revoke_offline_tokens_for_license(license_id: Uuid) {
    for mut entry in license_offline_tokens_store().iter_mut() {
        if entry.value().license_id == license_id {
            entry.value_mut().revoked = true;
        }
    }
}

// ---------------------------------------------------------------------------
// Capability checks and public API-key authentication (SaaS).
// ---------------------------------------------------------------------------

/// Caller identity: the server-verified caller (user id plus
/// server-resolved role, both required for capability checks),
/// exactly like the users, field_service,
/// and admissions modules. The role comes from a header the gateway
/// sets after authentication, never from a query parameter: the
/// previous `?viewer_role=` / `?viewer_id=` shape let any caller
/// claim any role, so it was removed.
pub(crate) struct Caller {
    pub(crate) actor: String,
    pub(crate) role: String,
}

pub(crate) fn caller(req: &HttpRequest) -> UnifiedResult<Caller> {
    let verified = unified_core::caller::require_caller(req)?;
    let actor = verified.user_id.clone();
    let role = verified.role().to_string();
    Ok(Caller { actor, role })
}

/// Fail when no role was supplied or the roles module does not grant
/// `permission` for it (the projects module's `require_capability`
/// pattern: ask `role.check` on the hook bus, fail closed on silence).
/// The role always arrives through `caller` (the `X-Role` header),
/// never from query parameters.
pub(crate) fn require_licensing_capability(
    ctx: &AppContext,
    role: Option<&str>,
    permission: &str,
) -> UnifiedResult<()> {
    let denied = || {
        UnifiedError::forbidden(format!(
            "Role lacks required capability: {permission}"
        ))
    };
    let Some(role) = role.map(str::trim).filter(|value| !value.is_empty()) else {
        return Err(denied());
    };
    let outcome = ctx.hooks.emit(
        "role.check",
        serde_json::json!({
            "role": role,
            "permission": permission,
            "allowed": false,
        }),
    );
    if outcome
        .get("allowed")
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
    {
        Ok(())
    } else {
        Err(denied())
    }
}

/// Resolve the caller from the `X-User-Id` / `X-Role` headers and
/// require `permission` for their role in one step (the sibling
/// modules' `require_caller_permission` shape). Returns the caller
/// so handlers can record the `X-User-Id` value as the audit actor.
pub(crate) fn require_caller_capability(
    ctx: &AppContext,
    req: &HttpRequest,
    permission: &str,
) -> UnifiedResult<Caller> {
    let caller = caller(req)?;
    require_licensing_capability(ctx, Some(&caller.role), permission)?;
    Ok(caller)
}

/// Read the presented API key from `Authorization: Bearer <key>` or
/// `X-Api-Key` (the MCP module's shape). The key value itself is
/// never logged and never stored.
pub(crate) fn presented_api_key(req: &HttpRequest) -> Option<String> {
    if let Some(header) = req.headers().get("authorization") {
        if let Ok(text) = header.to_str() {
            if let Some(key) = text.strip_prefix("Bearer ") {
                let key = key.trim();
                if !key.is_empty() {
                    return Some(key.to_string());
                }
            }
        }
    }
    req.headers()
        .get("x-api-key")
        .and_then(|header| header.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Per-API-key per-minute rate limit for public verification calls.
/// The cap is the key's own tier limit (`requests_per_minute` from
/// the API Keys module); 0 means unlimited. Returns 429 with
/// `retry_after_secs` in metadata when the window is exhausted.
pub(crate) fn check_verify_rate(key_id: &str, limit: u32) -> UnifiedResult<()> {
    if limit == 0 {
        return Ok(());
    }
    let now = Instant::now();
    let mut bucket = verify_buckets_store()
        .entry(key_id.to_string())
        .or_insert((now, 0));
    if bucket.0.elapsed() >= Duration::from_secs(60) {
        *bucket = (now, 1);
        return Ok(());
    }
    if bucket.1 >= limit {
        let retry_after = 60u64.saturating_sub(bucket.0.elapsed().as_secs()).max(1);
        return Err(UnifiedError::new(
            "API key rate limit exceeded for license verification",
            429,
        )
        .with_metadata("retry_after_secs", serde_json::json!(retry_after)));
    }
    bucket.1 += 1;
    Ok(())
}

/// Authenticate a public licensing call: a valid, unrevoked,
/// unexpired API key scoped to `licensing:<action>` (or
/// `licensing:*` / `*`, via `key_has_scope`), within its per-minute
/// rate limit. The license key itself is never an authentication
/// credential here. On success the key usage is recorded.
pub(crate) fn authenticate_public_key(
    req: &HttpRequest,
    action: &str,
) -> UnifiedResult<AuthenticatedKey> {
    let presented = presented_api_key(req).ok_or_else(|| {
        UnifiedError::unauthorized(
            "Missing API key: send Authorization: Bearer <key> or X-Api-Key",
        )
    })?;
    let authenticated = authenticate_key(&presented)?;
    if !key_has_scope(&authenticated.scopes, "licensing", action) {
        return Err(UnifiedError::forbidden(format!(
            "API key is not scoped to licensing:{action}; grant scope \"licensing:{action}\", \"licensing:*\", or \"*\""
        )));
    }
    check_verify_rate(
        &authenticated.id.to_string(),
        authenticated.requests_per_minute,
    )?;
    note_used(authenticated.id);
    Ok(authenticated)
}

/// Mask a license key for audit payloads: first four characters
/// plus an ellipsis (the full key never goes into a failure event).
pub(crate) fn mask_license_key(key: &str) -> String {
    let key = key.trim();
    if key.len() <= 4 {
        "****".to_string()
    } else {
        format!("{}...", &key[..4])
    }
}

/// Count a failed public verification and emit
/// `license.verify.failed` (masked key only).
pub(crate) fn record_failed_verification(
    ctx: &AppContext,
    key: &str,
    product: &str,
    reason: &str,
    api_key_id: Option<Uuid>,
) {
    let normalized = normalize_key(key);
    let mut count = failed_verifications_store()
        .entry(normalized)
        .or_insert(0);
    *count += 1;
    let attempts = *count;
    drop(count);
    ctx.hooks.emit(
        LICENSE_VERIFY_FAILED,
        serde_json::json!({
            "license_key_masked": mask_license_key(key),
            "product": product,
            "reason": reason,
            "api_key_id": api_key_id,
            "failed_attempts_for_key": attempts,
        }),
    );
}

/// Reject tenant-resolved requests from this control-plane
/// handler (C3 task 315, decision (a)): platform writes go only
/// through an explicit, audited platform scope. Mirrors
/// `tenancy::ControlPlaneGuard` — a request that resolved to a
/// tenant host is rejected outright (403); the guard's predicate
/// (`ControlPlaneGuard::is_control_plane_request`) is the single
/// source of truth, so this never drifts from the guard.
pub(crate) fn require_control_plane(req: &HttpRequest) -> UnifiedResult<()> {
    if unified_core::tenancy::ControlPlaneGuard::is_control_plane_request(req) {
        Ok(())
    } else {
        Err(UnifiedError::forbidden(
            "Tenant requests cannot access the control plane",
        )
        .with_metadata(
            "kind",
            serde_json::Value::String("tenant_control_plane_blocked".to_string()),
        ))
    }
}

#[cfg(test)]
mod tests {

    /// Build a request carrying the server-verified caller the auth
    /// middleware would attach for this user and role. The old
    /// self-asserted identity headers grant nothing,
    /// so tests attach the verified caller to the request
    /// extensions instead.
    fn verified_request(user_id: &str, role: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut().insert(
            unified_core::auth::Caller::verified(test_user_uuid(user_id), Vec::new())
                .with_roles(vec![role.to_string()]),
        );
        req
    }

    /// Build a request carrying a verified caller with no roles
    /// resolved: the caller resolves, but holds the anonymous role.
    /// This is the "no role" case in the new model (roles are
    /// resolved server-side, never claimed in a header).
    fn verified_request_without_roles(user_id: &str) -> actix_web::HttpRequest {
        use actix_web::HttpMessage;
        let req = actix_web::test::TestRequest::default().to_http_request();
        req.extensions_mut()
            .insert(unified_core::auth::Caller::verified(
                test_user_uuid(user_id),
                Vec::new(),
            ));
        req
    }

    /// Deterministic test-user identity: distinct names map to
    /// distinct UUIDs (FNV-1a folded into the low bits), so
    /// ownership tests can still tell two users apart. A value
    /// that already parses as a UUID is used as-is.
    fn test_user_uuid(name: &str) -> uuid::Uuid {
        if let Ok(id) = uuid::Uuid::parse_str(name) {
            return id;
        }
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in name.bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        uuid::Uuid::from_u128(u128::from(hash) | 0x0000_0000_0000_4000_8000_0000_0000_0000)
    }

    use super::*;

    fn test_license(expires_at: Option<chrono::DateTime<chrono::Utc>>) -> License {
        License {
            id: uuid::Uuid::new_v4(),
            license_key: "TEST-KEY".to_string(),
            product: "test-product-no-settings-9f2c".to_string(),
            user_id: None,
            company_id: None,
            max_activations: 1,
            status: "active".to_string(),
            expires_at,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn test_product() -> LicenseProduct {
        LicenseProduct {
            id: uuid::Uuid::new_v4(),
            product: "test-product".to_string(),
            display_name: None,
            owner_id: None,
            key_prefix: None,
            segments: 4,
            key_format: KEY_FORMAT_STANDARD.to_string(),
            key_pattern: None,
            default_max_activations: 1,
            default_validity_days: None,
            grace_period_days: 0,
            renewal_reminder_days: Vec::new(),
            offline_max_days: 0,
            allowed_distribution_channels: Vec::new(),
            api_allowed_endpoints: Vec::new(),
            api_rate_limit_per_minute: 60,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn sha256_hex_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(sha256_hex(b"abc").len(), 64);
        assert_ne!(sha256_hex(b"abc"), sha256_hex(b"abd"));
    }

    #[test]
    fn constant_time_equal_cases() {
        assert!(constant_time_equal("abc", "abc"));
        assert!(constant_time_equal("", ""));
        assert!(!constant_time_equal("abc", "abd"));
        assert!(!constant_time_equal("abc", "ab"));
        assert!(!constant_time_equal("ab", "abc"));
    }

    #[test]
    fn mask_license_key_cases() {
        assert_eq!(mask_license_key("ABCD-EFGH-IJKL-MNOP"), "ABCD...");
        assert_eq!(mask_license_key("ABC"), "****");
        assert_eq!(mask_license_key("ABCD"), "****");
        assert_eq!(mask_license_key("ABCDE"), "ABCD...");
        assert_eq!(mask_license_key("  ABCDEFGH  "), "ABCD...");
    }

    #[test]
    fn verify_event_chain_empty_store_is_valid() {
        assert_eq!(verify_event_chain(uuid::Uuid::nil()), (true, None));
    }

    #[test]
    fn grace_state_without_expiry_is_not_in_grace() {
        assert_eq!(grace_state(&test_license(None)), (false, 0));
    }

    #[test]
    fn grace_state_future_expiry_is_not_in_grace() {
        let future = chrono::Utc::now() + chrono::Duration::days(30);
        assert_eq!(grace_state(&test_license(Some(future))), (false, 0));
    }

    #[test]
    fn grace_state_expired_without_product_settings_is_not_in_grace() {
        // "test-product-no-settings-9f2c" has no settings in the in-memory store, so the
        // grace period is 0 and an expired license is not in grace.
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        assert_eq!(grace_state(&test_license(Some(past))), (false, 0));
    }

    #[test]
    fn generate_product_key_standard_has_valid_checksum() {
        let key = generate_product_key(&test_product()).expect("standard key");
        assert_eq!(key.len(), 19);
        assert!(key_checksum_valid(&key));
    }

    #[test]
    fn generate_product_key_with_prefix_is_uppercased() {
        let mut product = test_product();
        product.key_prefix = Some("unf".to_string());
        let key = generate_product_key(&product).expect("prefixed key");
        assert!(key.starts_with("UNF-"));
        assert_eq!(key.len(), 23);
        assert!(key_checksum_valid(&key[4..]));
    }

    #[test]
    fn generate_product_key_two_segments() {
        let mut product = test_product();
        product.segments = 2;
        let key = generate_product_key(&product).expect("two-segment key");
        assert_eq!(key.len(), 9);
        assert!(key_checksum_valid(&key));
    }

    #[test]
    fn generate_product_key_custom_without_pattern_fails() {
        let mut product = test_product();
        product.key_format = KEY_FORMAT_CUSTOM.to_string();
        product.key_pattern = None;
        assert!(generate_product_key(&product).is_err());
    }

    fn test_context() -> AppContext {
        AppContext::new(
            unified_core::config::AppConfig::default(),
            std::sync::Arc::new(unified_core::hooks::HookBus::new()),
            std::sync::Arc::new(unified_core::modules::ModuleRegistry::new()),
        )
    }

    fn test_request() -> HttpRequest {
        actix_web::test::verified_request("user-1", "admin")
    }

    #[test]
    fn caller_accepts_a_verified_caller() {
        let found = caller(&test_request()).expect("caller");
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    #[test]
    fn caller_rejects_a_request_without_a_verified_caller() {
        let req = actix_web::test::TestRequest::default().to_http_request();
        assert!(caller(&req).is_err());
    }

    #[test]
    fn caller_without_a_resolved_role_is_anonymous() {
        // A verified caller with no role resolved holds the anonymous
        // role: the identity resolves, the capability check is what
        // refuses it.
        let req = verified_request_without_roles("user-1");
        let found = caller(&req).expect("a verified caller resolves without a role");
        assert_eq!(found.role, "anonymous");
    }

    #[test]
    fn require_caller_capability_fails_closed_without_a_role_check_handler() {
        // No `role.check` handler is registered on this bus, so the
        // preset `"allowed": false` must stand and the call is refused.
        let ctx = test_context();
        assert!(require_caller_capability(&ctx, &test_request(), "licensing:manage").is_err());
    }

    #[test]
    fn require_caller_capability_accepts_when_role_check_allows() {
        let ctx = test_context();
        ctx.hooks.register(
            "role.check",
            "test",
            std::sync::Arc::new(|context: &mut unified_core::hooks::HookContext| {
                context.set("allowed", serde_json::Value::Bool(true));
            }),
        );
        let found =
            require_caller_capability(&ctx, &test_request(), "licensing:manage").expect("allowed");
        assert_eq!(found.actor, test_user_uuid("user-1").to_string());
        assert_eq!(found.role, "admin");
    }

    fn insert_test_product(slug: &str, grace_days: i64, key_prefix: Option<&str>) {
        let now = chrono::Utc::now();
        license_products_store().insert(
            uuid::Uuid::new_v4(),
            LicenseProduct {
                id: uuid::Uuid::new_v4(),
                product: slug.to_string(),
                display_name: None,
                owner_id: None,
                key_prefix: key_prefix.map(str::to_string),
                segments: 4,
                key_format: KEY_FORMAT_STANDARD.to_string(),
                key_pattern: None,
                default_max_activations: 1,
                default_validity_days: None,
                grace_period_days: grace_days,
                renewal_reminder_days: Vec::new(),
                offline_max_days: 0,
                allowed_distribution_channels: Vec::new(),
                api_allowed_endpoints: Vec::new(),
                api_rate_limit_per_minute: 60,
                created_at: now,
                updated_at: now,
            },
        );
    }

    fn grace_test_license(
        slug: &str,
        status: &str,
        expires_at: chrono::DateTime<chrono::Utc>,
    ) -> License {
        License {
            id: uuid::Uuid::new_v4(),
            license_key: "TEST-KEY".to_string(),
            product: slug.to_string(),
            user_id: None,
            company_id: None,
            max_activations: 1,
            status: status.to_string(),
            expires_at: Some(expires_at),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn grace_state_boundary_is_exact_to_the_second() {
        let slug = format!("grace-boundary-{}", uuid::Uuid::new_v4());
        insert_test_product(&slug, 7, None);
        let now = chrono::Utc::now();
        // Exactly 7 days overdue: still in grace, on its last second.
        let license =
            grace_test_license(&slug, "active", now - chrono::Duration::seconds(7 * 86_400));
        assert_eq!(grace_state(&license), (true, 7));
        // One second past the 7-day grace period: out of grace. The
        // old whole-day comparison kept this license in grace.
        let license =
            grace_test_license(&slug, "active", now - chrono::Duration::seconds(7 * 86_400 + 1));
        assert_eq!(grace_state(&license), (false, 0));
        // 7 days and 23 hours overdue: out of grace, not day 7.
        let license = grace_test_license(
            &slug,
            "active",
            now - chrono::Duration::seconds(7 * 86_400 + 23 * 3_600),
        );
        assert_eq!(grace_state(&license), (false, 0));
        // One day overdue inside a 7-day grace period: day 1.
        let license =
            grace_test_license(&slug, "active", now - chrono::Duration::seconds(86_400));
        assert_eq!(grace_state(&license), (true, 1));
    }

    #[test]
    fn grace_state_never_revives_a_non_active_license() {
        let slug = format!("grace-status-{}", uuid::Uuid::new_v4());
        insert_test_product(&slug, 7, None);
        let past = chrono::Utc::now() - chrono::Duration::days(1);
        // Already marked expired: grace does not revive it, even
        // though the overrun is inside the product grace period.
        assert_eq!(
            grace_state(&grace_test_license(&slug, "expired", past)),
            (false, 0)
        );
        assert_eq!(
            grace_state(&grace_test_license(&slug, "suspended", past)),
            (false, 0)
        );
        assert_eq!(
            grace_state(&grace_test_license(&slug, "revoked", past)),
            (false, 0)
        );
    }

    #[test]
    fn strip_product_prefix_cases() {
        // No settings for the product: the key is unchanged.
        assert_eq!(
            strip_product_prefix("ABCD-EFGH", "no-such-product-xyz"),
            "ABCD-EFGH"
        );
        let slug = format!("prefix-product-{}", uuid::Uuid::new_v4());
        insert_test_product(&slug, 0, Some("unf"));
        assert_eq!(
            strip_product_prefix("UNF-ABCD-EFGH", &slug),
            "ABCD-EFGH"
        );
        // A key without the prefix is unchanged.
        assert_eq!(
            strip_product_prefix("ABCD-EFGH", &slug),
            "ABCD-EFGH"
        );
    }

    #[test]
    fn load_license_db_first_falls_back_to_memory_without_a_pool() {
        let ctx = test_context();
        let license = test_license(None);
        licenses_store().insert(license.id, license.clone());
        assert_eq!(
            load_license_db_first(&ctx, license.id)
                .expect("load")
                .map(|row| row.id),
            Some(license.id)
        );
        assert!(load_license_db_first(&ctx, uuid::Uuid::new_v4())
            .expect("load")
            .is_none());
    }

    #[test]
    fn append_license_event_concurrent_appends_keep_chain_valid() {
        let ctx = std::sync::Arc::new(test_context());
        let license_id = uuid::Uuid::new_v4();
        let threads = 16;
        std::thread::scope(|scope| {
            for index in 0..threads {
                let ctx = std::sync::Arc::clone(&ctx);
                scope.spawn(move || {
                    append_license_event(
                        &ctx,
                        license_id,
                        &format!("event-{index}"),
                        None,
                        serde_json::json!({ "n": index }),
                    );
                });
            }
        });
        let count = license_events_store()
            .iter()
            .filter(|entry| entry.value().license_id == license_id)
            .count();
        assert_eq!(count, threads);
        assert_eq!(verify_event_chain(license_id), (true, None));
    }
}
