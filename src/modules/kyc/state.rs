//! In-memory KYC store.

use super::types::{
    KycAuditEntry, KycCase, KycDocument, KycDocumentType, KycInstruction, KycRiskLevel,
    KycSettings, KycSubjectType, KycVerificationContext,
};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// Shared state

/// In-memory stores for the KYC module. The hook bus is attached
/// in `init` (the bus lives in `AppContext`); handlers tolerate
/// a missing bus by skipping event emission. Durable rows for
/// these types are defined by migration
/// `2026-11-22-000115_kyc`; until that migration is executed,
/// KYC data does not survive a restart.
#[derive(Clone)]
pub struct KycState {
    pub cases: Arc<DashMap<Uuid, KycCase>>,
    pub documents: Arc<DashMap<Uuid, KycDocument>>,
    pub document_types: Arc<DashMap<Uuid, KycDocumentType>>,
    pub subject_types: Arc<DashMap<Uuid, KycSubjectType>>,
    pub contexts: Arc<DashMap<Uuid, KycVerificationContext>>,
    pub instructions: Arc<DashMap<Uuid, KycInstruction>>,
    pub risk_levels: Arc<DashMap<Uuid, KycRiskLevel>>,
    pub audit_entries: Arc<DashMap<Uuid, KycAuditEntry>>,
    pub settings: Arc<DashMap<String, KycSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for KycState {
    fn default() -> Self {
        Self {
            cases: Arc::new(DashMap::new()),
            documents: Arc::new(DashMap::new()),
            document_types: Arc::new(DashMap::new()),
            subject_types: Arc::new(DashMap::new()),
            contexts: Arc::new(DashMap::new()),
            instructions: Arc::new(DashMap::new()),
            risk_levels: Arc::new(DashMap::new()),
            audit_entries: Arc::new(DashMap::new()),
            settings: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl KycState {
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
                "module": "kyc",
                "category": "kyc",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Per-tenant settings, defaulting to `KycSettings::default`
    /// (auto-approve off) when the tenant has no row yet.
    pub(crate) fn current_settings(&self, tenant_id: &str) -> KycSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    pub(crate) fn save_settings(&self, tenant_id: &str, settings: KycSettings) {
        self.settings.insert(tenant_id.to_string(), settings);
    }

    /// The tenant's active default verification context, if one
    /// exists. When several rows claim to be active (only
    /// possible for rows created before the single-active
    /// invariant was enforced), the highest version wins.
    pub(crate) fn active_context(&self, tenant_id: &str) -> Option<KycVerificationContext> {
        let mut active: Vec<KycVerificationContext> = self
            .contexts
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|context| context.tenant_id == tenant_id && context.active)
            .collect();
        active.sort_by(|a, b| b.version.cmp(&a.version));
        active.into_iter().next()
    }

    /// Active risk levels of a tenant, in `min_score` order.
    pub(crate) fn active_risk_levels(&self, tenant_id: &str) -> Vec<KycRiskLevel> {
        let mut levels: Vec<KycRiskLevel> = self
            .risk_levels
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|level| level.tenant_id == tenant_id && level.active)
            .collect();
        levels.sort_by_key(|level| level.min_score);
        levels
    }

    /// Active instructions that apply to `context_id` (plus the
    /// tenant-global ones, which carry no context id), in merge
    /// order (priority, then creation time).
    pub(crate) fn applicable_instructions(
        &self,
        tenant_id: &str,
        context_id: Uuid,
    ) -> Vec<KycInstruction> {
        let mut rows: Vec<KycInstruction> = self
            .instructions
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|instruction| {
                instruction.tenant_id == tenant_id
                    && instruction.active
                    && (instruction.context_id.is_none()
                        || instruction.context_id == Some(context_id))
            })
            .collect();
        rows.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        rows
    }

    /// Documents linked to one case, in creation order.
    pub(crate) fn case_documents(&self, case_id: Uuid) -> Vec<KycDocument> {
        let mut rows: Vec<KycDocument> = self
            .documents
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|document| document.case_id == case_id)
            .collect();
        rows.sort_by_key(|document| document.created_at);
        rows
    }
}
