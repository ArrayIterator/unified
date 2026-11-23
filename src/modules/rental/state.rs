//! In-memory rental store.

use super::types::{
    RentalDamageReport, RentalDelivery, RentalListing, RentalOrder, RentalRate, RentalSettings,
};
use dashmap::DashMap;
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use unified_core::hooks::HookBus;
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the rental module. The hook bus is
/// attached in `init` (the bus lives in `AppContext`); handlers
/// tolerate a missing bus by skipping event emission. Durable
/// rows for these types are defined by migration
/// `2026-11-22-000111_rental`; until that migration is executed,
/// rental data does not survive a restart.
#[derive(Clone)]
pub struct RentalState {
    pub listings: Arc<DashMap<Uuid, RentalListing>>,
    pub rates: Arc<DashMap<Uuid, RentalRate>>,
    pub orders: Arc<DashMap<Uuid, RentalOrder>>,
    pub deliveries: Arc<DashMap<Uuid, RentalDelivery>>,
    pub damage_reports: Arc<DashMap<Uuid, RentalDamageReport>>,
    pub settings: Arc<RwLock<RentalSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for RentalState {
    fn default() -> Self {
        Self {
            listings: Arc::new(DashMap::new()),
            rates: Arc::new(DashMap::new()),
            orders: Arc::new(DashMap::new()),
            deliveries: Arc::new(DashMap::new()),
            damage_reports: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(RentalSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl RentalState {
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
                "module": "rental",
                "category": "rental",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn current_settings(&self) -> RentalSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}
