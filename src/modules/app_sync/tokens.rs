//! app_sync - tokens.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::modules::ModuleRegistry;
use unified_core::pagination::{PageResult, PaginationParams};
use base64::Engine;
use chrono::Utc;
use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{ Arc, RwLock };
use uuid::Uuid;
use super::{constants::*, helpers::*, types::*, inputs::*, handlers::*, module::*};


pub(crate) type HmacSha256 = Hmac<Sha256>;



// ---------------------------------------------------------------------------
// HMAC-signed device tokens.
//
// Token format: `<device-id>.<base64url(HMAC-SHA256(secret, device-id))>`.
// The secret is `app.secret_key`. There is no fallback secret: when no
// secret is configured, signing and validation fail closed (see
// `signing_secret`), so a deployment without a configured secret issues
// no tokens at all instead of issuing tokens anyone can forge.
// ---------------------------------------------------------------------------
pub(crate) fn signing_secret(ctx: &AppContext) -> UnifiedResult<String> {
    let config = ctx.config.read().expect("config lock poisoned");
    let secret = config.app.secret_key.trim().to_string();
    if secret.is_empty() {
        log::error!(
            "app_sync: app.secret_key is not configured; refusing to sign or validate device tokens"
        );
        return Err(UnifiedError::internal(
            "app.secret_key is not configured; device sync is disabled",
        ));
    }
    Ok(secret)
}


pub(crate) fn sign_device_token(secret: &str, device_id: Uuid) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .expect("HMAC key must accept any length");
    mac.update(device_id.as_bytes());
    let signature = mac.finalize().into_bytes();
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signature);
    format!("{device_id}.{encoded}")
}


pub(crate) fn validate_device_token(ctx: &AppContext, token: &str) -> UnifiedResult<Arc<DeviceState>> {
    let (device_id_text, signature_text) = token
        .split_once('.')
        .ok_or_else(|| UnifiedError::unauthorized("Invalid sync token format"))?;
    let device_id: Uuid = device_id_text
        .parse()
        .map_err(|_| UnifiedError::unauthorized("Invalid sync token format"))?;
    let provided = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(signature_text)
        .map_err(|_| UnifiedError::unauthorized("Invalid sync token"))?;
    // Constant-time comparison through the MAC itself: the provided
    // signature is verified with `verify_slice` instead of comparing
    // token strings, so no timing side channel leaks how many leading
    // characters of a forged signature were correct.
    let mut mac = HmacSha256::new_from_slice(signing_secret(ctx)?.as_bytes())
        .expect("HMAC key must accept any length");
    mac.update(device_id.as_bytes());
    mac.verify_slice(&provided)
        .map_err(|_| UnifiedError::unauthorized("Invalid sync token"))?;
    let device = devices_store()
        .get(&device_id)
        .map(|entry| Arc::clone(entry.value()))
        .ok_or_else(|| UnifiedError::unauthorized("Unknown device"))?;
    if device.revoked.load(Ordering::SeqCst) {
        return Err(UnifiedError::unauthorized("Device token has been revoked"));
    }
    Ok(device)
}


pub(crate) fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let end = (start + pagination.per_page as usize).min(items.len());
    let page_items = if start >= items.len() {
        Vec::new()
    } else {
        items[start..end].to_vec()
    };
    PageResult::new(page_items, total, &pagination)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_context() -> AppContext {
        let mut config = unified_core::config::AppConfig::default();
        config.app.secret_key = "test-only-app-sync-secret".to_string();
        AppContext::new(
            config,
            Arc::new(unified_core::hooks::HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    /// A context with no secret configured, as `AppConfig::default()`
    /// ships it: signing and validation must fail closed.
    fn unconfigured_context() -> AppContext {
        AppContext::new(
            unified_core::config::AppConfig::default(),
            Arc::new(unified_core::hooks::HookBus::new()),
            Arc::new(ModuleRegistry::new()),
        )
    }

    #[test]
    fn sign_device_token_is_deterministic_and_prefixed_with_device_id() {
        let device_id = Uuid::new_v4();
        let first = sign_device_token("secret-one", device_id);
        let second = sign_device_token("secret-one", device_id);
        assert_eq!(first, second);
        assert!(first.starts_with(&format!("{device_id}.")));
        assert_ne!(first, sign_device_token("secret-two", device_id));
        assert_ne!(
            first,
            sign_device_token("secret-one", Uuid::new_v4())
        );
    }

    #[test]
    fn validate_device_token_round_trips_a_registered_device() {
        let ctx = test_context();
        let device_id = Uuid::new_v4();
        let device = Arc::new(DeviceState {
            id: device_id,
            device_name: "Test laptop".to_string(),
            platform: "linux".to_string(),
            owner_id: "user-1".to_string(),
            created_at: Utc::now(),
            last_sync_at: RwLock::new(None),
            revoked: AtomicBool::new(false),
            synced_once: AtomicBool::new(false),
            version: AtomicU64::new(0),
        });
        devices_store().insert(device_id, Arc::clone(&device));
        let token = sign_device_token(&signing_secret(&ctx).expect("test secret"), device_id);
        let validated = validate_device_token(&ctx, &token).expect("valid token");
        assert_eq!(validated.id, device_id);
    }

    #[test]
    fn validate_device_token_rejects_tampered_and_unknown_tokens() {
        let ctx = test_context();
        let device_id = Uuid::new_v4();
        let token = sign_device_token(&signing_secret(&ctx).expect("test secret"), device_id);
        let tampered = format!("{token}x");
        assert!(validate_device_token(&ctx, &tampered).is_err());
        assert!(validate_device_token(&ctx, "not-a-token").is_err());
        assert!(validate_device_token(&ctx, "").is_err());
    }

    #[test]
    fn validate_device_token_rejects_a_token_signed_with_another_secret() {
        // The signature is well-formed base64 of the right length, but
        // was produced with a different secret: only the constant-time
        // MAC verification can tell, and it must reject it.
        let ctx = test_context();
        let device_id = Uuid::new_v4();
        let device = Arc::new(DeviceState {
            id: device_id,
            device_name: "Test laptop".to_string(),
            platform: "linux".to_string(),
            owner_id: "user-1".to_string(),
            created_at: Utc::now(),
            last_sync_at: RwLock::new(None),
            revoked: AtomicBool::new(false),
            synced_once: AtomicBool::new(false),
            version: AtomicU64::new(0),
        });
        devices_store().insert(device_id, Arc::clone(&device));
        let foreign_token = sign_device_token("a-completely-different-secret", device_id);
        assert!(validate_device_token(&ctx, &foreign_token).is_err());
    }

    #[test]
    fn signing_secret_fails_closed_when_no_secret_is_configured() {
        // No hard-coded fallback secret may ever be used: without
        // `app.secret_key`, signing and validation refuse outright.
        let ctx = unconfigured_context();
        assert!(signing_secret(&ctx).is_err());
        let device_id = Uuid::new_v4();
        let device = Arc::new(DeviceState {
            id: device_id,
            device_name: "Test laptop".to_string(),
            platform: "linux".to_string(),
            owner_id: "user-1".to_string(),
            created_at: Utc::now(),
            last_sync_at: RwLock::new(None),
            revoked: AtomicBool::new(false),
            synced_once: AtomicBool::new(false),
            version: AtomicU64::new(0),
        });
        devices_store().insert(device_id, Arc::clone(&device));
        // Even a token signed with an arbitrary secret must not
        // validate once no secret is configured.
        let arbitrary_token = sign_device_token("some-arbitrary-secret", device_id);
        assert!(validate_device_token(&ctx, &arbitrary_token).is_err());
    }
}
