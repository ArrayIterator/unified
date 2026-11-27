//! Portals - shared in-memory state.
//!
//! Part of the `portals` module split; see `src/modules/portals.rs`.

use unified_core::hooks::HookBus;
use dashmap::DashMap;
use serde_json::Value;
use std::sync::Arc;
use std::sync::RwLock;
use uuid::Uuid;

use super::types::Portal;
use super::types::PortalMenu;
use super::types::PortalRecord;
use super::types::PortalView;

// Shared state

/// In-memory stores for portals. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000046_portals`; until that
/// migration is executed, portals do not survive a restart.
#[derive(Clone)]
pub struct PortalsState {
    pub portals: Arc<DashMap<Uuid, Portal>>,
    pub menus: Arc<DashMap<Uuid, PortalMenu>>,
    pub records: Arc<DashMap<String, PortalRecord>>,
    pub views: Arc<DashMap<Uuid, PortalView>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PortalsState {
    fn default() -> Self {
        Self {
            portals: Arc::new(DashMap::new()),
            menus: Arc::new(DashMap::new()),
            records: Arc::new(DashMap::new()),
            views: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PortalsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}
