//! In-memory ESG store.

use super::types::{AuditRecord, Entry, EntryHistory, Framework, Metric, Period, Target};
use dashmap::DashMap;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the ESG module. The hook bus is
/// attached in `init` (the bus lives in `AppContext`);
/// handlers tolerate a missing bus by skipping event emission.
/// Durable rows for these types are defined by migration
/// `2026-11-22-000119_esg`; until that migration is executed,
/// ESG data does not survive a restart.
#[derive(Clone)]
pub struct EsgState {
    pub frameworks: Arc<DashMap<Uuid, Framework>>,
    pub metrics: Arc<DashMap<Uuid, Metric>>,
    pub periods: Arc<DashMap<Uuid, Period>>,
    pub entries: Arc<DashMap<Uuid, Entry>>,
    pub entry_history: Arc<DashMap<Uuid, EntryHistory>>,
    pub targets: Arc<DashMap<Uuid, Target>>,
    pub audit: Arc<DashMap<Uuid, AuditRecord>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for EsgState {
    fn default() -> Self {
        Self {
            frameworks: Arc::new(DashMap::new()),
            metrics: Arc::new(DashMap::new()),
            periods: Arc::new(DashMap::new()),
            entries: Arc::new(DashMap::new()),
            entry_history: Arc::new(DashMap::new()),
            targets: Arc::new(DashMap::new()),
            audit: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl EsgState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Frameworks of one tenant, newest first.
    pub(crate) fn frameworks_of(&self, tenant_id: &str) -> Vec<Framework> {
        let mut items: Vec<Framework> = self
            .frameworks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|framework| framework.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Metrics of one tenant, newest first.
    pub(crate) fn metrics_of(&self, tenant_id: &str) -> Vec<Metric> {
        let mut items: Vec<Metric> = self
            .metrics
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|metric| metric.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Periods of one tenant, by start date, newest first.
    pub(crate) fn periods_of(&self, tenant_id: &str) -> Vec<Period> {
        let mut items: Vec<Period> = self
            .periods
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|period| period.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| {
            b.starts_on
                .cmp(&a.starts_on)
                .then(b.created_at.cmp(&a.created_at))
        });
        items
    }

    /// Entries of one tenant for one period, in entry order.
    pub(crate) fn entries_of_period(&self, tenant_id: &str, period_id: Uuid) -> Vec<Entry> {
        let mut items: Vec<Entry> = self
            .entries
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|entry| entry.tenant_id == tenant_id && entry.period_id == period_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// The entry for one (metric, period, scope) combination,
    /// when one exists. The combination is unique, so at most
    /// one row matches.
    pub(crate) fn entry_for(
        &self,
        tenant_id: &str,
        metric_id: Uuid,
        period_id: Uuid,
        scope_type: &str,
        scope_id: &str,
    ) -> Option<Entry> {
        self.entries
            .iter()
            .map(|entry| entry.value().clone())
            .find(|entry| {
                entry.tenant_id == tenant_id
                    && entry.metric_id == metric_id
                    && entry.period_id == period_id
                    && entry.scope_type == scope_type
                    && entry.scope_id == scope_id
            })
    }

    /// History rows of one entry, oldest first.
    pub(crate) fn history_of(&self, entry_id: Uuid) -> Vec<EntryHistory> {
        let mut items: Vec<EntryHistory> = self
            .entry_history
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.entry_id == entry_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// The target for one (metric, period, scope)
    /// combination, when one exists. The combination is
    /// unique, so at most one row matches.
    pub(crate) fn target_for(
        &self,
        tenant_id: &str,
        metric_id: Uuid,
        period_id: Uuid,
        scope_type: &str,
        scope_id: &str,
    ) -> Option<Target> {
        self.targets
            .iter()
            .map(|entry| entry.value().clone())
            .find(|target| {
                target.tenant_id == tenant_id
                    && target.metric_id == metric_id
                    && target.period_id == period_id
                    && target.scope_type == scope_type
                    && target.scope_id == scope_id
            })
    }

    /// Targets of one tenant for one period.
    pub(crate) fn targets_of_period(&self, tenant_id: &str, period_id: Uuid) -> Vec<Target> {
        let mut items: Vec<Target> = self
            .targets
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|target| target.tenant_id == tenant_id && target.period_id == period_id)
            .collect();
        items.sort_by(|a, b| a.created_at.cmp(&b.created_at));
        items
    }

    /// Audit rows of one tenant, newest first.
    pub(crate) fn audit_of(&self, tenant_id: &str) -> Vec<AuditRecord> {
        let mut items: Vec<AuditRecord> = self
            .audit
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|row| row.tenant_id == tenant_id)
            .collect();
        items.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        items
    }

    /// Emit a `notification.requested` event for a user, in the
    /// notifications module's payload shape.
    pub(crate) fn notify(&self, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": "esg",
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "esg",
                "category": "esg",
                "priority": "normal",
                "link": link,
            }),
        );
    }
}
