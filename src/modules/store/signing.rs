// Store module - signed download links.
//
// Split from `src/modules/store.rs`; the module root re-exports the public API of this module.

use unified_core::config::AppConfig;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::events;
use unified_core::json;
use chrono::{DateTime, Utc};
use hmac::Mac;
use serde_json::json;
use std::sync::{Arc, OnceLock, RwLock};

use super::state::{StoreState, new_id, now};
use super::types::{DownloadEntitlement, DownloadRecord, HmacSha256};

/// Application configuration stashed by `StoreModule::init`, so the
/// download signing secret can come from config instead of a fresh
/// random value on every start. See `signing_secret`.
pub(crate) static STORE_CONFIG: OnceLock<Arc<RwLock<AppConfig>>> = OnceLock::new();

// Signed download links (HMAC-SHA256, expiring)

/// HMAC key for signed download links. When
/// `store.download_signing_secret` is configured, that value is used,
/// so signed links survive restarts. Otherwise a random 32-byte
/// per-process secret is generated once and a warning is logged: links
/// signed before a restart stop verifying after one. The configured
/// secret is read once at first use; changing it requires a restart.
pub(crate) fn signing_secret() -> &'static [u8] {
    static SECRET: OnceLock<Vec<u8>> = OnceLock::new();
    SECRET.get_or_init(|| {
        if let Some(config) = STORE_CONFIG.get() {
            let secret = config
                .read()
                .expect("config lock poisoned")
                .store
                .download_signing_secret
                .clone();
            if !secret.trim().is_empty() {
                return secret.into_bytes();
            }
        }
        log::warn!(
            "store.download_signing_secret is not set; using a random per-process download signing secret, so signed download links will stop verifying after a restart"
        );
        use rand::Rng;
        let mut rng = rand::rng();
        (0..32).map(|_| rng.random::<u8>()).collect()
    })
}

pub(crate) fn sign_download(license_id: &str, release_id: &str, expires: i64) -> String {
    let message = format!("{license_id}|{release_id}|{expires}");
    let mut mac = HmacSha256::new_from_slice(signing_secret())
        .expect("HMAC accepts any key length");
    mac.update(message.as_bytes());
    {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let tag = mac.finalize().into_bytes();
        let mut out = String::with_capacity(tag.len() * 2);
        for &byte in tag.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
}

pub(crate) fn signature_matches(expected: &str, actual: &str) -> bool {
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

pub(crate) fn build_signed_url(license_id: &str, release_id: &str) -> (String, DateTime<Utc>) {
    let expires_at = now() + chrono::Duration::hours(1);
    let expires = expires_at.timestamp();
    let sig = sign_download(license_id, release_id, expires);
    (
        format!(
            "/api/v1/store/downloads/signed?license_id={license_id}&release_id={release_id}&expires={expires}&sig={sig}"
        ),
        expires_at,
    )
}

pub(crate) fn catalog_release_file(state: &StoreState, release_id: &str) -> Option<String> {
    state
        .catalog
        .iter()
        .find(|entry| entry.value().release_id.as_deref() == Some(release_id))
        .and_then(|entry| entry.value().file_id.clone())
}

/// Refuse a download for an entitlement whose order has not been
/// paid. This is the defense behind the `pending` entitlement
/// status: even if a `pending` record were flipped to `active` by
/// another path, or an entitlement predates the pending rule, an
/// unpaid order still downloads nothing. Only `paid` and
/// `completed` orders download.
pub(crate) fn require_paid_order_for_entitlement(
    state: &StoreState,
    entitlement: &DownloadEntitlement,
) -> UnifiedResult<()> {
    if entitlement.status == "pending" {
        return Err(UnifiedError::forbidden(
            "Download entitlement is not active yet; the order has not been paid",
        ));
    }
    let Some(order) = state
        .orders
        .get(&entitlement.order_id)
        .map(|entry| entry.value().clone())
    else {
        return Err(UnifiedError::forbidden(
            "Download entitlement has no order; the download cannot be verified",
        ));
    };
    if order.status != "paid" && order.status != "completed" {
        return Err(UnifiedError::forbidden(
            "Digital delivery is only available for paid orders",
        ));
    }
    Ok(())
}

/// Record one download against an entitlement: counts it, stores the
/// history row, and emits `download.started`. Returns the updated
/// entitlement, or an error when the entitlement is missing, revoked,
/// expired, or out of downloads.
pub(crate) fn record_download(
    state: &StoreState,
    entitlement_id: &str,
    release_id: &str,
) -> UnifiedResult<DownloadEntitlement> {
    let mut entitlement = state
        .entitlements
        .get_mut(entitlement_id)
        .ok_or_else(|| UnifiedError::not_found("Download entitlement not found"))?;
    require_paid_order_for_entitlement(state, &entitlement)?;
    if entitlement.status == "revoked" {
        return Err(UnifiedError::forbidden(
            "Download entitlement has been revoked",
        ));
    }
    if let Some(expires_at) = entitlement.expires_at {
        if expires_at <= now() {
            entitlement.status = "expired".to_string();
            entitlement.updated_at = now();
            return Err(UnifiedError::forbidden(
                "Download entitlement has expired",
            ));
        }
    }
    if entitlement.downloads_used >= entitlement.download_limit {
        return Err(UnifiedError::forbidden(
            "Download limit reached for this license",
        ));
    }
    entitlement.downloads_used += 1;
    entitlement.updated_at = now();
    let updated = entitlement.clone();
    let license_id = updated.license_id.clone().unwrap_or_default();
    drop(entitlement);
    let record = DownloadRecord {
        id: new_id(),
        license_id: license_id.clone(),
        release_id: release_id.to_string(),
        software_slug: updated.software_slug.clone(),
        order_id: Some(updated.order_id.clone()),
        downloaded_at: now(),
    };
    state.downloads.insert(record.id.clone(), record);
    state.emit(
        events::DOWNLOAD_STARTED,
        json!({
            "license_id": license_id,
            "release_id": release_id,
            "order_id": updated.order_id,
            "downloads_used": updated.downloads_used,
            "download_limit": updated.download_limit,
        }),
    );
    Ok(updated)
}

/// Suspend every entitlement and issued license of one order (used by
/// refunds). Returns the affected license ids.
pub(crate) fn suspend_order_licenses(state: &StoreState, order_id: &str) -> Vec<String> {
    let mut license_ids: Vec<String> = Vec::new();
    for mut entry in state.entitlements.iter_mut() {
        if entry.value().order_id == order_id && entry.value().status == "active" {
            entry.value().status = "revoked".to_string();
            entry.value().updated_at = now();
            if let Some(license_id) = &entry.value().license_id {
                license_ids.push(license_id.clone());
            }
        }
    }
    for mut entry in state.issued_licenses.iter_mut() {
        if entry.value().order_id.as_deref() == Some(order_id) {
            entry.value().status = "suspended".to_string();
            let license_id = entry.value().license_id.clone();
            if !license_ids.contains(&license_id) {
                license_ids.push(license_id);
            }
        }
    }
    license_ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_matches_is_exact() {
        assert!(signature_matches("abc", "abc"));
        assert!(!signature_matches("abc", "abd"));
        assert!(!signature_matches("abc", "abcd"));
    }

    #[test]
    fn sign_download_is_stable_within_process() {
        let first = sign_download("lic-1", "rel-1", 1_700_000_000);
        let second = sign_download("lic-1", "rel-1", 1_700_000_000);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert_ne!(first, sign_download("lic-2", "rel-1", 1_700_000_000));
        assert_ne!(first, sign_download("lic-1", "rel-1", 1_700_000_001));
    }

    #[test]
    fn build_signed_url_shape() {
        let (url, expires_at) = build_signed_url("lic-1", "rel-1");
        assert!(url.starts_with(
            "/api/v1/store/downloads/signed?license_id=lic-1&release_id=rel-1&expires="
        ));
        assert!(url.contains("&sig="));
        let minutes = (expires_at - Utc::now()).num_minutes();
        assert!((59..=60).contains(&minutes));
    }

    #[test]
    fn catalog_release_file_returns_none_when_missing() {
        let state = StoreState::new();
        assert_eq!(catalog_release_file(&state, "missing-release"), None);
    }

    fn paid_order_state(order_status: &str) -> (StoreState, String) {
        use crate::modules::store::types::Order;
        let state = StoreState::new();
        let timestamp = now();
        let order = Order {
            id: "ord-1".to_string(),
            reference: "ORD-1".to_string(),
            user_id: "user-1".to_string(),
            items: Vec::new(),
            subtotal: 4900,
            discount_total: 0,
            total: 4900,
            currency: "USD".to_string(),
            coupon_code: None,
            discount_evaluation: None,
            company_id: None,
            trial_until: None,
            status: order_status.to_string(),
            provider: None,
            payment_reference: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.orders.insert(order.id.clone(), order);
        let entitlement = DownloadEntitlement {
            id: "ent-1".to_string(),
            license_id: Some("lic-1".to_string()),
            license_key: Some("KEY-1".to_string()),
            order_id: "ord-1".to_string(),
            user_id: "user-1".to_string(),
            product_id: "prod-1".to_string(),
            tier_id: None,
            software_id: None,
            software_slug: None,
            release_id: Some("rel-1".to_string()),
            download_limit: 5,
            downloads_used: 0,
            status: "active".to_string(),
            expires_at: None,
            created_at: timestamp,
            updated_at: timestamp,
        };
        state.entitlements.insert(entitlement.id.clone(), entitlement);
        (state, "ent-1".to_string())
    }

    #[test]
    fn record_download_refuses_unpaid_orders() {
        for status in ["pending", "payment_pending", "pending_verification", "cancelled", "refunded"] {
            let (state, entitlement_id) = paid_order_state(status);
            assert!(
                record_download(&state, &entitlement_id, "rel-1").is_err(),
                "status {status} must not download"
            );
            assert_eq!(state.downloads.len(), 0);
        }
    }

    #[test]
    fn record_download_refuses_pending_entitlements() {
        let (state, entitlement_id) = paid_order_state("paid");
        {
            let mut entitlement = state.entitlements.get_mut(&entitlement_id).unwrap();
            entitlement.status = "pending".to_string();
        }
        assert!(record_download(&state, &entitlement_id, "rel-1").is_err());
    }

    #[test]
    fn record_download_allows_paid_and_completed_orders() {
        for status in ["paid", "completed"] {
            let (state, entitlement_id) = paid_order_state(status);
            let updated = record_download(&state, &entitlement_id, "rel-1")
                .unwrap_or_else(|error| panic!("status {status} must download: {}", error.message));
            assert_eq!(updated.downloads_used, 1);
            assert_eq!(state.downloads.len(), 1);
        }
    }
}
