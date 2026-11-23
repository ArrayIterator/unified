// forms — store.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, NaiveDate, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::common::*;
use super::validation::*;
use super::submission::*;
use super::module::*;
use super::handlers_forms::*;
use super::handlers_fields::*;
use super::handlers_submissions::*;

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for forms. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by skipping event emission. Durable rows for these types are
/// defined by migration `2026-11-22-000047_forms`; until that
/// migration is executed, forms do not survive a restart.
/// `attempts` holds submit timestamps per rate-limit key, pruned to
/// the last hour on every submit.
#[derive(Clone)]
pub struct FormsState {
    pub forms: Arc<DashMap<Uuid, Form>>,
    pub fields: Arc<DashMap<Uuid, FormField>>,
    pub submissions: Arc<DashMap<Uuid, FormSubmission>>,
    pub values: Arc<DashMap<Uuid, FormSubmissionValue>>,
    pub attempts: Arc<DashMap<String, Vec<DateTime<Utc>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for FormsState {
    fn default() -> Self {
        Self {
            forms: Arc::new(DashMap::new()),
            fields: Arc::new(DashMap::new()),
            submissions: Arc::new(DashMap::new()),
            values: Arc::new(DashMap::new()),
            attempts: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl FormsState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}
