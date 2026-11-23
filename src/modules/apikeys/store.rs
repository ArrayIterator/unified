//! apikeys module - In-memory key stores and hook-bus emission.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, OnceLock};
use uuid::Uuid;

use super::types::ApiKeyRecord;

// ---------------------------------------------------------------------------
// State.
// ---------------------------------------------------------------------------

/// Hook bus stashed in `init` so `note_used` (called from middleware,
/// outside any handler) can emit `apikey.used`.
pub(crate) static HOOK_BUS: OnceLock<Arc<HookBus>> = OnceLock::new();

pub(crate) static KEYS: OnceLock<DashMap<Uuid, ApiKeyRecord>> = OnceLock::new();

/// Index from key digest to key id, for authentication lookups.
/// Digests are `hmac1:`-prefixed HMAC-SHA256 values for keys issued
/// after the pepper fix, or bare plain SHA-256 hex digests for
/// older keys (dual-read; see `service::authenticate_key`).
pub(crate) static KEYS_BY_HASH: OnceLock<DashMap<String, Uuid>> = OnceLock::new();

pub(crate) fn keys_store() -> &'static DashMap<Uuid, ApiKeyRecord> {
    KEYS.get_or_init(DashMap::new)
}

pub(crate) fn hash_index() -> &'static DashMap<String, Uuid> {
    KEYS_BY_HASH.get_or_init(DashMap::new)
}

pub(crate) fn emit(event: &str, payload: Value) {
    if let Some(bus) = HOOK_BUS.get() {
        bus.emit(event, payload);
    }
}
