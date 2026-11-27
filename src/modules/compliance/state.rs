//! Compliance Center module - GDPR / ISO support: data export - Shared state.
//!
//! Split from `src/modules/compliance.rs`; behaviour unchanged.

use super::breaches::*;
use super::checklists::*;
use super::consents::*;
use super::constants::*;
use super::deletion_listeners::*;
use super::deletions::*;
use super::exports::*;
use super::helpers::*;
use super::module::*;
use super::records::*;
use super::requests::*;
use super::retention::*;
use super::types::*;

use unified_core::hooks::HookBus;
use unified_core::json;
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

/// In-memory stores for compliance. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000057_compliance`; until that migration is
/// executed, compliance data does not survive a restart.
#[derive(Clone)]
pub struct ComplianceState {
    pub exports: Arc<DashMap<Uuid, ExportRequest>>,
    pub deletions: Arc<DashMap<Uuid, DeletionRequest>>,
    pub deletion_items: Arc<DashMap<Uuid, DeletionItem>>,
    pub consents: Arc<DashMap<Uuid, ConsentLog>>,
    pub retention_policies: Arc<DashMap<Uuid, RetentionPolicy>>,
    pub retention_reviews: Arc<DashMap<Uuid, RetentionReview>>,
    pub processing_records: Arc<DashMap<Uuid, ProcessingRecord>>,
    pub checklists: Arc<DashMap<Uuid, ComplianceChecklist>>,
    pub breaches: Arc<DashMap<Uuid, BreachLog>>,
    pub breach_timeline: Arc<DashMap<Uuid, BreachTimelineEntry>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ComplianceState {
    fn default() -> Self {
        Self {
            exports: Arc::new(DashMap::new()),
            deletions: Arc::new(DashMap::new()),
            deletion_items: Arc::new(DashMap::new()),
            consents: Arc::new(DashMap::new()),
            retention_policies: Arc::new(DashMap::new()),
            retention_reviews: Arc::new(DashMap::new()),
            processing_records: Arc::new(DashMap::new()),
            checklists: Arc::new(DashMap::new()),
            breaches: Arc::new(DashMap::new()),
            breach_timeline: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ComplianceState {
    pub(super) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(super) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(super) fn notify(
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
                "module": "compliance",
                "category": "compliance",
                "priority": "high",
                "link": link,
            }),
        );
    }
}
