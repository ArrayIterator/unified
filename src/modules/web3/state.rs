// web3 -- Shared in-memory state.

use unified_core::context::AppContext;
use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, OnceLock, RwLock};

use super::constants::*;
use super::types::*;
use super::providers::{ChainProviderRegistry};

/// Stashed app context for the sync `web3.poll_requested` hook:
/// the poll needs the encryption secret from config, and hook
/// handlers only receive the module state. Set once in `init`;
/// the same stash pattern as the geo and licensing modules.
pub(crate) static WEB3_CTX: OnceLock<Arc<AppContext>> = OnceLock::new();

pub(crate) fn stash_ctx(ctx: &AppContext) -> Arc<AppContext> {
    Arc::new(AppContext {
        config: ctx.config.clone(),
        hooks: ctx.hooks.clone(),
        modules: ctx.modules.clone(),
        cache: ctx.cache.clone(),
        geo: ctx.geo.clone(),
        db: ctx.db.clone(),
        routes: ctx.routes.clone(),
        storage_registry: ctx.storage_registry.clone(),
        tls: ctx.tls.clone(),
        start_time: std::time::Instant::now(),
        paused: std::sync::atomic::AtomicBool::new(ctx.is_paused()),
        maintenance: std::sync::atomic::AtomicBool::new(ctx.is_maintenance()),
    })
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

pub struct Web3State {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) http: reqwest::Client,
    /// Live providers built from stored network records, keyed by
    /// network id. A network update or delete drops its cached
    /// provider so the next call rebuilds it from the new record.
    pub(crate) providers: ChainProviderRegistry,
    pub(crate) networks: DashMap<String, ChainNetwork>,
    pub(crate) watched: DashMap<String, WatchedAddress>,
    pub(crate) transactions: DashMap<String, ChainTxRecord>,
    /// Index of stored transactions by (`network_id`, lowercased
    /// `tx_hash`) to record id. The DashMap entry API makes
    /// insert-if-absent atomic per key, so concurrent polls cannot
    /// store the same transaction twice (see `insert_tx_if_new` in
    /// polling.rs). Hashes are lowercased: providers return the
    /// same hash in different cases, and a case-sensitive check
    /// would store duplicates.
    pub(crate) transactions_by_hash: DashMap<(String, String), String>,
    pub(crate) anchors: DashMap<String, AnchorRecord>,
    pub(crate) wallets: DashMap<String, WalletLink>,
    pub(crate) balances: DashMap<String, BalanceSnapshot>,
    pub(crate) challenges: DashMap<String, WalletChallenge>,
    pub(crate) settings: RwLock<Web3Settings>,
}

impl Web3State {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            providers: ChainProviderRegistry::new(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(PROVIDER_TIMEOUT_SECS))
                .connect_timeout(std::time::Duration::from_secs(5))
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("unified/", env!("CARGO_PKG_VERSION")))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            networks: DashMap::new(),
            watched: DashMap::new(),
            transactions: DashMap::new(),
            transactions_by_hash: DashMap::new(),
            anchors: DashMap::new(),
            wallets: DashMap::new(),
            balances: DashMap::new(),
            challenges: DashMap::new(),
            settings: RwLock::new(Web3Settings::default()),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    pub(crate) fn settings_snapshot(&self) -> Web3Settings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}
