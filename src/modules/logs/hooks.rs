// logs — hooks.

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
use super::service::*;
use super::tracing::*;
use super::common::*;
use super::handlers::*;
use super::module::*;

// ---------------------------------------------------------------------------
// Hook handlers
// ---------------------------------------------------------------------------

pub(crate) fn on_request_after(_state: Arc<LogsState>, ctx: &mut HookContext) {
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

pub(crate) fn on_logs_record(_state: Arc<LogsState>, ctx: &mut HookContext) {
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

pub(crate) fn on_error_occurred(_state: Arc<LogsState>, ctx: &mut HookContext) {
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

pub(crate) fn on_cleanup_requested(state: Arc<LogsState>, _ctx: &mut HookContext) {
    let report = state.prune();
    state.emit("logs.pruned", serde_json::to_value(&report).unwrap_or(Value::Null));
}
