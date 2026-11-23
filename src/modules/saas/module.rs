// saas module -- Module definition, registration, and route table.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use crate::modules::ai;
use actix_web::web;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use super::*;

pub struct SaasModule {
    pub(crate) state: Arc<SaasState>,
}

impl SaasModule {
    pub fn new() -> Self {
        Self {
            state: Arc::new(SaasState::new()),
        }
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(SaasModule::new()));
}

impl Module for SaasModule {
    fn id(&self) -> &'static str {
        "saas"
    }

    fn name(&self) -> &'static str {
        "SaaS Multi-Site"
    }

    fn description(&self) -> &'static str {
        "SaaS multi-site provisioning: per-company sites with their own domain, theme, SSL status, and settings namespace."
    }

    fn dependencies(&self) -> Vec<&'static str> {
        vec!["company", "domains", "theme"]
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {

        ai::register_ai_capabilities("saas", AI_CAPABILITIES);
        self.state.debug.store(ctx.is_debug(), Ordering::SeqCst);
        self.state.set_hooks(ctx.hooks.clone());
        Ok(())
    }

    fn register_hooks(&self, _bus: &HookBus) {}

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/saas/sites", owner: "saas", description: "List sites", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas/sites", owner: "saas", description: "Create site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas/sites/{id}", owner: "saas", description: "Get site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/saas/sites/{id}", owner: "saas", description: "Update site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/saas/sites/{id}", owner: "saas", description: "Delete site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas/sites/{id}/suspend", owner: "saas", description: "Suspend site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "POST", path: "/api/v1/saas/sites/{id}/activate", owner: "saas", description: "Activate site", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/saas/sites/{id}/settings", owner: "saas", description: "Get site settings", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/saas/sites/{id}/settings", owner: "saas", description: "Update site settings", auth: unified_core::auth::RouteAuth::Authenticated},
        ]
    }

    fn configure_routes(&self, cfg: &mut actix_web::web::ServiceConfig) {
        let state = self.state.clone();
        cfg.service(
            web::scope("/api/v1/saas")
                .app_data(web::Data::new(state))
                .route("/sites", web::get().to(list_sites))
                .route("/sites", web::post().to(create_site))
                .route("/sites/{id}", web::get().to(get_site))
                .route("/sites/{id}", web::put().to(update_site))
                .route("/sites/{id}", web::delete().to(delete_site))
                .route("/sites/{id}/suspend", web::post().to(suspend_site))
                .route("/sites/{id}/activate", web::post().to(activate_site))
                .route("/sites/{id}/settings", web::get().to(get_site_settings))
                .route("/sites/{id}/settings", web::put().to(update_site_settings)),
        );
    }
}
