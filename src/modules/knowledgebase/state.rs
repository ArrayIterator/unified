//! Knowledgebase - In-memory state and its constructors.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::HookBus;
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cmp::Ordering;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::{constants::*, types::*, helpers::*, module::*, handlers_articles::*, handlers_tickets::*, handlers_categories::*};

// ---------------------------------------------------------------------------
// Shared state
// ---------------------------------------------------------------------------

/// In-memory stores for the knowledgebase. The hook bus is attached in
/// `init` (the bus lives in `AppContext`); handlers tolerate a missing bus
/// by simply skipping event emission.
#[derive(Clone, Default)]
pub struct KnowledgebaseState {
    pub articles: Arc<DashMap<Uuid, Article>>,
    pub versions: Arc<DashMap<Uuid, Vec<ArticleVersion>>>,
    pub categories: Arc<DashMap<Uuid, Category>>,
    pub ticket_links: Arc<DashMap<Uuid, Vec<TicketLink>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl KnowledgebaseState {
    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.hooks.read().expect("hooks lock poisoned").clone() {
            bus.emit(event, data);
        }
    }
}
