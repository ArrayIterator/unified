// providers module -- Module definition, registration, and route table.

use unified_core::hooks::{events, HookBus};
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use actix_web::web;
use std::sync::Arc;

use super::*;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct ProvidersModule;

impl Module for ProvidersModule {
    fn id(&self) -> &'static str {
        "providers"
    }

    fn name(&self) -> &'static str {
        "Cloud Providers"
    }

    fn description(&self) -> &'static str {
        "Cloud, VPS, and container provider connectors: Hetzner, OVH, Tencent, Huawei, AWS EC2, Google Compute, Virtualizor, Docker, Kubernetes, Redshift, DigitalOcean, Linode, OpenStack, Proxmox VE, KVM (via Proxmox), XenServer / XCP-ng, and config-driven custom providers, with a hookable adapter registry, cached inventory, and sync."
    }

    fn register_hooks(&self, bus: &HookBus) {
        // Wiring to the service monitor through hooks only: a config
        // reload (emitted by the service monitor reload endpoint)
        // invalidates the cached inventory, since provider
        // configuration may have changed.
        bus.register(
            events::APP_RELOAD,
            "providers",
            std::sync::Arc::new(|_context| {
                inventory_store().clear();
            }),
        );
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/providers")
                .route("", web::get().to(list_kinds))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection))
                .route("/connections/{id}/servers", web::get().to(list_servers))
                .route("/connections/{id}/servers", web::post().to(create_server))
                .route("/connections/{id}/servers/{server_id}", web::get().to(get_server_status))
                .route("/connections/{id}/servers/{server_id}", web::delete().to(delete_server))
                .route("/connections/{id}/servers/{server_id}/scale", web::post().to(scale_server))
                .route("/connections/{id}/servers/{server_id}/start", web::post().to(start_server))
                .route("/connections/{id}/servers/{server_id}/stop", web::post().to(stop_server))
                .route("/connections/{id}/servers/{server_id}/suspend", web::post().to(suspend_server))
                .route("/connections/{id}/servers/{server_id}/unsuspend", web::post().to(unsuspend_server))
                .route("/connections/{id}/servers/{server_id}/renew", web::post().to(renew_server))
                .route("/connections/{id}/servers/{server_id}/change-password", web::post().to(change_server_password))
                .route("/connections/{id}/servers/{server_id}/usage", web::get().to(get_server_usage))
                .route("/connections/{id}/inventory", web::get().to(get_inventory_view))
                .route("/connections/{id}/sync", web::post().to(sync_now)),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers",
                owner: "providers",
                description: "List provider kinds and their capabilities.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections",
                owner: "providers",
                description: "List provider connections (credentials masked).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections",
                owner: "providers",
                description: "Register a provider connection (credentials write-only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}",
                owner: "providers",
                description: "Get one provider connection (credentials masked).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/providers/connections/{id}",
                owner: "providers",
                description: "Delete a provider connection and its cached inventory.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/test",
                owner: "providers",
                description: "Test a provider connection against the real provider API.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/servers",
                owner: "providers",
                description: "List servers / containers for a connection (paginated, cached).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers",
                owner: "providers",
                description: "Create a server where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}",
                owner: "providers",
                description: "Status of one server / container.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}",
                owner: "providers",
                description: "Delete a server / container where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/scale",
                owner: "providers",
                description: "Scale a server where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/start",
                owner: "providers",
                description: "Start a container / VPS where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/stop",
                owner: "providers",
                description: "Stop a container / VPS where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/suspend",
                owner: "providers",
                description: "Suspend a server / container where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/unsuspend",
                owner: "providers",
                description: "Unsuspend a server / container where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/renew",
                owner: "providers",
                description: "Renew a server where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/change-password",
                owner: "providers",
                description: "Change a server password where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/servers/{server_id}/usage",
                owner: "providers",
                description: "Usage metrics for a server / container where the provider supports it.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/providers/connections/{id}/inventory",
                owner: "providers",
                description: "Cached inventory for a connection with fetch metadata.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/providers/connections/{id}/sync",
                owner: "providers",
                description: "Refresh cached inventory now and emit sync events.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    // Force the built-in adapters to register before the module is used.
    let _ = provider_registry();
    registry.register(Arc::new(ProvidersModule));
}
