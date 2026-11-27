// cloud module - Module definition.
//
// The cloud module is the canonical home of cloud hosting, provider
// connectors, containers, and app deployment. The legacy module ids
// `hosting` and `providers` stay registered as deprecated compat
// shims (see `src/modules/hosting.rs` / `src/modules/providers.rs`);
// their routes (`/api/v1/hosting/*`, `/api/v1/providers/*`) keep
// working and share the same handler functions and the same shared
// hosting state as the canonical `/api/v1/cloud/*` routes.
//
// The module is a sellable package (site-builder pattern):
// `is_core` is false, `available_as_product` is true, category
// `infrastructure`. Every handler behind these routes calls
// `require_cloud_entitlement` after its permission check, so a tenant
// without the cloud package gets a 403; the control plane (no company
// scope) is never gated.
//
// Hook subscriptions: the compat `hosting` and `providers` modules
// keep their existing `register_hooks` (each subscription is
// registered exactly once); this module adds none for them, so no
// event is ever observed twice.

use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::ai_capabilities::AI_CAPABILITIES;
use super::containers::{
    create_app, delete_app, get_app, list_apps, start_app, stop_app,
};
use super::handlers::{list_cloud_panels, list_cloud_ssl_providers};
use super::hosting::types::shared_state;
use super::hosting::types::HostingState;
use super::hosting::{
    handlers_accounts::{
        activate_account, cancel_account, create_account, delete_account, get_account,
        get_account_requirements, list_accounts, suspend_account, unsuspend_account,
        update_account,
    },
    handlers_plans::{create_plan, delete_plan, get_plan, list_plans, update_plan},
};
use super::providers::handlers::{
    change_server_password, create_connection, create_server, delete_connection,
    delete_server, get_connection, get_inventory_view, get_server_status, get_server_usage,
    list_connections, list_kinds, list_servers, renew_server, scale_server, start_server,
    stop_server, suspend_server, sync_now, test_connection, unsuspend_server,
};
use super::providers::provider_registry;
use super::virtualization::list_virtualization;
use crate::modules::ai;

// Module definition.

pub struct CloudModule {
    state: Arc<HostingState>,
}

impl CloudModule {
    pub fn new() -> Self {
        // Shared with the compat `hosting` module (see
        // `hosting::types::shared_state`): both scopes read and write
        // the same plans and accounts.
        Self {
            state: shared_state(),
        }
    }
}

impl Default for CloudModule {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for CloudModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("cloud", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "cloud"
    }

    fn name(&self) -> &'static str {
        "Cloud"
    }

    fn description(&self) -> &'static str {
        "Cloud hosting, provider connectors, containers, and app deployment: hosting plans and accounts sold as a package, cloud provider adapters (OpenStack, Docker, Kubernetes, Proxmox, and more) translating each platform into one uniform interface, container app deployment through those connectors, and a virtualization grouping view."
    }

    fn is_core(&self) -> bool {
        // Sellable package, not core: a tenant without the cloud
        // package gets a 403 on these routes (site-builder pattern).
        false
    }

    fn available_as_product(&self) -> bool {
        true
    }

    fn product_category(&self) -> Option<&'static str> {
        Some("infrastructure")
    }

    fn requires(&self) -> &[&'static str] {
        // Store (plans are sold as store products), Domains (accounts
        // and apps are bound to a registered domain), Network (domain
        // syntax checks and the DNS / TLS probe behind domain
        // verification). Same set as the hosting module.
        &["store", "domains", "network"]
    }

    fn requires_libraries(&self) -> &[&'static str] {
        &[
            "serde",
            "uuid",
            "chrono",
            "actix-web",
            "dashmap",
            "diesel",
            "reqwest",
            "async-trait",
        ]
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.app_data(web::Data::new(self.state.clone()));
        cfg.service(
            web::scope("/api/v1/cloud/hosting")
                .route("/plans", web::get().to(list_plans))
                .route("/plans", web::post().to(create_plan))
                .route("/plans/{id}", web::get().to(get_plan))
                .route("/plans/{id}", web::put().to(update_plan))
                .route("/plans/{id}", web::delete().to(delete_plan))
                .route("/accounts", web::get().to(list_accounts))
                .route("/accounts", web::post().to(create_account))
                .route("/accounts/{id}", web::get().to(get_account))
                .route("/accounts/{id}", web::put().to(update_account))
                .route("/accounts/{id}", web::delete().to(delete_account))
                .route(
                    "/accounts/{id}/requirements",
                    web::get().to(get_account_requirements),
                )
                .route(
                    "/accounts/{id}/activate",
                    web::post().to(activate_account),
                )
                .route(
                    "/accounts/{id}/suspend",
                    web::post().to(suspend_account),
                )
                .route(
                    "/accounts/{id}/unsuspend",
                    web::post().to(unsuspend_account),
                )
                .route(
                    "/accounts/{id}/cancel",
                    web::post().to(cancel_account),
                ),
        );
        cfg.service(
            web::scope("/api/v1/cloud/providers")
                .route("", web::get().to(list_kinds))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection))
                .route("/connections/{id}/servers", web::get().to(list_servers))
                .route("/connections/{id}/servers", web::post().to(create_server))
                .route(
                    "/connections/{id}/servers/{server_id}",
                    web::get().to(get_server_status),
                )
                .route(
                    "/connections/{id}/servers/{server_id}",
                    web::delete().to(delete_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/scale",
                    web::post().to(scale_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/start",
                    web::post().to(start_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/stop",
                    web::post().to(stop_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/suspend",
                    web::post().to(suspend_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/unsuspend",
                    web::post().to(unsuspend_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/renew",
                    web::post().to(renew_server),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/change-password",
                    web::post().to(change_server_password),
                )
                .route(
                    "/connections/{id}/servers/{server_id}/usage",
                    web::get().to(get_server_usage),
                )
                .route("/connections/{id}/inventory", web::get().to(get_inventory_view))
                .route("/connections/{id}/sync", web::post().to(sync_now)),
        );
        cfg.service(
            web::scope("/api/v1/cloud/apps")
                .route("", web::get().to(list_apps))
                .route("", web::post().to(create_app))
                .route("/{id}", web::get().to(get_app))
                .route("/{id}", web::delete().to(delete_app))
                .route("/{id}/start", web::post().to(start_app))
                .route("/{id}/stop", web::post().to(stop_app)),
        );
        cfg.service(
            web::scope("/api/v1/cloud")
                .route("/virtualization", web::get().to(list_virtualization))
                .route("/panels", web::get().to(list_cloud_panels))
                .route("/ssl-providers", web::get().to(list_cloud_ssl_providers)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        let mut routes = Vec::new();
        let mut add = |method: &'static str, path: &'static str, description: &'static str| {
            routes.push(RouteInfo {
                method,
                path,
                owner: "cloud",
                description,
                auth: unified_core::auth::RouteAuth::Authenticated,
            });
        };
        // Hosting (canonical; legacy aliases live in the compat module).
        add("GET", "/api/v1/cloud/hosting/plans", "List hosting plans");
        add("POST", "/api/v1/cloud/hosting/plans", "Create a hosting plan");
        add("GET", "/api/v1/cloud/hosting/plans/{id}", "Get one hosting plan");
        add("PUT", "/api/v1/cloud/hosting/plans/{id}", "Update a hosting plan");
        add("DELETE", "/api/v1/cloud/hosting/plans/{id}", "Delete a hosting plan");
        add("GET", "/api/v1/cloud/hosting/accounts", "List hosting accounts");
        add("POST", "/api/v1/cloud/hosting/accounts", "Create a hosting account");
        add("GET", "/api/v1/cloud/hosting/accounts/{id}", "Get one hosting account");
        add("PUT", "/api/v1/cloud/hosting/accounts/{id}", "Update a hosting account");
        add("DELETE", "/api/v1/cloud/hosting/accounts/{id}", "Delete a pending or cancelled hosting account");
        add("GET", "/api/v1/cloud/hosting/accounts/{id}/requirements", "Check a hosting account's activation requirements");
        add("POST", "/api/v1/cloud/hosting/accounts/{id}/activate", "Activate a hosting account after its requirements pass");
        add("POST", "/api/v1/cloud/hosting/accounts/{id}/suspend", "Suspend a hosting account");
        add("POST", "/api/v1/cloud/hosting/accounts/{id}/unsuspend", "Unsuspend a hosting account");
        add("POST", "/api/v1/cloud/hosting/accounts/{id}/cancel", "Cancel a hosting account");
        // Providers (canonical; legacy aliases live in the compat module).
        add("GET", "/api/v1/cloud/providers", "List provider kinds and their capabilities.");
        add("GET", "/api/v1/cloud/providers/connections", "List provider connections (credentials masked).");
        add("POST", "/api/v1/cloud/providers/connections", "Register a provider connection (credentials write-only).");
        add("GET", "/api/v1/cloud/providers/connections/{id}", "Get one provider connection (credentials masked).");
        add("DELETE", "/api/v1/cloud/providers/connections/{id}", "Delete a provider connection and its cached inventory.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/test", "Test a provider connection against the real provider API.");
        add("GET", "/api/v1/cloud/providers/connections/{id}/servers", "List servers / containers for a connection (paginated, cached).");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers", "Create a server where the provider supports it.");
        add("GET", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}", "Status of one server / container.");
        add("DELETE", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}", "Delete a server / container where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/scale", "Scale a server where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/start", "Start a container / VPS where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/stop", "Stop a container / VPS where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/suspend", "Suspend a server / container where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/unsuspend", "Unsuspend a server / container where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/renew", "Renew a server where the provider supports it.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/change-password", "Change a server password where the provider supports it.");
        add("GET", "/api/v1/cloud/providers/connections/{id}/servers/{server_id}/usage", "Usage metrics for a server / container where the provider supports it.");
        add("GET", "/api/v1/cloud/providers/connections/{id}/inventory", "Cached inventory for a connection with fetch metadata.");
        add("POST", "/api/v1/cloud/providers/connections/{id}/sync", "Refresh cached inventory now and emit sync events.");
        // Apps (deploy applications through the connectors).
        add("GET", "/api/v1/cloud/apps", "List deployed apps.");
        add("POST", "/api/v1/cloud/apps", "Deploy an app through a Docker or Kubernetes connection.");
        add("GET", "/api/v1/cloud/apps/{id}", "Get one deployed app (live status refresh).");
        add("DELETE", "/api/v1/cloud/apps/{id}", "Delete a deployed app and its remote resource.");
        add("POST", "/api/v1/cloud/apps/{id}/start", "Start a deployed app (Docker only).");
        add("POST", "/api/v1/cloud/apps/{id}/stop", "Stop a deployed app (Docker only).");
        // Virtualization grouping + hook-collected connector catalogues.
        add("GET", "/api/v1/cloud/virtualization", "Virtualization connectors and their cached inventory counts.");
        add("GET", "/api/v1/cloud/panels", "Cloud panel connector definitions (hook-collected).");
        add("GET", "/api/v1/cloud/ssl-providers", "SSL provider definitions (hook-collected).");
        routes
    }
}

pub fn register(registry: &ModuleRegistry) {
    // Force the built-in adapters to register before the module is used.
    let _ = provider_registry();
    registry.register(Arc::new(CloudModule::new()));
}
