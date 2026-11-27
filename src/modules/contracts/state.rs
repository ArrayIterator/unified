// Contracts - Shared state.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use rand::{distr::Alphanumeric, Rng};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::*;

// Shared state

/// In-memory stores for contracts. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a
/// missing bus by skipping event emission. Durable rows for these
/// types are defined by migration
/// `2026-11-22-000048_contracts`; until that migration is
/// executed, contracts do not survive a restart.
#[derive(Clone)]
pub struct ContractsState {
    pub templates: Arc<DashMap<Uuid, ContractTemplate>>,
    pub contracts: Arc<DashMap<Uuid, Contract>>,
    pub signers: Arc<DashMap<Uuid, ContractSigner>>,
    pub audit_trail: Arc<DashMap<Uuid, ContractAuditEntry>>,
    pub alert_days: Arc<RwLock<Vec<i64>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ContractsState {
    fn default() -> Self {
        Self {
            templates: Arc::new(DashMap::new()),
            contracts: Arc::new(DashMap::new()),
            signers: Arc::new(DashMap::new()),
            audit_trail: Arc::new(DashMap::new()),
            alert_days: Arc::new(RwLock::new(DEFAULT_ALERT_DAYS.to_vec())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ContractsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    /// Emit a `notification.requested` event in the notifications
    /// module's payload shape.
    pub(crate) fn notify(&self, kind: &str, user_id: &str, title: &str, body: &str, link: Option<String>) {
        self.emit(
            "notification.requested",
            json!({
                "kind": kind,
                "user_id": user_id,
                "title": title,
                "body": body,
                "module": "contracts",
                "category": "contracts",
                "priority": "normal",
                "link": link,
            }),
        );
    }

    pub(crate) fn default_alert_days(&self) -> Vec<i64> {
        self.alert_days
            .read()
            .expect("alert days lock poisoned")
            .clone()
    }
}
