// OKR / Goals - In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_cycles::*;
use super::handlers_objectives::*;
use super::handlers_key_results::*;
use super::hooks::*;
use super::module::*;

// Shared state

/// In-memory stores for OKRs. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000043_okrs`; until that
/// migration is executed, cycles, objectives, and key results do
/// not survive a restart. `completed_tasks` records the ids of
/// tasks seen completed through the task-completion listeners, so
/// a roll-up can be recomputed even when the completion event
/// arrived before the link was created.
#[derive(Clone)]
pub struct OkrState {
    pub cycles: Arc<DashMap<Uuid, OkrCycle>>,
    pub objectives: Arc<DashMap<Uuid, OkrObjective>>,
    pub key_results: Arc<DashMap<Uuid, OkrKeyResult>>,
    pub checkins: Arc<DashMap<Uuid, OkrCheckin>>,
    pub links: Arc<DashMap<Uuid, OkrLink>>,
    pub completed_tasks: Arc<DashMap<Uuid, DateTime<Utc>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for OkrState {
    fn default() -> Self {
        Self {
            cycles: Arc::new(DashMap::new()),
            objectives: Arc::new(DashMap::new()),
            key_results: Arc::new(DashMap::new()),
            checkins: Arc::new(DashMap::new()),
            links: Arc::new(DashMap::new()),
            completed_tasks: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl OkrState {
    fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}
