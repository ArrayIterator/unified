// logs - store.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc, OnceLock, RwLock,
};
use uuid::Uuid;
use super::*;
use super::redaction::*;
use super::types::*;
use super::service::*;
use super::tracing::*;
use super::common::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// State (single global instance so `push_log` and the module share one
// ring buffer even before the module is initialized)

pub struct LogsState {
    pub(crate) hooks: RwLock<Arc<HookBus>>,
    pub(crate) entries: DashMap<String, LogEntry>,
    pub(crate) seq: AtomicU64,
    pub(crate) settings: RwLock<LogsSettings>,
}

impl LogsState {
    pub(crate) fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            entries: DashMap::new(),
            seq: AtomicU64::new(1),
            settings: RwLock::new(LogsSettings::default()),
        }
    }

    pub(crate) fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    pub(crate) fn emit(&self, event: &str, data: Value) {
        self.bus().emit(event, data);
    }

    /// Store one entry (redacting secrets first) and enforce the size
    /// cap by dropping the oldest entries.
    pub fn push(&self, mut entry: LogEntry) -> LogEntry {
        entry.id = new_id();
        entry.seq = self.seq.fetch_add(1, Ordering::SeqCst);
        entry.timestamp = now();
        entry.message = scrub_message(&entry.message);
        entry.context = redact_value(entry.context);
        self.entries.insert(entry.id.clone(), entry.clone());
        self.enforce_limit();
        entry
    }

    pub(crate) fn enforce_limit(&self) {
        let max = self
            .settings
            .read()
            .expect("logs settings lock poisoned")
            .max_entries;
        let len = self.entries.len();
        if len <= max {
            return;
        }
        let excess = len - max;
        let mut ordered: Vec<(u64, String)> = self
            .entries
            .iter()
            .map(|entry| (entry.value().seq, entry.key().clone()))
            .collect();
        ordered.sort();
        for (_, id) in ordered.into_iter().take(excess) {
            self.entries.remove(&id);
        }
    }

    /// Delete entries older than the retention window, then enforce the
    /// size cap. Returns how many entries were removed.
    pub fn prune(&self) -> PruneReport {
        let (retention_days, max_entries) = {
            let settings = self
                .settings
                .read()
                .expect("logs settings lock poisoned");
            (settings.retention_days, settings.max_entries)
        };
        let cutoff = now() - chrono::Duration::days(i64::from(retention_days));
        let stale: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| entry.value().timestamp < cutoff)
            .map(|entry| entry.key().clone())
            .collect();
        let removed = stale.len() as u64;
        for id in stale {
            self.entries.remove(&id);
        }
        self.enforce_limit();
        PruneReport {
            removed,
            remaining: self.entries.len() as u64,
            retention_days,
            max_entries,
        }
    }

    pub fn settings(&self) -> LogsSettings {
        self.settings
            .read()
            .expect("logs settings lock poisoned")
            .clone()
    }
}
