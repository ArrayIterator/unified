//! In-memory ESG store.

use super::types::{AuditRecord, Entry, EntryHistory, Framework, Metric, Period, Target};
use dashmap::DashMap;
use diesel::prelude::*;
use serde_json::{Value, json};
use std::sync::{Arc, RwLock};
use unified_core::db::{
    DbPool,
    models::{
        EsgAudit as DbEsgAudit, EsgEntry as DbEsgEntry, EsgEntryHistory as DbEsgEntryHistory,
        EsgFramework as DbEsgFramework, EsgMetric as DbEsgMetric, EsgPeriod as DbEsgPeriod,
        EsgTarget as DbEsgTarget,
    },
};
use unified_core::error::{UnifiedError, UnifiedResult};
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

    /// Hydrate the in-memory ESG maps from the database (C11,
    /// task 271): frameworks, metrics, periods, entries, entry
    /// history, targets, and audit rows are loaded from the
    /// `esg_*` tables and replace the map contents, so a restart
    /// restores the reporting state instead of losing it. Every
    /// row already carries its `tenant_id` (the tables stamp it
    /// `NOT NULL`), and entries / targets keep their
    /// `scope_type` / `scope_id` as stored: no re-derivation,
    /// the database row is the record. Rows whose fixed
    /// vocabulary does not parse (an unknown category, data
    /// type, period status, or target direction) are skipped,
    /// never fatal: hydration must not fail startup on one bad
    /// row. Call once from `EsgModule::init` after the pool is
    /// attached (see `docs/integration/INTEGRATION_NOTES_C11.md`
    /// for the exact call). Returns the number of rows loaded.
    pub(crate) fn hydrate_from_db(&self, pool: &DbPool) -> UnifiedResult<usize> {
        use super::types::{Category, MetricDataType, PeriodStatus, TargetDirection};
        use unified_core::db::schema::{
            esg_audit::dsl as audit_dsl, esg_entries::dsl as entry_dsl,
            esg_entry_history::dsl as hist_dsl, esg_frameworks::dsl as fw_dsl,
            esg_metrics::dsl as metric_dsl, esg_periods::dsl as period_dsl,
            esg_targets::dsl as target_dsl,
        };
        let mut conn = pool.get().map_err(|error| {
            UnifiedError::internal(format!("Cannot get database connection: {error}"))
        })?;
        let db_frameworks = fw_dsl::esg_frameworks
            .select(DbEsgFramework::as_select())
            .load::<DbEsgFramework>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load ESG frameworks: {error}"))
            })?;
        let db_metrics = metric_dsl::esg_metrics
            .select(DbEsgMetric::as_select())
            .load::<DbEsgMetric>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load ESG metrics: {error}")))?;
        let db_periods = period_dsl::esg_periods
            .select(DbEsgPeriod::as_select())
            .load::<DbEsgPeriod>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load ESG periods: {error}")))?;
        let db_entries = entry_dsl::esg_entries
            .select(DbEsgEntry::as_select())
            .load::<DbEsgEntry>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load ESG entries: {error}")))?;
        let db_history = hist_dsl::esg_entry_history
            .select(DbEsgEntryHistory::as_select())
            .load::<DbEsgEntryHistory>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load ESG entry history: {error}"))
            })?;
        let db_targets = target_dsl::esg_targets
            .select(DbEsgTarget::as_select())
            .load::<DbEsgTarget>(&mut conn)
            .map_err(|error| UnifiedError::internal(format!("Cannot load ESG targets: {error}")))?;
        let db_audit = audit_dsl::esg_audit
            .select(DbEsgAudit::as_select())
            .load::<DbEsgAudit>(&mut conn)
            .map_err(|error| {
                UnifiedError::internal(format!("Cannot load ESG audit rows: {error}"))
            })?;

        fn string_list(value: &Value) -> Vec<String> {
            value
                .as_array()
                .map(|values| {
                    values
                        .iter()
                        .filter_map(|item| item.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        }

        let mut loaded = 0;
        self.frameworks.clear();
        for row in &db_frameworks {
            self.frameworks.insert(
                row.id,
                Framework {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    name: row.name.clone(),
                    description: row.description.clone(),
                    version: row.version.clone(),
                    created_by: row.created_by.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.metrics.clear();
        for row in &db_metrics {
            let Some(category) = Category::from_str(&row.category) else {
                continue;
            };
            let Some(data_type) = MetricDataType::from_str(&row.data_type) else {
                continue;
            };
            self.metrics.insert(
                row.id,
                Metric {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    name: row.name.clone(),
                    description: row.description.clone(),
                    category,
                    unit: row.unit.clone(),
                    data_type,
                    frequency: row.frequency.clone(),
                    framework_ids: string_list(&row.framework_ids),
                    active: row.active,
                    created_by: row.created_by.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.periods.clear();
        for row in &db_periods {
            let Some(status) = PeriodStatus::from_str(&row.status) else {
                continue;
            };
            self.periods.insert(
                row.id,
                Period {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    code: row.code.clone(),
                    name: row.name.clone(),
                    starts_on: row.starts_on,
                    ends_on: row.ends_on,
                    status,
                    created_by: row.created_by.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.entries.clear();
        for row in &db_entries {
            self.entries.insert(
                row.id,
                Entry {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    metric_id: row.metric_id,
                    period_id: row.period_id,
                    scope_type: row.scope_type.clone(),
                    scope_id: row.scope_id.clone(),
                    value_text: row.value_text.clone(),
                    source_note: row.source_note.clone(),
                    document_ids: string_list(&row.document_ids),
                    entered_by: row.entered_by.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.entry_history.clear();
        for row in &db_history {
            self.entry_history.insert(
                row.id,
                EntryHistory {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    entry_id: row.entry_id,
                    metric_id: row.metric_id,
                    period_id: row.period_id,
                    value_text: row.value_text.clone(),
                    source_note: row.source_note.clone(),
                    entered_by: row.entered_by.clone(),
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        self.targets.clear();
        for row in &db_targets {
            let Some(direction) = TargetDirection::from_str(&row.direction) else {
                continue;
            };
            self.targets.insert(
                row.id,
                Target {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    metric_id: row.metric_id,
                    period_id: row.period_id,
                    scope_type: row.scope_type.clone(),
                    scope_id: row.scope_id.clone(),
                    target_value: row.target_value.clone(),
                    direction,
                    deadline: row.deadline,
                    created_by: row.created_by.clone(),
                    created_at: row.created_at,
                    updated_at: row.updated_at,
                },
            );
            loaded += 1;
        }
        self.audit.clear();
        for row in &db_audit {
            self.audit.insert(
                row.id,
                AuditRecord {
                    id: row.id,
                    tenant_id: row.tenant_id.clone(),
                    entity_type: row.entity_type.clone(),
                    entity_id: row.entity_id.clone(),
                    action: row.action.clone(),
                    actor: row.actor.clone(),
                    before: row.before.clone(),
                    after: row.after.clone(),
                    created_at: row.created_at,
                },
            );
            loaded += 1;
        }
        Ok(loaded)
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
