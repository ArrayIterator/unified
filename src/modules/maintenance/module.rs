// Maintenance module - module definition and registration.
// Split from the former single-file module; items keep their original names and behaviour. Private items are `pub(crate)` so sibling files in this module can use them.

use unified_core::context::AppContext;
use unified_core::error::{UnifiedError, UnifiedResult};
use unified_core::hooks::{HookBus, HookContext, HookHandler};
use unified_core::json;
use unified_core::modules::{Module, ModuleRegistry};
use unified_core::pagination::{PageResult, PaginationParams};
use crate::modules::ai;
use actix_web::{web, HttpResponse};
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, RwLock};
use uuid::Uuid;
use super::*;

// Module

pub struct MaintenanceModule {
    pub(crate) state: Arc<MaintenanceState>,
}

impl MaintenanceModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(MaintenanceState::new()),
        }
    }
}

impl Default for MaintenanceModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for MaintenanceModule {
    fn id(&self) -> &'static str {
        "maintenance"
    }

    fn name(&self) -> &'static str {
        "Maintenance Windows"
    }

    fn description(&self) -> &'static str {
        "Scheduled maintenance windows with time-based activation, a global maintenance-mode flag, subscriber notifications, and history."
    }

    fn requires(&self) -> &[&'static str] {
        &["notifications", "scheduler"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("maintenance", AI_CAPABILITIES);
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = ctx.hooks.clone();
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        let handler: HookHandler =
            Arc::new(move |ctx: &mut HookContext| on_maintenance_check(state.clone(), ctx));
        bus.register("maintenance.check", "maintenance", handler);
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/maintenance")
                .route("/windows", web::get().to(list_windows))
                .route("/windows", web::post().to(create_window))
                .route("/windows/{id}", web::get().to(get_window))
                .route("/windows/{id}", web::put().to(update_window))
                .route("/windows/{id}", web::delete().to(delete_window))
                .route("/windows/{id}/activate", web::post().to(activate_window))
                .route("/windows/{id}/deactivate", web::post().to(deactivate_window))
                .route("/windows/{id}/cancel", web::post().to(cancel_window))
                .route("/status", web::get().to(current_status))
                .route("/history", web::get().to(list_history))
                .route("/tick", web::post().to(run_tick))
                .route("/mode", web::post().to(set_maintenance_mode)),
        );
    }

    fn route_info(&self) -> Vec<unified_core::modules::RouteInfo> {
        use unified_core::modules::RouteInfo;
        vec![
            RouteInfo { method: "GET", path: "/api/v1/maintenance/windows", owner: "maintenance", description: "List maintenance windows (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows", owner: "maintenance", description: "Create a maintenance window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/maintenance/windows/{id}", owner: "maintenance", description: "Get one maintenance window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/maintenance/windows/{id}", owner: "maintenance", description: "Update a scheduled maintenance window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/maintenance/windows/{id}", owner: "maintenance", description: "Delete a non-active maintenance window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows/{id}/activate", owner: "maintenance", description: "Activate a scheduled window now.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows/{id}/deactivate", owner: "maintenance", description: "End an active window now.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/maintenance/windows/{id}/cancel", owner: "maintenance", description: "Cancel a scheduled window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/maintenance/status", owner: "maintenance", description: "Current maintenance status, active and next window.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/maintenance/history", owner: "maintenance", description: "Paginated maintenance history.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/maintenance/tick", owner: "maintenance", description: "Run time-based window transitions now.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/maintenance/mode", owner: "maintenance", description: "Set the global maintenance-mode flag directly.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(MaintenanceModule::new()));
}
