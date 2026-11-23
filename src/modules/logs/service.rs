// logs — service.

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
use super::store::*;
use super::tracing::*;
use super::common::*;
use super::handlers::*;
use super::hooks::*;
use super::module::*;

/// The one shared log buffer. The module and every free ingestion
/// function below use this instance, so entries recorded before the
/// module finishes initializing are still visible afterwards.
pub(crate) static GLOBAL_STATE: OnceLock<Arc<LogsState>> = OnceLock::new();

pub fn global_state() -> Arc<LogsState> {
    GLOBAL_STATE
        .get_or_init(|| Arc::new(LogsState::new()))
        .clone()
}

// ---------------------------------------------------------------------------
// Public ingestion API (used by other modules and by the tracing layer)
// ---------------------------------------------------------------------------

/// Record an application log entry in the shared ring buffer. The
/// level is normalized; an unknown level falls back to `info`.
/// Context values are redacted before storage.
pub fn push_log(level: &str, source: &str, message: &str, context: Value) -> LogEntry {
    push_log_full(level, source, message, context, None, None)
}

/// Record a log entry with optional request and user correlation ids.
pub fn push_log_full(
    level: &str,
    source: &str,
    message: &str,
    context: Value,
    request_id: Option<String>,
    user_id: Option<String>,
) -> LogEntry {
    let state = global_state();
    state.push(LogEntry {
        id: String::new(),
        seq: 0,
        timestamp: now(),
        level: normalize_level(level).unwrap_or_else(|| "info".to_string()),
        kind: "application".to_string(),
        source: if source.trim().is_empty() {
            "application".to_string()
        } else {
            source.trim().to_string()
        },
        message: message.to_string(),
        context,
        request_id: request_id.filter(|id| !id.trim().is_empty()),
        user_id: user_id.filter(|id| !id.trim().is_empty()),
        method: None,
        path: None,
        status: None,
        duration_ms: None,
        ip: None,
    })
}

/// Record one HTTP access-log line (method, path, status, duration,
/// client IP). The level is derived from the status code: 5xx is
/// `error`, 4xx is `warn`, anything else is `info`.
pub fn record_access(
    method: &str,
    path: &str,
    status: u16,
    duration_ms: u64,
    ip: Option<String>,
) -> LogEntry {
    let state = global_state();
    let level = if status >= 500 {
        "error"
    } else if status >= 400 {
        "warn"
    } else {
        "info"
    };
    state.push(LogEntry {
        id: String::new(),
        seq: 0,
        timestamp: now(),
        level: level.to_string(),
        kind: "access".to_string(),
        source: "http".to_string(),
        message: format!("{method} {path} -> {status} ({duration_ms}ms)"),
        context: json!({
            "method": method,
            "path": path,
            "status": status,
            "duration_ms": duration_ms,
        }),
        request_id: None,
        user_id: None,
        method: Some(method.to_string()),
        path: Some(path.to_string()),
        status: Some(status),
        duration_ms: Some(duration_ms),
        ip,
    })
}

/// Record an error-log entry (level `error`, kind `error`).
pub fn record_error(source: &str, message: &str, context: Value) -> LogEntry {
    let state = global_state();
    state.push(LogEntry {
        id: String::new(),
        seq: 0,
        timestamp: now(),
        level: "error".to_string(),
        kind: "error".to_string(),
        source: if source.trim().is_empty() {
            "application".to_string()
        } else {
            source.trim().to_string()
        },
        message: message.to_string(),
        context,
        request_id: None,
        user_id: None,
        method: None,
        path: None,
        status: None,
        duration_ms: None,
        ip: None,
    })
}
