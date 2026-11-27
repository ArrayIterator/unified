// records - Module registration: struct, defaults, routes, and hooks.

use super::*;
use unified_core::hooks::{HookBus, HookContext, HookHandler, events};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

// Module.

pub struct RecordsModule {
    state: Arc<RecordsState>,
}

impl RecordsModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(RecordsState::new()),
        }
    }
}

impl Default for RecordsModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for RecordsModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("records", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "records"
    }

    fn name(&self) -> &'static str {
        "Records"
    }

    fn description(&self) -> &'static str {
        "Queryable system-records store behind the audit trail: who did what, when, on which entity. Append-only; records are created from hook events and removed only by a confirmed retention purge."
    }

    fn requires(&self) -> &[&'static str] {
        &["audit", "tasks"]
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in RECORDED_EVENTS {
            let event_name = (*event).to_string();
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                state.record(&event_name, &ctx.data.clone());
            });
            bus.register(event, "records", handler);
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/records", owner: "records", description: "List records with pagination and filters.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/records/{id}", owner: "records", description: "Get one record.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/records/counts/by-entity", owner: "records", description: "Record counts grouped by entity type.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/records/counts/by-action", owner: "records", description: "Record counts grouped by action.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/records/export", owner: "records", description: "Export records as a JSON download.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/records/retention", owner: "records", description: "Show the retention setting and store size.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/records/purge", owner: "records", description: "Purge records older than keep_days (requires confirm=true).", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/records")
                // Static routes first: they must win over `/{id}`.
                .route("", web::get().to(list_records))
                .route("/counts/by-entity", web::get().to(counts_by_entity))
                .route("/counts/by-action", web::get().to(counts_by_action))
                .route("/export", web::get().to(export_records))
                .route("/retention", web::get().to(retention_view))
                .route("/purge", web::post().to(purge_records))
                .route("/{id}", web::get().to(get_record)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(RecordsModule::new()));
}
