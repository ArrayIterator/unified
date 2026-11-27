// Whitelabel module - module registration and routes.
//
// Split from `src/modules/whitelabel.rs`; the module root re-exports the public API of this module.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::hooks::{HookBus, HookContext};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo, RouteVersionSupport};
use actix_web::web;
use std::sync::Arc;

use super::constants::{HOOK_ALLOWANCE_CHECK, HOOK_BRANDING_RESOLVE};
use super::events::{on_allowance_check, on_branding_resolve, on_domain_verify};
use super::handlers_admin::{get_admin_allowance, get_admin_defaults, get_admin_profile, get_company_allowance, list_admin_history, list_admin_profiles, put_admin_defaults, put_company_allowance, put_global_allowance};
use super::handlers_branding::{delete_my_branding, get_my_allowance, get_my_branding, get_public_branding, list_my_history, put_my_branding};
use super::state::{WhitelabelState};

// Module

pub struct WhitelabelModule {
    pub(crate) state: WhitelabelState,
}

impl WhitelabelModule {
    pub fn new() -> Self {
        Self {
            state: WhitelabelState::new(),
        }
    }
}

impl Default for WhitelabelModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for WhitelabelModule {
    fn id(&self) -> &'static str {
        "whitelabel"
    }

    fn name(&self) -> &'static str {
        "White Labelling"
    }

    fn description(&self) -> &'static str {
        "Admin-gated white labelling: global and per-company allowance, server-side branding resolution (tenant override, company default, global defaults), custom domains, email sender and login branding, with a full audit history."
    }

    fn requires(&self) -> &[&'static str] {
        &["theme", "portals", "mail", "domains"]
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn init(&self, ctx: &AppContext) -> UnifiedResult<()> {
        *self
            .state
            .hooks
            .write()
            .expect("hooks lock poisoned") = Some(Arc::clone(&ctx.hooks));
        Ok(())
    }

    fn register_hooks(&self, bus: &HookBus) {
        let state = self.state.clone();
        bus.register(
            HOOK_BRANDING_RESOLVE,
            "whitelabel",
            Arc::new(move |ctx: &mut HookContext| on_branding_resolve(&state, ctx)),
        );
        let state = self.state.clone();
        bus.register(
            HOOK_ALLOWANCE_CHECK,
            "whitelabel",
            Arc::new(move |ctx: &mut HookContext| on_allowance_check(&state, ctx)),
        );
        let state = self.state.clone();
        bus.register(
            "domain.verify",
            "whitelabel",
            Arc::new(move |ctx: &mut HookContext| on_domain_verify(&state, ctx)),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        let state = web::Data::new(self.state.clone());
        cfg.service(
            web::scope("/api/v1/whitelabel")
                .app_data(state)
                .route("/admin/allowance", web::get().to(get_admin_allowance))
                .route("/admin/allowance", web::put().to(put_global_allowance))
                .route(
                    "/admin/allowance/{company_id}",
                    web::get().to(get_company_allowance),
                )
                .route(
                    "/admin/allowance/{company_id}",
                    web::put().to(put_company_allowance),
                )
                .route("/admin/defaults", web::get().to(get_admin_defaults))
                .route("/admin/defaults", web::put().to(put_admin_defaults))
                .route("/admin/profiles", web::get().to(list_admin_profiles))
                .route(
                    "/admin/profiles/{company_id}",
                    web::get().to(get_admin_profile),
                )
                .route("/admin/history", web::get().to(list_admin_history))
                .route("/allowance", web::get().to(get_my_allowance))
                .route("/branding", web::get().to(get_my_branding))
                .route("/branding", web::put().to(put_my_branding))
                .route("/branding", web::delete().to(delete_my_branding))
                .route("/branding/history", web::get().to(list_my_history))
                .route(
                    "/public/branding/{company_id}",
                    web::get().to(get_public_branding),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/allowance", owner: "whitelabel", description: "List the global allowance plus per-company overrides (admin; paginated, filter by company_id, mode).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/admin/allowance", owner: "whitelabel", description: "Set the global default allowance (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/allowance/{company_id}", owner: "whitelabel", description: "Effective allowance for one company (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/admin/allowance/{company_id}", owner: "whitelabel", description: "Set the per-company allowance override: allowed, denied, or inherit (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/defaults", owner: "whitelabel", description: "Read the admin-editable global defaults record (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/admin/defaults", owner: "whitelabel", description: "Update the admin-editable global defaults record (admin).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/profiles", owner: "whitelabel", description: "List all branding profiles (admin; paginated, filter by company_id, tenant_id, domain_status).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/profiles/{company_id}", owner: "whitelabel", description: "Get one company's branding profile (admin; ?tenant_id= selects the tenant override).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/admin/history", owner: "whitelabel", description: "List the full branding history (admin; paginated, filter by company_id, action, allowed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/allowance", owner: "whitelabel", description: "The caller's own effective allowance.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/branding", owner: "whitelabel", description: "The caller's own server-resolved branding (platform defaults when not allowed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "PUT", path: "/api/v1/whitelabel/branding", owner: "whitelabel", description: "Save the caller's own branding profile (rejected with 403 + audit event when not allowed).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "DELETE", path: "/api/v1/whitelabel/branding", owner: "whitelabel", description: "Reset the caller's own branding to the platform defaults.", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/branding/history", owner: "whitelabel", description: "The caller's own branding history, scoped to the caller's company (paginated).", auth: unified_core::auth::RouteAuth::Authenticated},
            RouteInfo { method: "GET", path: "/api/v1/whitelabel/public/branding/{company_id}", owner: "whitelabel", description: "PUBLIC: server-resolved branding for a login/portal render; gated by the API Manager ApiAccessRule (default deny).", auth: unified_core::auth::RouteAuth::Public},
        ]
    }

    fn route_version_support(&self) -> RouteVersionSupport {
        RouteVersionSupport::All
    }
}

pub fn register(registry: &ModuleRegistry) {
    registry.register(Arc::new(WhitelabelModule::new()));
}
