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

use crate::core::context::AppContext;
use crate::core::error::{UnifiedError, UnifiedResult};
use crate::core::hooks::{events, HookBus, HookContext};
use crate::core::json;
use crate::core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::core::pagination::{PageResult, PaginationParams};
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

// ---------------------------------------------------------------------------
// Constants and redaction
// ---------------------------------------------------------------------------

/// Valid log levels, lowest to highest severity.
pub const LOG_LEVELS: &[&str] = &["trace", "debug", "info", "warn", "error"];

/// Valid log entry kinds.
pub const LOG_KINDS: &[&str] = &["application", "access", "error"];

/// Default number of days log entries are kept.
pub const DEFAULT_RETENTION_DAYS: u32 = 30;

/// Default maximum number of entries held in the ring buffer.
pub const DEFAULT_MAX_ENTRIES: usize = 10_000;

/// Context keys that name credentials. Any key containing one of these
/// substrings (case-insensitive) has its value replaced with
/// `[redacted]` before the entry is stored.
const SENSITIVE_KEYS: &[&str] = &[
    "password",
    "passwd",
    "token",
    "secret",
    "api_key",
    "apikey",
    "api-key",
    "authorization",
    "auth",
    "cookie",
    "session",
    "private_key",
    "privatekey",
    "bearer",
    "credential",
    "access_key",
    "accesskey",
    "refresh_token",
    "client_secret",
];

fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_lowercase();
    SENSITIVE_KEYS.iter().any(|needle| lower.contains(needle))
}

/// Recursively replace credential values in a JSON context object.
fn redact_value(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .map(|(key, item)| {
                    if is_sensitive_key(&key) {
                        (key, Value::String("[redacted]".to_string()))
                    } else {
                        (key, redact_value(item))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => {
            Value::Array(items.into_iter().map(redact_value).collect())
        }
        other => other,
    }
}

fn secret_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r#"(?i)(password|passwd|token|secret|api[_-]?key|authorization|bearer)\s*[:=]\s*\S+"#)
            .expect("secret scrub pattern is valid")
    })
}

/// Scrub `password=...`, `token: ...` style fragments out of a message.
fn scrub_message(message: &str) -> String {
    secret_pattern()
        .replace_all(message, "$1[redacted]")
        .to_string()
}

fn normalize_level(level: &str) -> Option<String> {
    let level = level.trim().to_lowercase();
    if LOG_LEVELS.contains(&level.as_str()) {
        Some(level)
    } else {
        None
    }
}

fn level_rank(level: &str) -> u8 {
    match level {
        "trace" => 0,
        "debug" => 1,
        "info" => 2,
        "warn" => 3,
        "error" => 4,
        _ => 2,
    }
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

fn now() -> DateTime<Utc> {
    Utc::now()
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// State (single global instance so `push_log` and the module share one
// ring buffer even before the module is initialized)
// ---------------------------------------------------------------------------

pub struct LogsState {
    hooks: RwLock<Arc<HookBus>>,
    entries: DashMap<String, LogEntry>,
    seq: AtomicU64,
    settings: RwLock<LogsSettings>,
}

impl LogsState {
    fn new() -> Self {
        Self {
            hooks: RwLock::new(Arc::new(HookBus::new())),
            entries: DashMap::new(),
            seq: AtomicU64::new(1),
            settings: RwLock::new(LogsSettings::default()),
        }
    }

    fn bus(&self) -> Arc<HookBus> {
        self.hooks.read().expect("hooks lock poisoned").clone()
    }

    fn emit(&self, event: &str, data: Value) {
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

    fn enforce_limit(&self) {
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

/// The one shared log buffer. The module and every free ingestion
/// function below use this instance, so entries recorded before the
/// module finishes initializing are still visible afterwards.
static GLOBAL_STATE: OnceLock<Arc<LogsState>> = OnceLock::new();

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

// ---------------------------------------------------------------------------
// Tracing subscriber bridge
// ---------------------------------------------------------------------------

/// Collects the fields of one tracing event as debug strings.
struct FieldVisitor {
    fields: Vec<(String, String)>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields
            .push((field.name().to_string(), format!("{value:?}")));
    }
}

/// A `tracing_subscriber::Layer` that forwards every tracing event into
/// the shared log buffer, so application logs written with `tracing`
/// or `log` appear in the log viewer. It never logs anything itself,
/// so it cannot recurse.
///
/// Integrator wiring (in the logging setup, next to the `fmt` layer):
///
/// ```rust
/// use tracing_subscriber::{prelude::*, Registry};
/// let subscriber = Registry::default()
///     .with(tracing_subscriber::fmt::layer().with_env_filter(filter))
///     .with(crate::modules::logs::LogsLayer);
/// let _ = tracing::subscriber::set_global_default(subscriber);
/// ```
pub struct LogsLayer;

impl<S> tracing_subscriber::Layer<S> for LogsLayer
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let metadata = event.metadata();
        let level = metadata.level().as_str().to_lowercase();
        let source = metadata.target().to_string();
        let mut visitor = FieldVisitor { fields: Vec::new() };
        event.record(&mut visitor);
        let mut message = String::new();
        let mut context = serde_json::Map::new();
        for (name, value) in visitor.fields {
            if name == "message" {
                message = value.trim_matches('"').to_string();
            } else {
                context.insert(name, Value::String(value));
            }
        }
        if message.is_empty() {
            message = format!("Event at {}", metadata.target());
        }
        push_log_full(
            &level,
            &source,
            &message,
            Value::Object(context),
            None,
            None,
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn paginate<T: Serialize + Clone>(items: Vec<T>, params: &PaginationParams) -> PageResult<T> {
    let pagination = params.normalized();
    let total = items.len() as u64;
    let start = ((pagination.page - 1) * pagination.per_page) as usize;
    let page_items: Vec<T> = items
        .into_iter()
        .skip(start)
        .take(pagination.per_page as usize)
        .collect();
    PageResult::new(page_items, total, &pagination)
}

fn matches_search(haystack: &str, params: &PaginationParams) -> bool {
    match &params.search {
        Some(needle) if !needle.trim().is_empty() => haystack
            .to_lowercase()
            .contains(&needle.trim().to_lowercase()),
        _ => true,
    }
}

fn parse_time(value: &str) -> UnifiedResult<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(value)
        .map(|parsed| parsed.with_timezone(&Utc))
        .map_err(|_| {
            UnifiedError::bad_request(format!("Invalid RFC 3339 timestamp: {value}"))
        })
}

/// Apply the shared list filters (level, source, kind, search, date
/// range) to the buffered entries, newest first by default sort.
fn filtered_entries(
    state: &LogsState,
    params: &PaginationParams,
) -> UnifiedResult<Vec<LogEntry>> {
    let from = match params.filters.get("from") {
        Some(value) => Some(parse_time(value)?),
        None => None,
    };
    let to = match params.filters.get("to") {
        Some(value) => Some(parse_time(value)?),
        None => None,
    };
    let sort = params.allowed_sort(&["timestamp", "level", "source"], "timestamp");
    let mut items: Vec<LogEntry> = state
        .entries
        .iter()
        .map(|entry| entry.value().clone())
        .filter(|entry| {
            params
                .filters
                .get("level")
                .is_none_or(|level| entry.level == *level)
                && params
                    .filters
                    .get("source")
                    .is_none_or(|source| entry.source == *source)
                && params
                    .filters
                    .get("kind")
                    .is_none_or(|kind| entry.kind == *kind)
                && params.filters.get("request_id").is_none_or(|id| {
                    entry.request_id.as_deref() == Some(id.as_str())
                })
                && params.filters.get("user_id").is_none_or(|id| {
                    entry.user_id.as_deref() == Some(id.as_str())
                })
                && from.is_none_or(|from| entry.timestamp >= from)
                && to.is_none_or(|to| entry.timestamp <= to)
                && matches_search(
                    &format!(
                        "{} {} {} {}",
                        entry.message,
                        entry.source,
                        entry.request_id.clone().unwrap_or_default(),
                        entry.path.clone().unwrap_or_default()
                    ),
                    params,
                )
        })
        .collect();
    match sort.as_str() {
        "level" => items.sort_by_key(|entry| level_rank(&entry.level)),
        "source" => items.sort_by(|a, b| a.source.cmp(&b.source)),
        _ => items.sort_by(|a, b| a.timestamp.cmp(&b.timestamp).then(a.seq.cmp(&b.seq))),
    }
    if params.normalized().order == "desc" {
        items.reverse();
    }
    Ok(items)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn list_logs(
    state: web::Data<Arc<LogsState>>,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    let params = query.into_inner();
    match filtered_entries(&state, &params) {
        Ok(items) => {
            let page = paginate(items, &params);
            json::success_response(page, 200)
        }
        Err(error) => json::error_response(&error, false),
    }
}

async fn get_log(state: web::Data<Arc<LogsState>>, path: web::Path<String>) -> HttpResponse {
    let result: UnifiedResult<LogEntry> = state
        .entries
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Log entry not found"));
    json::respond(result, false)
}

async fn get_counts(state: web::Data<Arc<LogsState>>) -> HttpResponse {
    let mut by_level: HashMap<String, u64> = LOG_LEVELS
        .iter()
        .map(|level| (level.to_string(), 0))
        .collect();
    let mut by_source: HashMap<String, u64> = HashMap::new();
    let mut by_kind: HashMap<String, u64> = LOG_KINDS
        .iter()
        .map(|kind| (kind.to_string(), 0))
        .collect();
    let mut total = 0u64;
    for entry in state.entries.iter() {
        let entry = entry.value();
        total += 1;
        *by_level.entry(entry.level.clone()).or_insert(0) += 1;
        *by_source.entry(entry.source.clone()).or_insert(0) += 1;
        *by_kind.entry(entry.kind.clone()).or_insert(0) += 1;
    }
    json::success_response(
        json!({
            "total": total,
            "by_level": by_level,
            "by_source": by_source,
            "by_kind": by_kind,
        }),
        200,
    )
}

async fn get_settings(state: web::Data<Arc<LogsState>>) -> HttpResponse {
    let settings = state.settings();
    json::success_response(
        json!({
            "retention_days": settings.retention_days,
            "max_entries": settings.max_entries,
            "total_entries": state.entries.len(),
        }),
        200,
    )
}

async fn update_settings(
    state: web::Data<Arc<LogsState>>,
    body: web::Json<UpdateLogsSettings>,
) -> HttpResponse {
    let result: UnifiedResult<LogsSettings> = (|| {
        let input = body.into_inner();
        if let Some(days) = input.retention_days {
            if days == 0 || days > 3650 {
                return Err(UnifiedError::bad_request(
                    "retention_days must be between 1 and 3650",
                ));
            }
        }
        if let Some(max) = input.max_entries {
            if !(100..=1_000_000).contains(&max) {
                return Err(UnifiedError::bad_request(
                    "max_entries must be between 100 and 1000000",
                ));
            }
        }
        let mut settings = state
            .settings
            .write()
            .expect("logs settings lock poisoned");
        if let Some(days) = input.retention_days {
            settings.retention_days = days;
        }
        if let Some(max) = input.max_entries {
            settings.max_entries = max;
        }
        let updated = settings.clone();
        drop(settings);
        state.enforce_limit();
        Ok(updated)
    })();
    json::respond(result, false)
}

async fn record_log(
    state: web::Data<Arc<LogsState>>,
    body: web::Json<RecordLogRequest>,
) -> HttpResponse {
    let result: UnifiedResult<LogEntry> = (|| {
        let input = body.into_inner();
        let level = normalize_level(&input.level)
            .ok_or_else(|| {
                UnifiedError::bad_request(
                    "level must be one of trace, debug, info, warn, error",
                )
            })?;
        if input.source.trim().is_empty() {
            return Err(UnifiedError::bad_request("source must not be empty"));
        }
        if input.message.trim().is_empty() {
            return Err(UnifiedError::bad_request("message must not be empty"));
        }
        let kind = match input.kind.as_deref() {
            None => "application".to_string(),
            Some(kind) if LOG_KINDS.contains(&kind) => kind.to_string(),
            Some(_) => {
                return Err(UnifiedError::bad_request(
                    "kind must be one of application, access, error",
                ));
            }
        };
        let entry = state.push(LogEntry {
            id: String::new(),
            seq: 0,
            timestamp: now(),
            level,
            kind,
            source: input.source.trim().to_string(),
            message: input.message.trim().to_string(),
            context: input.context.unwrap_or(Value::Null),
            request_id: input.request_id.filter(|id| !id.trim().is_empty()),
            user_id: input.user_id.filter(|id| !id.trim().is_empty()),
            method: None,
            path: None,
            status: None,
            duration_ms: None,
            ip: None,
        });
        Ok(entry)
    })();
    match result {
        Ok(entry) => json::success_response(entry, 201),
        Err(error) => json::error_response(&error, false),
    }
}

async fn prune_logs(
    state: web::Data<Arc<LogsState>>,
    body: web::Json<PruneRequest>,
) -> HttpResponse {
    let result: UnifiedResult<PruneReport> = (|| {
        if !body.into_inner().confirm {
            return Err(UnifiedError::unprocessable(
                "Pruning deletes log entries permanently; pass {\"confirm\": true} to confirm",
            ));
        }
        let report = state.prune();
        state.emit("logs.pruned", serde_json::to_value(&report).unwrap_or(Value::Null));
        Ok(report)
    })();
    json::respond(result, false)
}

async fn export_logs(
    state: web::Data<Arc<LogsState>>,
    query: web::Query<PaginationParams>,
    format_query: web::Query<ExportQuery>,
) -> HttpResponse {
    let params = query.into_inner();
    let format = format_query
        .into_inner()
        .format
        .unwrap_or_else(|| "json".to_string())
        .to_lowercase();
    if format != "json" && format != "ndjson" {
        return json::error_response(
            &UnifiedError::bad_request("format must be json or ndjson"),
            false,
        );
    }
    let items = match filtered_entries(&state, &params) {
        Ok(items) => items,
        Err(error) => return json::error_response(&error, false),
    };
    // A download, not an API payload, so the body is the raw export
    // rather than the {"data": ...} envelope.
    let date = now().format("%Y%m%d");
    let (body, content_type, filename) = if format == "ndjson" {
        let mut lines = String::new();
        for entry in &items {
            lines.push_str(&serde_json::to_string(entry).unwrap_or_default());
            lines.push('\n');
        }
        (
            lines,
            "application/x-ndjson",
            format!("logs-export-{date}.ndjson"),
        )
    } else {
        (
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string()),
            "application/json",
            format!("logs-export-{date}.json"),
        )
    };
    HttpResponse::Ok()
        .content_type(content_type)
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ))
        .body(body)
}

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

fn on_request_after(_state: Arc<LogsState>, ctx: &mut HookContext) {
    let data = &ctx.data;
    let method = data
        .get("method")
        .and_then(|value| value.as_str())
        .unwrap_or("-");
    let path = data
        .get("path")
        .and_then(|value| value.as_str())
        .unwrap_or("-");
    let status = data
        .get("status")
        .and_then(|value| value.as_u64())
        .unwrap_or(0) as u16;
    let duration_ms = data
        .get("duration_ms")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let ip = data
        .get("ip")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    record_access(method, path, status, duration_ms, ip);
}

fn on_logs_record(_state: Arc<LogsState>, ctx: &mut HookContext) {
    let data = &ctx.data;
    let Some(message) = data.get("message").and_then(|value| value.as_str()) else {
        return;
    };
    if message.trim().is_empty() {
        return;
    }
    let level = data
        .get("level")
        .and_then(|value| value.as_str())
        .unwrap_or("info");
    let source = data
        .get("source")
        .and_then(|value| value.as_str())
        .unwrap_or("application");
    let context = data.get("context").cloned().unwrap_or(Value::Null);
    let request_id = data
        .get("request_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    let user_id = data
        .get("user_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);
    push_log_full(level, source, message, context, request_id, user_id);
}

fn on_error_occurred(_state: Arc<LogsState>, ctx: &mut HookContext) {
    let data = &ctx.data;
    let message = data
        .get("message")
        .and_then(|value| value.as_str())
        .or_else(|| data.get("error").and_then(|value| value.as_str()))
        .unwrap_or("An error occurred");
    let source = data
        .get("source")
        .and_then(|value| value.as_str())
        .unwrap_or("application");
    record_error(source, message, data.clone());
}

fn on_cleanup_requested(state: Arc<LogsState>, _ctx: &mut HookContext) {
    let report = state.prune();
    state.emit("logs.pruned", serde_json::to_value(&report).unwrap_or(Value::Null));
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct LogsModule {
    state: Arc<LogsState>,
}

impl LogsModule {
    pub fn new() -> Self {
        Self {
            state: global_state(),
        }
    }
}

impl Default for LogsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for LogsModule {
    fn id(&self) -> &'static str {
        "logs"
    }

    fn name(&self) -> &'static str {
        "Logs"
    }

    fn description(&self) -> &'static str {
        "Central log viewer: application, access and error logs with search, counts, retention and export."
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            events::REQUEST_AFTER,
            "logs",
            Arc::new(move |ctx: &mut HookContext| on_request_after(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "logs.record",
            "logs",
            Arc::new(move |ctx: &mut HookContext| on_logs_record(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "error.occurred",
            "logs",
            Arc::new(move |ctx: &mut HookContext| on_error_occurred(state.clone(), ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "cleanup.requested",
            "logs",
            Arc::new(move |ctx: &mut HookContext| on_cleanup_requested(state.clone(), ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/logs")
                .route("", web::get().to(list_logs))
                .route("", web::post().to(record_log))
                .route("/counts", web::get().to(get_counts))
                .route("/export", web::get().to(export_logs))
                .route("/settings", web::get().to(get_settings))
                .route("/settings", web::put().to(update_settings))
                .route("/prune", web::post().to(prune_logs))
                .route("/{id}", web::get().to(get_log)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/logs", owner: "logs", description: "List log entries with pagination and filters." },
            RouteInfo { method: "POST", path: "/api/v1/logs", owner: "logs", description: "Record one application or error log entry." },
            RouteInfo { method: "GET", path: "/api/v1/logs/counts", owner: "logs", description: "Counts of log entries by level, source and kind." },
            RouteInfo { method: "GET", path: "/api/v1/logs/export", owner: "logs", description: "Download log entries as JSON or NDJSON." },
            RouteInfo { method: "GET", path: "/api/v1/logs/settings", owner: "logs", description: "Read log retention settings." },
            RouteInfo { method: "PUT", path: "/api/v1/logs/settings", owner: "logs", description: "Update log retention settings." },
            RouteInfo { method: "POST", path: "/api/v1/logs/prune", owner: "logs", description: "Prune log entries by retention (requires confirmation)." },
            RouteInfo { method: "GET", path: "/api/v1/logs/{id}", owner: "logs", description: "Get one log entry." },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LogsModule::new()));
}
