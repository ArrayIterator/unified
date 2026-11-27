// Properties - Shared state.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use uuid::Uuid;

use super::*;

// Shared state

/// In-memory stores for properties. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000060_properties`; until that migration is
/// executed, properties do not survive a restart.
#[derive(Clone)]
pub struct PropertiesState {
    pub properties: Arc<DashMap<Uuid, Property>>,
    pub plans: Arc<DashMap<Uuid, PropertyPlan>>,
    pub blocks: Arc<DashMap<Uuid, PropertyBlock>>,
    pub history: Arc<DashMap<Uuid, PropertyBlockHistory>>,
    pub purchases: Arc<DashMap<Uuid, BlockPurchase>>,
    pub location_settings: Arc<RwLock<LocationSettings>>,
    pub location_provider: Arc<RwLock<Arc<dyn LocationProvider>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for PropertiesState {
    fn default() -> Self {
        Self {
            properties: Arc::new(DashMap::new()),
            plans: Arc::new(DashMap::new()),
            blocks: Arc::new(DashMap::new()),
            history: Arc::new(DashMap::new()),
            purchases: Arc::new(DashMap::new()),
            location_settings: Arc::new(RwLock::new(LocationSettings::default())),
            location_provider: Arc::new(RwLock::new(Arc::new(NoneProvider))),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl PropertiesState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape (see the events module's `notify`).
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "properties",
                "category": "properties",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn provider(&self) -> Arc<dyn LocationProvider> {
        self.location_provider
            .read()
            .expect("provider lock poisoned")
            .clone()
    }

    pub(crate) fn blocks_of_plan(&self, plan_id: Uuid) -> Vec<PropertyBlock> {
        let mut items: Vec<PropertyBlock> = self
            .blocks
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|block| block.plan_id == plan_id)
            .collect();
        items.sort_by(|a, b| a.title.cmp(&b.title).then(a.created_at.cmp(&b.created_at)));
        items
    }

    pub(crate) fn plans_of_property(&self, property_id: Uuid) -> Vec<PropertyPlan> {
        let mut items: Vec<PropertyPlan> = self
            .plans
            .iter()
            .map(|entry| entry.value().clone())
            .filter(|plan| plan.property_id == property_id)
            .collect();
        items.sort_by(|a, b| a.name.cmp(&b.name).then(a.created_at.cmp(&b.created_at)));
        items
    }
}
