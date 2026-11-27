//! calendar module - In-memory calendar state.

use unified_core::hooks::HookBus;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::constants::MAX_FEEDS_PER_USER;
use super::types::CalendarFeed;
use super::types::CalendarSettings;
use super::types::CalendarSource;

// Shared state.

/// In-memory stores for the calendar. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers use
/// `ctx.hooks` directly. Durable rows for these types are defined
/// by migration `2026-11-22-000095_calendar`; until that migration
/// is wired into a database-backed implementation, feeds,
/// settings, and sources do not survive a restart.
#[derive(Clone)]
pub struct CalendarState {
    /// Keyed by feed id.
    pub feeds: Arc<DashMap<Uuid, CalendarFeed>>,
    /// Company-level settings, keyed by `"{tenant_id}:{company_id}"`.
    pub company_settings: Arc<DashMap<String, CalendarSettings>>,
    /// User-level settings, keyed by `"{tenant_id}:{user_id}"`.
    pub user_settings: Arc<DashMap<String, CalendarSettings>>,
    /// Keyed by `"{tenant_id}:{user_id}:{module}"`.
    pub sources: Arc<DashMap<String, CalendarSource>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
    /// HMAC signing key for feed tokens. Starts as a per-process
    /// random key (two random UUIDs, 32 bytes - same approach as
    /// the library module); `init` replaces it with the key
    /// derived from the configured app secret
    /// (`use_derived_signing_key`), so the same configuration
    /// yields the same key on every restart and outstanding feed
    /// tokens keep verifying after a restart. While no app secret
    /// is configured the random key stays, and tokens issued by
    /// this process do not survive a restart (same fallback as
    /// the auth manager). The key never leaves memory.
    pub signing_key: Arc<RwLock<[u8; 32]>>,
    /// Per-user locks serializing feed creation, keyed by
    /// `"{tenant_id}:{user_id}"`. The active-feed cap check and
    /// the insert run under the owner's lock (see
    /// `insert_feed_if_under_cap`), so concurrent creates can
    /// never push the count past the cap.
    pub(crate) feed_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl Default for CalendarState {
    fn default() -> Self {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let mut key = [0u8; 32];
        key[..16].copy_from_slice(first.as_bytes());
        key[16..].copy_from_slice(second.as_bytes());
        Self {
            feeds: Arc::new(DashMap::new()),
            company_settings: Arc::new(DashMap::new()),
            user_settings: Arc::new(DashMap::new()),
            sources: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
            signing_key: Arc::new(RwLock::new(key)),
            feed_locks: Arc::new(DashMap::new()),
        }
    }
}

impl CalendarState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// HMAC-SHA-256 signature hex for a feed id + expiry, same
    /// token form as the library module.
    pub(crate) fn sign_token(&self, feed_id: &str, expiry: i64) -> String {
        let key = *self.signing_key.read().expect("signing key lock poisoned");
        let payload = format!("{feed_id}:{expiry}");
        let mut mac =
            Hmac::<Sha256>::new_from_slice(&key).expect("HMAC key is always a valid length");
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

    /// SHA-256 hex of a full issued token (the only form stored).
    pub(crate) fn hash_token(token: &str) -> String {
        let digest = Sha256::digest(token.as_bytes());
        {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut out = String::with_capacity(digest.len() * 2);
        for &byte in digest.iter() {
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0f) as usize] as char);
        }
        out
    }
    }

    /// Derive the persistent feed-signing key from the configured
    /// app secret and salt: SHA-256 over a domain-separated label
    /// plus secret and salt (same secret+salt pairing the auth
    /// manager uses for its signing key). Deterministic: the same
    /// configuration yields the same 32-byte key on every restart,
    /// and a different secret or salt yields a different key.
    /// The label keeps this key distinct from every other key
    /// derived from the same secret.
    pub(crate) fn derive_signing_key(secret_key: &str, salt_key: &str) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(b"calendar-feed-signing-v1:");
        hasher.update(secret_key.as_bytes());
        hasher.update(b":");
        hasher.update(salt_key.as_bytes());
        let digest = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&digest);
        key
    }

    /// Replace the per-process random signing key with the key
    /// derived from the app secret. Called from `init`; a no-op
    /// when no secret is configured, so the random fallback (and
    /// its restart behaviour) stays until a real secret exists.
    pub(crate) fn use_derived_signing_key(&self, secret_key: &str, salt_key: &str) {
        if secret_key.trim().is_empty() {
            return;
        }
        let key = Self::derive_signing_key(secret_key, salt_key);
        *self
            .signing_key
            .write()
            .expect("signing key lock poisoned") = key;
    }

    /// Per-user feed-creation lock for (`tenant_id`, `user_id`),
    /// created on first use.
    pub(crate) fn feed_creation_lock(&self, tenant_id: &str, user_id: &str) -> Arc<Mutex<()>> {
        let key = format!("{tenant_id}:{user_id}");
        self.feed_locks
            .entry(key)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .value()
            .clone()
    }

    /// Active feeds for one user: unrevoked and unexpired, in
    /// this tenant.
    pub(crate) fn count_active_feeds(&self, tenant_id: &str, user_id: &str) -> usize {
        let now = Utc::now();
        self.feeds
            .iter()
            .filter(|entry| {
                let feed = entry.value();
                feed.tenant_id == tenant_id
                    && feed.user_id == user_id
                    && !feed.revoked
                    && feed.expires_at > now
            })
            .count()
    }

    /// Atomically check the per-user active-feed cap and insert
    /// the feed. The count and the insert run under the owner's
    /// feed-creation lock, so two concurrent creates cannot both
    /// pass the check and push the user past MAX_FEEDS_PER_USER.
    /// Returns false (and inserts nothing) when the user already
    /// holds the maximum number of active feeds.
    pub(crate) fn insert_feed_if_under_cap(&self, feed: CalendarFeed) -> bool {
        let lock = self.feed_creation_lock(&feed.tenant_id, &feed.user_id);
        let _guard = lock.lock().expect("feed creation lock poisoned");
        if self.count_active_feeds(&feed.tenant_id, &feed.user_id) >= MAX_FEEDS_PER_USER {
            return false;
        }
        self.feeds.insert(feed.id, feed);
        true
    }

    /// Issue a signed feed token for a feed id. Returns the token
    /// string (shown to the caller exactly once) and its expiry.
    pub(crate) fn issue_token(&self, feed_id: Uuid, expires_secs: i64) -> (String, DateTime<Utc>) {
        let expires_at = Utc::now() + chrono::Duration::seconds(expires_secs);
        let expiry = expires_at.timestamp();
        let signature = self.sign_token(&feed_id.to_string(), expiry);
        (format!("{feed_id}.{expiry}.{signature}"), expires_at)
    }
}

#[cfg(test)]
mod tests {
    use super::CalendarState;
    use crate::modules::calendar::CalendarFeed;
    use crate::modules::calendar::MAX_FEEDS_PER_USER;
    use chrono::Utc;
    use uuid::Uuid;

    fn feed(user_id: &str) -> CalendarFeed {
        let now = Utc::now();
        CalendarFeed {
            id: Uuid::new_v4(),
            tenant_id: "tenant-1".to_string(),
            company_id: None,
            user_id: user_id.to_string(),
            token_hash: "hash".to_string(),
            modules: Vec::new(),
            label: None,
            expires_at: now + chrono::Duration::days(30),
            revoked: false,
            created_at: now,
            last_used_at: None,
        }
    }

    #[test]
    fn derived_signing_key_is_deterministic() {
        assert_eq!(
            CalendarState::derive_signing_key("secret", "salt"),
            CalendarState::derive_signing_key("secret", "salt")
        );
    }

    #[test]
    fn derived_signing_key_changes_with_secret_or_salt() {
        let base = CalendarState::derive_signing_key("secret", "salt");
        assert_ne!(base, CalendarState::derive_signing_key("other-secret", "salt"));
        assert_ne!(base, CalendarState::derive_signing_key("secret", "other-salt"));
    }

    #[test]
    fn derived_key_survives_a_restart() {
        // Simulate a restart: a fresh state that derives its key
        // from the same configured secret must sign exactly like
        // the state before the restart.
        let before = CalendarState::default();
        before.use_derived_signing_key("app-secret", "app-salt");
        let after = CalendarState::default();
        after.use_derived_signing_key("app-secret", "app-salt");
        assert_eq!(
            before.sign_token("feed-id", 1_700_000_000),
            after.sign_token("feed-id", 1_700_000_000)
        );
    }

    #[test]
    fn random_keys_differ_without_a_configured_secret() {
        // Two fresh states keep different random keys until a
        // secret is configured (the documented fallback).
        let first = CalendarState::default();
        let second = CalendarState::default();
        assert_ne!(
            first.sign_token("feed-id", 1_700_000_000),
            second.sign_token("feed-id", 1_700_000_000)
        );
    }

    #[test]
    fn empty_secret_leaves_the_random_key_alone() {
        let state = CalendarState::default();
        let before = state.sign_token("feed-id", 1_700_000_000);
        state.use_derived_signing_key("", "salt");
        state.use_derived_signing_key("   ", "salt");
        assert_eq!(before, state.sign_token("feed-id", 1_700_000_000));
    }

    #[test]
    fn feed_cap_rejects_the_feed_past_the_limit() {
        let state = CalendarState::default();
        for _ in 0..MAX_FEEDS_PER_USER {
            assert!(state.insert_feed_if_under_cap(feed("user-1")));
        }
        assert_eq!(state.count_active_feeds("tenant-1", "user-1"), MAX_FEEDS_PER_USER);
        assert!(!state.insert_feed_if_under_cap(feed("user-1")));
        assert_eq!(state.count_active_feeds("tenant-1", "user-1"), MAX_FEEDS_PER_USER);
    }

    #[test]
    fn feed_cap_is_per_user_and_per_tenant() {
        let state = CalendarState::default();
        for _ in 0..MAX_FEEDS_PER_USER {
            assert!(state.insert_feed_if_under_cap(feed("user-1")));
        }
        // Another user (or tenant) still has a free quota.
        assert!(state.insert_feed_if_under_cap(feed("user-2")));
        let mut other_tenant = feed("user-1");
        other_tenant.tenant_id = "tenant-2".to_string();
        assert!(state.insert_feed_if_under_cap(other_tenant));
    }

    #[test]
    fn feed_cap_counts_only_active_feeds() {
        let state = CalendarState::default();
        let mut revoked = feed("user-1");
        revoked.revoked = true;
        assert!(state.insert_feed_if_under_cap(revoked));
        let mut expired = feed("user-1");
        expired.expires_at = Utc::now() - chrono::Duration::days(1);
        assert!(state.insert_feed_if_under_cap(expired));
        // Neither counts against the cap.
        assert_eq!(state.count_active_feeds("tenant-1", "user-1"), 0);
        assert!(state.insert_feed_if_under_cap(feed("user-1")));
    }

    #[test]
    fn concurrent_feed_creates_never_exceed_the_cap() {
        let state = CalendarState::default();
        let mut handles = Vec::new();
        for _ in 0..(MAX_FEEDS_PER_USER + 10) {
            let state = state.clone();
            handles.push(std::thread::spawn(move || {
                state.insert_feed_if_under_cap(feed("user-1"))
            }));
        }
        let mut accepted = 0usize;
        for handle in handles {
            if handle.join().expect("feed insert thread panicked") {
                accepted += 1;
            }
        }
        assert_eq!(accepted, MAX_FEEDS_PER_USER);
        assert_eq!(state.count_active_feeds("tenant-1", "user-1"), MAX_FEEDS_PER_USER);
    }
    #[test]
    fn hash_token_matches_known_sha256_vector() {
        assert_eq!(
            CalendarState::hash_token("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(CalendarState::hash_token("abc").len(), 64);
    }

}
