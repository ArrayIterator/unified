// Audit module - module registration and routes.
//
// Split from `src/modules/audit.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::constants::{AUDITED_EVENTS};
use super::handlers::{get_entry, list_entries, verify_chain};
use super::state::{AuditState, DEFAULT_WATERMARK_PATH, WATERMARK_PATH_ENV};
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

pub struct AuditModule {
    pub(crate) state: Arc<AuditState>,
}

impl AuditModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(AuditState::new()),
        }
    }
}

impl Default for AuditModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for AuditModule {
    fn id(&self) -> &'static str {
        "audit"
    }

    fn name(&self) -> &'static str {
        "Audit Log"
    }

    fn description(&self) -> &'static str {
        "Append-only audit log of major events across the application."
    }

    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("audit", AI_CAPABILITIES);
        // Durable chain-head/sequence watermark: a restart resumes
        // after the last persisted sequence instead of silently
        // restarting at 1. The path is overridable through
        // `UNIFIED_AUDIT_WATERMARK_PATH`; loading is best-effort and
        // never fails startup.
        let path = std::env::var(WATERMARK_PATH_ENV)
            .ok()
            .filter(|value| !value.trim().is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from(DEFAULT_WATERMARK_PATH));
        self.state.enable_persistence(path);
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        for event in AUDITED_EVENTS {
            let event_name = (*event).to_string();
            let state = self.state.clone();
            let handler: HookHandler = Arc::new(move |ctx: &mut HookContext| {
                state.record(&event_name, &ctx.data.clone());
            });
            bus.register(event, "audit", handler);
        }
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/audit/entries", owner: "audit", description: "List entries", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/audit/entries/{id}", owner: "audit", description: "Get entry", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/audit/verify", owner: "audit", description: "Verify hash chain", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/audit")
                .route("/entries", web::get().to(list_entries))
                .route("/entries/{id}", web::get().to(get_entry))
                .route("/verify", web::get().to(verify_chain)),
        );
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(AuditModule::new()));
}
