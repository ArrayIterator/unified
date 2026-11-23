//! panels module - Module registration: struct, Module impl, register.

use unified_core::hooks::HookBus;
use unified_core::modules::{Module, ModuleRegistry, RouteInfo};
use unified_core::context::AppContext;
use unified_core::error::UnifiedResult;
use crate::modules::ai;
use actix_web::web;
use std::sync::Arc;

use super::handlers_connections::*;
use super::handlers_records::*;
use super::registry::panel_registry;
use super::ai_capabilities::AI_CAPABILITIES;

// ---------------------------------------------------------------------------
// Module definition.
// ---------------------------------------------------------------------------

pub struct PanelsModule;

impl Module for PanelsModule {
    fn init(&self, _ctx: &AppContext) -> UnifiedResult<()> {
        ai::register_ai_capabilities("panels", AI_CAPABILITIES);
        Ok(())
    }

    fn id(&self) -> &'static str {
        "panels"
    }

    fn name(&self) -> &'static str {
        "Panel Integrations"
    }

    fn description(&self) -> &'static str {
        "Hosting and billing panel connectors: WHMCS, cPanel/WHM, DirectAdmin, Plesk, CyberPanel, and a configuration-driven generic panel, with a hookable adapter registry, masked credentials, in-memory import with two-way sync and conflict reporting."
    }

    fn register_hooks(&self, _bus: &HookBus) {
        // Imported records are stored data, not a cache, so they are
        // deliberately NOT cleared on `app.reload`: a reload must not
        // discard synced records. Hook wiring for this module is
        // emission only (see `panel.connected`, `panel.client.synced`,
        // `panel.service.synced`, `panel.sync.completed`,
        // `panel.sync.failed`, and `panel.sync.conflict`); the audit
        // module subscribes to those events for its trail.
    }

    fn configure_routes(&self, cfg: &mut web::ServiceConfig) {
        cfg.service(
            web::scope("/api/v1/panels")
                .route("", web::get().to(list_kinds))
                .route("/connections", web::get().to(list_connections))
                .route("/connections", web::post().to(create_connection))
                .route("/connections/{id}", web::get().to(get_connection))
                .route("/connections/{id}", web::patch().to(update_connection))
                .route("/connections/{id}", web::delete().to(delete_connection))
                .route("/connections/{id}/test", web::post().to(test_connection))
                .route(
                    "/connections/{id}/clients",
                    web::get().to(list_clients_records),
                )
                .route(
                    "/connections/{id}/products",
                    web::get().to(list_products_records),
                )
                .route(
                    "/connections/{id}/services",
                    web::get().to(list_services_records),
                )
                .route(
                    "/connections/{id}/domains",
                    web::get().to(list_domains_records),
                )
                .route(
                    "/connections/{id}/invoices",
                    web::get().to(list_invoices_records),
                )
                .route(
                    "/connections/{id}/server-accounts",
                    web::get().to(list_server_accounts_records),
                )
                .route(
                    "/connections/{id}/records/{collection}/{remote_id}",
                    web::get().to(get_record),
                )
                .route("/connections/{id}/sync", web::post().to(sync_now))
                .route("/connections/{id}/sync-now", web::post().to(sync_now))
                .route(
                    "/connections/{id}/sync/{collection}",
                    web::post().to(sync_collection),
                )
                .route(
                    "/connections/{id}/sync-status",
                    web::get().to(get_sync_status),
                )
                .route(
                    "/connections/{id}/sync-status/refresh",
                    web::post().to(refresh_sync_status),
                )
                .route(
                    "/connections/{id}/records/{collection}/{remote_id}/local-change",
                    web::post().to(mark_local_changed),
                )
                .route(
                    "/connections/{id}/records/{collection}/{remote_id}/resolve-conflict",
                    web::post().to(resolve_conflict),
                ),
        );
    }

    fn route_info(&self) -> Vec<RouteInfo> {
        vec![
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels",
                owner: "panels",
                description: "List panel kinds and their capabilities.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections",
                owner: "panels",
                description: "List panel connections (credentials masked).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections",
                owner: "panels",
                description: "Register a panel connection (credentials write-only).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}",
                owner: "panels",
                description: "Get one panel connection (credentials masked).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "PATCH",
                path: "/api/v1/panels/connections/{id}",
                owner: "panels",
                description: "Update a panel connection (credentials merge, values never returned).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "DELETE",
                path: "/api/v1/panels/connections/{id}",
                owner: "panels",
                description: "Delete a panel connection and its imported records.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/test",
                owner: "panels",
                description: "Test a panel connection against the real panel API.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/clients",
                owner: "panels",
                description: "List imported clients for a connection (paginated, filterable).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/products",
                owner: "panels",
                description: "List imported products for a connection (paginated, filterable).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/services",
                owner: "panels",
                description: "List imported services for a connection (paginated, filterable).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/domains",
                owner: "panels",
                description: "List imported domains for a connection (paginated, filterable).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/invoices",
                owner: "panels",
                description: "List imported invoices for a connection (paginated, filterable).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/server-accounts",
                owner: "panels",
                description: "List imported server accounts for a connection (paginated, filterable).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/records/{collection}/{remote_id}",
                owner: "panels",
                description: "Get one imported record with its sync state.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync",
                owner: "panels",
                description: "Sync every supported collection for a connection now.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync-now",
                owner: "panels",
                description: "Manual sync-now: sync every supported collection for a connection.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync/{collection}",
                owner: "panels",
                description: "Sync one collection for a connection now.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "GET",
                path: "/api/v1/panels/connections/{id}/sync-status",
                owner: "panels",
                description: "Two-way sync status from stored state only (no live panel fetch).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/sync-status/refresh",
                owner: "panels",
                description: "Live sync status: fetches every supported collection from the panel and compares it with stored records.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/records/{collection}/{remote_id}/local-change",
                owner: "panels",
                description: "Flag a stored record as locally changed (optionally preferring the local version).",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
            RouteInfo {
                method: "POST",
                path: "/api/v1/panels/connections/{id}/records/{collection}/{remote_id}/resolve-conflict",
                owner: "panels",
                description: "Resolve a sync conflict with remote_wins or local_wins.",
                auth: unified_core::auth::RouteAuth::Authenticated,
            },
        ]
    }
}

pub fn register(registry: &ModuleRegistry) {
    // Force the built-in adapters to register before the module is used.
    let _ = panel_registry();
    registry.register(Arc::new(PanelsModule));
}
