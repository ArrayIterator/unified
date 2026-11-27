// logs - handlers.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use actix_web::{http::header, web, HttpRequest, HttpResponse};
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
use super::service::*;
use super::tracing::*;
use super::common::*;
use super::hooks::*;
use super::module::*;

// Handlers

pub(crate) async fn list_logs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_VIEW) {
        return json::error_response(&error, false);
    }
    let params = query.into_inner();
    match filtered_entries(&state, &params) {
        Ok(items) => {
            let page = paginate(items, &params);
            json::success_response(page, 200)
        }
        Err(error) => json::error_response(&error, false),
    }
}

pub(crate) async fn get_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
    path: web::Path<String>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_VIEW) {
        return json::error_response(&error, false);
    }
    let result: UnifiedResult<LogEntry> = state
        .entries
        .get(path.as_str())
        .map(|entry| entry.value().clone())
        .ok_or_else(|| UnifiedError::not_found("Log entry not found"));
    json::respond(result, false)
}

pub(crate) async fn get_counts(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_VIEW) {
        return json::error_response(&error, false);
    }
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

pub(crate) async fn get_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_VIEW) {
        return json::error_response(&error, false);
    }
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

pub(crate) async fn update_settings(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
    body: web::Json<UpdateLogsSettings>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_MANAGE) {
        return json::error_response(&error, false);
    }
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

pub(crate) async fn record_log(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
    body: web::Json<RecordLogRequest>,
) -> HttpResponse {
    // Forging log entries needs the manage capability: in-process
    // ingestion (`push_log`, the `logs.record` hook) is unaffected,
    // only this HTTP endpoint is gated.
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_MANAGE) {
        return json::error_response(&error, false);
    }
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
        check_record_log_limits(input.source.trim(), input.message.trim(), &input.context)?;
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

pub(crate) async fn prune_logs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
    body: web::Json<PruneRequest>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_MANAGE) {
        return json::error_response(&error, false);
    }
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

/// Serialize entries as NDJSON lines, skipping entries that fail
/// to serialize and counting them instead of emitting an empty line
/// for the failure.
pub(crate) fn ndjson_lines<T: Serialize>(items: &[T]) -> (String, usize) {
    let mut lines = String::new();
    let mut skipped = 0usize;
    for entry in items {
        match serde_json::to_string(entry) {
            Ok(line) => {
                lines.push_str(&line);
                lines.push('\n');
            }
            Err(_) => skipped += 1,
        }
    }
    (lines, skipped)
}

pub(crate) async fn export_logs(
    ctx: web::Data<Arc<AppContext>>,
    state: web::Data<Arc<LogsState>>,
    req: HttpRequest,
    query: web::Query<PaginationParams>,
    format_query: web::Query<ExportQuery>,
) -> HttpResponse {
    if let Err(error) = authorize(&ctx, &req, PERM_LOGS_VIEW) {
        return json::error_response(&error, false);
    }
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
    // SANCTIONED CARVE-OUT from the standard API envelope (see
    // `crates/unified-core/src/json.rs`): a download, not an API
    // payload, so the body is the raw export rather than the
    // {"data": ...} envelope. Errors before the download starts
    // still use the envelope.
    let date = now().format("%Y%m%d");
    let (body, content_type, filename, skipped) = if format == "ndjson" {
        let (lines, skipped) = ndjson_lines(&items);
        (
            lines,
            "application/x-ndjson",
            format!("logs-export-{date}.ndjson"),
            skipped,
        )
    } else {
        (
            serde_json::to_string_pretty(&items).unwrap_or_else(|_| "[]".to_string()),
            "application/json",
            format!("logs-export-{date}.json"),
            0,
        )
    };
    let mut response = HttpResponse::Ok();
    response
        .content_type(content_type)
        .insert_header((
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        ));
    if skipped > 0 {
        // Unserializable entries are skipped, never silently emitted
        // as empty lines; the count travels on a response header.
        response.insert_header(("x-logs-export-skipped", skipped.to_string()));
    }
    response.body(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FailingValue;

    impl Serialize for FailingValue {
        fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
        where
            S: serde::Serializer,
        {
            Err(<S::Error as serde::ser::Error>::custom("boom"))
        }
    }

    #[test]
    fn ndjson_export_skips_unserializable_entries_and_counts_them() {
        let (lines, skipped) = ndjson_lines(&[json!({ "id": 1 }), json!({ "id": 2 })]);
        assert_eq!(skipped, 0);
        assert_eq!(lines.lines().count(), 2);
        assert!(lines.contains("\"id\":1"));

        let (lines, skipped) = ndjson_lines(&[FailingValue, FailingValue]);
        assert_eq!(skipped, 2);
        assert!(lines.is_empty());

        // A mix keeps the good lines and reports only the bad ones.
        let good = json!({ "id": 1 });
        let (lines, skipped) = ndjson_lines(&[good, json!({ "id": 2 })]);
        assert_eq!(skipped, 0);
        assert_eq!(lines.lines().count(), 2);
    }
}
