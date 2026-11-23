//! In-memory delivery notes store.

use super::types::{
    format_number, number_period_key, DeliveryNote, DeliveryNoteItem, DeliveryNoteSettings,
};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the delivery notes module. The hook bus
/// is attached in `init` (the bus lives in `AppContext`);
/// handlers tolerate a missing bus by skipping event emission.
/// Durable rows for these types are defined by migration
/// `2026-11-22-000117_delivery_notes`; until that migration is
/// executed, delivery notes data does not survive a restart.
#[derive(Clone)]
pub struct DeliveryNoteState {
    pub notes: Arc<DashMap<Uuid, DeliveryNote>>,
    pub items: Arc<DashMap<Uuid, DeliveryNoteItem>>,
    pub settings: Arc<DashMap<String, DeliveryNoteSettings>>,
    /// Number sequences per tenant and period: the key is
    /// `{tenant_id}:{period_key}` and the value is the last
    /// sequence issued in that period.
    pub sequences: Arc<DashMap<String, i64>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for DeliveryNoteState {
    fn default() -> Self {
        Self {
            notes: Arc::new(DashMap::new()),
            items: Arc::new(DashMap::new()),
            settings: Arc::new(DashMap::new()),
            sequences: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl DeliveryNoteState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a hook query and return the answering context, or
    /// `None` when no handler is registered for the event.
    /// Callers treat an unanswered query as "unknown", never
    /// as a denial — see the shipment-link note in the module
    /// header.
    pub(crate) fn query(
        &self,
        event: &str,
        data: Value,
    ) -> Option<unified_core::hooks::HookContext> {
        let bus = self.bus()?;
        if !bus.has(event) {
            return None;
        }
        Some(bus.emit(event, data))
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
                "module": "delivery_notes",
                "category": "delivery_notes",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    /// Per-tenant settings, defaulting to
    /// `DeliveryNoteSettings::default` when the tenant has no
    /// row yet.
    pub(crate) fn current_settings(&self, tenant_id: &str) -> DeliveryNoteSettings {
        self.settings
            .get(tenant_id)
            .map(|entry| entry.value().clone())
            .unwrap_or_default()
    }

    pub(crate) fn save_settings(&self, tenant_id: &str, settings: DeliveryNoteSettings) {
        self.settings.insert(tenant_id.to_string(), settings);
    }

    /// Issue the next DO number for a tenant under the
    /// tenant's settings, advancing the per-period sequence.
    /// The number is unique per tenant and period as long as
    /// this process owns the sequence; the migration also
    /// enforces uniqueness per tenant in the database.
    pub(crate) fn next_number(
        &self,
        tenant_id: &str,
        settings: &DeliveryNoteSettings,
        now: DateTime<Utc>,
    ) -> String {
        let period_key = number_period_key(&settings.number_reset_rule, now);
        let key = format!("{tenant_id}:{period_key}");
        let sequence = {
            let mut entry = self.sequences.entry(key).or_insert(0);
            *entry += 1;
            *entry
        };
        format_number(
            &settings.number_prefix,
            &settings.number_reset_rule,
            settings.number_width,
            &period_key,
            sequence,
        )
    }

    /// Items of one DO, in creation order.
    pub(crate) fn note_items(&self, note_id: Uuid) -> Vec<DeliveryNoteItem> {
        let mut rows: Vec<DeliveryNoteItem> = self
            .items
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|item| item.delivery_note_id == note_id)
            .collect();
        rows.sort_by_key(|item| item.created_at);
        rows
    }

    /// DOs linked to one shipment, in creation order.
    pub(crate) fn notes_for_shipment(&self, shipment_id: Uuid) -> Vec<DeliveryNote> {
        let mut rows: Vec<DeliveryNote> = self
            .notes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|note| note.shipment_id == Some(shipment_id))
            .collect();
        rows.sort_by_key(|note| note.created_at);
        rows
    }

    /// DOs linked to one rental order, in creation order.
    pub(crate) fn notes_for_rental_order(&self, rental_order_id: &str) -> Vec<DeliveryNote> {
        let mut rows: Vec<DeliveryNote> = self
            .notes
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|note| note.rental_order_id.as_deref() == Some(rental_order_id))
            .collect();
        rows.sort_by_key(|note| note.created_at);
        rows
    }

    /// Remove every item row of one DO. Used when a draft DO is
    /// deleted; issued DOs are never deleted, only cancelled.
    pub(crate) fn remove_items_of(&self, note_id: Uuid) {
        let keys: Vec<Uuid> = self
            .items
            .iter()
            .map(|entry| *entry.key())
            .filter(|key| {
                self.items
                    .get(key)
                    .map(|entry| entry.value().delivery_note_id == note_id)
                    .unwrap_or(false)
            })
            .collect();
        for key in keys {
            self.items.remove(&key);
        }
    }
}
