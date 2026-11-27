// bank_transfer - In-memory state and its methods.

use super::*;
use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::{Arc, OnceLock, RwLock};
use uuid::Uuid;

// State and in-memory stores.

pub struct BankTransferState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
}

impl BankTransferState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}

static ACCOUNTS: OnceLock<DashMap<Uuid, BankAccount>> = OnceLock::new();

pub(crate) fn accounts_store() -> &'static DashMap<Uuid, BankAccount> {
    ACCOUNTS.get_or_init(DashMap::new)
}

static VERIFICATIONS: OnceLock<DashMap<Uuid, TransferVerification>> = OnceLock::new();

pub(crate) fn verifications_store() -> &'static DashMap<Uuid, TransferVerification> {
    VERIFICATIONS.get_or_init(DashMap::new)
}

static AUDIT: OnceLock<DashMap<Uuid, VerificationAuditEntry>> = OnceLock::new();

pub(crate) fn audit_store() -> &'static DashMap<Uuid, VerificationAuditEntry> {
    AUDIT.get_or_init(DashMap::new)
}

static SETTINGS: OnceLock<RwLock<BankTransferSettings>> = OnceLock::new();

pub(crate) fn settings_store() -> &'static RwLock<BankTransferSettings> {
    SETTINGS.get_or_init(|| RwLock::new(BankTransferSettings::default()))
}

pub(crate) fn settings() -> BankTransferSettings {
    settings_store()
        .read()
        .expect("settings lock poisoned")
        .clone()
}
