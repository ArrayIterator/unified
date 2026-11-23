// Generic Approvals — In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use unified_core::tenancy::TenantContext as TenancyContext;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use uuid::Uuid;

use super::constants::*;
use super::types::*;
use super::helpers::*;
use super::validation::*;
use super::service::*;
use super::handlers_definitions::*;
use super::handlers_requests::*;
use super::handlers_dashboards::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct ApprovalsState {
    pub definitions: Arc<DashMap<Uuid, ApprovalDefinition>>,
    pub steps: Arc<DashMap<Uuid, ApprovalStep>>,
    pub requests: Arc<DashMap<Uuid, ApprovalRequest>>,
    pub decisions: Arc<DashMap<Uuid, ApprovalDecision>>,
    pub delegations: Arc<DashMap<Uuid, ApprovalDelegation>>,
    pub escalations: Arc<DashMap<Uuid, ApprovalEscalation>>,
    /// Per-request override of one step's approvers, set by the
    /// reassign endpoint: (request_id, step_index) -> approvers.
    pub step_overrides: Arc<DashMap<(Uuid, i32), Vec<StepApprover>>>,
    pub settings: Arc<RwLock<ApprovalSettings>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for ApprovalsState {
    fn default() -> Self {
        Self {
            definitions: Arc::new(DashMap::new()),
            steps: Arc::new(DashMap::new()),
            requests: Arc::new(DashMap::new()),
            decisions: Arc::new(DashMap::new()),
            delegations: Arc::new(DashMap::new()),
            escalations: Arc::new(DashMap::new()),
            step_overrides: Arc::new(DashMap::new()),
            settings: Arc::new(RwLock::new(ApprovalSettings::default())),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl ApprovalsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }

    pub(crate) fn settings(&self) -> ApprovalSettings {
        self.settings
            .read()
            .expect("settings lock poisoned")
            .clone()
    }
}
