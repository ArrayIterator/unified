//! In-memory sign store.

use super::types::{
    SignAuditEntry, SignCertificate, SignEnvelope, SignSettings, SignSigner, SignTemplate,
};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the sign module. The hook bus is attached
/// in `init` (the bus lives in `AppContext`); handlers tolerate
/// a missing bus by skipping event emission. Durable rows for
/// these types are defined by migration
/// `2026-11-22-000114_sign`; until that migration is executed,
/// sign data does not survive a restart.
#[derive(Clone)]
pub struct SignState {
    pub envelopes: Arc<DashMap<Uuid, SignEnvelope>>,
    pub signers: Arc<DashMap<Uuid, SignSigner>>,
    pub templates: Arc<DashMap<Uuid, SignTemplate>>,
    pub audit: Arc<DashMap<Uuid, Vec<SignAuditEntry>>>,
    pub certificates: Arc<DashMap<String, SignCertificate>>,
    pub settings: Arc<DashMap<String, SignSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for SignState {
    fn default() -> Self {
        Self {
            envelopes: Arc::new(DashMap::new()),
            signers: Arc::new(DashMap::new()),
            templates: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            certificates: Arc::new(DashMap::new()),
            settings: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl SignState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

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
                "module": "sign",
                "category": "sign",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Per-tenant settings, falling back to the module default
    /// when the tenant has never saved settings.
    pub(crate) fn settings_for(&self, tenant_id: &str) -> SignSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    /// Signers of one envelope, in signing order.
    pub(crate) fn signers_of(&self, envelope_id: Uuid) -> Vec<SignSigner> {
        let mut signers: Vec<SignSigner> = self
            .signers
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|signer| signer.envelope_id == envelope_id)
            .collect();
        signers.sort_by_key(|signer| signer.order_index);
        signers
    }

    /// Audit trail of one envelope, in insertion (chain) order.
    pub(crate) fn audit_of(&self, envelope_id: Uuid) -> Vec<SignAuditEntry> {
        self.audit
            .get(&envelope_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }
}
