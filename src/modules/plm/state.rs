//! In-memory PLM store.

use super::types::{
    PlmApproval, PlmAuditEntry, PlmBomVersion, PlmEco, PlmSettings, PlmStage, PlmVersion,
};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the PLM module. The hook bus is attached
/// in `init` (the bus lives in `AppContext`); handlers tolerate
/// a missing bus by skipping event emission. Durable rows for
/// these types are defined by migration
/// `2026-11-22-000113_plm`; until that migration is executed,
/// PLM data does not survive a restart.
#[derive(Clone)]
pub struct PlmState {
    pub stages: Arc<DashMap<Uuid, PlmStage>>,
    pub versions: Arc<DashMap<Uuid, PlmVersion>>,
    pub ecos: Arc<DashMap<Uuid, PlmEco>>,
    pub approvals: Arc<DashMap<Uuid, PlmApproval>>,
    pub bom_versions: Arc<DashMap<Uuid, PlmBomVersion>>,
    pub audit: Arc<DashMap<Uuid, PlmAuditEntry>>,
    pub settings: Arc<RwLock<PlmSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PlmState {
    fn default() -> Self {
        Self {
            stages: Arc::new(DashMap::new()),
            versions: Arc::new(DashMap::new()),
            ecos: Arc::new(DashMap::new()),
            approvals: Arc::new(DashMap::new()),
            bom_versions: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(PlmSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PlmState {
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
                "module": "plm",
                "category": "plm",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn current_settings(&self) -> PlmSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }

    pub(crate) fn update_settings(&self, settings: PlmSettings) {
        *self.settings.write().expect("settings lock poisoned") = settings;
    }

    /// Append one audit trail entry. Every stage change,
    /// version creation, ECO status change, approval decision,
    /// and BOM versioning calls this, so the history of a
    /// product's lifecycle can be replayed from the audit
    /// table alone.
    pub(crate) fn record_audit(
        &self,
        tenant_id: &str,
        entity_type: &str,
        entity_id: Uuid,
        action: &str,
        actor_id: &str,
        comment: Option<String>,
        from_value: Option<String>,
        to_value: Option<String>,
    ) -> PlmAuditEntry {
        let entry = PlmAuditEntry {
            id: Uuid::now_v7(),
            tenant_id: tenant_id.to_string(),
            entity_type: entity_type.to_string(),
            entity_id,
            action: action.to_string(),
            actor_id: actor_id.to_string(),
            comment,
            from_value,
            to_value,
            created_at: chrono::Utc::now(),
        };
        self.audit.insert(entry.id, entry.clone());
        entry
    }
}
