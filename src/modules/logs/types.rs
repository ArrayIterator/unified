// Logs module - central log viewer.
//
// Log entries come from three sources:
//
//   1. Application logs: any module (or the tracing layer below) calls
//      `push_log` / `push_log_full`, or emits the `logs.record` hook with
//      `{"level", "source", "message", "context"?, "request_id"?,
//      "user_id"?}`. A `tracing` subscriber bridge is also provided:
//      `LogsLayer` implements `tracing_subscriber::Layer` and forwards
//      every tracing event into the same ring buffer. Wiring it in is a
//      one-line change in the logging setup (see the note at the bottom
//      of this file); until then `push_log` and the hook are the live
//      ingestion paths.
//   2. Access logs: the module subscribes to the core `request.after`
//      hook, which the HTTP server emits for every request with
//      `{"method", "path", "status", "duration_ms"}`.
//   3. Error logs: errors are recorded through `record_error`, the
//      `logs.record` hook with `level: "error"`, or the `error.occurred`
//      hook.
//
// Entries live in an in-memory ring buffer (bounded by settings) and
// are never persisted to the database by this module; the migration
// snippet for a durable `logs` table is reported to the coordinator.
// Secrets are never stored: context keys that look like credentials
// are replaced with `[redacted]` and `password=...` / `token: ...`
// fragments are scrubbed out of messages before an entry is kept.
//
// Retention pruning runs on the scheduler's `cleanup.requested` hook
// and through the manual prune endpoint (which requires confirmation).

// logs — types.

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
use super::store::*;
use super::service::*;
use super::tracing::*;
use super::common::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Capability required to read log entries, counts, settings and exports.
pub const PERM_LOGS_VIEW: &str = "logs:view";

/// Capability required to record log entries, change retention settings
/// and prune the buffer.
pub const PERM_LOGS_MANAGE: &str = "logs:manage";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub id: String,
    pub seq: u64,
    pub timestamp: DateTime<Utc>,
    /// One of `trace`, `debug`, `info`, `warn`, `error`.
    pub level: String,
    /// One of `application`, `access`, `error`.
    pub kind: String,
    /// Module or component that produced the entry, for example
    /// `billing` or `http`.
    pub source: String,
    pub message: String,
    /// Structured context, redacted before storage. Never contains
    /// secrets, tokens, or passwords.
    pub context: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    // Access-log fields (populated when `kind` is `access`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsSettings {
    pub retention_days: u32,
    pub max_entries: usize,
}

impl Default for LogsSettings {
    fn default() -> Self {
        Self {
            retention_days: DEFAULT_RETENTION_DAYS,
            max_entries: DEFAULT_MAX_ENTRIES,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneReport {
    pub removed: u64,
    pub remaining: u64,
    pub retention_days: u32,
    pub max_entries: usize,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordLogRequest {
    pub level: String,
    pub source: String,
    pub message: String,
    #[serde(default)]
    pub context: Option<Value>,
    #[serde(default)]
    pub request_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateLogsSettings {
    pub retention_days: Option<u32>,
    pub max_entries: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PruneRequest {
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ExportQuery {
    #[serde(default)]
    pub format: Option<String>,
}
