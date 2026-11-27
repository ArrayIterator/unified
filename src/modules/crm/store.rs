// crm - store.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::common::*;
use super::handlers_contacts::*;
use super::handlers_leads::*;
use super::handlers_activities::*;
use super::module::*;

// State

pub struct CrmState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) contacts: DashMap<String, Contact>,
    pub(crate) leads: DashMap<String, Lead>,
    pub(crate) activities: DashMap<String, Vec<Activity>>,
}

impl CrmState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            contacts: DashMap::new(),
            leads: DashMap::new(),
            activities: DashMap::new(),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }
}
