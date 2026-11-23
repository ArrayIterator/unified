//! vault module - In-memory vault state.

use unified_core::hooks::HookBus;
use chrono::Utc;
use dashmap::DashMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::atomic::AtomicI64;
use std::sync::Arc;
use std::sync::RwLock;
use uuid::Uuid;

use super::types::VaultAccessLog;
use super::types::VaultAccessRequest;
use super::types::VaultDocument;
use super::types::VaultDocumentVersion;
use super::types::VaultFolder;
use super::types::VaultShareLink;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the vault. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration `2026-11-22-000052_vault`; until
/// that migration is executed, documents do not survive a restart.
#[derive(Clone)]
pub struct VaultState {
    pub folders: Arc<DashMap<Uuid, VaultFolder>>,
    pub documents: Arc<DashMap<Uuid, VaultDocument>>,
    pub versions: Arc<DashMap<Uuid, VaultDocumentVersion>>,
    pub access_log: Arc<DashMap<Uuid, VaultAccessLog>>,
    pub share_links: Arc<DashMap<Uuid, VaultShareLink>>,
    /// Share-link lookup index: SHA-256 hex of the token to the
    /// link id. Resolving a link is a hash-map lookup on this
    /// index, never a linear scan comparing tokens, so neither
    /// the token value nor the match position leaks through
    /// timing. Every write to `share_links` must go through
    /// `insert_share_link` / `remove_share_link` so the index
    /// stays in sync with the rows.
    pub share_link_tokens: Arc<DashMap<String, Uuid>>,
    pub access_requests: Arc<DashMap<Uuid, VaultAccessRequest>>,
    /// Per-document version-number counters (see
    /// `access::allocate_version_number`). Seeded from the highest
    /// stored version number on first use, then incremented
    /// atomically, so concurrent uploads can never be handed the
    /// same version number.
    pub version_counters: Arc<DashMap<Uuid, AtomicI64>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for VaultState {
    fn default() -> Self {
        Self {
            folders: Arc::new(DashMap::new()),
            documents: Arc::new(DashMap::new()),
            versions: Arc::new(DashMap::new()),
            access_log: Arc::new(DashMap::new()),
            share_links: Arc::new(DashMap::new()),
            share_link_tokens: Arc::new(DashMap::new()),
            access_requests: Arc::new(DashMap::new()),
            version_counters: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl VaultState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(crate) fn notify(
        &self,
        kind: &str,
        user_id: &str,
        title: &str,
        body: &str,
        link: Option<String>,
    ) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "vault",
                "category": "vault",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Append one row to the access log. The log is append-only:
    /// this is the only writer, and no handler ever updates or
    /// removes a row. The row id is generated exactly once and
    /// used for both the map key and the row's own `id`, so the
    /// row stays reachable by its own id.
    pub(crate) fn log_access(
        &self,
        document_id: Uuid,
        actor_id: &str,
        action: &str,
        detail: Option<String>,
    ) {
        let id = Uuid::now_v7();
        self.access_log.insert(
            id,
            VaultAccessLog {
                id,
                document_id,
                actor_id: actor_id.to_string(),
                action: action.to_string(),
                detail,
                created_at: Utc::now(),
            },
        );
    }

    /// SHA-256 hex of a share token. Only the hash is ever used
    /// as a lookup key; the raw token is never compared against
    /// stored tokens.
    pub(crate) fn hash_share_token(token: &str) -> String {
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

    /// Insert a share link and its token-hash index entry together.
    /// All share-link creation must go through this helper so the
    /// index can never drift from the rows.
    pub(crate) fn insert_share_link(&self, link: VaultShareLink) {
        let token_hash = Self::hash_share_token(&link.token);
        self.share_links.insert(link.id, link);
        self.share_link_tokens.insert(token_hash, link.id);
    }

    /// Remove a share link and its token-hash index entry together.
    /// All share-link deletion must go through this helper.
    pub(crate) fn remove_share_link(&self, id: Uuid) {
        if let Some((_, link)) = self.share_links.remove(&id) {
            self.share_link_tokens
                .remove(&Self::hash_share_token(&link.token));
        }
    }

    /// Resolve a share token to its link id through the hash
    /// index. No linear scan, no token comparison: an unknown
    /// token and a known token cost the same hash-map lookup.
    pub(crate) fn lookup_share_link_id(&self, token: &str) -> Option<Uuid> {
        self.share_link_tokens
            .get(&Self::hash_share_token(token))
            .map(|entry| *entry.value())
    }
}

#[cfg(test)]
mod tests {
    use super::VaultState;
    use crate::modules::vault::VaultShareLink;
    use chrono::Utc;
    use uuid::Uuid;

    fn link(token: &str) -> VaultShareLink {
        let now = Utc::now();
        VaultShareLink {
            id: Uuid::now_v7(),
            document_id: Uuid::now_v7(),
            token: token.to_string(),
            tenant_id: Some("tenant-1".to_string()),
            expires_at: now + chrono::Duration::seconds(3600),
            max_downloads: 5,
            download_count: 0,
            revoked: false,
            created_by: "owner-1".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn log_access_row_is_reachable_by_its_own_id() {
        let state = VaultState::default();
        let document_id = Uuid::now_v7();
        state.log_access(document_id, "actor-1", "view", None);
        state.log_access(document_id, "actor-1", "download", None);
        assert_eq!(state.access_log.len(), 2);
        for entry in state.access_log.iter() {
            // The map key and the row's own id are the same value.
            assert_eq!(*entry.key(), entry.value().id);
            assert_eq!(entry.value().document_id, document_id);
        }
    }

    #[test]
    fn share_link_token_index_resolves_and_cleans_up() {
        let state = VaultState::default();
        let first = link("token-alpha");
        let second = link("token-beta");
        state.insert_share_link(first.clone());
        state.insert_share_link(second.clone());
        assert_eq!(state.lookup_share_link_id("token-alpha"), Some(first.id));
        assert_eq!(state.lookup_share_link_id("token-beta"), Some(second.id));
        assert_eq!(state.lookup_share_link_id("token-unknown"), None);
        // Removing a link removes its index entry, not the other's.
        state.remove_share_link(first.id);
        assert_eq!(state.lookup_share_link_id("token-alpha"), None);
        assert_eq!(state.lookup_share_link_id("token-beta"), Some(second.id));
        assert!(state.share_links.get(&second.id).is_some());
    }

    #[test]
    fn share_link_token_hash_is_stable_and_not_the_token() {
        let first = VaultState::hash_share_token("token-alpha");
        let second = VaultState::hash_share_token("token-alpha");
        assert_eq!(first, second);
        assert_ne!(first, "token-alpha");
        assert_eq!(first.len(), 64);
        assert_ne!(first, VaultState::hash_share_token("token-beta"));
    }
}
