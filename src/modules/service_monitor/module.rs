// service_monitor module -- Module definition, registration, and route table.

use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::*;

pub struct ServiceMonitorModule;

impl ServiceMonitorModule {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ServiceMonitorModule {
    fn default() -> Self {
        Self::new()
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(std::sync::Arc::new(ServiceMonitorModule::new()));
}

impl Module for ServiceMonitorModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("service-monitor", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "service-monitor"
    }

    fn name(&self) -> &'static str {
        "Service Monitor"
    }

    fn description(&self) -> &'static str {
        "Monitors process health, tracked services and server information, reloads the configuration, and checks the external system libraries modules require, locally and over SSH."
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/status", owner: "service-monitor", description: "Status", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/services", owner: "service-monitor", description: "List services", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/processes", owner: "service-monitor", description: "Top processes by CPU (default) or memory (`?sort=memory`), up to `?limit=` rows (default 10, clamped to 50).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/service-monitor/reload", owner: "service-monitor", description: "Reload", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/info", owner: "service-monitor", description: "Info", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/libraries", owner: "service-monitor", description: "List the latest external library check results (paginated, filter by status/target). Runs a fresh local check when nothing has been checked yet.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/service-monitor/libraries/check", owner: "service-monitor", description: "Re-check external libraries locally and on remote SSH servers; emits library.check.failed per missing/outdated library.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/libraries/reports", owner: "service-monitor", description: "List the latest library check report per target (local first, then remote targets).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/service-monitor/ssh-servers", owner: "service-monitor", description: "List registered SSH servers for remote library checks (no secret material is stored).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/service-monitor/ssh-servers", owner: "service-monitor", description: "Register (or replace) an SSH server for remote library checks.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/service-monitor/ssh-servers/{id}", owner: "service-monitor", description: "Remove a registered SSH server.", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/service-monitor")
                .route("/status", web::get().to(status))
                .route("/services", web::get().to(list_services))
                .route("/processes", web::get().to(list_processes))
                .route("/reload", web::post().to(reload))
                .route("/info", web::get().to(info))
                .route("/libraries", web::get().to(list_libraries))
                .route("/libraries/check", web::post().to(recheck_libraries))
                .route("/libraries/reports", web::get().to(list_library_reports))
                .route("/ssh-servers", web::get().to(list_registered_ssh_servers))
                .route("/ssh-servers", web::post().to(register_ssh_server_endpoint))
                .route("/ssh-servers/{id}", web::delete().to(remove_ssh_server_endpoint)),
        );
    }
}
