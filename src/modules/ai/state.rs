//! ai module - In-memory AI state.

use unified_core::hooks::{HookBus, HookContext};
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;

use super::types::AiCompanyPolicy;
use super::types::AiDraft;
use super::types::AiGlobalSettings;
use super::types::AiPersonalKey;
use super::types::AiProviderConnection;
use super::types::AiUsageRecord;
use super::types::AiUserMcpServer;
use super::types::AiUserPolicy;
use super::types::CapReservation;
use super::types::RateBucket;

// Shared state.

/// In-memory stores for the AI module. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers use the
/// `AppContext` bus directly, while the synchronous hook handlers
/// registered here read the bus from this state.
#[derive(Clone, Default)]
pub struct AiState {
    pub providers: Arc<DashMap<Uuid, AiProviderConnection>>,
    pub drafts: Arc<DashMap<Uuid, AiDraft>>,
    pub usage: Arc<DashMap<Uuid, AiUsageRecord>>,
    /// Per-minute rate-limit buckets, keyed by
    /// `(connection_id, user_id)`: one user cannot exhaust the
    /// shared per-minute budget for everyone on a connection.
    /// `user_id` is `None` for callers whose actor id is not a
    /// UUID (those callers share one anonymous bucket per
    /// connection).
    pub rate_buckets: Arc<DashMap<(Uuid, Option<Uuid>), RateBucket>>,
    /// Live cap reservations (see `CapReservation`): estimates
    /// held from authorization until the call's actual usage is
    /// recorded.
    pub cap_reservations: Arc<DashMap<Uuid, CapReservation>>,
    /// Serializes the cap check + reservation insert (and usage
    /// recording) so two concurrent calls cannot both pass a cap
    /// check against the same pre-call usage.
    pub cap_ledger_lock: Arc<Mutex<()>>,
    /// Company access policies, keyed by `company_id` (one policy
    /// per company). Empty store + empty user-policy store means
    /// the allowlist has never been switched on: see
    /// `authorize_ai_call`. Enforcement is default-deny unless
    /// the global settings explicitly opt into open mode
    /// (`AiGlobalSettings::open_mode`).
    pub company_policies: Arc<DashMap<Uuid, AiCompanyPolicy>>,
    /// User access policies, keyed by `user_id` (one policy per
    /// user).
    pub user_policies: Arc<DashMap<Uuid, AiUserPolicy>>,
    /// Personal API keys, keyed by `user_id` (one key per user).
    /// Ciphertext only; the plaintext is never stored.
    pub personal_keys: Arc<DashMap<Uuid, AiPersonalKey>>,
    /// Personal MCP servers, keyed by server id. Every lookup
    /// filters on `user_id`: a server is never visible to another
    /// user.
    pub personal_mcp_servers: Arc<DashMap<Uuid, AiUserMcpServer>>,
    /// Global AI settings (kill-switch + global feature switches).
    /// Read fresh on every gate evaluation; never cached.
    pub global_settings: Arc<RwLock<AiGlobalSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl AiState {
    pub(crate) fn emit(&self, event: &str, data: Value) -> Option<HookContext> {
        self.hooks
            .read()
            .expect("hooks lock poisoned")
            .clone()
            .map(|bus| bus.emit(event, data))
    }
}
