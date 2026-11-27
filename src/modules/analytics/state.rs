//! analytics - state.

use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use actix_web::{ http::header };
use dashmap::DashMap;
use std::sync::{ Arc, RwLock };
use super::{events::*, helpers::*, types::*, views::*, inputs::*, hooks::*, reports::*, handlers_dashboard::*, handlers_definitions::*, handlers_runs::*, handlers_scheduled::*, module::*};



// State.
pub struct AnalyticsState {
    /// Hook bus, replaced with the global bus in `init` (same pattern
    /// as the store and scheduler modules) so hook listeners can emit
    /// events of their own.
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) sales: DashMap<String, SaleRecord>,
    pub(crate) licenses: DashMap<String, LicenseSnapshot>,
    pub(crate) metrics: DashMap<String, MetricSample>,
    pub(crate) health: DashMap<String, HealthSample>,
    pub(crate) definitions: DashMap<String, ReportDefinition>,
    pub(crate) runs: DashMap<String, ReportRun>,
    pub(crate) scheduled: DashMap<String, ScheduledReport>,
    pub(crate) tasks: DashMap<String, TaskMirror>,
    pub(crate) pools: DashMap<String, PoolMirror>,
}


impl AnalyticsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            sales: DashMap::new(),
            licenses: DashMap::new(),
            metrics: DashMap::new(),
            health: DashMap::new(),
            definitions: DashMap::new(),
            runs: DashMap::new(),
            scheduled: DashMap::new(),
            tasks: DashMap::new(),
            pools: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn definition(&self, id: &str) -> UnifiedResult<ReportDefinition> {
        self.definitions
            .get(id)
            .map(|entry| entry.value().clone())
            .ok_or_else(|| UnifiedError::not_found(format!("Report definition \"{id}\" not found")))
    }
}
