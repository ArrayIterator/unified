// todos - store.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{web, HttpRequest, HttpResponse};
use chrono::{DateTime, Datelike, NaiveDate, TimeZone, Utc};
use dashmap::{DashMap, DashSet};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::sync::{Arc, Mutex, RwLock};
use uuid::Uuid;
use super::*;
use super::types::*;
use super::common::*;
use super::validation::*;
use super::access::*;
use super::service::*;
use super::handlers_lists::*;
use super::handlers_todos::*;
use super::handlers_subtasks::*;
use super::handlers_ai::*;
use super::module::*;

// Shared state.

/// In-memory stores for todos. The hook bus is attached in `init`
/// (the bus lives in `AppContext`); handlers use `ctx.hooks` directly
/// and tolerate a missing bus only where noted. Durable rows for these
/// types are defined by migration `2026-11-22-000035_todos`; until
/// that migration is wired into a database-backed implementation,
/// todos do not survive a restart.
#[derive(Clone)]
pub struct TodosState {
    pub lists: Arc<DashMap<Uuid, TodoList>>,
    pub todos: Arc<DashMap<Uuid, Todo>>,
    /// Keyed by `"{list_id}:{shared_with_user_id}"`.
    pub shares: Arc<DashMap<String, TodoShare>>,
    pub completion_log: Arc<DashMap<Uuid, TodoCompletionLog>>,
    pub reminders: Arc<DashMap<Uuid, TodoReminder>>,
    /// Overdue notifications already sent, keyed by
    /// `"{todo_id}:{utc-date}"`, so the reminder job emits
    /// `todos.todo.overdue` at most once per todo per day.
    pub overdue_notified: Arc<DashSet<String>>,
    /// One lock per user, serialising default-list creation for
    /// that user (see `ensure_default_list` in `access.rs`).
    pub default_list_locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
    pub hooks: Arc<RwLock<Option<Arc<HookBus>>>>,
}

impl Default for TodosState {
    fn default() -> Self {
        Self {
            lists: Arc::new(DashMap::new()),
            todos: Arc::new(DashMap::new()),
            shares: Arc::new(DashMap::new()),
            completion_log: Arc::new(DashMap::new()),
            reminders: Arc::new(DashMap::new()),
            overdue_notified: Arc::new(DashSet::new()),
            default_list_locks: Arc::new(DashMap::new()),
            hooks: Arc::new(RwLock::new(None)),
        }
    }
}

impl TodosState {
    pub(crate) fn bus(&self) -> Option<Arc<HookBus>> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        if let Some(bus) = self.bus() {
            bus.emit(event, data);
        }
    }
}
