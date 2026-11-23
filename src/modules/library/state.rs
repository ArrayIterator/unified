// Library - In-memory library state.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde_json::{json, Value};
use sha2::Sha256;
use std::hash::Hash;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::{DigitalAccess, InventoryAudit, InventoryDiscrepancy, LibraryCategory, LibraryCopy,
    LibraryFine, LibraryHold, LibraryItem, LibraryLoan, LibraryMember, LibrarySettings, LoanPolicy,
    constant_time_eq};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the library. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000091_library`; until that migration is executed,
/// library data does not survive a restart.
#[derive(Clone)]
pub struct LibraryState {
    pub items: Arc<DashMap<Uuid, LibraryItem>>,
    pub copies: Arc<DashMap<Uuid, LibraryCopy>>,
    pub categories: Arc<DashMap<Uuid, LibraryCategory>>,
    pub members: Arc<DashMap<Uuid, LibraryMember>>,
    pub loans: Arc<DashMap<Uuid, LibraryLoan>>,
    pub holds: Arc<DashMap<Uuid, LibraryHold>>,
    pub fines: Arc<DashMap<Uuid, LibraryFine>>,
    pub policies: Arc<DashMap<Uuid, LoanPolicy>>,
    pub audits: Arc<DashMap<Uuid, InventoryAudit>>,
    pub discrepancies: Arc<DashMap<Uuid, InventoryDiscrepancy>>,
    pub digital_access: Arc<DashMap<Uuid, DigitalAccess>>,
    pub settings: Arc<RwLock<LibrarySettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    pub signing_key: Arc<RwLock<[u8; 32]>>,
    pub search_rate: Arc<DashMap<String, (u64, i64)>>,
    /// Per-member checkout locks. A checkout holds its member's lock
    /// from the concurrent-loan count check through the loan insert,
    /// so two concurrent checkouts for one member cannot both pass
    /// the policy `max_concurrent` limit.
    pub checkout_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
    /// Per-user registration locks. A member registration holds its
    /// user id's lock across the duplicate scan and the insert, so
    /// two concurrent registrations for one user cannot both create
    /// an active membership.
    pub member_registration_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    /// Per-loan fine locks. Fine creation holds its loan's lock
    /// across the "one pending fine per loan and reason" scan and
    /// the insert, so two concurrent overdue sweeps cannot raise
    /// duplicate pending fines for the same loan.
    pub fine_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
}

impl Default for LibraryState {
    fn default() -> Self {
        // Per-process signing key from two random UUIDs (32 bytes),
        // same approach as the documents module: tokens issued by
        // this process cannot be forged without the key, and the key
        // never leaves memory.
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        Self {
            items: Arc::new(DashMap::new()),
            copies: Arc::new(DashMap::new()),
            categories: Arc::new(DashMap::new()),
            members: Arc::new(DashMap::new()),
            loans: Arc::new(DashMap::new()),
            holds: Arc::new(DashMap::new()),
            fines: Arc::new(DashMap::new()),
            policies: Arc::new(DashMap::new()),
            audits: Arc::new(DashMap::new()),
            discrepancies: Arc::new(DashMap::new()),
            digital_access: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(LibrarySettings::default())),
            hooks: Arc::new(RwLock::new(None)),
            signing_key: Arc::new(RwLock::new(key)),
            search_rate: Arc::new(DashMap::new()),
            checkout_locks: Arc::new(DashMap::new()),
            member_registration_locks: Arc::new(DashMap::new()),
            fine_locks: Arc::new(DashMap::new()),
        }
    }
}

/// One shared mutex per key, created on first use. The lock registry
/// itself is a `DashMap`, so fetching a key's lock never blocks on
/// another key's lock; only callers sharing a key serialise.
fn keyed_lock<K: Eq + Hash + Clone>(map: &DashMap<K, Arc<Mutex<()>>>, key: K) -> Arc<Mutex<()>> {
    map.entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

impl LibraryState {
    /// The checkout lock for one member. Hold it across the
    /// concurrent-loan count check and the loan insert.
    pub(crate) fn checkout_lock(&self, member_id: Uuid) -> Arc<Mutex<()>> {
        keyed_lock(&self.checkout_locks, member_id)
    }

    /// The registration lock for one user id. Hold it across the
    /// duplicate-membership scan and the member insert.
    pub(crate) fn registration_lock(&self, user_id: &str) -> Arc<Mutex<()>> {
        keyed_lock(&self.member_registration_locks, user_id.to_string())
    }

    /// The fine lock for one loan. Hold it across the pending-fine
    /// scan and the fine insert.
    pub(crate) fn fine_lock(&self, loan_id: Uuid) -> Arc<Mutex<()>> {
        keyed_lock(&self.fine_locks, loan_id)
    }

    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> LibrarySettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "library",
                "category": "library",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Request a fine charge from billing. This module never moves
    /// money: it records the fine and emits the request event, and
    /// billing owns the charge and reports the outcome.
    pub(crate) fn request_fine(&self, fine: &LibraryFine) {
        self.emit(
            "library.fine.requested",
            json!({
                "fine_id": fine.id.to_string(),
                "loan_id": fine.loan_id.to_string(),
                "member_id": fine.member_id.to_string(),
                "user_id": fine.user_id,
                "amount_minor": fine.amount_minor,
                "currency": fine.currency,
                "reason": fine.reason,
                "module": "library",
            }),
        );
    }

    /// HMAC-SHA-256 signature hex for a loan id + expiry, same token
    /// form as the documents module.
    fn sign_token(&self, loan_id: &str, expiry: i64) -> String {
        let key = *self.signing_key.read().expect("signing key lock poisoned");
        let payload = format!("{loan_id}:{expiry}");
        let mut mac = Hmac::<Sha256>::new_from_slice(&key)
            .expect("HMAC key is always a valid length");
        mac.update(payload.as_bytes());
        let tag = mac.finalize().into_bytes();
        {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            let mut out = String::with_capacity(tag.len() * 2);
            for &byte in tag.iter() {
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
            out
        }
    }

    /// Issue a signed digital-access token for a loan.
    pub(crate) fn issue_signed_token(&self, loan_id: &str, expires_secs: u64) -> (String, DateTime<Utc>) {
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs as i64);
        let expiry = expires_at.timestamp();
        let signature = self.sign_token(loan_id, expiry);
        (
            format!("{loan_id}.{expiry}.{signature}"),
            expires_at,
        )
    }

    /// Validate a signed digital-access token. Returns the loan id on
    /// success; rejects expired or tampered tokens.
    pub(crate) fn verify_signed_token(&self, token: &str) -> UnifiedResult<String> {
        let parts: Vec<&str> = token.split('.').collect();
        if parts.len() != 3 {
            return Err(UnifiedError::forbidden("Invalid access token"));
        }
        let loan_id = parts[0].to_string();
        let expiry: i64 = parts[1]
            .parse()
            .map_err(|_| UnifiedError::forbidden("Invalid access token"))?;
        if expiry <= Utc::now().timestamp() {
            return Err(UnifiedError::forbidden("Access token has expired"));
        }
        let expected = self.sign_token(&loan_id, expiry);
        if !constant_time_eq(&expected, parts[2]) {
            return Err(UnifiedError::forbidden("Access token is invalid"));
        }
        Ok(loan_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_token_is_deterministic_64_hex() {
        let state = LibraryState::default();
        let first = state.sign_token("loan-1", 1_700_000_000);
        assert_eq!(first.len(), 64);
        assert!(first.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(state.sign_token("loan-1", 1_700_000_000), first);
        assert_ne!(state.sign_token("loan-2", 1_700_000_000), first);
    }
}
