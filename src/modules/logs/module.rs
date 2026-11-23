// logs — module.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{events, HookBus, HookContext};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
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
use super::hooks::*;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct LogsModule {
    pub(crate) state: Arc<LogsState>,
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

        ai::register_ai_capabilities("logs", AI_CAPABILITIES);
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
            RouteInfo { method: "GET", path: "/api/v1/logs", owner: "logs", description: "List log entries with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logs", owner: "logs", description: "Record one application or error log entry.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logs/counts", owner: "logs", description: "Counts of log entries by level, source and kind.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logs/export", owner: "logs", description: "Download log entries as JSON or NDJSON.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logs/settings", owner: "logs", description: "Read log retention settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/logs/settings", owner: "logs", description: "Update log retention settings.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/logs/prune", owner: "logs", description: "Prune log entries by retention (requires confirmation).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/logs/{id}", owner: "logs", description: "Get one log entry.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(LogsModule::new()));
}
